//! What chemistry does to the room.
//!
//! This is the consumer for reaction effects: it turns a line in a reaction's
//! data into a cloud hanging in the lab, a bang that takes the glassware, or a
//! force pulse that moves and disorients nearby bodies.
//!
//! Everything here is server-authoritative. Clouds replicate as entities, so no
//! `*Sync` message is needed; the *feel* of being caught in a blast is a
//! separate presentation-only message, because a screen flash is not state.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use chem_sim::body::{blast_radius, explosion_damage};
use chem_sim::{PulseKind, ReactionEffect, Route, Solution, Units};
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::net::is_authority;

use crate::audio::{EmitWorldSfx, Sfx};
use crate::body::{Bloodstream, Body, MetabolismClock};
use crate::chem_data::ChemDb;
use crate::chem_world::{
    assess_exposure, order_authorizes_dose, ChemicalExposure, ChemicallyReactive, ExposureSource,
};
use crate::containers::{ArmedCharge, Container, HeldBy, InSlot, InSlotB};
use crate::lab::{CrisisSpots, MapReady, Solid, WalkableAreas};
use crate::machines::{Machine, ReactionsFired};
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// How much of the batch a smoke cloud carries off with it.
///
/// Smoke that costs nothing is a light show. Taking a share of the beaker means
/// a reaction that smokes is a reaction you have to plan around.
const SMOKE_PAYLOAD: Units = Units::whole(10);

/// Seconds a cloud hangs around.
const SMOKE_LIFETIME: f32 = 12.0;

/// How much of its payload a cloud presses onto each body in it, per tick.
const SMOKE_DOSE: Units = Units::whole(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReactionSource {
    Container,
    Buffer,
    Stomach,
    Blood,
    Puddle,
}

#[derive(Clone, Copy, Debug)]
pub struct ReactionOrigin {
    pub kind: ReactionSource,
    pub position: Vec3,
    pub owner: Option<Entity>,
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ReactionHazards;

pub struct HazardPlugin;

impl Plugin for HazardPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<HazardScript>::new(&["hazards.ron"]))
            .add_message::<ChemicalExposure>()
            .add_message::<ChemicalImpulse>()
            .add_message::<ElectricalPulse>()
            .add_message::<EmitWorldSfx>()
            .add_server_message::<HazardFelt>(Channel::Ordered)
            .init_resource::<IncidentSchedule>()
            .init_resource::<IncidentGrace>()
            .add_systems(Startup, start_loading_hazards)
            .add_systems(
                Update,
                (
                    (
                        spawn_hazards
                            .in_set(ReactionHazards)
                            .before(crate::utility_ai::UtilityAiSet::Observe),
                        apply_electrical_pulses.before(crate::utility_ai::UtilityAiSet::Observe),
                        // Forced motion is an explicit post-navigation
                        // override. Utility routes, errands and Medical
                        // attachment finish first, then the blast displacement
                        // wins deterministically instead of racing a second
                        // Transform writer.
                        apply_chemical_impulses
                            .after(crate::utility_ai::UtilityAiSet::Navigate)
                            .before(crate::utility_ai::UtilityAiSet::Attach),
                        expose_to_smoke,
                        fade_smoke,
                    )
                        .chain()
                        .run_if(is_authority),
                    // A grace period on session start, not a phase: there is
                    // no "prep" any more for this to be the safe alternative
                    // to, just a short window so a brand new chemist is not
                    // hit by an incident before they have found the door.
                    (schedule_incidents, run_incidents)
                        .chain()
                        .run_if(crate::session::career_session)
                        .run_if(is_authority)
                        .run_if(resource_exists::<MapReady>),
                    // Both presentation, so neither is authority-gated: a
                    // joining client builds its own from the replicated data.
                    (build_smoke_visuals, build_hazard_visuals),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// Scripted incidents
// ---------------------------------------------------------------------------

/// `assets/data/station.hazards.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct HazardScript {
    /// Seconds after entering the lab before the first incident can be
    /// scheduled — long enough for a new chemist to find the door before
    /// anything goes wrong.
    pub grace_seconds: f32,
    pub gap_seconds: (f32, f32),
    pub warning_seconds: f32,
    pub incidents: Vec<IncidentDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IncidentDef {
    pub id: String,
    /// Semantic map marker where this incident manifests. The loaded map owns
    /// its actual coordinates, so rearranging the station cannot leave a
    /// hazard behind in the old lab footprint.
    pub spot: String,
    pub radius: f32,
    pub duration: f32,
    pub warning: String,
    pub onset: String,
    pub intensity: f32,
    /// The tint of the sphere the player actually sees, so a radiation leak and
    /// a coolant vent are told apart at a glance rather than by reading the
    /// radio. Defaulted so an incident that omits it still parses.
    #[serde(default = "default_hazard_color")]
    pub color: (f32, f32, f32),
    /// Whether this is a *radiation* incident, which is what trips the lab's
    /// rad klaxon (`audio::sync_radiation_alarm`) for as long as it runs.
    ///
    /// Narrower than "does it hurt you": every incident doses whoever stands
    /// in it, the coolant vent included, and that is deliberately untouched
    /// here. Only the leak sets off a counter. Defaulted to `false` so a new
    /// incident is silent until its data says otherwise, rather than
    /// inheriting an alarm nobody authored.
    #[serde(default)]
    pub radiological: bool,
}

/// Hazard amber, for an incident whose data does not pick a colour.
fn default_hazard_color() -> (f32, f32, f32) {
    (0.95, 0.65, 0.15)
}

#[derive(Resource)]
struct PendingHazardScript(Handle<HazardScript>);

/// Seconds this session has been running, for the opening grace period.
///
/// A resource rather than the `Local<f32>` it was, so `crate::session` can put
/// it back to zero: left as a `Local` it carried over, and the second save
/// opened in a process started with its grace period already spent — a brand
/// new chemist could be hit by an incident before finding the door.
#[derive(Resource, Default)]
pub struct IncidentGrace(pub f32);

/// When the next incident is due, and which one is running.
#[derive(Resource, Default)]
pub struct IncidentSchedule {
    /// `None` until the first one is scheduled for this shift.
    next_in: Option<f32>,
    warning_in: Option<f32>,
    pending: Option<IncidentDef>,
}

/// A hazard currently affecting part of the room.
///
/// An entity rather than a resource so replication carries it — the same reason
/// smoke clouds are entities.
#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct ActiveHazard {
    pub radius: f32,
    pub remaining: f32,
    pub intensity: f32,
    /// Carried on the replicated component rather than looked up from the
    /// script, so a joining client can build the sphere from the entity alone —
    /// it has no `IncidentDef` to consult, and the authority is the only peer
    /// that ever reads the hazard script.
    pub color: (f32, f32, f32),
    /// See [`IncidentDef::radiological`]. Carried across the wire for the same
    /// reason `color` is: the klaxon is presentation, both chemists are in the
    /// same room, and the client has no script to ask.
    pub radiological: bool,
}

fn start_loading_hazards(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingHazardScript(assets.load("data/station.hazards.ron")));
}

/// Picks an incident, warns the lab, then starts it.
fn schedule_incidents(
    mut commands: Commands,
    time: Res<Time>,
    // Time actually spent in the lab, not wall-clock time since the process
    // started — the grace period should not burn away while a player sits at
    // the menu. See [`IncidentGrace`] for why it is a resource and not the
    // `Local` it reads like.
    mut elapsed: ResMut<IncidentGrace>,
    scripts: Res<Assets<HazardScript>>,
    pending: Option<Res<PendingHazardScript>>,
    mut schedule: ResMut<IncidentSchedule>,
    spots: Res<CrisisSpots>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(script) = pending.and_then(|handle| scripts.get(&handle.0)) else {
        return;
    };
    let dt = time.delta_secs();
    elapsed.0 += dt;
    if elapsed.0 < script.grace_seconds {
        return;
    }

    let mut rng = rand::rng();

    // First call past the grace period: put one on the clock.
    if schedule.next_in.is_none() {
        schedule.next_in = Some(rng.random_range(script.gap_seconds.0..=script.gap_seconds.1));
        return;
    }

    // A warning already given: count down to the incident itself.
    if let Some(warning) = schedule.warning_in {
        let warning = warning - dt;
        if warning > 0.0 {
            schedule.warning_in = Some(warning);
            return;
        }
        schedule.warning_in = None;
        let Some(def) = schedule.pending.take() else {
            return;
        };
        schedule.next_in = Some(rng.random_range(script.gap_seconds.0..=script.gap_seconds.1));
        let Some(transform) = spots.get(&def.spot) else {
            error!(
                "hazard '{}' names missing crisis spot '{}'; skipping",
                def.id, def.spot
            );
            return;
        };

        commands.spawn((
            ActiveHazard {
                radius: def.radius,
                remaining: def.duration,
                intensity: def.intensity,
                color: def.color,
                radiological: def.radiological,
            },
            transform,
            Visibility::default(),
            Replicated,
            crate::until_we_leave_the_lab(),
        ));
        radio.push(
            RadioEntry::new(crate::radio::RadioChannel::Lab, def.onset.clone())
                .negative()
                .urgent(),
        );
        info!("hazard: {} for {}s", def.id, def.duration);
        return;
    }

    let due = schedule.next_in.unwrap_or_default() - dt;
    if due > 0.0 {
        schedule.next_in = Some(due);
        return;
    }

    let Some(def) = script.incidents.choose(&mut rng) else {
        return;
    };
    radio.push(
        RadioEntry::new(crate::radio::RadioChannel::Engineering, def.warning.clone())
            .speaker("Tech Lindqvist")
            .negative()
            .urgent(),
    );
    schedule.warning_in = Some(script.warning_seconds);
    schedule.pending = Some(def.clone());
}

/// Irradiates whoever is standing in an active hazard, and expires it.
fn run_incidents(
    mut commands: Commands,
    time: Res<Time>,
    clock: Res<MetabolismClock>,
    mut felt: MessageWriter<ToClients<HazardFelt>>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
    mut hazards: Query<(Entity, &mut ActiveHazard, &Transform)>,
    mut bodies: Query<
        (&Transform, &mut Bloodstream, &crate::player::Chemist),
        Without<ActiveHazard>,
    >,
) {
    let dt = time.delta_secs();
    for (entity, mut hazard, hazard_transform) in &mut hazards {
        hazard.remaining -= dt;
        if hazard.remaining <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        // Dosing rides the metabolism beat so it accrues at a rate a player can
        // reason about rather than one per rendered frame.
        if !clock.0.just_finished() {
            continue;
        }

        if let Some(sounds) = &mut sounds {
            sounds.write(EmitWorldSfx::new(
                Sfx::RadiationPulse,
                hazard_transform.translation,
            ));
        }

        for (body_transform, mut blood, chemist) in &mut bodies {
            if body_transform
                .translation
                .distance(hazard_transform.translation)
                > hazard.radius
            {
                continue;
            }
            // Topped up while you stand in it and decaying once you leave, so
            // walking out is a real answer and dosing hyronalin is the other.
            blood.0.add_status(
                chem_sim::StatusKind::Irradiated,
                chem_sim::body::TICK_SECONDS * 2.0,
                hazard.intensity,
            );
            felt.write(ToClients {
                targets: SendTargets::Single(chemist.client),
                message: HazardFelt {
                    kind: HazardKind::Radiation,
                    strength: hazard.intensity,
                },
            });
        }
    }
}

/// A cloud hanging in the room.
///
/// `remaining` is a plain `f32` rather than a `Timer`, the same reason
/// `Order.patience`/`waited` are: `Timer` is not `Serialize`, and this crosses
/// the wire.
#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SmokeCloud {
    pub radius: f32,
    pub remaining: f32,
}

