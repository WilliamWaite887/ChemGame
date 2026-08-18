//! Chemistry that has left the glassware.
//!
//! A spill is shared simulation state rather than a one-shot particle: it can
//! merge with another spill, dose somebody who walks through it, be cleaned,
//! ignite, and be seen by every connected player.  [`ChemicalExposure`] is
//! the matching audit trail for chemistry that reaches a body.  Body, smoke
//! and puddle systems all write the same record, so consent and station
//! consequences do not depend on which delivery mechanic happened to be used.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{Category, ReagentEffect, Route, Solution, StatusKind, Units, WorldEffect};
use serde::{Deserialize, Serialize};

use crate::body::{Bloodstream, Body, MetabolismClock};
use crate::chem_data::ChemDb;
use crate::crew::{CrewMember, CrewRoute};
use crate::net::is_authority;
use crate::orders::{Department, Order, OrderKind, Shift, Wanted};
use crate::player::Chemist;
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// A deliberate hand transfer is ten units, but standing in a spill should be
/// a warning before it is a death sentence.
const PUDDLE_TOUCH_DOSE: Units = Units::whole(2);
const PUDDLE_LIFETIME: f32 = 50.0;
const MIN_RADIUS: f32 = 0.28;
const MAX_RADIUS: f32 = 1.35;
const MERGE_PADDING: f32 = 0.10;
const PROP_DISSOLVE_STRENGTH: f32 = 3.0;

pub struct ChemWorldPlugin;

impl Plugin for ChemWorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ChemicalExposure>().add_systems(
            Update,
            (
                (
                    initialize_puddle_profile,
                    clean_with_new_puddles,
                    affect_world_with_new_puddles,
                    spread_puddle_ignition,
                    merge_puddles,
                    expose_bodies_to_puddles,
                    age_puddles,
                    respond_to_unwanted_exposure,
                )
                    .chain()
                    .run_if(is_authority),
                (
                    mark_reactive_station_props.before(affect_world_with_new_puddles),
                    present_corroded_props.after(affect_world_with_new_puddles),
                )
                    .chain(),
                build_puddle_visuals,
                update_puddle_visuals,
            )
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// Persistent, replicated chemistry on the floor.
#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct ChemicalPuddle {
    pub solution: Solution,
    pub radius: f32,
    pub remaining: f32,
    /// Who made the spill, when attributable. Entity mapping is supplied by
    /// Bevy's component derive through `#[entities]`.
    #[entities]
    pub owner: Option<Entity>,
    pub ignited: bool,
    /// Remaining slippery-surface lifetime. Zero means ordinary footing.
    pub slippery: f32,
    /// Fuel strength and remaining lifetime, set by oil/napalm profiles.
    pub flammable_intensity: f32,
    pub flammable_remaining: f32,
    /// Extra status intensity applied by an ignition world effect.
    pub ignition_intensity: f32,
    /// Surface cold applied to anyone who touches it.
    pub chill_intensity: f32,
    /// Cleaning wash acts on overlaps, never merges into them, then quickly
    /// evaporates. This preserves any second world effect on the same reagent
    /// (hydrogen peroxide also corrodes) until that one-shot pass runs.
    pub cleaner: bool,
}

impl ChemicalPuddle {
    pub fn from_solution(solution: Solution, owner: Option<Entity>) -> Self {
        let radius = radius_for(solution.total_volume());
        Self {
            solution,
            radius,
            remaining: PUDDLE_LIFETIME,
            owner,
            ignited: false,
            slippery: 0.0,
            flammable_intensity: 0.0,
            flammable_remaining: 0.0,
            ignition_intensity: 0.0,
            chill_intensity: 0.0,
            cleaner: false,
        }
    }
}

/// Why a dose reached somebody. Route answers *how it entered*; source answers
/// *what interaction caused it*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposureSource {
    Direct,
    Smoke,
    Puddle,
}

/// Authority-owned record of a chemical exposure.
///
/// `solution` is a snapshot taken before [`Bloodstream::receive`] consumes the
/// dose. The booleans are explicit authorization context rather than something
/// a downstream reputation system has to reconstruct after the bloodstream has
/// already reacted and metabolised.
#[derive(Message, Clone, Debug)]
pub struct ChemicalExposure {
    pub actor: Option<Entity>,
    pub target: Entity,
    pub route: Route,
    pub source: ExposureSource,
    pub solution: Solution,
    pub authorized: bool,
    pub helpful: bool,
    pub harmful: bool,
    pub illicit: bool,
    pub overdose: bool,
}

