//! Turns a `Bloodstream` into something you can see.
//!
//! Two halves, both presentation-only and client-side: (A) `HazardFelt` — a
//! one-shot camera kick declared back when hazards were built and never
//! consumed until now — and (B) continuous, every-frame presentation driven
//! by whatever is currently in your blood. Neither is replicated, and
//! neither ever touches the authoritative chemist `Transform` that
//! movement/interaction/replication depend on: the camera is a separate
//! entity (`player::PlayerCamera`), and every model wobble/tint here targets
//! a *child* mesh entity (`player::ChemistBody`/`crew::CrewBody`), never the
//! root.
//!
//! Runs on every peer, gated only by `AppState::Playing` — this is exactly
//! the "presentation everywhere, authority nowhere" split the rest of the
//! game already draws between state and how it looks.

use bevy::prelude::*;
use bevy::render::view::ColorGrading;
use chem_sim::{Bloodstream as ChemBloodstream, StatusKind};
use rand::prelude::*;

use crate::body::Bloodstream;
use crate::crew::CrewBody;
use crate::hazards::{HazardFelt, HazardKind};
use crate::player::{ChemistBody, LocalPlayer, PlayerCamera};
use crate::AppState;

pub struct FxPlugin;

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScreenFx>()
            .add_systems(OnEnter(AppState::Playing), build_overlay)
            .add_systems(
                Update,
                (
                    ensure_camera_fx_components,
                    apply_hazard_felt,
                    // Chained *after* `follow_chemist` (a cross-module
                    // ordering constraint, not a same-plugin `.chain()`):
                    // every effect here nudges the camera the base placement
                    // already set this frame, never replaces it.
                    apply_camera_fx.after(crate::player::follow_chemist),
                    animate_chemist_body,
                    animate_crew_body,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// One-shot hazard feedback, decaying every frame. Not replicated — this is
/// exactly the "presentation, not state" `HazardFelt` was declared to drive.
#[derive(Resource, Default)]
struct ScreenFx {
    shake: f32,
    flash_color: Color,
    flash_alpha: f32,
}

/// Marks the full-screen overlay `apply_camera_fx` paints flashes and
/// status vignettes onto.
#[derive(Component)]
struct ScreenOverlay;

fn build_overlay(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: px(0),
            left: px(0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        // Fully transparent until something actually happens to you.
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
        ScreenOverlay,
        GlobalZIndex(1000),
    ));
}

/// `Camera3d` requires `Projection` (for FOV) but not `ColorGrading` — insert
/// it once, on sight, rather than at every spawn site that might one day
/// create a camera.
fn ensure_camera_fx_components(
    mut commands: Commands,
    cameras: Query<Entity, Added<PlayerCamera>>,
) {
    for camera in &cameras {
        commands.entity(camera).insert(ColorGrading::default());
    }
}

/// Reads `HazardFelt` and turns it into a kick and a flash.
///
/// A plain `MessageReader`, no entity mapping and no authority gate — the
/// same pattern `interaction::apply_machine_opened` already proves works for
/// a server-targeted message on every peer, including singleplayer and the
/// host, since replicon re-emits a message addressed to `ClientId::Server`
/// locally.
fn apply_hazard_felt(mut fx: ResMut<ScreenFx>, mut felt: MessageReader<HazardFelt>) {
    for message in felt.read() {
        let strength = message.strength.clamp(0.0, 1.0);
        fx.shake = fx.shake.max(strength);
        fx.flash_alpha = fx.flash_alpha.max(strength * 0.6);
        fx.flash_color = match message.kind {
            HazardKind::Blast => Color::srgb(1.0, 0.95, 0.85),
            HazardKind::Smoke => Color::srgb(0.55, 0.62, 0.42),
            HazardKind::Radiation => Color::srgb(0.45, 0.85, 0.35),
            // A harsh red, distinct from the blast's white-orange — a fist
            // or a baton, not an explosion.
            HazardKind::Assault => Color::srgb(0.75, 0.08, 0.10),
        };
    }
}

/// The persistent, status-driven half of the vignette — separate from the
/// one-shot flash so the two can be blended rather than one clobbering the
/// other.
fn vignette_tint(blood: &ChemBloodstream) -> (Color, f32) {
    let drunk = blood.status(StatusKind::Drunk).intensity;
    let irradiated = blood.status(StatusKind::Irradiated).intensity;
    let blurred = blood.status(StatusKind::Blurred).intensity;

    // Whichever status is currently strongest wins the colour; their alphas
    // still stack, so two mild statuses together read as "off" without
    // fighting each other over a single colour.
    let strongest = [
        (drunk, Color::srgb(0.75, 0.55, 0.15)),
        (irradiated, Color::srgb(0.35, 0.85, 0.30)),
        (blurred, Color::srgb(0.85, 0.85, 0.90)),
    ]
    .into_iter()
    .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let alpha = ((drunk * 0.12) + (irradiated * 0.10) + (blurred * 0.18)).clamp(0.0, 0.55);
    match strongest {
        Some((intensity, color)) if intensity > 0.0 => (color, alpha),
        _ => (Color::NONE, 0.0),
    }
}

/// Everything that lands on the camera itself: the hazard shake/flash decay,
/// per-status sway/jitter/FOV, and the overlay's final blended colour.
#[allow(clippy::too_many_arguments)]
fn apply_camera_fx(
    time: Res<Time>,
    mut fx: ResMut<ScreenFx>,
    local: Query<&Bloodstream, With<LocalPlayer>>,
    mut cameras: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
    mut overlay: Query<&mut BackgroundColor, With<ScreenOverlay>>,
) {
    let dt = time.delta_secs();
    let t = time.elapsed_secs();

    // Hazard kick decays fast — it is a jolt, not a mood.
    fx.shake = (fx.shake - fx.shake.max(0.3) * 3.5 * dt).max(0.0);
    fx.flash_alpha = (fx.flash_alpha - 1.4 * dt).max(0.0);

    let Ok(blood) = local.single() else {
        return;
    };
    let drunk = blood.0.status(StatusKind::Drunk).intensity;
    let unsteady = blood.0.status(StatusKind::Unsteady).intensity;
    let sluggish = blood.0.status(StatusKind::Sluggish).intensity;
    let hastened = blood.0.status(StatusKind::Hastened).intensity;

    let mut rng = rand::rng();
    for (mut transform, mut projection) in &mut cameras {
        // Hazard shake: a random jolt, strongest the instant it lands.
        if fx.shake > 0.0 {
            let jolt = Vec3::new(
                rng.random_range(-1.0..1.0),
                rng.random_range(-1.0..1.0),
                0.0,
            ) * fx.shake
                * 0.06;
            transform.translation += jolt;
            transform.rotate_local_z(rng.random_range(-1.0..1.0) * fx.shake * 0.03);
        }

        // Drunk: a slow, deliberate sway — the room tilting with you.
        if drunk > 0.0 {
            transform.rotate_local_z((t * 1.1).sin() * 0.05 * drunk);
            transform.translation.y += (t * 0.9).sin() * 0.03 * drunk;
        }

        // Unsteady: fast, small, arrhythmic jitter — the room will not hold
        // still, as distinct from drunk's slow roll.
        if unsteady > 0.0 {
            transform.translation.x += (t * 13.0).sin() * 0.012 * unsteady;
            transform.translation.y += (t * 17.0).cos() * 0.012 * unsteady;
        }

        // Sluggish narrows the view (tunnel vision, heavy legs); hastened
        // widens it (fisheye speed). They already cancel on walk speed
        // (`player::walk_speed`) and cancel here for the same reason.
        if let Projection::Perspective(perspective) = projection.as_mut() {
            let base = std::f32::consts::FRAC_PI_4;
            let widen = 0.12 * hastened.min(2.0);
            let narrow = 0.10 * sluggish.min(2.0);
            perspective.fov = (base + widen - narrow).clamp(base * 0.6, base * 1.6);
        }
    }

    let (vignette_color, vignette_alpha) = vignette_tint(&blood.0);
    let total_alpha = (vignette_alpha + fx.flash_alpha).clamp(0.0, 0.85);
    let color = if fx.flash_alpha > vignette_alpha {
        fx.flash_color
    } else {
        vignette_color
    };
    if let Ok(mut background) = overlay.single_mut() {
        background.0 = color.with_alpha(total_alpha);
    }
}

// ---------------------------------------------------------------------------
// "The model reacts" — shared procedural presentation
// ---------------------------------------------------------------------------

/// A gait wobble, computed fresh from the current time rather than
/// accumulated onto — a status wearing off shrinks the wobble back toward
/// zero instead of leaving whatever the last frame's offset happened to be.
fn gait_offset(blood: &ChemBloodstream, t: f32) -> (Vec3, f32) {
    let drunk = blood.status(StatusKind::Drunk).intensity;
    let unsteady = blood.status(StatusKind::Unsteady).intensity;
    let hastened = blood.status(StatusKind::Hastened).intensity;
    let sluggish = blood.status(StatusKind::Sluggish).intensity;
    let irradiated = blood.status(StatusKind::Irradiated).intensity;

    let mut offset = Vec3::ZERO;
    let mut roll = 0.0_f32;

    // Drunk: a slow stumble, side to side.
    if drunk > 0.0 {
        offset.x += (t * 1.4).sin() * 0.06 * drunk;
        roll += (t * 1.4).sin() * 0.12 * drunk;
    }
    // Unsteady: fine, fast jitter — the reason the room "will not hold still".
    if unsteady > 0.0 {
        offset.x += (t * 9.0).sin() * 0.015 * unsteady;
        offset.z += (t * 11.0).cos() * 0.015 * unsteady;
    }
    // Hastened: a quicker, twitchier bob.
    if hastened > 0.0 {
        offset.y += (t * 10.0).sin().abs() * 0.03 * hastened;
    }
    // Sluggish: slumped and slow.
    if sluggish > 0.0 {
        offset.y -= 0.03 * sluggish.min(1.0);
        offset.y += (t * 2.0).sin() * 0.01 * sluggish;
    }
    // Irradiated: a fine shaking jitter.
    if irradiated > 0.0 {
        offset.x += (t * 20.0).sin() * 0.008 * irradiated;
        offset.z += (t * 23.0).cos() * 0.008 * irradiated;
    }

    (offset, roll)
}

/// Blends a status-driven tint onto `base`, never mutating `base` itself —
/// every caller passes the part's fixed, un-tinted reference colour, so
/// tinting never drifts frame over frame the way blending onto the *current*
/// material colour would.
fn status_tint(base: Color, blood: &ChemBloodstream) -> Color {
    let base = base.to_srgba();
    let (mut r, mut g, mut b) = (base.red, base.green, base.blue);

    let irradiated = blood.status(StatusKind::Irradiated).intensity;
    let drunk = blood.status(StatusKind::Drunk).intensity;
    let hastened = blood.status(StatusKind::Hastened).intensity;
    let sluggish = blood.status(StatusKind::Sluggish).intensity;

    // Sickly green cast.
    let ir = (irradiated / 2.0).clamp(0.0, 1.0);
    g += (1.0 - g) * ir * 0.5;
    r *= 1.0 - ir * 0.4;
    b *= 1.0 - ir * 0.4;

    // Flushed red — drunk or wired.
    let flush = ((drunk + hastened) / 3.0).clamp(0.0, 0.6);
    r += (1.0 - r) * flush * 0.6;
    g *= 1.0 - flush * 0.3;
    b *= 1.0 - flush * 0.3;

    // Pale and grey — worn down.
    let pale = (sluggish / 3.0).clamp(0.0, 0.5);
    let grey = (r + g + b) / 3.0;
    r += (grey - r) * pale;
    g += (grey - g) * pale;
    b += (grey - b) * pale;

    Color::srgb(r, g, b)
}

fn animate_chemist_body(
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    bloods: Query<&Bloodstream>,
    mut parts: Query<(&ChemistBody, &mut Transform, &MeshMaterial3d<StandardMaterial>)>,
) {
    let t = time.elapsed_secs();
    for (part, mut transform, material) in &mut parts {
        let Ok(blood) = bloods.get(part.chemist) else {
            continue;
        };
        let (offset, roll) = gait_offset(&blood.0, t);
        apply_part_pose(&mut transform, part.rest, offset, roll);
        apply_part_tint(&mut materials, &material.0, part.base_color, &blood.0);
    }
}

/// Writes a body part's pose only when it actually moved.
///
/// The common case is an untouched bloodstream, where `gait_offset` returns
/// `(Vec3::ZERO, 0.0)` every frame. Writing that unconditionally marked the
/// part `Changed<Transform>` and re-ran propagation for the whole body
/// hierarchy on every peer, every frame, for a pose that had not moved.
fn apply_part_pose(transform: &mut Transform, rest: Vec3, offset: Vec3, roll: f32) {
    let translation = rest + offset;
    if transform.translation != translation {
        transform.translation = translation;
    }
    let rotation = Quat::from_rotation_z(roll);
    if transform.rotation != rotation {
        transform.rotation = rotation;
    }
}

/// Writes a body part's tint only when the colour actually changed.
///
/// `Assets::get_mut` emits `AssetEvent::Modified` unconditionally, which forces
/// the render world to re-extract and re-prepare that `StandardMaterial` — a
/// uniform write plus a bind group — every frame. Materials are deliberately
/// per-chemist rather than shared (see `player::ChemistBody::base_color`), so
/// that cost scaled with everyone in the room, overwhelmingly to re-upload a
/// colour identical to the one already there.
fn apply_part_tint(
    materials: &mut Assets<StandardMaterial>,
    handle: &Handle<StandardMaterial>,
    base_color: Color,
    blood: &chem_sim::Bloodstream,
) {
    let tint = status_tint(base_color, blood);
    // Peeked through the immutable getter first: reaching for `get_mut` at all
    // is what dirties the asset, so the comparison has to happen before it.
    if materials.get(handle).is_some_and(|m| m.base_color == tint) {
        return;
    }
    if let Some(mut material) = materials.get_mut(handle) {
        material.base_color = tint;
    }
}

fn animate_crew_body(
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    bloods: Query<&Bloodstream>,
    mut parts: Query<(&CrewBody, &mut Transform, &MeshMaterial3d<StandardMaterial>)>,
) {
    let t = time.elapsed_secs();
    for (part, mut transform, material) in &mut parts {
        let Ok(blood) = bloods.get(part.crew) else {
            continue;
        };
        let (offset, roll) = gait_offset(&blood.0, t);
        apply_part_pose(&mut transform, part.rest, offset, roll);
        apply_part_tint(&mut materials, &material.0, part.base_color, &blood.0);
    }
}

#[cfg(test)]
mod tests {
    //! The camera/material systems need a real window and are exercised by
    //! playtesting, per the plan. What is worth pinning headlessly is the
    //! pure arithmetic: a status should move and tint something, and nothing
    //! should when there is nothing to react to.

    use super::*;
    use chem_sim::Bloodstream as ChemBloodstream;

    #[test]
    fn an_empty_bloodstream_wobbles_and_tints_nothing() {
        let blood = ChemBloodstream::default();
        let (offset, roll) = gait_offset(&blood, 1.23);
        assert_eq!(offset, Vec3::ZERO);
        assert_eq!(roll, 0.0);

        // `Color` equality is variant-sensitive (`Srgba` vs `LinearRgba`) and
        // float-exact, neither of which this cares about — only that an
        // untouched bloodstream leaves the colour visually unchanged.
        let tinted = status_tint(Color::WHITE, &blood).to_srgba();
        let base = Color::WHITE.to_srgba();
        assert!((tinted.red - base.red).abs() < 1e-4);
        assert!((tinted.green - base.green).abs() < 1e-4);
        assert!((tinted.blue - base.blue).abs() < 1e-4);
    }

    #[test]
    fn a_drunk_status_moves_the_gait_and_warms_the_tint() {
        let mut blood = ChemBloodstream::default();
        blood.add_status(StatusKind::Drunk, 4.0, 1.0);

        let (offset, roll) = gait_offset(&blood, 0.4);
        assert_ne!((offset, roll), (Vec3::ZERO, 0.0));

        let tinted = status_tint(Color::WHITE, &blood).to_srgba();
        let base = Color::WHITE.to_srgba();
        assert!(
            tinted.blue < base.blue,
            "a drunk flush should warm white away from blue"
        );
    }

    #[test]
    fn an_irradiated_status_pushes_green() {
        let mut blood = ChemBloodstream::default();
        blood.add_status(StatusKind::Irradiated, 4.0, 2.0);

        let tinted = status_tint(Color::WHITE, &blood).to_srgba();
        let base = Color::WHITE.to_srgba();
        assert!(tinted.green >= base.green);
        assert!(tinted.red < base.red);
        assert!(tinted.blue < base.blue);
    }
}
