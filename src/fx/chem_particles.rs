//! Small, client-local chemistry particles.
//!
//! The authoritative objects are still [`ChemicalPuddle`] and [`Bloodstream`].
//! This module only reads those replicated components and builds disposable
//! child entities from them. In particular, body particles are parented below
//! a chemist/crew root but never write that root's transform, preserving the
//! movement, interaction and replication contract used by the rest of the
//! game.
//!
//! This deliberately starts with Bevy-native meshes instead of introducing a
//! GPU-particle dependency. The profiles and emitter boundary are kept small
//! enough that the renderer can be swapped later without changing what a
//! chemical is supposed to look like.

use std::cmp::Ordering;
use std::collections::HashMap;

use bevy::prelude::*;
use chem_sim::{
    Bloodstream as ChemBloodstream, ChemFamily, Reagent, ReagentEffect, Solution, StatusKind,
    WorldEffect,
};
use rand::prelude::*;

use super::status_color;
use crate::body::Bloodstream;
use crate::chem_data::ChemDb;
use crate::chem_world::ChemicalPuddle;
use crate::player::Player;
use crate::AppState;

/// Mesh entities are intentionally capped: effects should make a crowded lab
/// more readable, not turn a 20-person co-op shift into a stress test.
const MAX_LIVE_PARTICLES: usize = 720;
const MAX_SPAWN_PER_EMITTER_FRAME: usize = 7;

pub(super) struct ChemicalParticlePlugin;