/// Classifies a dose against its intended recipient before it lands.
pub fn assess_exposure(
    solution: &Solution,
    route: Route,
    body: &Body,
    blood: &Bloodstream,
    db: &ChemDb,
) -> ExposureAssessment {
    let current = blood.0.contents();
    let mut assessment = ExposureAssessment::default();

    // A medicine is only "helpful" when at least one of its beneficial
    // operations has work to do right now. This keeps, for example, burn
    // medicine splashed onto somebody with only brute trauma from being
    // treated as tolerated first aid merely because both facts are medical.
    let current_has_harmful_reagent = current.iter().any(|(id, amount)| {
        let reagent = db.reagents.get(*id);
        reagent.effects.iter().any(|effect| effect.is_harmful())
            || reagent.categories.iter().any(|category| {
                matches!(
                    category,
                    Category::Illicit | Category::Poisons | Category::Pyrotechnics
                )
            })
            || reagent.overdose.is_some_and(|limit| *amount > limit)
    });

    for (id, amount) in solution.iter() {
        let reagent = db.reagents.get(id);
        let incoming = Units::from_f64(amount.as_f64() * route.absorbed().as_f64());
        let already_present = current
            .iter()
            .find_map(|(present, amount)| (*present == id).then_some(*amount))
            .unwrap_or(Units::ZERO);

        let legitimately_medical = reagent
            .categories
            .iter()
            .any(|category| category.is_legitimately_orderable());
        let addresses_current_need = reagent.effects.iter().any(|effect| match *effect {
            ReagentEffect::Heal(kind, amount) => {
                amount.is_positive() && body.0.damage.get(kind).is_positive()
            }
            ReagentEffect::Counter {
                kind,
                seconds,
                intensity,
            } => {
                seconds > 0.0
                    && intensity > 0.0
                    && blood.0.status(kind).remaining > 0.0
                    && blood.0.status(kind).intensity > 0.0
            }
            ReagentEffect::Purge(amount) => amount.is_positive() && current_has_harmful_reagent,
            // Stabilization really does address any current injury by raising
            // the collapse threshold. Other status additions are preventive,
            // recreational, or harmful rather than treatment of an existing
            // condition; medicines such as mannitol/synaptizine declare their
            // curative work explicitly with Counter effects.
            ReagentEffect::Status {
                kind: StatusKind::Stabilized,
                seconds,
                intensity,
            } => seconds > 0.0 && intensity > 0.0 && body.0.is_hurt(),
            ReagentEffect::Harm(..) | ReagentEffect::Contact(..) | ReagentEffect::Status { .. } => {
                false
            }
        });
        assessment.helpful |= legitimately_medical && addresses_current_need;
        assessment.illicit |= reagent.categories.contains(&Category::Illicit);
        assessment.overdose |= reagent
            .overdose
            .is_some_and(|limit| already_present + incoming > limit);
        assessment.harmful |= reagent.effects.iter().any(|effect| effect.is_harmful())
            || reagent.categories.contains(&Category::Poisons)
            || reagent.categories.contains(&Category::Pyrotechnics);
    }

    assessment.harmful |= assessment.illicit || assessment.overdose;
    assessment
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExposureAssessment {
    pub helpful: bool,
    pub harmful: bool,
    pub illicit: bool,
    pub overdose: bool,
}

/// Whether a clean dose is one the target's active request explicitly permits.
/// This is intentionally narrower than delivery grading: partial treatment is
/// still consensual, but a mixed beaker cannot use one requested ingredient to
/// launder an unwanted contaminant.
pub fn order_authorizes_dose(
    solution: &Solution,
    order: &Order,
    kind: OrderKind,
    db: &ChemDb,
) -> bool {
    if solution.len() != 1 {
        return false;
    }
    let Some((reagent, amount)) = solution.iter().next() else {
        return false;
    };
    if !amount.is_positive() {
        return false;
    }
    match crate::orders::wanted_for(order, kind, db) {
        Wanted::Exact(wanted) => reagent == wanted,
        Wanted::Category(category) => db.reagents.get(reagent).categories.contains(&category),
    }
}

/// Marks a prop that chemistry is allowed to corrode. Doors are intrinsically
/// reactive; ordinary scenery is opt-in so a thermite splash cannot dissolve
/// the whole authored map.
#[derive(Component, Default)]
pub struct ChemicallyReactive;

/// Deduplicates the victim response to one continuing cloud/spill/direct dose.
/// A victim leaves after reporting, so one marker covers the remaining life of
/// that entity without conflating "already leaving" with "already reported".
#[derive(Component)]
struct ReportedChemicalIncident;

/// The storage locker is authored station equipment but not required to make
/// an order, making it a safe, real prop on which to expose the corrosion
/// system without letting thermite permanently soft-lock the core chemistry
/// lanes. Derived on every peer from replicated machine kind.
fn mark_reactive_station_props(
    mut commands: Commands,
    machines: Query<(Entity, &crate::machines::Machine), Added<crate::machines::Machine>>,
) {
    for (entity, machine) in &machines {
        if machine.kind == crate::machines::MachineKind::Locker {
            commands.entity(entity).insert(ChemicallyReactive);
        }
    }
}

/// A corroded reactive prop is dissolved: no collision, no interaction, and
/// no visible shell. This runs on every peer because those three components
/// are locally derived presentation/geometry while `Corroded` is replicated.
fn present_corroded_props(
    mut commands: Commands,
    props: Query<
        (Entity, &crate::door::Corroded),
        (With<ChemicallyReactive>, Changed<crate::door::Corroded>),
    >,
) {
    for (entity, corrosion) in &props {
        if corrosion.strength < PROP_DISSOLVE_STRENGTH {
            continue;
        }
        commands
            .entity(entity)
            .remove::<crate::lab::Solid>()
            .remove::<crate::interaction::Interactable>()
            .insert(Visibility::Hidden);
    }
}

/// Spawns a floor spill. The caller has already removed the solution from its
/// source, so an empty solution is simply ignored.
pub fn spawn_puddle(
    commands: &mut Commands,
    solution: Solution,
    point: Vec3,
    owner: Option<Entity>,
) -> Option<Entity> {
    if solution.total_volume().is_zero() {
        return None;
    }
    let puddle = ChemicalPuddle::from_solution(solution, owner);
    Some(
        commands
            .spawn((
                puddle,
                // Puddles belong to the floor even if the camera ray hit a
                // wall or a person's upper body.
                Transform::from_xyz(point.x, 0.018, point.z),
                Visibility::default(),
                Replicated,
                crate::until_we_leave_the_lab(),
            ))
            .id(),
    )
}

fn radius_for(volume: Units) -> f32 {
    (MIN_RADIUS + volume.as_f32().sqrt() * 0.075).clamp(MIN_RADIUS, MAX_RADIUS)
}

/// Resolves declarative reagent world profiles once, when a spill first
/// enters the station simulation.
fn initialize_puddle_profile(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut puddles: Query<(Entity, &Transform, &mut ChemicalPuddle), Added<ChemicalPuddle>>,
) {
    for (entity, transform, mut puddle) in &mut puddles {
        let contents: Vec<_> = puddle.solution.iter().collect();
        let mut smoke_radius: f32 = 0.0;
        let mut smoke_seconds: f32 = 0.0;
        let mut is_cleaner = false;

        for (reagent, amount) in contents {
            for effect in &db.reagents.get(reagent).world_effects {
                match *effect {
                    WorldEffect::Clean { .. } => is_cleaner = true,
                    WorldEffect::Ignite { intensity, seconds } => {
                        puddle.ignited = true;
                        puddle.ignition_intensity = puddle.ignition_intensity.max(intensity);
                        puddle.remaining = puddle.remaining.max(seconds);
                    }
                    WorldEffect::ReleaseSmoke { radius, seconds } => {
                        smoke_radius = smoke_radius.max(radius);
                        smoke_seconds = smoke_seconds.max(seconds);
                    }
                    WorldEffect::Slippery { seconds } => {
                        puddle.slippery = puddle.slippery.max(seconds);
                    }
                    WorldEffect::Flammable { intensity, seconds } => {
                        puddle.flammable_intensity = puddle.flammable_intensity.max(intensity);
                        puddle.flammable_remaining = puddle.flammable_remaining.max(seconds);
                    }
                    WorldEffect::Chill { kelvin_per_unit } => {
                        puddle.solution.temperature.0 = (puddle.solution.temperature.0
                            - kelvin_per_unit * amount.as_f32())
                        .max(120.0);
                        puddle.chill_intensity = puddle
                            .chill_intensity
                            .max((kelvin_per_unit / 2.0).clamp(0.25, 2.5));
                    }
                    WorldEffect::Corrode { .. } | WorldEffect::Flash { .. } => {}
                }
            }
        }

        if smoke_radius > 0.0 {
            let payload = puddle.solution.clone();
            commands.spawn((
                crate::hazards::SmokeCloud {
                    radius: smoke_radius,
                    remaining: smoke_seconds,
                },
                crate::hazards::SmokePayload(payload),
                crate::hazards::SmokeOwner(puddle.owner),
                *transform,
                Visibility::default(),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
            // Smoke Powder is transportable in glass and vents when released;
            // it does not also leave a duplicate dose on the floor.
            puddle.solution.clear();
            puddle.remaining = 0.0;
        } else if is_cleaner {
            // A cleaner spill acts, then evaporates instead of becoming the
            // next residue somebody has to clean.
            puddle.remaining = puddle.remaining.min(3.0);
            puddle.cleaner = true;
        }

        // Ensure Changed<ChemicalPuddle> readers see the initialized profile
        // even if the profile happened to contain only zero-independent data.
        let _ = entity;
    }
}

/// Cleaner removes overlapping chemical material instead of merely tinting
/// it. A `ParamSet` makes the added-cleaner read and all-puddle mutation
/// explicitly disjoint in time.
fn clean_with_new_puddles(
    db: Res<ChemDb>,
    mut sets: ParamSet<(
        Query<(Entity, &Transform, &ChemicalPuddle), Added<ChemicalPuddle>>,
        Query<(Entity, &Transform, &mut ChemicalPuddle)>,
    )>,
) {
    let cleaners: Vec<(Entity, Vec3, f32)> = sets
        .p0()
        .iter()
        .filter_map(|(entity, transform, puddle)| {
            let strength = puddle
                .solution
                .iter()
                .flat_map(|(id, _)| db.reagents.get(id).world_effects.iter())
                .filter_map(|effect| match effect {
                    WorldEffect::Clean { strength } => Some(*strength),
                    _ => None,
                })
                .fold(0.0f32, f32::max);
            (strength > 0.0).then_some((entity, transform.translation, strength))
        })
        .collect();

    for (cleaner, position, strength) in cleaners {
        let targets: Vec<Entity> = sets
            .p1()
            .iter()
            .filter_map(|(entity, transform, _)| {
                (entity != cleaner
                    && horizontal_distance(position, transform.translation)
                        <= 0.75 + strength * 0.35)
                    .then_some(entity)
            })
            .collect();
        for target in targets {
            if let Ok((_, _, mut puddle)) = sets.p1().get_mut(target) {
                let removed = Units::from_f64((strength * 12.0) as f64);
                puddle.solution.take(removed);
                puddle.radius = radius_for(puddle.solution.total_volume());
                puddle.remaining -= strength * 8.0;
            }
        }
    }
}

/// Corrosion and flash are spatial one-shot effects rather than persistent
/// puddle properties.
fn affect_world_with_new_puddles(
    mut commands: Commands,
    db: Res<ChemDb>,
    puddles: Query<(&Transform, &ChemicalPuddle), Added<ChemicalPuddle>>,
    mut doors: Query<
        (Entity, &Transform, Option<&mut crate::door::Corroded>),
        With<crate::door::Door>,
    >,
    mut reactive_props: Query<
        (Entity, &Transform, Option<&mut crate::door::Corroded>),
        (With<ChemicallyReactive>, Without<crate::door::Door>),
    >,
    mut bodies: Query<(&Transform, &mut Bloodstream)>,
) {
    for (transform, puddle) in &puddles {
        for (reagent, _) in puddle.solution.iter() {
            for effect in &db.reagents.get(reagent).world_effects {
                match *effect {
                    WorldEffect::Corrode { strength } => {
                        let radius = 0.65 + strength * 0.45;
                        for (entity, door_transform, corroded) in &mut doors {
                            if horizontal_distance(
                                transform.translation,
                                door_transform.translation,
                            ) > radius
                            {
                                continue;
                            }
                            if let Some(mut corroded) = corroded {
                                corroded.strength = corroded.strength.max(strength);
                            } else {
                                commands
                                    .entity(entity)
                                    .insert(crate::door::Corroded { strength });
                            }
                        }
                        for (entity, prop_transform, corroded) in &mut reactive_props {
                            if horizontal_distance(
                                transform.translation,
                                prop_transform.translation,
                            ) > radius
                            {
                                continue;
                            }
                            if let Some(mut corroded) = corroded {
                                corroded.strength = corroded.strength.max(strength);
                            } else {
                                commands
                                    .entity(entity)
                                    .insert(crate::door::Corroded { strength });
                            }
                        }
                    }
                    WorldEffect::Flash { radius, seconds } => {
                        for (body_transform, mut blood) in &mut bodies {
                            if horizontal_distance(
                                transform.translation,
                                body_transform.translation,
                            ) <= radius
                            {
                                blood.0.add_status(StatusKind::Blurred, seconds, 2.0);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Fire propagates deterministically to overlapping fuel. No random ignition
/// roll means the same spill layout behaves identically for every host.
fn spread_puddle_ignition(mut puddles: Query<(&Transform, &mut ChemicalPuddle)>) {
    let flames: Vec<(Vec3, f32)> = puddles
        .iter()
        .filter(|(_, puddle)| puddle.ignited)
        .map(|(transform, puddle)| (transform.translation, puddle.radius))
        .collect();
    if flames.is_empty() {
        return;
    }
    for (transform, mut puddle) in &mut puddles {
        if puddle.ignited || puddle.flammable_remaining <= 0.0 {
            continue;
        }
        if flames.iter().any(|(position, radius)| {
            horizontal_distance(*position, transform.translation)
                <= *radius + puddle.radius + MERGE_PADDING
        }) {
            puddle.ignited = true;
            puddle.ignition_intensity = puddle.ignition_intensity.max(puddle.flammable_intensity);
        }
    }
}

/// Merge overlapping puddles deterministically. Each entity can take part in
/// at most one merge per frame, preventing an already-commanded despawn from
/// being consumed a second time before deferred commands apply.
fn merge_puddles(
    mut commands: Commands,
    mut puddles: Query<(Entity, &Transform, &mut ChemicalPuddle)>,
) {
    let mut snapshot: Vec<(Entity, Vec3, f32, bool)> = puddles
        .iter()
        .map(|(entity, transform, puddle)| {
            (entity, transform.translation, puddle.radius, puddle.cleaner)
        })
        .collect();
    snapshot.sort_by_key(|(entity, _, _, _)| entity.to_bits());
    let mut merged = HashSet::new();

    for (index, &(left, left_position, left_radius, left_cleaner)) in snapshot.iter().enumerate() {
        if merged.contains(&left) || left_cleaner {
            continue;
        }
        let Some(&(right, _, _, _)) =
            snapshot[index + 1..]
                .iter()
                .find(|(right, position, radius, cleaner)| {
                    !*cleaner
                        && !merged.contains(right)
                        && horizontal_distance(left_position, *position)
                            <= left_radius + *radius + MERGE_PADDING
                })
        else {
            continue;
        };

        let Ok([(_, _, mut keep), (_, _, mut consume)]) = puddles.get_many_mut([left, right])
        else {
            continue;
        };
        let left_volume = keep.solution.total_volume().as_f32();
        let right_volume = consume.solution.total_volume().as_f32();
        let total_volume = left_volume + right_volume;
        if total_volume > 0.0 {
            keep.solution.temperature.0 = (keep.solution.temperature.0 * left_volume
                + consume.solution.temperature.0 * right_volume)
                / total_volume;
        }
        let amount = consume.solution.total_volume();
        consume.solution.transfer_to(&mut keep.solution, amount);
        keep.radius = radius_for(keep.solution.total_volume());
        keep.remaining = keep.remaining.max(consume.remaining);
        keep.ignited |= consume.ignited;
        keep.slippery = keep.slippery.max(consume.slippery);
        keep.flammable_intensity = keep.flammable_intensity.max(consume.flammable_intensity);
        keep.flammable_remaining = keep.flammable_remaining.max(consume.flammable_remaining);
        keep.ignition_intensity = keep.ignition_intensity.max(consume.ignition_intensity);
        keep.chill_intensity = keep.chill_intensity.max(consume.chill_intensity);
        keep.cleaner |= consume.cleaner;
        keep.owner = match (keep.owner, consume.owner) {
            (Some(left), Some(right)) if left == right => Some(left),
            (None, None) => None,
            _ => None,
        };
        merged.insert(left);
        merged.insert(right);
        commands.entity(right).despawn();
    }
}

/// Dose everybody standing in a puddle on the metabolism beat.
#[allow(clippy::too_many_arguments)]
fn expose_bodies_to_puddles(
    db: Res<ChemDb>,
    clock: Res<MetabolismClock>,
    mut puddles: Query<(&Transform, &mut ChemicalPuddle)>,
    mut bodies: Query<(Entity, &Transform, &mut Body, &mut Bloodstream)>,
    chemists: Query<(), With<Chemist>>,
    orders: Query<(
        &Order,
        Has<crate::orders::IllicitOrder>,
        Has<crate::orders::CrisisOrder>,
        Has<crate::orders::CounterOrder>,
    )>,
    crisis_targets: Query<(), With<crate::crisis::CrisisResponse>>,
    mut exposures: MessageWriter<ChemicalExposure>,
) {
    if !clock.0.just_finished() {
        return;
    }

    for (puddle_transform, mut puddle) in &mut puddles {
        if puddle.solution.is_empty() {
            continue;
        }
        for (target, body_transform, mut body, mut blood) in &mut bodies {
            if horizontal_distance(puddle_transform.translation, body_transform.translation)
                > puddle.radius
            {
                continue;
            }
            let mut dose = puddle.solution.split(PUDDLE_TOUCH_DOSE);
            if dose.is_empty() {
                break;
            }
            let snapshot = dose.clone();
            let assessment = assess_exposure(&snapshot, Route::Touched, &body, &blood, &db);
            blood.0.receive(&mut dose, Route::Touched, &mut body.0, &db);
            if puddle.slippery > 0.0 {
                blood.0.add_status(StatusKind::Unsteady, 3.0, 1.0);
            }
            if puddle.chill_intensity > 0.0 {
                blood
                    .0
                    .add_status(StatusKind::Chilled, 4.0, puddle.chill_intensity);
            }
            if puddle.ignited {
                blood.0.add_status(
                    StatusKind::Burning,
                    5.0,
                    puddle.ignition_intensity.max(0.75),
                );
            }
            let requested = orders
                .get(target)
                .is_ok_and(|(order, illicit, crisis, counter)| {
                    !assessment.overdose
                        && order_authorizes_dose(
                            &snapshot,
                            order,
                            OrderKind::of(illicit, crisis, counter),
                            &db,
                        )
                });
            let crisis_care = crisis_targets.contains(target)
                && assessment.helpful
                && !assessment.illicit
                && !assessment.overdose;
            exposures.write(ChemicalExposure {
                actor: puddle.owner,
                target,
                route: Route::Touched,
                source: ExposureSource::Puddle,
                solution: snapshot,
                // Walking through your own spill is self-inflicted. A spill
                // made by somebody else is never affirmative treatment.
                authorized: puddle.owner == Some(target)
                    || chemists.contains(target)
                    || requested
                    || crisis_care,
                helpful: assessment.helpful,
                harmful: assessment.harmful,
                illicit: assessment.illicit,
                overdose: assessment.overdose,
            });
        }
        puddle.radius = radius_for(puddle.solution.total_volume());
    }
}

fn age_puddles(
    mut commands: Commands,
    time: Res<Time>,
    mut puddles: Query<(Entity, &mut ChemicalPuddle)>,
) {
    for (entity, mut puddle) in &mut puddles {
        puddle.remaining -= time.delta_secs();
        puddle.slippery = (puddle.slippery - time.delta_secs()).max(0.0);
        puddle.flammable_remaining = (puddle.flammable_remaining - time.delta_secs()).max(0.0);
        // Ignited spills burn off faster; the damaging world-effect pass can
        // set this flag without inventing a separate fire entity.
        if puddle.ignited {
            puddle.remaining -= time.delta_secs() * 2.0;
        }
        if puddle.remaining <= 0.0 || puddle.solution.is_empty() {
            commands.entity(entity).despawn();
        }
    }
}

/// Consequences for deliberately dosing an ordinary resident without a valid
/// treatment context. A useful, non-overdose treatment on an injured resident
/// is tolerated but deliberately unrewarded.
#[allow(clippy::too_many_arguments)]
fn respond_to_unwanted_exposure(
    mut commands: Commands,
    mut exposures: MessageReader<ChemicalExposure>,
    db: Option<Res<ChemDb>>,
    mut shift: Option<ResMut<Shift>>,
    mut suspicion: Option<ResMut<crate::antagonist::SecuritySuspicion>>,
    mut radio: Option<ResMut<RadioLog>>,
    areas: Option<Res<crate::lab::WalkableAreas>>,
    mut victims: Query<(
        &CrewMember,
        &mut CrewRoute,
        &Transform,
        Option<&ReportedChemicalIncident>,
    )>,
    witnesses: Query<(Entity, &Transform), With<CrewMember>>,
) {
    let mut reported_this_frame = HashSet::new();
    for exposure in exposures.read() {
        if exposure.authorized {
            continue;
        }
        let Ok((victim, mut route, victim_transform, reported)) = victims.get_mut(exposure.target)
        else {
            continue;
        };
        if exposure.helpful && !exposure.harmful && !exposure.illicit && !exposure.overdose {
            continue;
        }
        if reported.is_some() || !reported_this_frame.insert(exposure.target) {
            continue;
        }

        let room = areas
            .as_ref()
            .and_then(|areas| areas.room_at(victim_transform.translation));
        let witnessed = witnesses.iter().any(|(entity, transform)| {
            entity != exposure.target
                && horizontal_distance(transform.translation, victim_transform.translation) <= 8.0
                && room.is_some_and(|room| {
                    areas
                        .as_ref()
                        .and_then(|areas| areas.room_at(transform.translation))
                        == Some(room)
                })
        });
        let severity = if exposure.illicit || exposure.overdose {
            3
        } else if exposure.harmful {
            2
        } else {
            1
        } + i32::from(witnessed);

        route.leave();
        commands
            .entity(exposure.target)
            .insert(ReportedChemicalIncident);
        if let Some(shift) = shift.as_deref_mut() {
            if let Some(department) = Department::from_role(&victim.role) {
                shift.adjust(department, -severity);
            }
        }
        if let Some(suspicion) = suspicion.as_deref_mut() {
            suspicion.0 += severity;
        }
        if let Some(radio) = radio.as_deref_mut() {
            let composition = db
                .as_ref()
                .map(|db| {
                    exposure
                        .solution
                        .iter()
                        .map(|(id, amount)| format!("{amount}u {}", db.reagents.get(id).name))
                        .collect::<Vec<_>>()
                        .join(" + ")
                })
                .filter(|line| !line.is_empty())
                .unwrap_or_else(|| "an unknown chemical".to_string());
            let attribution = if exposure.actor.is_some() {
                " The source was identified."
            } else {
                ""
            };
            let action = match exposure.source {
                ExposureSource::Direct => exposure.route.label(),
                ExposureSource::Smoke => "exposed to chemical smoke",
                ExposureSource::Puddle => "exposed through a chemical spill",
            };
            radio.push(RadioEntry {
                channel: "SEC".to_string(),
                text: format!(
                    "{} reports being {} with {} in the lab{}.{}",
                    victim.name,
                    action,
                    composition,
                    if witnessed { " with witnesses" } else { "" },
                    attribution
                ),
                good: false,
            });
        }
    }
}

#[derive(Component)]
pub struct PuddleVisual {
    material: Handle<StandardMaterial>,
}

fn build_puddle_visuals(
    mut commands: Commands,
    db: Option<Res<ChemDb>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    puddles: Query<(Entity, &ChemicalPuddle), Added<ChemicalPuddle>>,
) {
    let Some(db) = db else {
        return;
    };
    for (entity, puddle) in &puddles {
        let [r, g, b] = puddle.solution.color(&db.reagents);
        let material = materials.add(StandardMaterial {
            base_color: Color::srgba(r, g, b, 0.58),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.18,
            ..default()
        });
        commands.spawn((
            Mesh3d(meshes.add(Cylinder::new(1.0, 0.012))),
            MeshMaterial3d(material.clone()),
            Transform::from_scale(Vec3::new(puddle.radius, 1.0, puddle.radius)),
            PuddleVisual { material },
            ChildOf(entity),
        ));
    }
}

fn update_puddle_visuals(
    db: Option<Res<ChemDb>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    puddles: Query<(&ChemicalPuddle, &Children), Changed<ChemicalPuddle>>,
    mut visuals: Query<(&mut Transform, &PuddleVisual)>,
) {
    let Some(db) = db else {
        return;
    };
    for (puddle, children) in &puddles {
        for child in children.iter() {
            if let Ok((mut transform, visual)) = visuals.get_mut(child) {
                transform.scale = Vec3::new(puddle.radius, 1.0, puddle.radius);
                if let Some(mut material) = materials.get_mut(&visual.material) {
                    let [r, g, b] = puddle.solution.color(&db.reagents);
                    material.base_color = if puddle.ignited {
                        Color::srgba(1.0, 0.28, 0.04, 0.78)
                    } else {
                        Color::srgba(r, g, b, 0.58)
                    };
                    material.emissive = if puddle.ignited {
                        LinearRgba::new(2.0, 0.18, 0.01, 1.0)
                    } else {
                        LinearRgba::BLACK
                    };
                }
            }
        }
    }
}

fn horizontal_distance(left: Vec3, right: Vec3) -> f32 {
    Vec2::new(left.x - right.x, left.z - right.z).length()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::CrewPhase;
    use std::time::Duration;

    fn chemistry() -> ChemDb {
        ChemDb(
            chem_sim::ChemData::from_ron(
                include_str!("../../assets/data/chem.reagents.ron"),
                include_str!("../../assets/data/chem.reactions.ron"),
            )
            .expect("chemistry data should load"),
        )
    }

    fn solution(db: &ChemDb, reagent: &str, amount: i32) -> Solution {
        let mut solution = Solution::unbounded();
        let _ = solution.add(db.reagent(reagent), Units::whole(amount));
        solution
    }

    #[test]
    fn puddle_radius_grows_but_is_bounded() {
        assert_eq!(radius_for(Units::ZERO), MIN_RADIUS);
        assert!(radius_for(Units::whole(40)) > radius_for(Units::whole(5)));
        assert_eq!(radius_for(Units::whole(10_000)), MAX_RADIUS);
    }

    #[test]
    fn horizontal_distance_ignores_body_height() {
        assert_eq!(
            horizontal_distance(Vec3::new(1.0, 0.0, 2.0), Vec3::new(1.0, 9.0, 2.0)),
            0.0
        );
    }

    #[test]
    fn active_orders_authorize_only_clean_matching_doses() {
        let db = chemistry();
        let bicaridine = db.reagent("bicaridine");
        let order = Order {
            reagent: bicaridine,
            specific: true,
            amount: Units::whole(15),
            plea: String::new(),
            patience: 30.0,
            waited: 0.0,
        };
        let clean = solution(&db, "bicaridine", 5);
        assert!(order_authorizes_dose(
            &clean,
            &order,
            OrderKind::Normal,
            &db,
        ));

        let mut contaminated = clean;
        let _ = contaminated.add(db.reagent("sulphuric_acid"), Units::whole(1));
        assert!(!order_authorizes_dose(
            &contaminated,
            &order,
            OrderKind::Normal,
            &db,
        ));
    }

    #[test]
    fn unrelated_medicine_is_not_tolerated_as_helpful_first_aid() {
        let db = chemistry();
        let mut body = Body::default();
        body.0.damage.brute = Units::whole(20);
        let blood = Bloodstream::default();

        let wrong_treatment = assess_exposure(
            &solution(&db, "kelotane", 5),
            Route::Touched,
            &body,
            &blood,
            &db,
        );
        assert!(
            !wrong_treatment.helpful,
            "burn medicine does not address isolated brute trauma",
        );

        let matching_treatment = assess_exposure(
            &solution(&db, "bicaridine", 5),
            Route::Touched,
            &body,
            &blood,
            &db,
        );
        assert!(matching_treatment.helpful);
    }

    #[test]
    fn overlapping_puddles_merge_without_losing_chemistry() {
        let db = chemistry();
        let mut app = App::new();
        app.add_systems(Update, merge_puddles);
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "water", 10), None),
            Transform::default(),
        ));
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "sugar", 5), None),
            Transform::from_xyz(0.1, 0.0, 0.0),
        ));

        app.update();

        let puddles: Vec<_> = app
            .world_mut()
            .query::<&ChemicalPuddle>()
            .iter(app.world())
            .collect();
        assert_eq!(puddles.len(), 1);
        assert_eq!(puddles[0].solution.total_volume(), Units::whole(15));
    }

    #[test]
    fn puddles_merge_when_their_edges_overlap() {
        let db = chemistry();
        let mut app = App::new();
        app.add_systems(Update, merge_puddles);
        let left = ChemicalPuddle::from_solution(solution(&db, "water", 10), None);
        let right = ChemicalPuddle::from_solution(solution(&db, "sugar", 10), None);
        let distance = left.radius + right.radius - 0.02;
        assert!(
            distance > left.radius.max(right.radius) + MERGE_PADDING,
            "fixture must catch max-radius rather than sum-radius merging",
        );
        app.world_mut().spawn((left, Transform::default()));
        app.world_mut()
            .spawn((right, Transform::from_xyz(distance, 0.0, 0.0)));

        app.update();

        assert_eq!(
            app.world_mut()
                .query::<&ChemicalPuddle>()
                .iter(app.world())
                .count(),
            1,
        );
    }

    #[test]
    fn mixed_attribution_becomes_unknown_when_puddles_merge() {
        let db = chemistry();
        let mut app = App::new();
        app.add_systems(Update, merge_puddles);
        let owner = app.world_mut().spawn_empty().id();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "water", 10), Some(owner)),
            Transform::default(),
        ));
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "sugar", 10), None),
            Transform::from_xyz(0.1, 0.0, 0.0),
        ));

        app.update();

        let mut query = app.world_mut().query::<&ChemicalPuddle>();
        assert_eq!(query.single(app.world()).unwrap().owner, None);
    }

    #[test]
    fn chill_scales_with_the_cold_reagent_not_the_whole_mixture() {
        let db = chemistry();
        let mut mixed = Solution::unbounded();
        let _ = mixed.add(db.reagent("water"), Units::whole(99));
        let _ = mixed.add(db.reagent("cryostylane"), Units::whole(1));
        mixed.temperature.0 = 300.0;
        let puddle = ChemicalPuddle::from_solution(mixed, None);

        let mut app = App::new();
        let puddle = app.world_mut().spawn((puddle, Transform::default())).id();
        app.insert_resource(db)
            .add_systems(Update, initialize_puddle_profile);
        app.update();

        assert_eq!(
            app.world()
                .get::<ChemicalPuddle>(puddle)
                .unwrap()
                .solution
                .temperature
                .0,
            297.0,
        );
    }

    #[test]
    fn space_cleaner_removes_an_overlapping_spill() {
        let db = chemistry();
        let mut app = App::new();
        let residue = app
            .world_mut()
            .spawn((
                ChemicalPuddle::from_solution(solution(&db, "oil", 20), None),
                Transform::default(),
            ))
            .id();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "space_cleaner", 10), None),
            Transform::from_xyz(0.2, 0.0, 0.0),
        ));
        app.insert_resource(db).add_systems(
            Update,
            (initialize_puddle_profile, clean_with_new_puddles).chain(),
        );

        app.update();

        let residue = app.world().get::<ChemicalPuddle>(residue).unwrap();
        assert!(residue.solution.is_empty());
        assert!(residue.remaining < PUDDLE_LIFETIME);
    }

    #[test]
    fn cleaner_cannot_merge_into_and_increase_persistent_residue() {
        let db = chemistry();
        let oil = db.reagent("oil");
        let cleaner = db.reagent("space_cleaner");
        let mut app = App::new();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "oil", 50), None),
            Transform::default(),
        ));
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "space_cleaner", 10), None),
            Transform::from_xyz(0.2, 0.0, 0.0),
        ));
        app.insert_resource(db).init_resource::<Time>().add_systems(
            Update,
            (
                initialize_puddle_profile,
                clean_with_new_puddles,
                merge_puddles,
                age_puddles,
            )
                .chain(),
        );

        app.update();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(3.1));
        app.update();

        let puddles: Vec<_> = app
            .world_mut()
            .query::<&ChemicalPuddle>()
            .iter(app.world())
            .collect();
        assert_eq!(puddles.len(), 1);
        assert!(puddles[0].solution.volume_of(oil) <= Units::whole(26));
        assert_eq!(puddles[0].solution.volume_of(cleaner), Units::ZERO);
        assert!(puddles[0].solution.total_volume() <= Units::whole(26));
    }

    #[test]
    fn flame_deterministically_ignites_overlapping_fuel() {
        let db = chemistry();
        let mut app = App::new();
        let fuel = app
            .world_mut()
            .spawn((
                ChemicalPuddle::from_solution(solution(&db, "oil", 10), None),
                Transform::default(),
            ))
            .id();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "phlogiston", 10), None),
            Transform::from_xyz(0.2, 0.0, 0.0),
        ));
        app.insert_resource(db).add_systems(
            Update,
            (initialize_puddle_profile, spread_puddle_ignition).chain(),
        );

        app.update();

        assert!(app.world().get::<ChemicalPuddle>(fuel).unwrap().ignited);
    }

    #[test]
    fn flash_powder_blinds_bodies_in_its_real_radius() {
        let db = chemistry();
        let mut app = App::new();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "flash_powder", 5), None),
            Transform::default(),
        ));
        let target = app
            .world_mut()
            .spawn((Transform::from_xyz(3.5, 0.0, 0.0), Bloodstream::default()))
            .id();
        app.insert_resource(db)
            .add_systems(Update, affect_world_with_new_puddles);

        app.update();

        assert!(
            app.world()
                .get::<Bloodstream>(target)
                .unwrap()
                .0
                .status(StatusKind::Blurred)
                .remaining
                > 0.0
        );
    }

    #[test]
    fn thermite_marks_a_nearby_door_as_corroded() {
        let db = chemistry();
        let mut app = App::new();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "thermite", 10), None),
            Transform::default(),
        ));
        let door = app
            .world_mut()
            .spawn((
                crate::door::Door {
                    open: false,
                    doorway_index: 0,
                },
                Transform::from_xyz(0.4, 0.0, 0.0),
            ))
            .id();
        app.insert_resource(db)
            .add_systems(Update, affect_world_with_new_puddles);

        app.update();

        assert!(app.world().get::<crate::door::Corroded>(door).is_some());
    }

    #[test]
    fn thermite_dissolves_the_designated_station_locker_prop() {
        let db = chemistry();
        let mut app = App::new();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "thermite", 10), None),
            Transform::default(),
        ));
        let locker = app
            .world_mut()
            .spawn((
                crate::machines::Machine::new(crate::machines::MachineKind::Locker),
                Transform::from_xyz(0.4, 0.0, 0.0),
                Visibility::default(),
                crate::lab::Solid {
                    half_extents: Vec3::splat(0.5),
                },
                crate::interaction::Interactable::new("Storage Locker"),
            ))
            .id();
        app.insert_resource(db).add_systems(
            Update,
            (
                mark_reactive_station_props,
                affect_world_with_new_puddles,
                present_corroded_props,
            )
                .chain(),
        );

        app.update();

        assert!(app.world().get::<crate::door::Corroded>(locker).is_some());
        assert!(app.world().get::<crate::lab::Solid>(locker).is_none());
        assert!(app
            .world()
            .get::<crate::interaction::Interactable>(locker)
            .is_none());
        assert_eq!(
            *app.world().get::<Visibility>(locker).unwrap(),
            Visibility::Hidden
        );
    }

    #[test]
    fn weak_bleach_corrosion_does_not_dissolve_a_station_prop() {
        let db = chemistry();
        let mut app = App::new();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "hydrogen_peroxide", 10), None),
            Transform::default(),
        ));
        let locker = app
            .world_mut()
            .spawn((
                crate::machines::Machine::new(crate::machines::MachineKind::Locker),
                Transform::from_xyz(0.4, 0.0, 0.0),
                Visibility::default(),
                crate::lab::Solid {
                    half_extents: Vec3::splat(0.5),
                },
                crate::interaction::Interactable::new("Storage Locker"),
            ))
            .id();
        app.insert_resource(db).add_systems(
            Update,
            (
                mark_reactive_station_props,
                affect_world_with_new_puddles,
                present_corroded_props,
            )
                .chain(),
        );

        app.update();

        assert_eq!(
            app.world()
                .get::<crate::door::Corroded>(locker)
                .unwrap()
                .strength,
            0.4,
        );
        assert!(app.world().get::<crate::lab::Solid>(locker).is_some());
        assert!(app
            .world()
            .get::<crate::interaction::Interactable>(locker)
            .is_some());
        assert_ne!(
            *app.world().get::<Visibility>(locker).unwrap(),
            Visibility::Hidden
        );
    }

    #[test]
    fn puddle_contact_is_attributed_and_uses_the_touch_route() {
        let db = chemistry();
        let mut app = App::new();
        let actor = app.world_mut().spawn_empty().id();
        let target = app
            .world_mut()
            .spawn((
                Transform::default(),
                Body::default(),
                Bloodstream::default(),
            ))
            .id();
        app.world_mut().spawn((
            ChemicalPuddle::from_solution(solution(&db, "sulphuric_acid", 10), Some(actor)),
            Transform::default(),
        ));
        let mut clock = MetabolismClock::default();
        clock
            .0
            .tick(Duration::from_secs_f32(chem_sim::body::TICK_SECONDS));
        app.insert_resource(db)
            .insert_resource(clock)
            .add_message::<ChemicalExposure>()
            .add_systems(Update, expose_bodies_to_puddles);

        app.update();

        let records: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ChemicalExposure>>()
            .drain()
            .collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].actor, Some(actor));
        assert_eq!(records[0].target, target);
        assert_eq!(records[0].route, Route::Touched);
        assert!(records[0].harmful);
    }

    #[test]
    fn touch_doses_shrink_the_replicated_puddle_radius() {
        let db = chemistry();
        let mut app = App::new();
        app.world_mut().spawn((
            Transform::default(),
            Body::default(),
            Bloodstream::default(),
        ));
        let puddle = app
            .world_mut()
            .spawn((
                ChemicalPuddle::from_solution(solution(&db, "water", 40), None),
                Transform::default(),
            ))
            .id();
        let before = app.world().get::<ChemicalPuddle>(puddle).unwrap().radius;
        let mut clock = MetabolismClock::default();
        clock
            .0
            .tick(Duration::from_secs_f32(chem_sim::body::TICK_SECONDS));
        app.insert_resource(db)
            .insert_resource(clock)
            .add_message::<ChemicalExposure>()
            .add_systems(Update, expose_bodies_to_puddles);

        app.update();

        let puddle = app.world().get::<ChemicalPuddle>(puddle).unwrap();
        assert!(puddle.radius < before);
        assert_eq!(puddle.radius, radius_for(puddle.solution.total_volume()));
    }

    fn abuse_app(with_witness: bool) -> (App, Entity, Entity) {
        let db = chemistry();
        let mut app = App::new();
        app.insert_resource(db)
            .insert_resource(crate::lab::WalkableAreas::from_floor_plan())
            .init_resource::<Shift>()
            .init_resource::<crate::antagonist::SecuritySuspicion>()
            .init_resource::<RadioLog>()
            .add_message::<ChemicalExposure>()
            .add_systems(Update, respond_to_unwanted_exposure);
        let actor = app.world_mut().spawn_empty().id();
        let victim = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Patient".to_string(),
                    role: "Engineering".to_string(),
                },
                CrewRoute::arrival(0.0),
                Transform::default(),
            ))
            .id();
        if with_witness {
            app.world_mut().spawn((
                CrewMember {
                    name: "Witness".to_string(),
                    role: "Service".to_string(),
                },
                Transform::from_xyz(0.5, 0.0, 0.0),
            ));
        }
        (app, actor, victim)
    }

    fn write_abuse(app: &mut App, actor: Entity, victim: Entity) {
        let dose = {
            let db = app.world().resource::<ChemDb>();
            solution(&db, "sulphuric_acid", 10)
        };
        app.world_mut().write_message(ChemicalExposure {
            actor: Some(actor),
            target: victim,
            route: Route::Touched,
            source: ExposureSource::Direct,
            solution: dose,
            authorized: false,
            helpful: false,
            harmful: true,
            illicit: false,
            overdose: false,
        });
    }

    #[test]
    fn unwanted_harm_is_reported_once_and_costs_standing() {
        let (mut app, actor, victim) = abuse_app(false);
        write_abuse(&mut app, actor, victim);
        app.update();

        assert_eq!(
            app.world()
                .resource::<crate::antagonist::SecuritySuspicion>()
                .0,
            2
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Engineering),
            -2
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
            0,
            "suspicion rises separately; Security standing is not a second penalty",
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
        assert_eq!(
            app.world().get::<CrewRoute>(victim).unwrap().phase,
            CrewPhase::Leaving
        );

        // The incident marker, rather than incidental route state, prevents a
        // continuing exposure from charging the station twice.
        write_abuse(&mut app, actor, victim);
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::antagonist::SecuritySuspicion>()
                .0,
            2
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn a_same_room_witness_amplifies_the_response_once() {
        let (mut app, actor, victim) = abuse_app(true);
        write_abuse(&mut app, actor, victim);
        app.update();

        assert_eq!(
            app.world()
                .resource::<crate::antagonist::SecuritySuspicion>()
                .0,
            3
        );
        assert!(app
            .world()
            .resource::<RadioLog>()
            .entries
            .back()
            .unwrap()
            .text
            .contains("with witnesses"));
    }

    #[test]
    fn attributed_smoke_and_puddle_abuse_are_reported() {
        for (source, wording) in [
            (ExposureSource::Smoke, "chemical smoke"),
            (ExposureSource::Puddle, "chemical spill"),
        ] {
            let (mut app, actor, victim) = abuse_app(false);
            let dose = {
                let db = app.world().resource::<ChemDb>();
                solution(&db, "sulphuric_acid", 10)
            };
            app.world_mut().write_message(ChemicalExposure {
                actor: Some(actor),
                target: victim,
                route: Route::Touched,
                source,
                solution: dose,
                authorized: false,
                helpful: false,
                harmful: true,
                illicit: false,
                overdose: false,
            });

            app.update();

            assert_eq!(
                app.world()
                    .resource::<crate::antagonist::SecuritySuspicion>()
                    .0,
                2,
            );
            assert!(app
                .world()
                .resource::<RadioLog>()
                .entries
                .back()
                .unwrap()
                .text
                .contains(wording));
        }
    }

    #[test]
    fn an_unrelated_leaving_route_does_not_hide_a_new_incident() {
        let (mut app, actor, victim) = abuse_app(false);
        app.world_mut()
            .get_mut::<CrewRoute>(victim)
            .unwrap()
            .leave();
        write_abuse(&mut app, actor, victim);

        app.update();

        assert_eq!(
            app.world()
                .resource::<crate::antagonist::SecuritySuspicion>()
                .0,
            2,
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn helpful_non_overdose_treatment_is_tolerated_without_reward() {
        let (mut app, actor, victim) = abuse_app(true);
        let dose = {
            let db = app.world().resource::<ChemDb>();
            solution(&db, "dylovene", 5)
        };
        app.world_mut().write_message(ChemicalExposure {
            actor: Some(actor),
            target: victim,
            route: Route::Touched,
            source: ExposureSource::Direct,
            solution: dose,
            authorized: false,
            helpful: true,
            harmful: false,
            illicit: false,
            overdose: false,
        });

        app.update();

        assert_ne!(
            app.world().get::<CrewRoute>(victim).unwrap().phase,
            CrewPhase::Leaving
        );
        assert_eq!(
            app.world()
                .resource::<crate::antagonist::SecuritySuspicion>()
                .0,
            0
        );
        assert!(app.world().resource::<RadioLog>().entries.is_empty());
        assert!(app
            .world()
            .resource::<Shift>()
            .department_standing
            .is_empty());
    }
}