/// What is in a cloud. A cloud is a solution with a position.
#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct SmokePayload(pub Solution);

/// Who was holding the vessel when it vented, when attributable.
#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SmokeOwner(#[entities] pub Option<Entity>);

/// Releases a prepared portable projector payload without pretending another
/// chemical reaction fired. Composition, purity and owner are carried by the
/// same replicated cloud components as reaction-generated smoke.
pub(crate) fn spawn_projected_smoke(
    commands: &mut Commands,
    payload: Solution,
    owner: Entity,
    origin: Vec3,
) -> Option<Entity> {
    if payload.is_empty() {
        return None;
    }
    let volume = payload.total_volume().as_f32();
    let radius = (2.2 + (volume / 10.0).sqrt() * 0.55).clamp(2.2, 4.0);
    Some(
        commands
            .spawn((
                SmokeCloud {
                    radius,
                    remaining: 16.0,
                },
                SmokePayload(payload),
                SmokeOwner(Some(owner)),
                Transform::from_translation(origin),
                Visibility::default(),
                Replicated,
                crate::until_we_leave_the_lab(),
            ))
            .id(),
    )
}

/// Marks the rendered sphere so the interaction raycast can ignore it.
///
/// Without this the whole lab becomes unusable the first time anything smokes:
/// the sphere has a mesh, so it sits between the crosshair and every machine in
/// the room.
#[derive(Component)]
pub struct SmokeVisual;

/// Something happened to you that the screen should react to.
///
/// Presentation only — the damage itself already arrived through the replicated
/// [`Body`]. This is the flash and the shake, which are not state and must not
/// be replicated as such.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct HazardFelt {
    pub kind: HazardKind,
    pub strength: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HazardKind {
    Blast,
    Smoke,
    Radiation,
    /// A rogue officer turning physical — see
    /// `rogue_security::expire_rogue_encounters`. The one non-chemistry
    /// source of `HazardFelt`, added for exactly the same reason every
    /// other one exists: the damage already arrived through the replicated
    /// `Body`, and this is only the screen reacting to it.
    Assault,
}

/// One authority-owned displacement generated by a chemical pulse.
///
/// Kept as a message between the effect consumer and the movement system so
/// several vessels going off in one frame accumulate instead of overwriting a
/// temporary component on the same body.
#[derive(Message, Clone, Copy, Debug)]
struct ChemicalImpulse {
    target: Entity,
    displacement: Vec3,
}

#[derive(Message, Clone, Copy, Debug)]
struct ElectricalPulse {
    origin: Vec3,
    emp_power: f32,
    arc_power: f32,
}

#[derive(SystemParam)]
struct HazardMessages<'w> {
    felt: MessageWriter<'w, ToClients<HazardFelt>>,
    impulses: MessageWriter<'w, ChemicalImpulse>,
    electrical: MessageWriter<'w, ElectricalPulse>,
}

fn pulse_radius(power: f32) -> f32 {
    (1.5 + power.sqrt() * 1.25).clamp(1.5, 8.0)
}

fn pulse_falloff(power: f32, distance: f32) -> f32 {
    (1.0 - distance / pulse_radius(power)).clamp(0.0, 1.0)
}

fn pulse_displacement(origin: Vec3, position: Vec3, power: f32, inward: bool) -> Vec3 {
    let mut offset = position - origin;
    offset.y = 0.0;
    let distance = offset.length();
    let falloff = pulse_falloff(power, distance);
    if falloff <= 0.0 {
        return Vec3::ZERO;
    }
    let direction = if distance <= f32::EPSILON {
        if inward {
            return Vec3::ZERO;
        }
        Vec3::X
    } else {
        offset / distance
    };
    let travel = (0.5 + power.sqrt() * 0.65).clamp(0.5, 3.0) * falloff;
    direction * travel * if inward { -1.0 } else { 1.0 }
}

/// The room position of a container, following its holder if it is being
/// carried.
///
/// A held container's own `Transform` is only meaningful on the client holding
/// it — the server never moves it, because `carry_held_containers` is a local
/// presentation trick. So a beaker that goes off in your hand has to be located
/// by finding the hand.
fn container_context(
    container: Entity,
    transforms: &Query<&Transform>,
    held: &Query<&HeldBy>,
    slots: &Query<&InSlot>,
    slots_b: &Query<&InSlotB>,
    machines: &Query<&Machine>,
    armed: &Query<&ArmedCharge>,
) -> Option<(Vec3, Option<Entity>)> {
    if let Ok(holder) = held.get(container) {
        return transforms
            .get(holder.0)
            .ok()
            .map(|transform| (transform.translation, Some(holder.0)));
    }
    // Explicit agitation can run inside the machine's internal chamber.
    // Its effects still originate at the workstation and belong to its operator.
    if let Ok(machine) = machines.get(container) {
        return transforms
            .get(container)
            .ok()
            .map(|transform| (transform.translation, machine.in_use_by));
    }
    let machine = slots
        .get(container)
        .ok()
        .map(|slot| slot.0)
        .or_else(|| slots_b.get(container).ok().map(|slot| slot.0));
    if let Some(machine_entity) = machine {
        let origin = transforms.get(machine_entity).ok()?.translation;
        let operator = machines
            .get(machine_entity)
            .ok()
            .and_then(|machine| machine.in_use_by);
        return Some((origin, operator));
    }
    transforms.get(container).ok().map(|transform| {
        (
            transform.translation,
            armed.get(container).ok().map(|charge| charge.owner),
        )
    })
}

