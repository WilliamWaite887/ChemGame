//! First-person controller.
//!
//! Note the split between [`Player`] and [`LocalPlayer`]: the first is anyone
//! working the lab, the second is the one *this client* drives. Co-op adds
//! more `Player` entities without touching movement, and nothing here assumes
//! there is only one.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};

use crate::interaction::{Focus, InteractionMode};
use crate::lab::{Solid, ROOM_HALF_X, ROOM_HALF_Z};
use crate::AppState;

pub const EYE_HEIGHT: f32 = 1.7;

const PLAYER_RADIUS: f32 = 0.35;
const WALK_SPEED: f32 = 4.2;
const MOUSE_SENSITIVITY: f32 = 0.0022;
/// Just under 90°, so looking straight up or down never flips the view.
const PITCH_LIMIT: f32 = 1.54;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Playing), (spawn_local_player, grab_cursor))
            .add_systems(
                Update,
                (mouse_look, movement)
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Anyone working the lab. In co-op there will be more than one.
#[derive(Component)]
pub struct Player;

/// The player this client controls. Cursor grab, HUD and panels key off this.
#[derive(Component)]
pub struct LocalPlayer;

/// The camera parented to a player's head.
#[derive(Component)]
pub struct PlayerCamera;

/// Yaw lives on the body, pitch on the camera, so movement can use the body
/// rotation directly without cancelling out the pitch.
#[derive(Component, Default)]
pub struct Look {
    pub yaw: f32,
    pub pitch: f32,
}

fn spawn_local_player(mut commands: Commands) {
    let player = commands
        .spawn((
            Player,
            LocalPlayer,
            Look::default(),
            InteractionMode::default(),
            Focus::default(),
            Transform::from_xyz(0.0, EYE_HEIGHT, 2.6),
            Visibility::default(),
        ))
        .id();

    commands.spawn((
        Camera3d::default(),
        PlayerCamera,
        Transform::IDENTITY,
        ChildOf(player),
    ));
}

fn grab_cursor(mut cursor: Single<&mut CursorOptions>) {
    cursor.visible = false;
    cursor.grab_mode = CursorGrabMode::Locked;
}

fn mouse_look(
    mut motion: MessageReader<MouseMotion>,
    cursor: Single<&CursorOptions>,
    mut players: Query<(&mut Look, &mut Transform, &InteractionMode), With<LocalPlayer>>,
    mut cameras: Query<&mut Transform, (With<PlayerCamera>, Without<LocalPlayer>)>,
) {
    let delta: Vec2 = motion.read().map(|m| m.delta).sum();
    if cursor.grab_mode == CursorGrabMode::None || delta == Vec2::ZERO {
        return;
    }

    for (mut look, mut transform, mode) in &mut players {
        // Looking around while a machine panel is open would be disorienting;
        // routing it through the mode keeps camera and UI focus consistent.
        if !mode.is_roaming() {
            continue;
        }
        look.yaw -= delta.x * MOUSE_SENSITIVITY;
        look.pitch = (look.pitch - delta.y * MOUSE_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        transform.rotation = Quat::from_rotation_y(look.yaw);

        for mut camera in &mut cameras {
            camera.rotation = Quat::from_rotation_x(look.pitch);
        }
    }
}

fn movement(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut players: Query<(&mut Transform, &Look, &InteractionMode), With<Player>>,
    solids: Query<(&Transform, &Solid), Without<Player>>,
) {
    let mut input = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        input.z -= 1.0;
    }
    if keys.pressed(KeyCode::KeyS) {
        input.z += 1.0;
    }
    if keys.pressed(KeyCode::KeyA) {
        input.x -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) {
        input.x += 1.0;
    }

    for (mut transform, look, mode) in &mut players {
        if !mode.is_roaming() || input == Vec3::ZERO {
            continue;
        }
        let direction = Quat::from_rotation_y(look.yaw) * input.normalize();
        let mut position = transform.translation + direction * WALK_SPEED * time.delta_secs();
        position.y = EYE_HEIGHT;

        for (solid_transform, solid) in &solids {
            position = push_out(position, solid_transform.translation, solid.half_extents);
        }

        // Backstop in case a gap opens between wall colliders.
        let limit_x = ROOM_HALF_X - PLAYER_RADIUS;
        let limit_z = ROOM_HALF_Z - PLAYER_RADIUS;
        position.x = position.x.clamp(-limit_x, limit_x);
        position.z = position.z.clamp(-limit_z, limit_z);

        transform.translation = position;
    }
}

/// Pushes `position` out of an axis-aligned box, along whichever horizontal
/// axis it has penetrated least.
///
/// Only the XZ plane matters: the floor is flat and everything in the lab is
/// tall enough to block at eye height, so vertical resolution would only add
/// ways to get stuck.
fn push_out(position: Vec3, center: Vec3, half_extents: Vec3) -> Vec3 {
    let dx = position.x - center.x;
    let dz = position.z - center.z;
    let overlap_x = half_extents.x + PLAYER_RADIUS - dx.abs();
    let overlap_z = half_extents.z + PLAYER_RADIUS - dz.abs();

    if overlap_x <= 0.0 || overlap_z <= 0.0 {
        return position;
    }

    let mut resolved = position;
    if overlap_x < overlap_z {
        resolved.x += overlap_x * dx.signum();
    } else {
        resolved.z += overlap_z * dz.signum();
    }
    resolved
}
