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
                    update_status_readout,
                    update_hallucination_cue,
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

/// Plain-language local status list. Colour and camera language make a drug
/// felt; this label makes it unambiguous which state the player is actually in.
#[derive(Component)]
struct StatusReadout;

/// A transient false station cue produced by hallucinogens. It is deliberately
/// presentation-only: believable enough to create doubt, never an input loss
/// or a fabricated gameplay state other systems can react to.
#[derive(Component)]
struct HallucinationCue;

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
        crate::until_we_leave_the_lab(),
    ));
    commands.spawn((
        Text::new(""),
        TextFont::from_font_size(14.0),
        TextColor(Color::srgb(0.90, 0.94, 1.0)),
        Node {
            position_type: PositionType::Absolute,
            left: px(18),
            bottom: px(76),
            ..default()
        },
        StatusReadout,
        GlobalZIndex(1001),
        crate::until_we_leave_the_lab(),
    ));
    commands.spawn((
        Text::new(""),
        TextFont::from_font_size(17.0),
        TextColor(Color::srgb(0.80, 0.67, 0.94)),
        Node {
            position_type: PositionType::Absolute,
            right: px(12),
            top: Val::Percent(38.0),
            max_width: px(290),
            ..default()
        },
        HallucinationCue,
        GlobalZIndex(1002),
        crate::until_we_leave_the_lab(),
    ));
}

fn status_readout_text(blood: &ChemBloodstream) -> String {
    let mut lines = Vec::new();
    if blood.appears_dead() {
        lines.push("APPARENT DEATH".to_string());
    } else if blood.incapacitated() {
        lines.push("CHEMICALLY INCAPACITATED".to_string());
    }
    lines.extend(blood.active_statuses().filter_map(|(kind, state)| {
        (state.intensity > 0.0 && state.remaining > 0.0).then(|| {
            format!(
                "{}  x{:.1}  {:.0}s",
                kind.label(),
                state.intensity,
                state.remaining.ceil(),
            )
        })
    }));
    lines.join("\n")
}

fn update_status_readout(
    local: Query<&Bloodstream, With<LocalPlayer>>,
    mut readout: Query<&mut Text, With<StatusReadout>>,
) {
    let Ok(blood) = local.single() else {
        return;
    };
    let Ok(mut text) = readout.single_mut() else {
        return;
    };
    let next = status_readout_text(&blood.0);
    if text.0 != next {
        text.0 = next;
    }
}

fn hallucination_cue(blood: &ChemBloodstream, t: f32) -> Option<&'static str> {
    let intensity = blood.status(StatusKind::Hallucinating).intensity;
    if intensity <= 0.0 {
        return None;
    }
    let period = (5.2 - intensity.min(3.0) * 0.45).max(3.4);
    let cycle = (t / period).floor() as usize;
    let phase = (t / period).fract();
    if !(0.58..0.82).contains(&phase) {
        return None;
    }
    const CUES: [&str; 4] = [
        "A beaker shatters somewhere behind you.",
        "Footsteps stop just outside the room.",
        "Someone whispers your name over the intercom.",
        "A figure slips past the edge of your vision.",
    ];
    Some(CUES[cycle % CUES.len()])
}