/// Turns reported effects into things in the room.
#[derive(SystemParam)]
struct ReactionSolutions<'w, 's> {
    containers: Query<'w, 's, &'static mut Container>,
    buffers: Query<'w, 's, &'static mut crate::machines::Buffer>,
    puddles: Query<'w, 's, &'static mut crate::chem_world::ChemicalPuddle>,
}

#[allow(clippy::too_many_arguments)]
fn spawn_hazards(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut reports: MessageReader<ReactionsFired>,
    mut messages: HazardMessages,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
    mut radio: ResMut<RadioLog>,
    transforms: Query<&Transform>,
    held: Query<&HeldBy>,
    slots: Query<&InSlot>,
    slots_b: Query<&InSlotB>,
    machines: Query<&Machine>,
    armed: Query<&ArmedCharge>,
    mut solutions: ReactionSolutions,
    mut bodies: Query<(
        Entity,
        &mut Body,
        Option<&mut Bloodstream>,
        Option<&crate::player::Chemist>,
    )>,
    doors: Query<(Entity, &Transform), With<crate::door::Door>>,
    breachable: Query<(Entity, &Transform), (With<ChemicallyReactive>, Without<crate::door::Door>)>,
) {
    for report in reports.read() {
        if report.effects.is_empty() {
            continue;
        }

        // A chain can fire the same reaction more than once in one resolve, so
        // fold before acting: one cloud at the widest radius, one blast at the
        // combined power. Otherwise a two-step recipe smokes twice.
        let mut radius: f32 = 0.0;
        let mut power: f32 = 0.0;
        let mut push_power: f32 = 0.0;
        let mut pull_power: f32 = 0.0;
        let mut concuss_power: f32 = 0.0;
        let mut emp_power: f32 = 0.0;
        let mut arc_power: f32 = 0.0;
        let mut burn_power: f32 = 0.0;
        for effect in &report.effects {
            match effect {
                ReactionEffect::Burn(strength) => burn_power += strength,
                ReactionEffect::Smoke(spread) => radius = radius.max(*spread),
                ReactionEffect::Explosion(strength) => power += strength,
                ReactionEffect::Pulse {
                    kind: PulseKind::Push,
                    power,
                } => push_power += power,
                ReactionEffect::Pulse {
                    kind: PulseKind::Pull,
                    power,
                } => pull_power += power,
                ReactionEffect::Pulse {
                    kind: PulseKind::Concuss,
                    power,
                } => concuss_power += power,
                // Profiles are resolved to concrete energy inside chem_sim;
                // receiving one here would indicate a malformed report.
                ReactionEffect::ExplosionProfile { .. } | ReactionEffect::PulseProfile { .. } => {}
                ReactionEffect::Emp(power) => emp_power += power,
                ReactionEffect::Electric(power) => arc_power += power,
                ReactionEffect::EmpProfile { .. } | ReactionEffect::ElectricProfile { .. } => {}
                ReactionEffect::Heat(_) => {}
            }
        }

        let context = report.source.map(|s| (s.position, s.owner)).or_else(|| {
            container_context(
                report.container,
                &transforms,
                &held,
                &slots,
                &slots_b,
                &machines,
                &armed,
            )
        });
        let Some((origin, operator)) = context else {
            continue;
        };
        let kind = report.source.map_or_else(
            || {
                if solutions.buffers.contains(report.container) {
                    ReactionSource::Buffer
                } else {
                    ReactionSource::Container
                }
            },
            |s| s.kind,
        );

        if burn_power > 0.0 {
            for (entity, mut body, mut blood, _) in &mut bodies {
                let Ok(transform) = transforms.get(entity) else {
                    continue;
                };
                let distance = transform.translation.distance(origin);
                if distance > 1.5 {
                    continue;
                }
                let previous = body.0.collapsed;
                body.0.apply(chem_sim::Damage::of(
                    chem_sim::DamageKind::Burn,
                    Units::from_f64((burn_power * (1.0 - distance / 1.5)) as f64),
                ));
                if let Some(blood) = blood.as_deref_mut() {
                    blood.0.reconcile_collapse(&mut body.0, previous);
                }
            }
        }

        if radius > 0.0 {
            // The cloud takes a share of the batch with it.
            let payload = if let Ok(mut container) = solutions.containers.get_mut(report.container)
            {
                container.mutate(&db, |s| s.split(SMOKE_PAYLOAD)).0
            } else if let Ok(mut buffer) = solutions.buffers.get_mut(report.container) {
                buffer.0.split(SMOKE_PAYLOAD)
            } else if let Ok(mut puddle) = solutions.puddles.get_mut(report.container) {
                puddle.solution.split(SMOKE_PAYLOAD)
            } else if let Ok((_, _, Some(mut blood), _)) = bodies.get_mut(report.container) {
                match kind {
                    ReactionSource::Stomach => blood.0.stomach.split(SMOKE_PAYLOAD),
                    ReactionSource::Blood => blood.0.blood.split(SMOKE_PAYLOAD),
                    _ => Solution::unbounded(),
                }
            } else {
                Solution::unbounded()
            };

            commands.spawn((
                SmokeCloud {
                    radius,
                    remaining: SMOKE_LIFETIME,
                },
                SmokePayload(payload),
                SmokeOwner(operator),
                Transform::from_translation(origin),
                Visibility::default(),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Lab,
                    "Something in the chem lab just started venting.",
                )
                .negative()
                .urgent(),
            );
            if let Some(sounds) = &mut sounds {
                sounds.write(EmitWorldSfx::new(Sfx::HazardSmoke, origin));
            }
        }

        if power > 0.0 {
            crate::stagecraft::blast(&mut commands, origin, power);
            if power >= 1.0 {
                if let Some(sounds) = &mut sounds {
                    sounds.write(EmitWorldSfx::new(Sfx::HazardExplosion, origin));
                }
            }
            // Glassware is destroyed; a fixed machine loses only its batch.
            if kind == ReactionSource::Buffer && power >= 1.0 {
                if let Ok(mut buffer) = solutions.buffers.get_mut(report.container) {
                    buffer.0.clear();
                }
            } else if kind == ReactionSource::Container && power >= 1.0 {
                if let Ok(mut entity) = commands.get_entity(report.container) {
                    entity.despawn();
                }
            }

            let reach = blast_radius(power);
            for (entity, mut body, mut blood, chemist) in &mut bodies {
                let Ok(transform) = transforms.get(entity) else {
                    continue;
                };
                let distance = transform.translation.distance(origin);
                let damage = explosion_damage(power, distance);
                if damage.total().is_zero() {
                    continue;
                }
                let previously_collapsed = body.0.collapsed;
                body.0.apply(damage);
                if let Some(blood) = blood.as_deref_mut() {
                    blood
                        .0
                        .reconcile_collapse(&mut body.0, previously_collapsed);
                }
                if let Some(chemist) = chemist {
                    messages.felt.write(ToClients {
                        targets: SendTargets::Single(chemist.client),
                        message: HazardFelt {
                            kind: HazardKind::Blast,
                            strength: (1.0 - distance / reach).clamp(0.0, 1.0),
                        },
                    });
                }
            }

            // The inner half of a blast breaches doors and explicitly
            // reactive scenery. Core machines are not in either query.
            let breach_radius = reach * 0.5;
            for (entity, transform) in &doors {
                if transform.translation.distance(origin) <= breach_radius {
                    commands
                        .entity(entity)
                        .insert(crate::door::Corroded { strength: 5.0 });
                }
            }
            for (entity, transform) in &breachable {
                if transform.translation.distance(origin) <= breach_radius {
                    commands
                        .entity(entity)
                        .insert(crate::door::Corroded { strength: 5.0 });
                }
            }
            // Trace reactions already have a quiet reaction cue. Do not turn
            // a slow trickle into a station-wide emergency every quantum.
            if power >= 1.0 {
                radio.push(
                    RadioEntry::new(
                        crate::radio::RadioChannel::Lab,
                        "Was that a bang? Chemistry, report.",
                    )
                    .negative()
                    .urgent(),
                );
            }
        }

        let strongest_pulse = push_power.max(pull_power).max(concuss_power);
        if strongest_pulse > 0.0 {
            if let Some(sounds) = &mut sounds {
                sounds.write(EmitWorldSfx::new(Sfx::HazardExplosion, origin));
            }
            for (entity, _body, mut blood, chemist) in &mut bodies {
                let Ok(transform) = transforms.get(entity) else {
                    continue;
                };
                let position = transform.translation;
                let displacement = pulse_displacement(origin, position, push_power, false)
                    + pulse_displacement(origin, position, pull_power, true);
                if displacement.length_squared() > 0.0 {
                    messages.impulses.write(ChemicalImpulse {
                        target: entity,
                        displacement,
                    });
                }

                let distance = position.distance(origin);
                let concussion = if concuss_power > 0.0 {
                    pulse_falloff(concuss_power, distance)
                } else {
                    0.0
                };
                if concussion > 0.0 {
                    if let Some(blood) = blood.as_deref_mut() {
                        blood.0.add_status(
                            chem_sim::StatusKind::Muted,
                            2.0 + concussion * 4.0,
                            0.5 + concussion,
                        );
                        blood.0.add_status(
                            chem_sim::StatusKind::Unsteady,
                            2.0 + concussion * 3.0,
                            0.5 + concussion,
                        );
                    }
                }
                if let Some(chemist) = chemist {
                    let strength = pulse_falloff(strongest_pulse, distance);
                    if strength > 0.0 {
                        messages.felt.write(ToClients {
                            targets: SendTargets::Single(chemist.client),
                            message: HazardFelt {
                                kind: HazardKind::Blast,
                                strength,
                            },
                        });
                    }
                }
            }
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Lab,
                    "A pressure pulse just came out of the chemistry lab.",
                )
                .negative()
                .urgent(),
            );
        }

        if emp_power > 0.0 || arc_power > 0.0 {
            messages.electrical.write(ElectricalPulse {
                origin,
                emp_power,
                arc_power,
            });
            if let Some(sounds) = &mut sounds {
                sounds.write(EmitWorldSfx::new(Sfx::HazardExplosion, origin));
            }
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Engineering,
                    "Electrical interference just surged out of the chemistry lab.",
                )
                .negative()
                .urgent(),
            );
        }
    }
}

