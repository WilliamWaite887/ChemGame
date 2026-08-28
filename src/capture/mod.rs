//! A developer-only "capture mode" for taking clean marketing screenshots.
//!
//! Pressing `F9` does two things, both purely presentational:
//! - Hides every root-level UI panel (order queue, hotbar, vitals, room
//!   label, toasts, radio dispatch cards, any open machine panel) by setting
//!   [`Visibility::Hidden`] on it. Nothing is despawned, so each panel's own
//!   update systems keep running underneath and the HUD comes back exactly
//!   as it was the instant capture mode turns back off.
//! - Detaches the local player's camera from [`player::follow_chemist`] and
//!   flies it freely on WASD + mouse + Space/Ctrl instead.
//!
//! Not a spectator mode, and nothing here touches gameplay state: the
//! chemist you're actually playing keeps standing exactly where you left
//! them (the camera is "not parented to the body" for exactly this reason —
//! see [`player::PlayerCamera`]'s own doc comment), the running shift is
//! untouched, and every other player at the table sees nothing different at
//! all — this is local-only, client-side presentation.
//!
//! Known limitation, left alone deliberately: `fx`'s camera shake/tint layer
//! still runs after this module's own camera system (it orders itself
//! `.after(follow_chemist)` specifically so it composes on top of whatever
//! placed the camera this frame — see that `pub(crate)` note on
//! `follow_chemist`), so a status effect like irradiation shake will still
//! jitter a "clean" shot taken while the chemist is affected. Wait it out,
//! or step away from the source first.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use crate::player::PlayerCamera;
use crate::AppState;

const FLY_SPEED: f32 = 6.0;
const FLY_SPEED_FAST: f32 = 18.0;
const LOOK_SENSITIVITY: f32 = 0.0025;
/// Matches `player::PITCH_LIMIT` — just under 90°, so looking straight up or
/// down never flips the view. Not reused directly since that constant is
/// private to `player` and this is deliberately a self-contained module.
const PITCH_LIMIT: f32 = 1.54;
const TOGGLE_KEY: KeyCode = KeyCode::F9;

pub struct CapturePlugin;

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CaptureState>().add_systems(
            Update,
            (
                toggle_capture_mode,
                hide_hud,
                fly_camera.run_if(|state: Res<CaptureState>| state.free_camera),
            )
                .chain()
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// Whether the free camera is currently flying instead of following the
/// local chemist, plus the orientation it remembers between frames. A UI
/// `Node` has no yaw/pitch of its own the way `player::Look` does for the
/// real player, so this resource is where the free camera's look state
/// lives instead.
#[derive(Resource, Default)]
pub(crate) struct CaptureState {
    free_camera: bool,
    yaw: f32,
    pitch: f32,
}

/// Gate for [`player::follow_chemist`] and the player's own look/move
/// systems: true exactly when the real player should still be driving the
/// camera and their own body, i.e. capture mode is off. Public so `player`
/// can `run_if` on it without this module needing to know anything about
/// player systems in return.
pub fn player_follows_camera(state: Option<Res<CaptureState>>) -> bool {
    !state.is_some_and(|s| s.free_camera)
}

fn toggle_capture_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<CaptureState>,
    cameras: Query<&Transform, With<PlayerCamera>>,
) {
    if !keys.just_pressed(TOGGLE_KEY) {
        return;
    }
    state.free_camera = !state.free_camera;
    if state.free_camera {
        // Start flying from wherever the player was already looking, not
        // some arbitrary default orientation.
        if let Ok(transform) = cameras.single() {
            let (yaw, pitch, _roll) = transform.rotation.to_euler(EulerRot::YXZ);
            state.yaw = yaw;
            state.pitch = pitch;
        }
        info!("capture mode on — HUD hidden, camera free (F9 to return)");
    } else {
        info!("capture mode off");
    }
}

/// Hides every root-level UI panel while capture mode is on, and restores
/// them the instant it isn't.
///
/// Deliberately generic — a `Node` with no `ChildOf` is exactly "a top-level
/// UI panel" in this codebase, whatever module spawned it, so this needs no
/// per-panel marker and nothing to remember to tag when a new HUD element is
/// added later. Guarded so it only ever writes `Visibility` on the frame it
/// actually changes, matching how the rest of the codebase avoids marking a
/// component `Changed` (and, for a replicated one, re-sent) every frame for
/// no reason.
fn hide_hud(
    state: Res<CaptureState>,
    mut roots: Query<&mut Visibility, (With<Node>, Without<ChildOf>)>,
) {
    let wanted = if state.free_camera {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for mut visibility in &mut roots {
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// Flies the local player's camera on WASD (horizontal, yaw-relative) plus
/// Space/Ctrl (vertical) and the mouse (look), independent of pitch — so
/// looking down while flying forward doesn't nosedive into the floor the
/// way it would if movement followed the full look direction.
fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut motion: MessageReader<MouseMotion>,
    mut state: ResMut<CaptureState>,
    mut cameras: Query<&mut Transform, With<PlayerCamera>>,
) {
    let Ok(mut camera) = cameras.single_mut() else {
        return;
    };

    let delta: Vec2 = motion.read().map(|m| m.delta).sum();
    state.yaw -= delta.x * LOOK_SENSITIVITY;
    state.pitch = (state.pitch - delta.y * LOOK_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    camera.rotation = Quat::from_rotation_y(state.yaw) * Quat::from_rotation_x(state.pitch);

    let mut direction = Vec2::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        direction.y -= 1.0;
    }
    if keys.pressed(KeyCode::KeyS) {
        direction.y += 1.0;
    }
    if keys.pressed(KeyCode::KeyA) {
        direction.x -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) {
        direction.x += 1.0;
    }
    let mut vertical: f32 = 0.0;
    if keys.pressed(KeyCode::Space) {
        vertical += 1.0;
    }
    if keys.pressed(KeyCode::ControlLeft) {
        vertical -= 1.0;
    }

    let local = Vec3::new(direction.x, 0.0, direction.y).normalize_or_zero();
    let horizontal = Quat::from_rotation_y(state.yaw) * local;
    let speed = if keys.pressed(KeyCode::ShiftLeft) {
        FLY_SPEED_FAST
    } else {
        FLY_SPEED
    };
    camera.translation += (horizontal + Vec3::Y * vertical) * speed * time.delta_secs();
}