fn update_hallucination_cue(
    time: Res<Time>,
    local: Query<&Bloodstream, With<LocalPlayer>>,
    mut cue: Query<&mut Text, With<HallucinationCue>>,
) {
    let Ok(blood) = local.single() else {
        return;
    };
    let Ok(mut text) = cue.single_mut() else {
        return;
    };
    let next = hallucination_cue(&blood.0, time.elapsed_secs()).unwrap_or_default();
    if text.0 != next {
        text.0 = next.to_string();
    }
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
fn status_color(kind: StatusKind) -> Color {
    match kind {
        StatusKind::Sluggish => Color::srgb(0.42, 0.46, 0.52),
        StatusKind::Hastened => Color::srgb(1.00, 0.27, 0.10),
        StatusKind::Blurred => Color::srgb(0.72, 0.78, 0.94),
        StatusKind::Unsteady => Color::srgb(0.92, 0.72, 0.16),
        StatusKind::Drunk => Color::srgb(0.78, 0.50, 0.10),
        StatusKind::Irradiated => Color::srgb(0.24, 0.88, 0.23),
        StatusKind::Stabilized => Color::srgb(0.16, 0.82, 0.70),
        StatusKind::Sedated => Color::srgb(0.16, 0.19, 0.42),
        StatusKind::Euphoric => Color::srgb(1.00, 0.38, 0.67),
        StatusKind::Hallucinating => Color::srgb(0.72, 0.18, 1.00),
        StatusKind::Paranoid => Color::srgb(0.92, 0.08, 0.12),
        StatusKind::Analgesic => Color::srgb(0.66, 0.48, 0.86),
        StatusKind::Burning => Color::srgb(1.00, 0.30, 0.03),
        StatusKind::Chilled => Color::srgb(0.18, 0.76, 1.00),
        StatusKind::RadiationShield => Color::srgb(0.92, 0.93, 0.25),
        StatusKind::Choking => Color::srgb(0.18, 0.36, 0.72),
        StatusKind::Mutating => Color::srgb(0.60, 0.10, 0.67),
        StatusKind::Focused => Color::srgb(0.16, 0.56, 1.00),
    }
}

/// All statuses get a screen-readable cue, including beneficial ones. Harmful
/// sensory statuses are stronger, while stabilization stays a restrained edge
/// tint rather than making treatment unpleasant to use.
fn vignette_tint(blood: &ChemBloodstream) -> (Color, f32) {
    let strongest = blood
        .active_statuses()
        .map(|(kind, state)| {
            let sensory_weight = 1.0 + kind.perception_distortion(state.intensity).abs();
            (state.intensity * sensory_weight, status_color(kind))
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let status_alpha = blood
        .active_statuses()
        .map(|(_, state)| state.intensity.max(0.0) * 0.035)
        .sum::<f32>();
    let alpha = (status_alpha + blood.perception_distortion() * 0.075).clamp(0.0, 0.58);

    if blood.appears_dead() {
        return (Color::srgb(0.025, 0.025, 0.035), alpha.max(0.68));
    }
    match strongest {
        Some((intensity, color)) if intensity > 0.0 => (color, alpha),
        _ => (Color::NONE, 0.0),
    }
}

/// Deterministic motor-warning waveform. The broad shoulder is the warning;
/// the brief peak is the visible stumble. It never changes or discards input.
fn stumble_cadence(t: f32, instability: f32) -> (f32, f32) {
    if instability <= 0.0 {
        return (0.0, 0.0);
    }
    let period = (3.6 - instability.min(4.0) * 0.55).max(1.25);
    let phase = (t / period).fract();
    let warning = ((phase - 0.68) / 0.18).clamp(0.0, 1.0);
    let stumble = if phase >= 0.86 {
        ((phase - 0.86) / 0.07).clamp(0.0, 1.0) * ((1.0 - phase) / 0.07).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (warning, stumble)
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
    let sluggish = blood.0.status(StatusKind::Sluggish).intensity;
    let hastened = blood.0.status(StatusKind::Hastened).intensity;
    let perception = blood.0.perception_distortion();
    let instability = blood.0.motor_instability();
    let incapacitated = blood.0.incapacitated();
    let appears_dead = blood.0.appears_dead();
    let (motor_warning, stumble) = stumble_cadence(t, instability);

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

        // Sensory distortion compounds into a small deterministic camera
        // drift; different statuses remain distinguishable by their vignette.
        if perception > 0.0 {
            transform.translation.x += (t * 7.0).sin() * 0.010 * perception.min(3.0);
            transform.translation.y += (t * 5.0).cos() * 0.007 * perception.min(3.0);
            transform.rotate_local_z((t * 2.3).sin() * 0.012 * perception.min(3.0));
        }

        // Motor impairment announces itself before the short camera dip. The
        // body keeps obeying input throughout; this is warning/presentation,
        // not a random movement veto.
        if instability > 0.0 {
            let strength = instability.min(3.0);
            transform.rotate_local_z(motor_warning * (t * 18.0).sin() * 0.018 * strength);
            transform.translation.y -= stumble * 0.09 * strength;
            transform.rotate_local_z(stumble * 0.055 * strength);
        }

        // Chemical unconsciousness lowers and rolls the point of view. The
        // strongest tier is unmistakably apparent-death presentation, while
        // the authoritative body remains alive and can recover.
        if incapacitated {
            transform.translation.y -= 0.55;
            transform.rotate_local_z(if appears_dead { 1.15 } else { 0.16 });
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
    let warning_alpha = motor_warning * instability.min(2.0) * 0.025;
    let total_alpha = (vignette_alpha + warning_alpha + fx.flash_alpha).clamp(0.0, 0.85);
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
    let stabilized = blood.status(StatusKind::Stabilized).intensity;
    let sedated = blood.status(StatusKind::Sedated).intensity;
    let euphoric = blood.status(StatusKind::Euphoric).intensity;
    let hallucinating = blood.status(StatusKind::Hallucinating).intensity;
    let paranoid = blood.status(StatusKind::Paranoid).intensity;
    let analgesic = blood.status(StatusKind::Analgesic).intensity;
    let burning = blood.status(StatusKind::Burning).intensity;
    let chilled = blood.status(StatusKind::Chilled).intensity;
    let radiation_shield = blood.status(StatusKind::RadiationShield).intensity;
    let choking = blood.status(StatusKind::Choking).intensity;
    let mutating = blood.status(StatusKind::Mutating).intensity;
    let focused = blood.status(StatusKind::Focused).intensity;

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

    // The expanded vocabulary deliberately uses posture and cadence as well
    // as colour. Every state remains legible under the station's coloured
    // lighting, and the root entity is never displaced by presentation.
    offset.y += 0.012 * stabilized.min(1.0);
    offset.y += (t * 5.0).sin().abs() * 0.025 * euphoric;
    offset.y += 0.008 * analgesic.min(1.0);
    offset.y += 0.010 * radiation_shield.min(1.0);
    offset.y += 0.010 * focused.min(1.0);

    if sedated > 0.0 {
        offset.y -= 0.07 * sedated.min(2.0);
        roll += (t * 0.7).sin() * 0.035 * sedated;
    }
    if hallucinating > 0.0 {
        offset.x += (t * 2.7).sin() * 0.030 * hallucinating;
        roll += (t * 1.9).cos() * 0.040 * hallucinating;
    }
    if paranoid > 0.0 {
        offset.x += (t * 14.0).sin() * 0.012 * paranoid;
        roll += (t * 9.0).sin() * 0.025 * paranoid;
    }
    if burning > 0.0 {
        offset.y += (t * 12.0).sin().abs() * 0.025 * burning;
        roll += (t * 15.0).sin() * 0.018 * burning;
    }
    if chilled > 0.0 {
        offset.x += (t * 24.0).sin() * 0.008 * chilled;
        offset.z += (t * 21.0).cos() * 0.008 * chilled;
    }
    if choking > 0.0 {
        offset.y -= (t * 5.5).sin().abs() * 0.025 * choking;
        roll += (t * 5.5).sin() * 0.025 * choking;
    }
    if mutating > 0.0 {
        offset.x += (t * 4.1).sin() * 0.025 * mutating;
        offset.z += (t * 3.3).cos() * 0.020 * mutating;
        roll += (t * 3.7).sin() * 0.035 * mutating;
    }

    // Focus visibly steadies rather than adding another wobble.
    let steadiness = (1.0 - focused * 0.22).clamp(0.35, 1.0);
    offset.x *= steadiness;
    offset.z *= steadiness;
    roll *= steadiness;

    if blood.appears_dead() {
        offset.y -= 0.22;
        roll = 1.40;
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

    // A stable palette gives every status a third-person identity. Blend in
    // enum order so the result is deterministic across peers and replays.
    for (kind, state) in blood.active_statuses() {
        let target = status_color(kind).to_srgba();
        let amount = (0.10 * state.intensity.max(0.0)).clamp(0.0, 0.42);
        r += (target.red - r) * amount;
        g += (target.green - g) * amount;
        b += (target.blue - b) * amount;
    }

    Color::srgb(r, g, b)
}

fn body_scale(blood: &ChemBloodstream, t: f32) -> Vec3 {
    let mutating = blood.status(StatusKind::Mutating).intensity.min(3.0);
    if mutating <= 0.0 {
        return Vec3::ONE;
    }
    let pulse = (t * 3.1).sin();
    Vec3::new(
        1.0 + pulse * 0.10 * mutating,
        1.0 - pulse * 0.07 * mutating,
        1.0 + (t * 2.3).cos() * 0.08 * mutating,
    )
}

fn animate_chemist_body(
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    bloods: Query<&Bloodstream>,
    mut parts: Query<(
        &ChemistBody,
        &mut Transform,
        &MeshMaterial3d<StandardMaterial>,
    )>,
) {
    let t = time.elapsed_secs();
    for (part, mut transform, material) in &mut parts {
        let Ok(blood) = bloods.get(part.chemist) else {
            continue;
        };
        let (offset, roll) = gait_offset(&blood.0, t);
        apply_part_pose(
            &mut transform,
            part.rest,
            offset,
            roll,
            body_scale(&blood.0, t),
        );
        apply_part_tint(&mut materials, &material.0, part.base_color, &blood.0);
    }
}

/// Writes a body part's pose only when it actually moved.
///
/// The common case is an untouched bloodstream, where `gait_offset` returns
/// `(Vec3::ZERO, 0.0)` every frame. Writing that unconditionally marked the
/// part `Changed<Transform>` and re-ran propagation for the whole body
/// hierarchy on every peer, every frame, for a pose that had not moved.
fn apply_part_pose(transform: &mut Transform, rest: Vec3, offset: Vec3, roll: f32, scale: Vec3) {
    let translation = rest + offset;
    if transform.translation != translation {
        transform.translation = translation;
    }
    let rotation = Quat::from_rotation_z(roll);
    if transform.rotation != rotation {
        transform.rotation = rotation;
    }
    if transform.scale != scale {
        transform.scale = scale;
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
        apply_part_pose(
            &mut transform,
            part.rest,
            offset,
            roll,
            body_scale(&blood.0, t),
        );
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
        assert!(tinted.green > tinted.red && tinted.green > tinted.blue);
        assert!(tinted.red < base.red);
        assert!(tinted.blue < base.blue);
    }

    #[test]
    fn every_status_has_first_and_third_person_presentation() {
        let base = Color::srgb(0.31, 0.34, 0.37);
        let base_srgba = base.to_srgba();

        for kind in StatusKind::ALL {
            let mut blood = ChemBloodstream::default();
            blood.add_status(kind, 4.0, 1.0);

            let (vignette, alpha) = vignette_tint(&blood);
            assert!(
                alpha > 0.0 && vignette != Color::NONE,
                "{} has no player presentation",
                kind.label(),
            );
            assert!(
                status_readout_text(&blood).contains(kind.label()),
                "{} is not named in the player status readout",
                kind.label(),
            );

            let tinted = status_tint(base, &blood).to_srgba();
            let delta = (tinted.red - base_srgba.red).abs()
                + (tinted.green - base_srgba.green).abs()
                + (tinted.blue - base_srgba.blue).abs();
            assert!(
                delta > 0.01,
                "{} has no third-person presentation",
                kind.label(),
            );
        }
    }

    #[test]
    fn motor_warning_and_stumble_are_deterministic_and_staged() {
        let period = 3.6 - 0.55;
        let warning = stumble_cadence(period * 0.78, 1.0);
        assert!(warning.0 > 0.0, "a warning must precede the stumble");
        assert_eq!(warning.1, 0.0);

        let stumble = stumble_cadence(period * 0.90, 1.0);
        assert!(stumble.1 > 0.0);
        assert_eq!(stumble, stumble_cadence(period * 0.90, 1.0));
        assert_eq!(stumble_cadence(10.0, 0.0), (0.0, 0.0));
    }

    #[test]
    fn apparent_death_is_visually_distinct_from_lethal_state() {
        let mut blood = ChemBloodstream::default();
        blood.add_status(StatusKind::Sedated, 4.0, 4.0);
        assert!(blood.appears_dead());

        let (color, alpha) = vignette_tint(&blood);
        let dark = color.to_srgba();
        assert!(alpha >= 0.68);
        assert!(dark.red < 0.1 && dark.green < 0.1 && dark.blue < 0.1);
    }

    #[test]
    fn mutation_visibly_changes_body_proportions() {
        let mut blood = ChemBloodstream::default();
        blood.add_status(StatusKind::Mutating, 4.0, 1.0);

        assert_ne!(body_scale(&blood, 0.4), Vec3::ONE);
        assert_eq!(body_scale(&ChemBloodstream::default(), 0.4), Vec3::ONE);
    }

    #[test]
    fn hallucinations_emit_deterministic_false_station_cues() {
        let mut blood = ChemBloodstream::default();
        blood.add_status(StatusKind::Hallucinating, 10.0, 1.0);
        let period = 5.2 - 0.45;
        let at = period * 0.70;

        let cue = hallucination_cue(&blood, at).expect("active cue window");
        assert!(!cue.is_empty());
        assert_eq!(Some(cue), hallucination_cue(&blood, at));
        assert_eq!(hallucination_cue(&blood, period * 0.2), None);
        assert_eq!(hallucination_cue(&ChemBloodstream::default(), at), None);
    }
}