fn electrical_radius(power: f32) -> f32 {
    (2.0 + power.sqrt() * 1.4).clamp(2.0, 9.0)
}

#[allow(clippy::type_complexity)]
fn apply_electrical_pulses(
    mut pulses: MessageReader<ElectricalPulse>,
    mut felt: MessageWriter<ToClients<HazardFelt>>,
    mut bodies: Query<(
        &Transform,
        &mut Body,
        Option<&mut Bloodstream>,
        Option<&crate::player::Chemist>,
    )>,
    mut machines: Query<(&Transform, &mut Machine), Without<Body>>,
    mut doors: Query<(&Transform, &mut crate::door::Door), (Without<Body>, Without<Machine>)>,
) {
    for pulse in pulses.read() {
        let equipment_power = pulse.emp_power.max(pulse.arc_power * 0.5);
        if equipment_power > 0.0 {
            let reach = electrical_radius(equipment_power);
            let disable_for = 4.0 + equipment_power.sqrt() * 2.0;
            for (transform, mut machine) in &mut machines {
                if transform.translation.distance(pulse.origin) <= reach {
                    machine.disabled_for = machine.disabled_for.max(disable_for);
                    machine.in_use_by = None;
                }
            }
            for (transform, mut door) in &mut doors {
                if transform.translation.distance(pulse.origin) <= reach {
                    door.disabled_for = door.disabled_for.max(disable_for);
                    door.open = true;
                }
            }
        }

        if pulse.arc_power <= 0.0 {
            continue;
        }
        let reach = electrical_radius(pulse.arc_power);
        for (transform, mut body, mut blood, chemist) in &mut bodies {
            let distance = transform.translation.distance(pulse.origin);
            let falloff = (1.0 - distance / reach).clamp(0.0, 1.0);
            if falloff <= 0.0 {
                continue;
            }
            let previously_collapsed = body.0.collapsed;
            let burn = Units::from_f64(((1.5 + pulse.arc_power.sqrt() * 2.0) * falloff) as f64);
            body.0
                .apply(chem_sim::Damage::of(chem_sim::DamageKind::Burn, burn));
            if let Some(blood) = blood.as_deref_mut() {
                blood
                    .0
                    .add_status(chem_sim::StatusKind::Unsteady, 3.0 + falloff * 3.0, 1.5);
                blood
                    .0
                    .reconcile_collapse(&mut body.0, previously_collapsed);
            }
            if let Some(chemist) = chemist {
                felt.write(ToClients {
                    targets: SendTargets::Single(chemist.client),
                    message: HazardFelt {
                        kind: HazardKind::Blast,
                        strength: falloff,
                    },
                });
            }
        }
    }
}

/// Applies pulse movement after all reaction reports have been consumed, using
/// the same collision resolution as ordinary walking.
fn apply_chemical_impulses(
    mut impulses: MessageReader<ChemicalImpulse>,
    mut bodies: Query<&mut Transform, With<Body>>,
    solids: Query<(&Transform, &Solid), Without<Body>>,
    areas: Option<Res<WalkableAreas>>,
) {
    for impulse in impulses.read() {
        let Ok(mut transform) = bodies.get_mut(impulse.target) else {
            continue;
        };
        transform.translation = crate::player::resolve_forced_body_position(
            transform.translation,
            impulse.displacement,
            solids.iter(),
            areas.as_deref(),
        );
    }
}

/// Presses a cloud's contents onto anyone standing in it.
///
/// Runs on the metabolism clock rather than every frame, so a chemist who walks
/// through a cloud takes a dose rather than one per frame at whatever rate their
/// machine happens to render.
///
/// Crew are dosed too, since M12 gave them a `Body`/`Bloodstream` of their
/// own — a second, disjoint query rather than folding them into the chemist
/// one, because there is no `HazardFelt` to send them: they have no
/// `ClientId` to target, and their side of "the screen reacting" is the
/// `fx::animate_crew_body` wobble/tint watching their `Bloodstream` directly,
/// which arguably reads clearer for a bystander than a shake would anyway.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn expose_to_smoke(
    time: Res<Time>,
    db: Res<ChemDb>,
    clock: Res<MetabolismClock>,
    mut felt: MessageWriter<ToClients<HazardFelt>>,
    mut clouds: Query<(
        &SmokeCloud,
        &Transform,
        &mut SmokePayload,
        Option<&SmokeOwner>,
    )>,
    mut bodies: Query<(
        Entity,
        &Transform,
        &mut Body,
        &mut Bloodstream,
        &crate::player::Chemist,
    )>,
    mut crew_bodies: Query<
        (Entity, &Transform, &mut Body, &mut Bloodstream),
        (
            With<crate::crew::CrewMember>,
            Without<crate::player::Chemist>,
        ),
    >,
    orders: Query<(
        &crate::orders::Order,
        Has<crate::orders::IllicitOrder>,
        Has<crate::orders::CrisisOrder>,
        Has<crate::orders::CounterOrder>,
    )>,
    crisis_targets: Query<(), With<crate::crisis::CrisisResponse>>,
    mut exposures: MessageWriter<ChemicalExposure>,
) {
    // The clock is ticked by `run_metabolism`, which is chained after this;
    // reading `just_finished` here means exposure lands on the same beat.
    let _ = time;
    if !clock.0.just_finished() {
        return;
    }

    for (cloud, cloud_transform, mut payload, owner) in &mut clouds {
        let owner = owner.and_then(|owner| owner.0);
        if payload.0.is_empty() {
            continue;
        }
        for (target, body_transform, mut body, mut blood, chemist) in &mut bodies {
            let distance = body_transform
                .translation
                .distance(cloud_transform.translation);
            if distance > cloud.radius {
                continue;
            }

            let mut dose = payload.0.split(SMOKE_DOSE);
            if dose.total_volume().is_zero() {
                continue;
            }
            // Inhalation reaches blood more efficiently than a splash, but
            // cannot trigger skin contact or topical repair.
            let snapshot = dose.clone();
            let assessment = assess_exposure(&snapshot, Route::Inhaled, &body, &blood, &db);
            blood.0.receive(&mut dose, Route::Inhaled, &mut body.0, &db);
            exposures.write(ChemicalExposure {
                actor: owner,
                target,
                route: Route::Inhaled,
                source: ExposureSource::Smoke,
                solution: snapshot,
                // A cloud attributable to a chemist is consensual for either
                // member of that chemist's team. Crew authorization is
                // evaluated separately against their own request below.
                authorized: owner.is_some(),
                helpful: assessment.helpful,
                harmful: assessment.harmful,
                illicit: assessment.illicit,
                overdose: assessment.overdose,
            });
            felt.write(ToClients {
                targets: SendTargets::Single(chemist.client),
                message: HazardFelt {
                    kind: HazardKind::Smoke,
                    strength: 0.5,
                },
            });
        }
        for (target, body_transform, mut body, mut blood) in &mut crew_bodies {
            let distance = body_transform
                .translation
                .distance(cloud_transform.translation);
            if distance > cloud.radius {
                continue;
            }
            let mut dose = payload.0.split(SMOKE_DOSE);
            if dose.total_volume().is_zero() {
                continue;
            }
            let snapshot = dose.clone();
            let assessment = assess_exposure(&snapshot, Route::Inhaled, &body, &blood, &db);
            let requested = orders
                .get(target)
                .is_ok_and(|(order, illicit, crisis, counter)| {
                    !assessment.overdose
                        && order_authorizes_dose(
                            &snapshot,
                            order,
                            crate::orders::OrderKind::of(illicit, crisis, counter),
                            &db,
                        )
                });
            let crisis_care = crisis_targets.contains(target)
                && assessment.helpful
                && !assessment.illicit
                && !assessment.overdose;
            blood.0.receive(&mut dose, Route::Inhaled, &mut body.0, &db);
            exposures.write(ChemicalExposure {
                actor: owner,
                target,
                route: Route::Inhaled,
                source: ExposureSource::Smoke,
                solution: snapshot,
                authorized: owner == Some(target) || requested || crisis_care,
                helpful: assessment.helpful,
                harmful: assessment.harmful,
                illicit: assessment.illicit,
                overdose: assessment.overdose,
            });
        }
    }
}