impl Plugin for ChemicalParticlePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ParticleAssets>()
            .init_resource::<ParticleMaterials>()
            .add_systems(OnExit(AppState::Playing), clear_particle_materials)
            .add_systems(
                Update,
                (
                    ensure_puddle_emitters,
                    ensure_body_emitters,
                    emit_particles,
                    animate_particles,
                    clean_removed_emitter_materials,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[derive(Resource)]
struct ParticleAssets {
    mote: Handle<Mesh>,
}

impl FromWorld for ParticleAssets {
    fn from_world(world: &mut World) -> Self {
        let mote = world.resource_mut::<Assets<Mesh>>().add(Sphere::new(1.0));
        Self { mote }
    }
}

/// Materials are private to emitters so two simultaneous mixtures retain
/// their own colors. Keep the handles here as well as on the emitters, which
/// lets `RemovedComponents` reclaim manually-created assets after a puddle or
/// visitor leaves the world.
#[derive(Resource, Default)]
struct ParticleMaterials(HashMap<Entity, Handle<StandardMaterial>>);

#[derive(Component)]
struct HasChemicalParticleEmitter;

type BodyWithoutParticleEmitter = (With<Bloodstream>, Without<HasChemicalParticleEmitter>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParticleSource {
    Puddle(Entity),
    Body(Entity),
}

#[derive(Component)]
struct ChemicalParticleEmitter {
    source: ParticleSource,
    material: Handle<StandardMaterial>,
    carry: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParticleMotion {
    /// Rounded surface activity: ordinary organics, lubricants and foam.
    Bubble,
    /// Slow, curling vapor: gases, halogens, smoke and corrosion.
    Wisp,
    /// Crisp upward flecks: metals, useful medicine and cleaning agents.
    Spark,
    /// Hot, fast, emissive tongues.
    Flame,
    /// Cold crystals drifting down and outward.
    Frost,
    /// Unnatural circulation: radiation, mutation and perceptual effects.
    Orbit,
    /// Heavy motes sinking around sluggish or sedated bodies.
    Fall,
}

#[derive(Clone, Copy, Debug)]
struct ParticleProfile {
    motion: ParticleMotion,
    color: Color,
    rate: f32,
    lifetime: f32,
    size: f32,
    speed: f32,
    spawn_radius: f32,
    min_y: f32,
    max_y: f32,
    emissive: f32,
}

#[derive(Component)]
struct ChemicalParticle {
    motion: ParticleMotion,
    velocity: Vec3,
    age: f32,
    lifetime: f32,
    phase: f32,
    drift: f32,
    spin: f32,
    base_scale: Vec3,
}

fn particle_material(profile: ParticleProfile) -> StandardMaterial {
    let color = profile.color.to_srgba();
    StandardMaterial {
        base_color: profile.color.with_alpha(0.78),
        emissive: LinearRgba::new(
            color.red * profile.emissive,
            color.green * profile.emissive,
            color.blue * profile.emissive,
            1.0,
        ),
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 0.35,
        metallic: if profile.motion == ParticleMotion::Spark {
            0.22
        } else {
            0.0
        },
        unlit: profile.emissive > 0.0,
        ..default()
    }
}

fn placeholder_profile() -> ParticleProfile {
    ParticleProfile {
        motion: ParticleMotion::Bubble,
        color: Color::WHITE,
        rate: 0.0,
        lifetime: 1.0,
        size: 0.025,
        speed: 0.2,
        spawn_radius: 0.2,
        min_y: 0.0,
        max_y: 0.0,
        emissive: 0.0,
    }
}

fn ensure_puddle_emitters(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut registry: ResMut<ParticleMaterials>,
    puddles: Query<Entity, (Added<ChemicalPuddle>, Without<HasChemicalParticleEmitter>)>,
) {
    for puddle in &puddles {
        let material = materials.add(particle_material(placeholder_profile()));
        let emitter = commands
            .spawn((
                Name::new("chemical spill particles"),
                Transform::default(),
                Visibility::default(),
                ChemicalParticleEmitter {
                    source: ParticleSource::Puddle(puddle),
                    material: material.clone(),
                    carry: 0.0,
                },
                ChildOf(puddle),
            ))
            .id();
        registry.0.insert(emitter, material);
        commands.entity(puddle).insert(HasChemicalParticleEmitter);
    }
}

fn ensure_body_emitters(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut registry: ResMut<ParticleMaterials>,
    bodies: Query<(Entity, Has<Player>), BodyWithoutParticleEmitter>,
) {
    for (body, is_player) in &bodies {
        // Replicated body roots do not carry presentation components over the
        // wire. A child with inherited visibility below a root with no
        // `Visibility` triggers Bevy B0004 and has undefined propagation.
        // Preserve an intentional Hidden root while completing the hierarchy
        // for a newly-arrived NPC/player before the emitter is attached.
        commands.entity(body).insert_if_new(Visibility::default());
        let material = materials.add(particle_material(placeholder_profile()));
        // A player's replicated root is at eye height while crew roots are at
        // body center. Moving only this local emitter aligns both with their
        // visible torso and leaves the authoritative root untouched.
        let offset = if is_player { -0.78 } else { 0.0 };
        let emitter = commands
            .spawn((
                Name::new("body chemistry particles"),
                Transform::from_xyz(0.0, offset, 0.0),
                Visibility::default(),
                ChemicalParticleEmitter {
                    source: ParticleSource::Body(body),
                    material: material.clone(),
                    carry: 0.0,
                },
                ChildOf(body),
            ))
            .id();
        registry.0.insert(emitter, material);
        commands.entity(body).insert(HasChemicalParticleEmitter);
    }
}

#[derive(Default)]
struct GroundTraits {
    clean: bool,
    corrode: bool,
    smoke: bool,
    flash: bool,
}

fn dominant_reagent<'a>(solution: &Solution, db: &'a ChemDb) -> Option<&'a Reagent> {
    solution
        .iter()
        .max_by(|(left_id, left), (right_id, right)| {
            left.as_f32()
                .partial_cmp(&right.as_f32())
                .unwrap_or(Ordering::Equal)
                .then_with(|| right_id.index().cmp(&left_id.index()))
        })
        .map(|(id, _)| db.reagents.get(id))
}

fn ground_traits(solution: &Solution, db: &ChemDb) -> GroundTraits {
    let mut traits = GroundTraits::default();
    for (id, _) in solution.iter() {
        for effect in &db.reagents.get(id).world_effects {
            match effect {
                WorldEffect::Clean { .. } | WorldEffect::Extinguish { .. } => traits.clean = true,
                WorldEffect::Corrode { .. } => traits.corrode = true,
                WorldEffect::ReleaseSmoke { .. } => traits.smoke = true,
                WorldEffect::Flash { .. } => traits.flash = true,
                WorldEffect::Ignite { .. }
                | WorldEffect::Slippery { .. }
                | WorldEffect::Flammable { .. }
                | WorldEffect::Chill { .. }
                | WorldEffect::ExpandFoam { .. } => {}
            }
        }
    }
    traits
}

fn family_motion(family: ChemFamily) -> ParticleMotion {
    match family {
        ChemFamily::Metal => ParticleMotion::Spark,
        ChemFamily::GasNonmetal | ChemFamily::Halogen | ChemFamily::AcidBaseBuffer => {
            ParticleMotion::Wisp
        }
        ChemFamily::Radioactive => ParticleMotion::Orbit,
        ChemFamily::Organic | ChemFamily::Industrial | ChemFamily::Unclassified => {
            ParticleMotion::Bubble
        }
    }
}

fn brighten(color: Color, amount: f32) -> Color {
    let color = color.to_srgba();
    Color::srgb(
        color.red + (1.0 - color.red) * amount,
        color.green + (1.0 - color.green) * amount,
        color.blue + (1.0 - color.blue) * amount,
    )
}

fn blend(left: Color, right: Color, right_weight: f32) -> Color {
    let left = left.to_srgba();
    let right = right.to_srgba();
    let weight = right_weight.clamp(0.0, 1.0);
    Color::srgb(
        left.red + (right.red - left.red) * weight,
        left.green + (right.green - left.green) * weight,
        left.blue + (right.blue - left.blue) * weight,
    )
}

fn puddle_profile(puddle: &ChemicalPuddle, db: &ChemDb) -> Option<ParticleProfile> {
    if puddle.solution.is_empty() {
        return None;
    }
    let [red, green, blue] = puddle.solution.color(&db.reagents);
    let chemical_color = brighten(Color::srgb(red, green, blue), 0.18);
    let family = dominant_reagent(&puddle.solution, db)
        .map(|reagent| reagent.family)
        .unwrap_or_default();
    let traits = ground_traits(&puddle.solution, db);

    let (motion, color, rate, lifetime, size, speed, emissive) = if puddle.ignited {
        (
            ParticleMotion::Flame,
            Color::srgb(1.0, 0.26, 0.025),
            17.0,
            0.65,
            0.065,
            0.85,
            3.4,
        )
    } else if puddle.chill_intensity > 0.0 {
        (
            ParticleMotion::Frost,
            blend(chemical_color, Color::srgb(0.62, 0.91, 1.0), 0.62),
            8.0,
            1.35,
            0.032,
            0.17,
            0.55,
        )
    } else if puddle.foam_height > 0.0 {
        (
            ParticleMotion::Bubble,
            brighten(chemical_color, 0.40),
            11.0,
            1.2,
            0.055,
            0.32,
            0.18,
        )
    } else if traits.corrode || traits.smoke {
        (
            ParticleMotion::Wisp,
            blend(chemical_color, Color::srgb(0.72, 0.90, 0.40), 0.18),
            8.0,
            1.65,
            0.045,
            0.30,
            0.30,
        )
    } else if puddle.cleaner || traits.clean || traits.flash {
        (
            ParticleMotion::Spark,
            blend(chemical_color, Color::WHITE, 0.52),
            6.5,
            0.85,
            0.026,
            0.62,
            1.1,
        )
    } else if puddle.flammable_remaining > 0.0 {
        (
            ParticleMotion::Wisp,
            blend(chemical_color, Color::srgb(1.0, 0.58, 0.12), 0.28),
            5.0,
            1.55,
            0.037,
            0.24,
            0.38,
        )
    } else {
        let motion = family_motion(family);
        (
            motion,
            chemical_color,
            match motion {
                ParticleMotion::Spark => 4.2,
                ParticleMotion::Wisp => 3.5,
                ParticleMotion::Orbit => 4.8,
                _ => 3.0,
            },
            1.35,
            0.032,
            0.25,
            if motion == ParticleMotion::Orbit {
                0.8
            } else {
                0.12
            },
        )
    };

    Some(ParticleProfile {
        motion,
        color,
        rate: rate * (0.62 + puddle.radius * 0.48),
        lifetime,
        size,
        speed,
        spawn_radius: (puddle.radius * 0.88).max(0.12),
        min_y: puddle.foam_height.max(0.018) + 0.015,
        max_y: puddle.foam_height.max(0.018) + 0.08,
        emissive,
    })
}

fn status_priority(kind: StatusKind) -> u8 {
    match kind {
        StatusKind::Burning => 100,
        StatusKind::Chilled => 96,
        StatusKind::Irradiated => 94,
        StatusKind::Mutating => 92,
        StatusKind::Choking => 90,
        StatusKind::RadiationShield => 86,
        StatusKind::Obscured => 84,
        StatusKind::Hallucinating => 82,
        StatusKind::Paranoid => 80,
        StatusKind::Sedated => 78,
        StatusKind::Drunk => 76,
        StatusKind::Unsteady => 74,
        StatusKind::Hastened => 72,
        StatusKind::Focused => 70,
        StatusKind::Stabilized => 68,
        StatusKind::Analgesic => 66,
        StatusKind::Blurred => 64,
        StatusKind::Sluggish => 62,
        StatusKind::Muted => 60,
        StatusKind::Pacified => 58,
        StatusKind::Euphoric => 56,
        StatusKind::Happiness => 54,
        StatusKind::Sadness => 52,
    }
}

fn status_motion(kind: StatusKind) -> ParticleMotion {
    match kind {
        StatusKind::Burning => ParticleMotion::Flame,
        StatusKind::Chilled => ParticleMotion::Frost,
        StatusKind::Irradiated
        | StatusKind::RadiationShield
        | StatusKind::Mutating
        | StatusKind::Hallucinating => ParticleMotion::Orbit,
        StatusKind::Choking | StatusKind::Obscured | StatusKind::Muted | StatusKind::Paranoid => {
            ParticleMotion::Wisp
        }
        StatusKind::Sluggish | StatusKind::Sedated | StatusKind::Sadness => ParticleMotion::Fall,
        StatusKind::Hastened
        | StatusKind::Stabilized
        | StatusKind::Analgesic
        | StatusKind::Focused
        | StatusKind::Pacified => ParticleMotion::Spark,
        StatusKind::Blurred
        | StatusKind::Unsteady
        | StatusKind::Drunk
        | StatusKind::Euphoric
        | StatusKind::Happiness => ParticleMotion::Bubble,
    }
}

fn effect_motion(reagent: &Reagent) -> ParticleMotion {
    let mut helpful = false;
    let mut cleansing = false;
    for effect in &reagent.effects {
        match effect {
            ReagentEffect::Harm(..)
            | ReagentEffect::VolumeScaledHarm(..)
            | ReagentEffect::Contact(..)
            | ReagentEffect::ConditionalHarm { .. }
            | ReagentEffect::AccumulatedHarm(..)
            | ReagentEffect::DelayedHarm { .. }
            | ReagentEffect::MedicinePurge(..) => return ParticleMotion::Wisp,
            ReagentEffect::Heal(..)
            | ReagentEffect::TopicalHeal(..)
            | ReagentEffect::ConditionalHeal { .. }
            | ReagentEffect::CriticalHeal(..) => helpful = true,
            ReagentEffect::Counter { .. } | ReagentEffect::Purge(..) => cleansing = true,
            ReagentEffect::Status { .. } | ReagentEffect::DelayedStatus { .. } => {}
        }
    }
    if cleansing {
        ParticleMotion::Orbit
    } else if helpful {
        ParticleMotion::Spark
    } else {
        family_motion(reagent.family)
    }
}

fn strongest_status(blood: &ChemBloodstream) -> Option<(StatusKind, f32)> {
    blood
        .active_statuses()
        .filter(|(_, state)| state.remaining > 0.0 && state.intensity > 0.0)
        .max_by(|(left_kind, left), (right_kind, right)| {
            status_priority(*left_kind)
                .cmp(&status_priority(*right_kind))
                .then_with(|| {
                    left.intensity
                        .partial_cmp(&right.intensity)
                        .unwrap_or(Ordering::Equal)
                })
        })
        .map(|(kind, state)| (kind, state.intensity))
}

fn body_profile(blood: &ChemBloodstream, db: &ChemDb) -> Option<ParticleProfile> {
    let status = strongest_status(blood);
    let reagent = dominant_reagent(&blood.blood, db);
    if status.is_none() && reagent.is_none() {
        return None;
    }

    let chemical_color = if blood.blood.is_empty() {
        status_color(status?.0)
    } else {
        let [red, green, blue] = blood.blood.color(&db.reagents);
        brighten(Color::srgb(red, green, blue), 0.20)
    };
    let (motion, color, intensity) = match status {
        Some((kind, intensity)) => (
            status_motion(kind),
            blend(chemical_color, status_color(kind), 0.38),
            intensity,
        ),
        None => (
            effect_motion(reagent.expect("checked above")),
            chemical_color,
            0.45,
        ),
    };
    let volume = blood.blood.total_volume().as_f32();
    let rate = (2.1 + intensity.min(3.0) * 1.7 + volume.sqrt() * 0.32).min(9.5);
    let (lifetime, size, speed, emissive) = match motion {
        ParticleMotion::Flame => (0.72, 0.052, 0.86, 3.2),
        ParticleMotion::Frost => (1.45, 0.029, 0.18, 0.65),
        ParticleMotion::Orbit => (1.65, 0.032, 0.55, 0.85),
        ParticleMotion::Wisp => (1.55, 0.040, 0.28, 0.28),
        ParticleMotion::Spark => (0.92, 0.026, 0.65, 1.05),
        ParticleMotion::Fall => (1.25, 0.038, 0.22, 0.10),
        ParticleMotion::Bubble => (1.20, 0.038, 0.30, 0.18),
    };

    Some(ParticleProfile {
        motion,
        color,
        rate,
        lifetime,
        size,
        speed,
        spawn_radius: 0.43,
        min_y: -0.66,
        max_y: 0.72,
        emissive,
    })
}

fn source_profile(
    source: ParticleSource,
    db: &ChemDb,
    puddles: &Query<&ChemicalPuddle>,
    bodies: &Query<&Bloodstream>,
) -> Option<ParticleProfile> {
    match source {
        ParticleSource::Puddle(entity) => puddles
            .get(entity)
            .ok()
            .and_then(|puddle| puddle_profile(puddle, db)),
        ParticleSource::Body(entity) => bodies
            .get(entity)
            .ok()
            .and_then(|blood| body_profile(&blood.0, db)),
    }
}

fn update_material(
    materials: &mut Assets<StandardMaterial>,
    handle: &Handle<StandardMaterial>,
    profile: ParticleProfile,
) {
    let next = particle_material(profile);
    if materials.get(handle).is_some_and(|current| {
        current.base_color == next.base_color
            && current.emissive == next.emissive
            && current.metallic == next.metallic
            && current.unlit == next.unlit
    }) {
        return;
    }
    if let Some(mut material) = materials.get_mut(handle) {
        *material = next;
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_particles(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    assets: Res<ParticleAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    puddles: Query<&ChemicalPuddle>,
    bodies: Query<&Bloodstream>,
    live_particles: Query<(), With<ChemicalParticle>>,
    mut emitters: Query<(Entity, &mut ChemicalParticleEmitter)>,
) {
    let mut room = MAX_LIVE_PARTICLES.saturating_sub(live_particles.iter().count());
    if room == 0 {
        return;
    }

    let dt = time.delta_secs().min(0.1);
    let mut rng = rand::rng();
    for (emitter_entity, mut emitter) in &mut emitters {
        let Some(profile) = source_profile(emitter.source, db.as_ref(), &puddles, &bodies) else {
            emitter.carry = 0.0;
            continue;
        };
        update_material(materials.as_mut(), &emitter.material, profile);

        emitter.carry = (emitter.carry + profile.rate * dt).min(2.0);
        let count = (emitter.carry.floor() as usize)
            .min(MAX_SPAWN_PER_EMITTER_FRAME)
            .min(room);
        emitter.carry -= count as f32;
        room -= count;

        for _ in 0..count {
            let angle = rng.random_range(0.0..std::f32::consts::TAU);
            let radius = profile.spawn_radius * rng.random::<f32>().sqrt();
            let y = rng.random_range(profile.min_y..=profile.max_y);
            let position = Vec3::new(angle.cos() * radius, y, angle.sin() * radius);
            let tangent = Vec3::new(-angle.sin(), 0.0, angle.cos());
            let random_drift = Vec3::new(
                rng.random_range(-0.24..0.24),
                0.0,
                rng.random_range(-0.24..0.24),
            );
            let velocity = match profile.motion {
                ParticleMotion::Bubble | ParticleMotion::Wisp => {
                    random_drift * profile.speed + Vec3::Y * profile.speed
                }
                ParticleMotion::Spark => {
                    random_drift * profile.speed * 0.55 + Vec3::Y * profile.speed
                }
                ParticleMotion::Flame => {
                    random_drift * profile.speed * 0.32 + Vec3::Y * profile.speed
                }
                ParticleMotion::Frost => random_drift * profile.speed - Vec3::Y * profile.speed,
                ParticleMotion::Orbit => tangent * profile.speed + Vec3::Y * profile.speed * 0.12,
                ParticleMotion::Fall => {
                    random_drift * profile.speed * 0.5 - Vec3::Y * profile.speed
                }
            };
            let shape = match profile.motion {
                ParticleMotion::Flame | ParticleMotion::Spark | ParticleMotion::Wisp => {
                    Vec3::new(0.62, 1.75, 0.62)
                }
                ParticleMotion::Frost => Vec3::new(0.55, 0.22, 1.35),
                _ => Vec3::ONE,
            };
            let varied_size = profile.size * rng.random_range(0.72..1.28);
            let base_scale = shape * varied_size;
            commands.spawn((
                Mesh3d(assets.mote.clone()),
                MeshMaterial3d(emitter.material.clone()),
                Transform::from_translation(position).with_scale(base_scale),
                Visibility::default(),
                ChemicalParticle {
                    motion: profile.motion,
                    velocity,
                    age: 0.0,
                    lifetime: profile.lifetime * rng.random_range(0.82..1.18),
                    phase: rng.random_range(0.0..std::f32::consts::TAU),
                    drift: rng.random_range(1.7..3.5),
                    spin: rng.random_range(-4.0..4.0),
                    base_scale,
                },
                ChildOf(emitter_entity),
            ));
        }
        if room == 0 {
            break;
        }
    }
}

fn animate_particles(
    mut commands: Commands,
    time: Res<Time>,
    mut particles: Query<(Entity, &mut Transform, &mut ChemicalParticle)>,
) {
    let dt = time.delta_secs().min(0.1);
    for (entity, mut transform, mut particle) in &mut particles {
        particle.age += dt;
        if particle.age >= particle.lifetime {
            commands.entity(entity).despawn();
            continue;
        }

        transform.translation += particle.velocity * dt;
        let wave = (particle.phase + particle.age * particle.drift).sin();
        match particle.motion {
            ParticleMotion::Bubble => {
                transform.translation.x += wave * 0.045 * dt;
                transform.translation.z += wave.cos() * 0.032 * dt;
            }
            ParticleMotion::Wisp => {
                transform.translation.x += wave * 0.095 * dt;
                transform.translation.z += (particle.phase + particle.age * 1.3).cos() * 0.075 * dt;
                transform.scale.x *= 1.0 + 0.18 * dt;
                transform.scale.z *= 1.0 + 0.18 * dt;
            }
            ParticleMotion::Spark => {
                particle.velocity.y -= 0.55 * dt;
            }
            ParticleMotion::Flame => {
                transform.translation.x += wave * 0.065 * dt;
                transform.scale.x *= 1.0 - 0.28 * dt;
                transform.scale.z *= 1.0 - 0.28 * dt;
            }
            ParticleMotion::Frost => {
                transform.translation.x += wave * 0.035 * dt;
                transform.rotate_y(particle.spin * dt);
            }
            ParticleMotion::Orbit => {
                let turn = particle.spin.signum() * 1.8 * dt;
                let (sin, cos) = turn.sin_cos();
                let x = transform.translation.x;
                let z = transform.translation.z;
                transform.translation.x = x * cos - z * sin;
                transform.translation.z = x * sin + z * cos;
                transform.translation.y += wave * 0.025 * dt;
            }
            ParticleMotion::Fall => {
                transform.translation.x += wave * 0.025 * dt;
            }
        }

        let life = particle.age / particle.lifetime;
        let appear = (life / 0.12).clamp(0.0, 1.0);
        let disappear = ((1.0 - life) / 0.30).clamp(0.0, 1.0);
        transform.scale = particle.base_scale * appear.min(disappear);
        if particle.motion != ParticleMotion::Frost {
            transform.rotate_y(particle.spin * dt);
        }
    }
}

fn clean_removed_emitter_materials(
    mut removed: RemovedComponents<ChemicalParticleEmitter>,
    mut registry: ResMut<ParticleMaterials>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for entity in removed.read() {
        if let Some(handle) = registry.0.remove(&entity) {
            materials.remove(handle.id());
        }
    }
}

fn clear_particle_materials(
    mut registry: ResMut<ParticleMaterials>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (_, handle) in registry.0.drain() {
        materials.remove(handle.id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chem_sim::Units;

    fn chemistry() -> ChemDb {
        ChemDb(
            chem_sim::ChemData::from_ron(
                include_str!("../../assets/data/chem.reagents.ron"),
                include_str!("../../assets/data/chem.reactions.ron"),
            )
            .expect("chemistry data should load"),
        )
    }

    fn solution(db: &ChemDb, reagent: &str) -> Solution {
        let id = db.reagent(reagent);
        let mut solution = Solution::unbounded();
        let _ = solution.add_profiled(id, Units::whole(10), 1.0, db.reagents.get(id).ph);
        solution
    }

    #[test]
    fn chemical_families_do_not_all_collapse_to_one_ground_effect() {
        assert_eq!(family_motion(ChemFamily::Metal), ParticleMotion::Spark);
        assert_eq!(family_motion(ChemFamily::GasNonmetal), ParticleMotion::Wisp);
        assert_eq!(
            family_motion(ChemFamily::Radioactive),
            ParticleMotion::Orbit
        );
        assert_eq!(family_motion(ChemFamily::Organic), ParticleMotion::Bubble);
    }

    #[test]
    fn every_status_selects_a_visible_body_particle_language() {
        for kind in StatusKind::ALL {
            let motion = status_motion(kind);
            assert!(
                matches!(
                    motion,
                    ParticleMotion::Bubble
                        | ParticleMotion::Wisp
                        | ParticleMotion::Spark
                        | ParticleMotion::Flame
                        | ParticleMotion::Frost
                        | ParticleMotion::Orbit
                        | ParticleMotion::Fall
                ),
                "{} has no particle motion",
                kind.label()
            );
            assert!(
                status_priority(kind) > 0,
                "{} has no priority",
                kind.label()
            );
        }
    }

    #[test]
    fn dangerous_body_states_win_over_mood_states() {
        assert!(status_priority(StatusKind::Burning) > status_priority(StatusKind::Happiness));
        assert!(status_priority(StatusKind::Choking) > status_priority(StatusKind::Euphoric));
        assert_eq!(status_motion(StatusKind::Burning), ParticleMotion::Flame);
        assert_eq!(status_motion(StatusKind::Chilled), ParticleMotion::Frost);
    }

    #[test]
    fn color_blending_preserves_chemical_identity() {
        let chemical = Color::srgb(0.1, 0.8, 0.2);
        let status = Color::srgb(0.9, 0.1, 0.8);
        let mixed = blend(chemical, status, 0.38).to_srgba();
        assert!(
            mixed.green > mixed.red,
            "the chemical's green should remain dominant"
        );
        assert!(
            mixed.blue > chemical.to_srgba().blue,
            "the status still contributes"
        );
    }

    #[test]
    fn every_catalog_reagent_can_drive_a_ground_profile() {
        let db = chemistry();
        for reagent in db.reagents.iter() {
            let puddle = ChemicalPuddle::from_solution(solution(&db, &reagent.key), None);
            let profile = puddle_profile(&puddle, &db)
                .unwrap_or_else(|| panic!("{} produced no ground particles", reagent.name));
            assert!(
                profile.rate > 0.0,
                "{} has a zero emission rate",
                reagent.name
            );
            assert!(
                profile.lifetime > 0.0 && profile.size > 0.0,
                "{} produced a non-visible particle",
                reagent.name
            );
        }
    }

    #[test]
    fn live_puddle_hazards_override_the_reagent_family() {
        let db = chemistry();
        let mut puddle = ChemicalPuddle::from_solution(solution(&db, "water"), None);
        assert_eq!(
            puddle_profile(&puddle, &db).unwrap().motion,
            ParticleMotion::Bubble
        );

        puddle.chill_intensity = 1.0;
        assert_eq!(
            puddle_profile(&puddle, &db).unwrap().motion,
            ParticleMotion::Frost
        );

        puddle.ignited = true;
        assert_eq!(
            puddle_profile(&puddle, &db).unwrap().motion,
            ParticleMotion::Flame,
            "visible fire must outrank the underlying cold/chemical family"
        );
    }

    #[test]
    fn bloodstream_color_survives_status_semantics() {
        let db = chemistry();
        let mut blood = ChemBloodstream::default();
        blood.blood = solution(&db, "uranium");
        blood.add_status(StatusKind::Irradiated, 5.0, 1.0);

        let profile = body_profile(&blood, &db).expect("active uranium should emit");
        assert_eq!(profile.motion, ParticleMotion::Orbit);
        let color = profile.color.to_srgba();
        assert!(
            color.green > color.red && color.green > color.blue,
            "uranium's green identity should survive the irradiated status tint"
        );
    }

    #[test]
    fn body_emitter_completes_visibility_without_moving_the_replicated_root() {
        let mut app = App::new();
        app.init_resource::<Assets<StandardMaterial>>()
            .init_resource::<ParticleMaterials>()
            .add_systems(Update, ensure_body_emitters);

        let original = Transform::from_xyz(4.0, 0.93, -2.0);
        let body = app
            .world_mut()
            .spawn((Bloodstream::default(), original))
            .id();
        app.update();

        assert_eq!(
            *app.world().get::<Transform>(body).unwrap(),
            original,
            "presentation must not move the replicated body root"
        );
        assert_eq!(
            app.world().get::<Visibility>(body),
            Some(&Visibility::Inherited),
            "a visible child requires visibility on every ancestor"
        );
        assert!(
            app.world()
                .get::<Children>(body)
                .is_some_and(|children| children
                    .iter()
                    .any(|child| app.world().get::<ChemicalParticleEmitter>(child).is_some())),
            "the completed root should receive exactly the intended presentation child"
        );
    }
}