fn fade_smoke(
    mut commands: Commands,
    time: Res<Time>,
    mut clouds: Query<(Entity, &mut SmokeCloud)>,
) {
    for (entity, mut cloud) in &mut clouds {
        cloud.remaining -= time.delta_secs();
        if cloud.remaining <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

/// Builds the sphere for a cloud, locally on each client.
///
/// The same split every other visual in this game uses: replication carries the
/// data, each end builds its own meshes and materials.
fn build_smoke_visuals(
    mut commands: Commands,
    db: Option<Res<ChemDb>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    new_clouds: Query<(Entity, &SmokeCloud, &SmokePayload), Added<SmokeCloud>>,
) {
    let Some(db) = db else {
        return;
    };
    for (entity, cloud, payload) in &new_clouds {
        let [r, g, b] = payload.0.color(&db.reagents);
        let material = materials.add(StandardMaterial {
            base_color: Color::srgba(r, g, b, 0.28),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        });
        commands.spawn((
            Mesh3d(meshes.add(Sphere::new(cloud.radius))),
            MeshMaterial3d(material),
            Transform::default(),
            SmokeVisual,
            ChildOf(entity),
        ));
    }
}

/// Marks the sphere drawn for an [`ActiveHazard`].
///
/// Excluded from the interaction raycast for exactly the reason `SmokeVisual`
/// is: the rad leak is a 4.5m sphere centred on the ChemMaster 5000, so without this
/// an incident would make the ChemMaster 5000 — and half the hall behind it —
/// unusable for the whole forty seconds it runs.
#[derive(Component)]
pub struct HazardVisual;

/// Builds the sphere for a scripted incident, locally on each client.
///
/// The counterpart to [`build_smoke_visuals`], and missing until now: an
/// `ActiveHazard` was spawned with a `Transform` and a `Visibility` but no mesh
/// at all, so the radiation leak and the coolant vent were unmarked volumes.
/// The player got a radio warning naming a rough location in prose and then a
/// green screen flash once they were already standing in it — the intended
/// counterplay, "walk out of it", had nothing to walk out of.
///
/// Runs on every peer with no authority gate, keyed on `Added<ActiveHazard>`:
/// replication carries the data, each end builds its own meshes and materials.
fn build_hazard_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    new_hazards: Query<(Entity, &ActiveHazard), Added<ActiveHazard>>,
) {
    for (entity, hazard) in &new_hazards {
        let (r, g, b) = hazard.color;
        let material = materials.add(StandardMaterial {
            base_color: Color::srgba(r, g, b, 0.16),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            // Visible from inside as well as outside. Backface culling would
            // make the boundary vanish the moment you stepped through it,
            // which is precisely when you most need to see where it is.
            cull_mode: None,
            double_sided: true,
            ..default()
        });
        commands.spawn((
            Mesh3d(meshes.add(Sphere::new(hazard.radius))),
            MeshMaterial3d(material),
            Transform::default(),
            HazardVisual,
            ChildOf(entity),
        ));
    }
}

#[cfg(test)]
mod tests {
    //! Headless: an effect goes in, something happens in the room.

    use super::*;
    use crate::containers::ContainerKind;
    use crate::player::Chemist;
    use chem_sim::ChemData;
    use std::collections::HashSet;
    use std::time::Duration;

    fn test_app() -> App {
        let data = ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should load");

        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .init_resource::<RadioLog>()
            .init_resource::<MetabolismClock>()
            .init_resource::<Time>()
            .add_message::<ReactionsFired>()
            .add_message::<ChemicalExposure>()
            .add_message::<ChemicalImpulse>()
            .add_message::<ElectricalPulse>()
            .add_message::<ToClients<HazardFelt>>()
            .add_systems(
                Update,
                (
                    spawn_hazards,
                    apply_electrical_pulses,
                    apply_chemical_impulses,
                    expose_to_smoke,
                    fade_smoke,
                )
                    .chain(),
            );
        app
    }

    /// A beaker sitting on a bench at `position`, holding `contents`.
    #[test]
    fn sb14_internal_explosion_keeps_patient_and_applies_point_blank_damage_once() {
        let mut app = test_app();
        app.add_systems(
            Update,
            crate::body::forward_body_reactions.before(spawn_hazards),
        );
        let victim = chemist_at(&mut app, Vec3::ZERO);
        let bystander = crew_at(&mut app, Vec3::X);
        let db = app.world().resource::<ChemDb>().0.clone();
        let mut blood = chem_sim::Bloodstream::default();
        let mut vitals = chem_sim::Vitals::default();
        let mut dose = Solution::unbounded();
        let _ = dose.add(db.reagent("potassium"), Units::whole(6));
        let _ = dose.add(db.reagent("water"), Units::whole(6));
        blood.receive(&mut dose, Route::Injected, &mut vitals, &db);
        app.world_mut()
            .entity_mut(victim)
            .insert((Bloodstream(blood), Body(vitals)));
        app.update();
        let expected = explosion_damage(12.0, 0.0);
        assert_eq!(app.world().get::<Body>(victim).unwrap().0.damage, expected);
        assert!(app.world().get::<Body>(victim).unwrap().0.collapsed);
        assert_eq!(
            app.world().get::<Body>(bystander).unwrap().0.damage,
            explosion_damage(12.0, 1.0)
        );
        app.update();
        assert_eq!(app.world().get::<Body>(victim).unwrap().0.damage, expected);
    }

    #[test]
    fn sb15_puddle_smoke_moves_payload_without_duplication() {
        let mut app = test_app();
        let water = app.world().resource::<ChemDb>().reagent("water");
        let mut s = Solution::unbounded();
        let _ = s.add(water, Units::whole(20));
        let puddle = app
            .world_mut()
            .spawn((
                crate::chem_world::ChemicalPuddle::from_solution(s, None),
                Transform::default(),
            ))
            .id();
        app.world_mut().write_message(ReactionsFired {
            source: Some(ReactionOrigin {
                kind: ReactionSource::Puddle,
                position: Vec3::ZERO,
                owner: None,
            }),
            container: puddle,
            effects: vec![ReactionEffect::Smoke(2.0)],
            reactions: Vec::new(),
            distinct_reagents: 1,
        });
        app.update();
        let left = app
            .world()
            .get::<crate::chem_world::ChemicalPuddle>(puddle)
            .unwrap()
            .solution
            .total_volume();
        let carried: Units = app
            .world_mut()
            .query::<&SmokePayload>()
            .iter(app.world())
            .map(|s| s.0.total_volume())
            .sum();
        assert_eq!(left, Units::whole(10));
        assert_eq!(left + carried, Units::whole(20));
    }

    /// A beaker sitting on a bench at `position`, holding `contents`.
    fn beaker_at(app: &mut App, position: Vec3, contents: &[(&str, i32)]) -> Entity {
        let db = app.world().resource::<ChemDb>().0.clone();
        let mut container = Container::new(ContainerKind::LargeBeaker);
        for (key, amount) in contents {
            let overflow = container
                .solution
                .add(db.reagent(key), Units::whole(*amount));
            assert!(overflow.is_zero(), "{key} overflowed the test beaker");
        }
        app.world_mut()
            .spawn((container, Transform::from_translation(position)))
            .id()
    }

    fn chemist_at(app: &mut App, position: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                Chemist {
                    client: ClientId::Server,
                },
                Body::default(),
                Bloodstream::default(),
                Transform::from_translation(position),
            ))
            .id()
    }

    /// A crew member with a body, standing at `position` — M12's proof that
    /// hazards no longer only ever touch the chemist.
    fn crew_at(app: &mut App, position: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                crate::crew::CrewMember {
                    name: "Bystander".to_string(),
                    role: "Service".to_string(),
                },
                Body::default(),
                Bloodstream::default(),
                Transform::from_translation(position),
            ))
            .id()
    }

    fn report(app: &mut App, container: Entity, effects: Vec<ReactionEffect>) {
        app.world_mut().write_message(ReactionsFired {
            source: None,
            reactions: Vec::new(),
            container,
            effects,
            distinct_reagents: 0,
        });
        app.update();
    }

    fn clouds(app: &mut App) -> usize {
        app.world_mut()
            .query::<&SmokeCloud>()
            .iter(app.world())
            .count()
    }

    /// Runs one metabolism beat, which is when smoke exposure lands.
    fn one_tick(app: &mut App) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(2.0));
        app.world_mut()
            .resource_mut::<MetabolismClock>()
            .0
            .tick(Duration::from_secs_f32(2.0));
        app.update();
    }

    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(seconds));
        app.update();
    }

    #[test]
    fn a_smoke_effect_hangs_a_cloud_where_the_beaker_was() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::new(2.0, 1.0, -1.0), &[("radium", 40)]);

        report(&mut app, beaker, vec![ReactionEffect::Smoke(2.5)]);

        assert_eq!(clouds(&mut app), 1);
        let mut query = app.world_mut().query::<(&SmokeCloud, &Transform)>();
        let (cloud, transform) = query.iter(app.world()).next().unwrap();
        assert_eq!(cloud.radius, 2.5);
        assert_eq!(transform.translation, Vec3::new(2.0, 1.0, -1.0));
    }

    #[test]
    fn machine_smoke_is_located_at_and_attributed_to_its_operator() {
        let mut app = test_app();
        let operator = app.world_mut().spawn_empty().id();
        let machine = app
            .world_mut()
            .spawn((
                Machine {
                    kind: crate::machines::MachineKind::MixingChamber,
                    in_use_by: Some(operator),
                    disabled_for: 0.0,
                },
                Transform::from_xyz(4.0, 0.8, -2.0),
            ))
            .id();
        let beaker = beaker_at(&mut app, Vec3::new(50.0, 0.0, 50.0), &[("radium", 40)]);
        app.world_mut().entity_mut(beaker).insert(InSlot(machine));
        let bystander = crew_at(&mut app, Vec3::new(4.0, 0.8, -2.0));

        report(&mut app, beaker, vec![ReactionEffect::Smoke(2.5)]);

        let mut query = app.world_mut().query::<(&SmokeOwner, &Transform)>();
        let (owner, transform) = query.iter(app.world()).next().unwrap();
        assert_eq!(owner.0, Some(operator));
        assert_eq!(transform.translation, Vec3::new(4.0, 0.8, -2.0));

        one_tick(&mut app);
        let records: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ChemicalExposure>>()
            .drain()
            .filter(|record| record.target == bystander)
            .collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].actor, Some(operator));
        assert_eq!(records[0].source, ExposureSource::Smoke);
        assert_eq!(records[0].route, Route::Inhaled);
    }

    #[test]
    fn a_cloud_costs_the_batch_it_came_from() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 40)]);

        report(&mut app, beaker, vec![ReactionEffect::Smoke(2.0)]);

        let left = app.world().get::<Container>(beaker).unwrap();
        assert_eq!(
            left.solution.total_volume(),
            Units::whole(30),
            "smoke that costs nothing is a light show"
        );
    }

    #[test]
    fn duplicate_effects_from_one_resolve_make_a_single_cloud() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 40)]);

        // A chain firing the same smoking reaction twice in one resolve.
        report(
            &mut app,
            beaker,
            vec![ReactionEffect::Smoke(1.0), ReactionEffect::Smoke(3.0)],
        );

        assert_eq!(clouds(&mut app), 1, "one resolve, one cloud");
        let mut query = app.world_mut().query::<&SmokeCloud>();
        assert_eq!(
            query.iter(app.world()).next().unwrap().radius,
            3.0,
            "at the widest radius reported"
        );
    }

    #[test]
    fn a_cloud_doses_whoever_is_standing_in_it_and_nobody_else() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("plasma", 40)]);
        let inside = chemist_at(&mut app, Vec3::new(1.0, 0.0, 0.0));
        let outside = chemist_at(&mut app, Vec3::new(6.0, 0.0, 0.0));

        report(&mut app, beaker, vec![ReactionEffect::Smoke(2.5)]);
        one_tick(&mut app);

        let plasma = app.world().resource::<ChemDb>().reagent("plasma");
        assert!(
            app.world()
                .get::<Bloodstream>(inside)
                .unwrap()
                .0
                .blood
                .volume_of(plasma)
                .is_positive(),
            "standing in it should get you a dose"
        );
        assert!(
            app.world()
                .get::<Bloodstream>(outside)
                .unwrap()
                .0
                .is_empty(),
            "and standing clear of it should not"
        );
    }

    #[test]
    fn a_cloud_doses_a_bystander_crew_member_too() {
        // M12: crew are no longer structurally immune to a chemist's own
        // chemistry — they lack `Chemist`, not `Body`, so the crew-side
        // query in `expose_to_smoke` has to catch them independently.
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("plasma", 40)]);
        let bystander = crew_at(&mut app, Vec3::new(1.0, 0.0, 0.0));

        report(&mut app, beaker, vec![ReactionEffect::Smoke(2.5)]);
        one_tick(&mut app);

        let plasma = app.world().resource::<ChemDb>().reagent("plasma");
        assert!(
            app.world()
                .get::<Bloodstream>(bystander)
                .unwrap()
                .0
                .blood
                .volume_of(plasma)
                .is_positive(),
            "a crew member standing in the cloud should be dosed exactly like a chemist"
        );
    }

    #[test]
    fn a_cloud_clears() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 40)]);
        report(&mut app, beaker, vec![ReactionEffect::Smoke(2.0)]);
        assert_eq!(clouds(&mut app), 1);

        for _ in 0..((SMOKE_LIFETIME / 0.5).ceil() as u32 + 1) {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(Duration::from_secs_f32(0.5));
            app.update();
        }

        assert_eq!(clouds(&mut app), 0, "smoke clears");
    }

    #[test]
    fn chamber_smoke_conserves_payload_and_identifies_operator() {
        let mut app = test_app();
        let player = chemist_at(&mut app, Vec3::splat(30.0));
        let reagent = app.world().resource::<ChemDb>().reagent("radium");
        let mut mixture = Solution::unbounded();
        let _ = mixture.add(reagent, Units::whole(40));
        let mut station = Machine::new(crate::machines::MachineKind::MixingChamber);
        station.in_use_by = Some(player);
        let machine = app
            .world_mut()
            .spawn((
                station,
                crate::machines::Buffer(mixture),
                Transform::from_xyz(4.0, 0.0, 4.0),
            ))
            .id();
        report(&mut app, machine, vec![ReactionEffect::Smoke(2.0)]);
        let (payload, owner, transform) = app
            .world_mut()
            .query::<(&SmokePayload, &SmokeOwner, &Transform)>()
            .single(app.world())
            .unwrap();
        assert_eq!(payload.0.total_volume(), SMOKE_PAYLOAD);
        assert_eq!(owner.0, Some(player));
        assert_eq!(transform.translation, Vec3::new(4.0, 0.0, 4.0));
        assert_eq!(
            app.world()
                .get::<crate::machines::Buffer>(machine)
                .unwrap()
                .0
                .total_volume(),
            Units::whole(40) - SMOKE_PAYLOAD
        );
    }

    #[test]
    fn chamber_explosion_consumes_batch_without_destroying_equipment() {
        let mut app = test_app();
        let player = chemist_at(&mut app, Vec3::new(0.5, 0.0, 0.0));
        let reagent = app.world().resource::<ChemDb>().reagent("radium");
        let mut mixture = Solution::unbounded();
        let _ = mixture.add(reagent, Units::whole(40));
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(crate::machines::MachineKind::MixingChamber),
                crate::machines::Buffer(mixture),
                Transform::default(),
            ))
            .id();
        report(&mut app, machine, vec![ReactionEffect::Explosion(3.0)]);
        assert!(app.world().get::<Machine>(machine).is_some());
        assert!(app
            .world()
            .get::<crate::machines::Buffer>(machine)
            .unwrap()
            .0
            .is_empty());
        assert!(app
            .world()
            .get::<Body>(player)
            .unwrap()
            .0
            .total()
            .is_positive());
    }

    #[test]
    fn an_explosion_takes_the_glassware_and_hurts_by_distance() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 40)]);
        let near = chemist_at(&mut app, Vec3::new(0.5, 0.0, 0.0));
        let far = chemist_at(&mut app, Vec3::new(3.0, 0.0, 0.0));
        let clear = chemist_at(&mut app, Vec3::new(20.0, 0.0, 0.0));

        report(&mut app, beaker, vec![ReactionEffect::Explosion(3.0)]);

        assert!(
            app.world().get_entity(beaker).is_err(),
            "the glass and everything in it are gone"
        );

        let hurt = |entity: Entity| app.world().get::<Body>(entity).unwrap().0.total();
        assert!(hurt(near) > hurt(far), "closer should hurt more");
        assert!(hurt(far).is_positive());
        assert_eq!(hurt(clear), Units::ZERO, "and the blast has a limit");
    }

    #[test]
    fn a_blast_in_an_empty_room_does_not_panic() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 40)]);

        report(&mut app, beaker, vec![ReactionEffect::Explosion(3.0)]);

        assert!(app.world().get_entity(beaker).is_err());
    }

    #[test]
    fn a_beaker_that_goes_off_in_your_hand_finds_your_hand() {
        // A held container's own transform is meaningless on the server, so the
        // blast has to locate the holder. Without this the bang lands at the
        // world origin and the person holding it walks away unhurt.
        let mut app = test_app();
        let chemist = chemist_at(&mut app, Vec3::new(4.0, 0.0, 4.0));
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 40)]);
        app.world_mut().entity_mut(beaker).insert(HeldBy(chemist));

        report(&mut app, beaker, vec![ReactionEffect::Explosion(3.0)]);

        assert!(
            app.world()
                .get::<Body>(chemist)
                .unwrap()
                .0
                .total()
                .is_positive(),
            "it went off in their hand; they should be hurt"
        );
    }

    #[test]
    fn push_and_pull_pulses_move_chemists_and_crew_in_opposite_directions() {
        let mut app = test_app();
        let push_beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 1)]);
        let chemist = chemist_at(&mut app, Vec3::new(2.0, 0.0, 0.0));
        let crew = crew_at(&mut app, Vec3::new(-2.0, 0.0, 0.0));

        report(
            &mut app,
            push_beaker,
            vec![ReactionEffect::Pulse {
                kind: PulseKind::Push,
                power: 4.0,
            }],
        );

        assert!(
            app.world().get::<Transform>(chemist).unwrap().translation.x > 2.0,
            "a chemist should be thrown away from the reaction"
        );
        assert!(
            app.world().get::<Transform>(crew).unwrap().translation.x < -2.0,
            "crew use the same physical pulse rules"
        );
        assert!(
            app.world().get_entity(push_beaker).is_ok(),
            "a pressure pulse is not an explosion and leaves its container"
        );

        let pull_beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 1)]);
        let before = app.world().get::<Transform>(chemist).unwrap().translation.x;
        report(
            &mut app,
            pull_beaker,
            vec![ReactionEffect::Pulse {
                kind: PulseKind::Pull,
                power: 4.0,
            }],
        );
        assert!(
            app.world().get::<Transform>(chemist).unwrap().translation.x < before,
            "an attraction pulse should draw the same chemist back inward"
        );
    }

    #[test]
    fn concussive_pulses_disorient_bodies_without_moving_them() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 1)]);
        let chemist = chemist_at(&mut app, Vec3::new(1.0, 0.0, 0.0));
        let original = app.world().get::<Transform>(chemist).unwrap().translation;

        report(
            &mut app,
            beaker,
            vec![ReactionEffect::Pulse {
                kind: PulseKind::Concuss,
                power: 4.0,
            }],
        );

        let blood = app.world().get::<Bloodstream>(chemist).unwrap();
        assert!(
            blood.0.status(chem_sim::StatusKind::Muted).intensity > 0.0,
            "sonic pressure should produce a readable deafening analogue"
        );
        assert!(
            blood.0.status(chem_sim::StatusKind::Unsteady).intensity > 0.0,
            "the same pulse should visibly disrupt coordination"
        );
        assert_eq!(
            app.world().get::<Transform>(chemist).unwrap().translation,
            original,
            "concussion is distinct from Sorium and Liquid Dark Matter force"
        );
    }

    #[test]
    fn chemical_force_respects_solids_and_never_moves_machinery() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 1)]);
        let chemist = chemist_at(&mut app, Vec3::new(2.0, 0.0, 0.0));
        app.world_mut().spawn((
            Transform::from_xyz(3.0, 0.0, 0.0),
            Solid {
                half_extents: Vec3::new(0.2, 2.0, 2.0),
            },
        ));
        let machine = app
            .world_mut()
            .spawn((
                Machine {
                    kind: crate::machines::MachineKind::MixingChamber,
                    in_use_by: None,
                    disabled_for: 0.0,
                },
                Transform::from_xyz(1.0, 0.0, 1.0),
            ))
            .id();

        report(
            &mut app,
            beaker,
            vec![ReactionEffect::Pulse {
                kind: PulseKind::Push,
                power: 9.0,
            }],
        );

        assert!(
            app.world().get::<Transform>(chemist).unwrap().translation.x <= 2.45,
            "forced movement must stop at the wall's player-radius envelope"
        );
        assert_eq!(
            app.world().get::<Transform>(machine).unwrap().translation,
            Vec3::new(1.0, 0.0, 1.0),
            "core lab machinery is not a movable body"
        );
    }

    #[test]
    fn emp_disables_nearby_machinery_and_fails_airlocks_open() {
        let mut app = test_app();
        let source = beaker_at(&mut app, Vec3::ZERO, &[("iron", 1)]);
        let owner = app.world_mut().spawn_empty().id();
        let near_machine = app
            .world_mut()
            .spawn((
                Machine {
                    kind: crate::machines::MachineKind::MixingChamber,
                    in_use_by: Some(owner),
                    disabled_for: 0.0,
                },
                Transform::from_xyz(1.0, 0.0, 0.0),
            ))
            .id();
        let far_machine = app
            .world_mut()
            .spawn((
                Machine::new(crate::machines::MachineKind::Grinder),
                Transform::from_xyz(20.0, 0.0, 0.0),
            ))
            .id();
        let door = app
            .world_mut()
            .spawn((
                crate::door::Door {
                    open: false,
                    bridge_id: "emp-test".to_string(),
                    along_x: true,
                    skin: crate::door::DoorSkin::Chemistry,
                    disabled_for: 0.0,
                },
                Transform::from_xyz(2.0, 0.0, 0.0),
            ))
            .id();

        report(&mut app, source, vec![ReactionEffect::Emp(4.0)]);

        let machine = app.world().get::<Machine>(near_machine).unwrap();
        assert!(machine.disabled_for >= 8.0);
        assert_eq!(machine.in_use_by, None, "the outage releases its operator");
        assert_eq!(
            app.world()
                .get::<Machine>(far_machine)
                .unwrap()
                .disabled_for,
            0.0,
            "equipment outside the pulse remains usable"
        );
        let door = app.world().get::<crate::door::Door>(door).unwrap();
        assert!(door.open);
        assert!(door.disabled_for >= 8.0);
    }

    #[test]
    fn tesla_arcs_burn_and_disorient_bodies_while_disrupting_equipment() {
        let mut app = test_app();
        let source = beaker_at(&mut app, Vec3::ZERO, &[("teslium", 1)]);
        let chemist = chemist_at(&mut app, Vec3::new(1.0, 0.0, 0.0));
        let crew = crew_at(&mut app, Vec3::new(-1.0, 0.0, 0.0));
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(crate::machines::MachineKind::MixingChamber),
                Transform::from_xyz(1.5, 0.0, 0.0),
            ))
            .id();

        report(&mut app, source, vec![ReactionEffect::Electric(4.0)]);

        for body_entity in [chemist, crew] {
            assert!(
                app.world()
                    .get::<Body>(body_entity)
                    .unwrap()
                    .0
                    .total()
                    .is_positive(),
                "every nearby body shares the electrical damage rules"
            );
            assert!(
                app.world()
                    .get::<Bloodstream>(body_entity)
                    .unwrap()
                    .0
                    .status(chem_sim::StatusKind::Unsteady)
                    .intensity
                    > 0.0,
                "the shock has a readable motor impairment"
            );
        }
        assert!(app.world().get::<Machine>(machine).unwrap().disabled_for > 0.0);
    }

    // -----------------------------------------------------------------------
    // Scripted incidents
    // -----------------------------------------------------------------------

    #[test]
    fn the_hazard_script_parses_and_is_sane() {
        // The asset loader would only complain about this at startup, and only
        // by panicking a player's game rather than a test.
        let script: HazardScript =
            ron::from_str(include_str!("../../assets/data/station.hazards.ron"))
                .expect("station.hazards.ron should parse");

        assert!(!script.incidents.is_empty());
        assert!(
            script.warning_seconds > 0.0,
            "a hazard with no warning is a gotcha"
        );
        assert!(script.gap_seconds.0 <= script.gap_seconds.1);
        let mut spots = HashSet::new();
        for incident in &script.incidents {
            assert!(!incident.spot.trim().is_empty());
            assert!(
                spots.insert(incident.spot.as_str()),
                "'{}' is reused; incidents need independently authored locations",
                incident.spot
            );
            assert!(incident.radius > 0.0, "'{}' reaches nobody", incident.id);
            assert!(incident.duration > 0.0, "'{}' never happens", incident.id);
            assert!(incident.intensity > 0.0, "'{}' does nothing", incident.id);
            assert!(
                !incident.warning.is_empty() && !incident.onset.is_empty(),
                "'{}' needs both lines: one to react to and one to confirm it",
                incident.id
            );
        }
    }

    fn scheduled_incident_app(spots: CrisisSpots) -> App {
        let script: HazardScript =
            ron::from_str(include_str!("../../assets/data/station.hazards.ron")).unwrap();
        let pending_def = script.incidents[0].clone();
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default())
            .init_asset::<HazardScript>();
        let handle = app
            .world_mut()
            .resource_mut::<Assets<HazardScript>>()
            .add(script);
        app.insert_resource(PendingHazardScript(handle))
            .insert_resource(IncidentGrace(f32::MAX))
            .insert_resource(IncidentSchedule {
                next_in: Some(0.0),
                warning_in: Some(0.0),
                pending: Some(pending_def),
            })
            .insert_resource(spots)
            .init_resource::<RadioLog>()
            .init_resource::<Time>()
            .add_systems(Update, schedule_incidents);
        app
    }

    #[test]
    fn a_scripted_incident_uses_its_authored_map_transform() {
        let expected =
            Transform::from_xyz(17.0, 2.0, -8.0).with_rotation(Quat::from_rotation_y(0.7));
        let mut spots = CrisisSpots::default();
        spots.insert("hazard.rad_leak", expected);
        let mut app = scheduled_incident_app(spots);

        advance(&mut app, 0.1);

        let mut hazards = app.world_mut().query::<(&ActiveHazard, &Transform)>();
        let (_, actual) = hazards
            .iter(app.world())
            .next()
            .expect("the pending incident should start");
        assert_eq!(*actual, expected);
    }

    #[test]
    fn only_a_radiological_incident_arms_the_klaxon() {
        // Both authored incidents dose whoever stands in one — that is not the
        // question the flag answers. The alarm belongs to the leak, and a
        // coolant line weeping in the reaction bay sounding a rad klaxon would
        // teach the player to stop trusting it.
        let script: HazardScript =
            ron::from_str(include_str!("../../assets/data/station.hazards.ron")).unwrap();
        let radiological: Vec<&str> = script
            .incidents
            .iter()
            .filter(|incident| incident.radiological)
            .map(|incident| incident.id.as_str())
            .collect();
        assert_eq!(radiological, ["rad_leak"]);

        // And the flag has to survive onto the spawned entity, which is the
        // only thing a client ever sees — `scheduled_incident_app` primes the
        // first authored incident, the leak.
        let mut spots = CrisisSpots::default();
        spots.insert("hazard.rad_leak", Transform::default());
        let mut app = scheduled_incident_app(spots);

        advance(&mut app, 0.1);

        let mut hazards = app.world_mut().query::<&ActiveHazard>();
        let hazard = hazards
            .iter(app.world())
            .next()
            .expect("the pending incident should start");
        assert!(hazard.radiological);
    }

    #[test]
    fn a_scripted_incident_with_a_missing_spot_is_skipped() {
        let mut app = scheduled_incident_app(CrisisSpots::default());

        advance(&mut app, 0.1);

        assert_eq!(
            app.world_mut()
                .query::<&ActiveHazard>()
                .iter(app.world())
                .count(),
            0
        );
        let schedule = app.world().resource::<IncidentSchedule>();
        assert!(schedule.pending.is_none());
        assert!(
            schedule.next_in.is_some(),
            "a skipped event should reschedule"
        );
    }

    #[test]
    fn an_incident_irradiates_whoever_is_inside_it() {
        let mut app = test_app();
        app.add_systems(Update, run_incidents);
        let inside = chemist_at(&mut app, Vec3::new(1.0, 0.0, 0.0));
        let outside = chemist_at(&mut app, Vec3::new(9.0, 0.0, 0.0));
        app.world_mut().spawn((
            ActiveHazard {
                radius: 4.5,
                remaining: 40.0,
                intensity: 2.0,
                color: default_hazard_color(),
                radiological: true,
            },
            Transform::default(),
        ));

        one_tick(&mut app);

        let irradiation = |entity: Entity| {
            app.world()
                .get::<Bloodstream>(entity)
                .unwrap()
                .0
                .status(chem_sim::StatusKind::Irradiated)
        };
        assert!(irradiation(inside).remaining > 0.0);
        assert_eq!(irradiation(inside).intensity, 2.0);
        assert_eq!(
            irradiation(outside).remaining,
            0.0,
            "walking out of it is a real answer"
        );
    }

    #[test]
    fn an_incident_expires() {
        let mut app = test_app();
        app.add_systems(Update, run_incidents);
        app.world_mut().spawn((
            ActiveHazard {
                radius: 4.5,
                remaining: 2.0,
                intensity: 2.0,
                color: default_hazard_color(),
                radiological: true,
            },
            Transform::default(),
        ));

        for _ in 0..8 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(Duration::from_secs_f32(0.5));
            app.update();
        }

        let mut query = app.world_mut().query::<&ActiveHazard>();
        assert_eq!(query.iter(app.world()).count(), 0);
    }

    #[test]
    fn an_incident_gets_a_sphere_the_player_can_actually_see() {
        // The zone used to be spawned with a `Transform` and a `Visibility` and
        // no mesh at all, which made the intended counterplay — "walk out of
        // it" — impossible to aim: you learned where the edge was by the screen
        // going green. Keyed on `Added`, unauthored by the authority, so a
        // joining client builds its own from the replicated component.
        let mut app = test_app();
        app.add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_systems(Update, build_hazard_visuals);

        let hazard = app
            .world_mut()
            .spawn((
                ActiveHazard {
                    radius: 4.5,
                    remaining: 40.0,
                    intensity: 2.0,
                    color: (0.45, 0.95, 0.35),
                    radiological: true,
                },
                Transform::default(),
            ))
            .id();

        app.update();

        let mut visuals = app.world_mut().query::<(&HazardVisual, &ChildOf)>();
        let parents: Vec<Entity> = visuals
            .iter(app.world())
            .map(|(_, parent)| parent.parent())
            .collect();
        assert_eq!(
            parents,
            vec![hazard],
            "the hazard should carry exactly one visual, parented to it so it \
             despawns with the incident"
        );

        // Twice must not build it twice — `Added` is the guard.
        app.update();
        let mut visuals = app.world_mut().query::<&HazardVisual>();
        assert_eq!(visuals.iter(app.world()).count(), 1);
    }

    #[test]
    fn heat_alone_is_not_a_hazard() {
        let mut app = test_app();
        let beaker = beaker_at(&mut app, Vec3::ZERO, &[("radium", 40)]);

        report(&mut app, beaker, vec![ReactionEffect::Heat(1.5)]);

        assert_eq!(clouds(&mut app), 0);
        assert!(app.world().get_entity(beaker).is_ok());
    }
}
