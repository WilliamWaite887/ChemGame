//! Chemists with bodies.
//!
//! The simulation lives in `chem_sim::body`; this is the Bevy wrapper around
//! it. Everything here is entities, timers, messages and authority — the
//! arithmetic of being poisoned is deliberately somewhere a unit test can reach
//! without an `App`.
//!
//! Authority sits with the server, like every other mutation in the game. The
//! tick runs there and the result reaches clients as replicated components, so
//! there is nothing to predict and nothing to reconcile.

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::body::{Health, TICK_SECONDS};
use chem_sim::{metabolise, DamageKind, Route, Units};
use serde::{Deserialize, Serialize};

use crate::audio::{EmitWorldSfx, Sfx};
use crate::chem_data::ChemDb;
use crate::chem_world::{
    assess_exposure, order_authorizes_dose, spawn_puddle, ChemicalExposure, ExposureSource,
};
use crate::containers::{Container, ContainerKind, HeldBy};
use crate::machines::{chemist_entity, ReactionsFired};
use crate::net::is_authority;
use crate::player::Chemist;
use crate::AppState;

/// How much of a beaker or bottle one press swallows.
///
/// A mouthful rather than the lot: drinking 50u of something in one keypress
/// would make the syringe pointless and the mistake unrecoverable.
const SIP: Units = Units::whole(5);

/// A hand pour is large enough to matter and small enough to correct.
const HAND_TRANSFER: Units = Units::whole(10);

/// Ticks to run at most in one frame.
///
/// A frame that stalled — an asset load, a window drag, a client joining — must
/// not deliver twenty ticks of plasma at once. Clamping here rather than
/// relying on `Time<Fixed>`'s own catch-up keeps the limit somewhere it can be
/// reasoned about and tested.
const MAX_CATCHUP_TICKS: u32 = 3;

fn emit_world_sfx(sounds: &mut Option<ResMut<Messages<EmitWorldSfx>>>, sound: Sfx, position: Vec3) {
    if let Some(sounds) = sounds {
        sounds.write(EmitWorldSfx::new(sound, position));
    }
}

/// Seconds before medical comes and drags a collapsed chemist out.
///
/// This is the anti-softlock, and singleplayer needs it: a chemist who goes
/// down alone with brute damage cannot walk, cannot drink, and cannot reach the
/// dispenser to make the thing that would fix them. In co-op the other chemist
/// can beat this clock with a syringe, which is the best reason the game has
/// for a second person being in the room.
const MEDBAY_SECONDS: f32 = 25.0;

/// Reputation lost for going down on shift. Matches `Outcome::Wrong`: it is the
/// same size of mistake.
///
/// `pub(crate)` since M12: `crew::handle_crew_collapse` reuses it for a crew
/// member going down in the lab, the same size of mistake either way.
pub(crate) const COLLAPSE_PENALTY: i32 = -3;

/// How hurt medical hands you back. Below `RECOVER`, so you are on your feet,
/// and not by much, so it is not a free heal.
const PATCHED_UP: Health = Units::whole(60);

pub struct BodyPlugin;

impl Plugin for BodyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MetabolismClock>()
            .add_message::<ChemicalExposure>()
            .add_message::<EmitWorldSfx>()
            .add_client_message::<ConsumeRequested>(Channel::Ordered)
            .add_mapped_client_message::<ApplyHeldRequested>(Channel::Ordered)
            .add_systems(
                Update,
                (
                    (
                        handle_apply_held,
                        handle_consume,
                        run_metabolism,
                        handle_chemical_incapacitation,
                        handle_collapse,
                        run_medbay_retrieval,
                    )
                        .chain()
                        .run_if(is_authority),
                    // Drinking and applying stop while the pause menu is up;
                    // collapsing does not, since nothing about being paused
                    // makes a panel safe to leave open on a chemist who just
                    // went down.
                    (request_consume, request_apply_held).run_if(crate::settings::not_paused),
                    close_panel_on_collapse,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// How hurt a chemist is.
///
/// A newtype in the shape of `Buffer(Solution)`, and separate from
/// [`Bloodstream`] so the HUD can watch `Changed<Body>` without waking up every
/// time a reagent ticks down by 0.4u.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Body(pub chem_sim::Vitals);

/// What is currently inside a chemist.
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bloodstream(pub chem_sim::Bloodstream);

/// Drives the 2-second metabolism cycle.
///
/// A plain `Timer` in `Update` rather than a `FixedUpdate` schedule, and that
/// is a considered choice. `Time<Fixed>` is a single global: setting it to two
/// seconds would set it for anything that ever wants a fixed step later, and
/// metabolism's cadence is a gameplay number rather than the app's simulation
/// rate. It also keeps the headless tests honest — they drive `app.update()` in
/// a tight loop where microseconds of real time elapse, so a two-second fixed
/// system would essentially never fire.
#[derive(Resource)]
pub struct MetabolismClock(pub Timer);

impl Default for MetabolismClock {
    fn default() -> Self {
        MetabolismClock(Timer::from_seconds(TICK_SECONDS, TimerMode::Repeating))
    }
}

/// Drink or swallow whatever is in hand.
///
/// No fields, exactly like `DropRequested`: the sender's identity comes from
/// the connection, never from the message, so one client cannot make the other
/// chemist swallow something.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct ConsumeRequested;

/// Use what is in hand on what is under the crosshair.
///
/// Carries the target but *not* the actor, for the same reason
/// `InteractRequested` does not: a message that named its own actor would let
/// one client inject the other chemist by editing a field.
///
/// The server decides what the pair means. A syringe on a container draws; a
/// syringe on a body injects; a syringe on nothing injects you.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct ApplyHeldRequested {
    #[entities]
    pub target: Option<Entity>,
    /// World-space camera hit, used when the application is a floor pour.
    pub point: Option<Vec3>,
}

/// **R** — take a mouthful.
///
/// Deliberately its own key rather than sharing `E`. `E` already means "pick up
/// or use", and a misfire that made you drink the plasma you were reaching for
/// is not an acceptable failure mode of a button you press constantly.
fn request_consume(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<crate::settings::Settings>,
    players: Query<&crate::interaction::InteractionMode, With<crate::player::LocalPlayer>>,
    mut requests: MessageWriter<ConsumeRequested>,
) {
    if !keys.just_pressed(settings.bindings.drink) {
        return;
    }
    // Not while a panel is open: R is a text-adjacent key and the panel has
    // focus.
    if players.iter().any(|mode| mode.is_roaming()) {
        requests.write(ConsumeRequested);
    }
}

/// **F** — use the thing in your hand on the thing you are looking at.
///
/// Deliberately not `E`. `E` already means "pick up, or load this machine", and
/// walking up to the Mixing Chamber holding a syringe would make it ambiguous
/// whether you meant to load the syringe or inject the machine.
fn request_apply_held(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<crate::settings::Settings>,
    players: Query<
        (
            &crate::interaction::Focus,
            &crate::interaction::InteractionMode,
        ),
        With<crate::player::LocalPlayer>,
    >,
    mut requests: MessageWriter<ApplyHeldRequested>,
) {
    if !keys.just_pressed(settings.bindings.apply) {
        return;
    }
    for (focus, mode) in &players {
        if mode.is_roaming() {
            requests.write(ApplyHeldRequested {
                target: focus.target,
                point: focus.point,
            });
        }
    }
}

/// Draws into a syringe, or empties one into somebody.
///
/// The whole of the syringe's behaviour lives here, keyed off what is in hand
/// and what is being pointed at. Anything else in hand is ignored — the beaker
/// you are carrying is not a weapon.
fn handle_apply_held(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<ApplyHeldRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    transforms: Query<&Transform>,
    solids: Query<(&Transform, &crate::lab::Solid)>,
    mut bodies: Query<(&mut Body, &mut Bloodstream)>,
    held: Query<(Entity, &HeldBy)>,
    mut containers: Query<&mut Container>,
    chemist_bodies: Query<(), With<Chemist>>,
    orders: Query<(
        &crate::orders::Order,
        Has<crate::orders::IllicitOrder>,
        Has<crate::orders::CrisisOrder>,
        Has<crate::orders::CounterOrder>,
    )>,
    crisis_targets: Query<(), With<crate::crisis::CrisisResponse>>,
    mut fired: MessageWriter<ReactionsFired>,
    mut exposures: MessageWriter<ChemicalExposure>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok(actor_transform) = transforms.get(player) else {
            continue;
        };
        let actor_position = actor_transform.translation;
        if !actor_position.is_finite() || request.point.is_some_and(|point| !point.is_finite()) {
            continue;
        }

        // Focus and its mesh raycast live on the client. Re-establish their
        // reach/occlusion guarantees on the authority before splitting even a
        // hundredth of a unit from the held solution. Entity roots get a small
        // collider-centre allowance; floor points are exact ray hits.
        let endpoint = if let Some(target) = request.target {
            let Ok(target_transform) = transforms.get(target) else {
                continue;
            };
            let endpoint = target_transform.translation;
            if !crate::interaction::authority_target_in_reach(actor_position, endpoint) {
                continue;
            }
            Some(endpoint)
        } else if let Some(point) = request.point {
            if !crate::interaction::authority_point_in_reach(actor_position, point) {
                continue;
            }
            Some(point)
        } else {
            // A syringe deliberately injects its holder when no target or
            // floor hit was supplied.
            None
        };
        if endpoint.is_some_and(|endpoint| {
            solids.iter().any(|(transform, solid)| {
                crate::interaction::authority_segment_blocked(
                    actor_position,
                    endpoint,
                    transform.translation,
                    solid.half_extents,
                )
            })
        }) {
            continue;
        }
        if bodies
            .get(player)
            .is_ok_and(|(body, blood)| body.0.collapsed || blood.0.incapacitated())
        {
            continue;
        }
        let Some((held_entity, _)) = held.iter().find(|(_, holder)| holder.0 == player) else {
            continue;
        };
        let Ok(container) = containers.get(held_entity) else {
            continue;
        };
        let kind = container.kind;

        if kind != ContainerKind::Syringe {
            // A pill has no open liquid surface; R remains its only deliberate
            // use. Beakers and bottles share the hand-pour interaction.
            if kind == ContainerKind::Pill {
                continue;
            }

            if let Some(target) = request.target.filter(|target| *target != held_entity) {
                if containers.contains(target) {
                    let space = containers
                        .get(target)
                        .map(|container| container.solution.available_volume())
                        .unwrap_or(Units::ZERO);
                    let amount = HAND_TRANSFER.min(space);
                    if !amount.is_positive() {
                        continue;
                    }
                    let moved = containers
                        .get_mut(held_entity)
                        .map(|mut source| source.mutate(&db, |solution| solution.split(amount)).0)
                        .unwrap_or_else(|_| chem_sim::Solution::unbounded());
                    if moved.is_empty() {
                        continue;
                    }
                    if let Ok(mut destination) = containers.get_mut(target) {
                        let (_, report) = destination.mutate(&db, |solution| {
                            let destination_volume = solution.total_volume().as_f32();
                            let moved_volume = moved.total_volume().as_f32();
                            let combined = destination_volume + moved_volume;
                            if combined > 0.0 {
                                solution.temperature.0 = (solution.temperature.0
                                    * destination_volume
                                    + moved.temperature.0 * moved_volume)
                                    / combined;
                            }
                            for (reagent, amount) in moved.iter() {
                                let _ = solution.add(reagent, amount);
                            }
                        });
                        if let Some(message) = ReactionsFired::from_report(target, &report) {
                            fired.write(message);
                        }
                        if let Ok(transform) = transforms.get(target) {
                            emit_world_sfx(&mut sounds, Sfx::DirectPour, transform.translation);
                        }
                    }
                    continue;
                }

                if bodies.contains(target) {
                    let mut dose = containers
                        .get_mut(held_entity)
                        .map(|mut source| {
                            source
                                .mutate(&db, |solution| solution.split(HAND_TRANSFER))
                                .0
                        })
                        .unwrap_or_else(|_| chem_sim::Solution::unbounded());
                    if dose.is_empty() {
                        continue;
                    }
                    let snapshot = dose.clone();
                    let Ok((mut body, mut blood)) = bodies.get_mut(target) else {
                        continue;
                    };
                    let assessment = assess_exposure(&snapshot, Route::Touched, &body, &blood, &db);
                    let requested =
                        orders
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
                    blood.0.receive(&mut dose, Route::Touched, &mut body.0, &db);
                    exposures.write(ChemicalExposure {
                        actor: Some(player),
                        target,
                        route: Route::Touched,
                        source: ExposureSource::Direct,
                        solution: snapshot,
                        authorized: target == player
                            || chemist_bodies.contains(target)
                            || requested
                            || crisis_care,
                        helpful: assessment.helpful,
                        harmful: assessment.harmful,
                        illicit: assessment.illicit,
                        overdose: assessment.overdose,
                    });
                    if let Ok(transform) = transforms.get(target) {
                        emit_world_sfx(&mut sounds, Sfx::Splash, transform.translation);
                    }
                    continue;
                }

                // Looking at a machine or prop is not a floor pour.
                continue;
            }

            if let Some(point) = request.point {
                let spilled = containers
                    .get_mut(held_entity)
                    .map(|mut source| {
                        source
                            .mutate(&db, |solution| solution.split(HAND_TRANSFER))
                            .0
                    })
                    .unwrap_or_else(|_| chem_sim::Solution::unbounded());
                if spawn_puddle(&mut commands, spilled, point, Some(player)).is_some() {
                    emit_world_sfx(&mut sounds, Sfx::Splash, point);
                }
            }
            continue;
        }

        // Pointing at a container means draw; anything else means inject.
        let drawing = request
            .target
            .is_some_and(|target| target != held_entity && containers.contains(target));

        if drawing {
            let source_entity = request.target.expect("a draw target was just checked");
            let space = {
                let syringe = containers
                    .get(held_entity)
                    .expect("the syringe was just checked");
                syringe.solution.available_volume()
            };
            if !space.is_positive() {
                continue;
            }

            // Proportional, because `transfer_to` is: you cannot syringe the
            // oxygen out of a mixed beaker and leave the sugar behind.
            let drawn = {
                let Ok(mut source) = containers.get_mut(source_entity) else {
                    continue;
                };
                source.mutate(&db, |solution| solution.split(space)).0
            };
            if let Ok(mut syringe) = containers.get_mut(held_entity) {
                syringe.mutate(&db, |solution| {
                    for (reagent, amount) in drawn.iter() {
                        let _ = solution.add(reagent, amount);
                    }
                });
            }
            continue;
        }

        // Injecting. The target is another chemist if one is under the
        // crosshair, otherwise yourself.
        let patient = match request.target {
            Some(target) if bodies.contains(target) => target,
            _ => player,
        };
        let Ok(mut syringe) = containers.get_mut(held_entity) else {
            continue;
        };
        let mut dose = syringe
            .mutate(&db, |solution| solution.split(solution.total_volume()))
            .0;
        if dose.total_volume().is_zero() {
            continue;
        }
        let snapshot = dose.clone();
        let Ok((mut body, mut blood)) = bodies.get_mut(patient) else {
            continue;
        };
        let assessment = assess_exposure(&snapshot, Route::Injected, &body, &blood, &db);
        let requested = orders
            .get(patient)
            .is_ok_and(|(order, illicit, crisis, counter)| {
                !assessment.overdose
                    && order_authorizes_dose(
                        &snapshot,
                        order,
                        crate::orders::OrderKind::of(illicit, crisis, counter),
                        &db,
                    )
            });
        let crisis_care = crisis_targets.contains(patient)
            && assessment.helpful
            && !assessment.illicit
            && !assessment.overdose;
        blood
            .0
            .receive(&mut dose, Route::Injected, &mut body.0, &db);
        exposures.write(ChemicalExposure {
            actor: Some(player),
            target: patient,
            route: Route::Injected,
            source: ExposureSource::Direct,
            solution: snapshot,
            authorized: patient == player
                || chemist_bodies.contains(patient)
                || requested
                || crisis_care,
            helpful: assessment.helpful,
            harmful: assessment.harmful,
            illicit: assessment.illicit,
            overdose: assessment.overdose,
        });
        if let Ok(transform) = transforms.get(patient) {
            emit_world_sfx(&mut sounds, Sfx::Inject, transform.translation);
        }
    }
}

fn handle_consume(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<ConsumeRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    transforms: Query<&Transform>,
    mut bodies: Query<(&mut Body, &mut Bloodstream)>,
    held: Query<(Entity, &HeldBy)>,
    mut containers: Query<&mut Container>,
    mut exposures: MessageWriter<ChemicalExposure>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok((mut body, mut blood)) = bodies.get_mut(player) else {
            continue;
        };
        if body.0.collapsed || blood.0.incapacitated() {
            continue;
        }
        let Some((entity, _)) = held.iter().find(|(_, holder)| holder.0 == player) else {
            continue;
        };
        let Ok(mut container) = containers.get_mut(entity) else {
            continue;
        };

        // A pill is swallowed whole; anything you drink from gives a mouthful.
        let whole = matches!(container.kind, ContainerKind::Pill);
        let amount = if whole {
            container.solution.total_volume()
        } else {
            SIP
        };
        let (mut dose, _) = container.mutate(&db, |solution| solution.split(amount));
        if dose.total_volume().is_zero() {
            continue;
        }

        let snapshot = dose.clone();
        let assessment = assess_exposure(&snapshot, Route::Ingested, &body, &blood, &db);
        blood
            .0
            .receive(&mut dose, Route::Ingested, &mut body.0, &db);
        exposures.write(ChemicalExposure {
            actor: Some(player),
            target: player,
            route: Route::Ingested,
            source: ExposureSource::Direct,
            solution: snapshot,
            authorized: true,
            helpful: assessment.helpful,
            harmful: assessment.harmful,
            illicit: assessment.illicit,
            overdose: assessment.overdose,
        });
        if let Ok(transform) = transforms.get(player) {
            emit_world_sfx(&mut sounds, Sfx::Drink, transform.translation);
        }

        // An empty pill is a wrapper. Nothing else despawns on use, because a
        // beaker you have drained is still a beaker.
        if whole {
            commands.entity(entity).despawn();
        }
    }
}

/// Runs the metabolism tick for every body in the lab.
fn run_metabolism(
    time: Res<Time>,
    db: Option<Res<ChemDb>>,
    mut clock: ResMut<MetabolismClock>,
    mut bodies: Query<(&mut Body, &mut Bloodstream)>,
) {
    let Some(db) = db else {
        return;
    };
    let ticks = clock
        .0
        .tick(time.delta())
        .times_finished_this_tick()
        .min(MAX_CATCHUP_TICKS);
    if ticks == 0 {
        return;
    }

    for (mut body, mut blood) in &mut bodies {
        // An untouched chemist with no damage has nothing a tick could change,
        // and mutating through the `Mut` guard would flag the component as
        // changed and wake the HUD and replication for nothing.
        if blood.0.is_empty() && !body.0.is_hurt() {
            continue;
        }
        for _ in 0..ticks {
            metabolise(&mut body.0, &mut blood.0, &db);
        }
    }
}

/// Counts down to medical arriving for a chemist who is on the floor.
#[derive(Component)]
pub struct MedbayRetrieval(pub f32);

/// Edge marker for status-driven incapacity. Unlike damage collapse this does
/// not summon Medical or penalize the shift; it exists so the server performs
/// the one-time drop/claim release when sedation crosses its threshold.
#[derive(Component)]
pub struct ChemicallyIncapacitated;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_chemical_incapacitation(
    mut commands: Commands,
    chemists: Query<
        (
            Entity,
            &Body,
            &Bloodstream,
            &Transform,
            Has<ChemicallyIncapacitated>,
        ),
        (With<Chemist>, Changed<Bloodstream>),
    >,
    mut modes: Query<&mut crate::interaction::InteractionMode>,
    mut machines: Query<&mut crate::machines::Machine>,
    held: Query<(Entity, &HeldBy)>,
) {
    for (player, body, blood, transform, was_incapacitated) in &chemists {
        let incapacitated = blood.0.incapacitated();
        if incapacitated == was_incapacitated {
            continue;
        }
        if !incapacitated {
            commands.entity(player).remove::<ChemicallyIncapacitated>();
            continue;
        }

        commands.entity(player).insert(ChemicallyIncapacitated);
        // Damage collapse owns the identical cleanup on its own edge. Avoid
        // giving two deferred command paths responsibility for the same hand.
        if body.0.collapsed {
            continue;
        }
        for (container, holder) in &held {
            if holder.0 != player {
                continue;
            }
            let ahead = transform.translation + transform.forward() * 0.4;
            commands
                .entity(container)
                .remove::<HeldBy>()
                .insert(Transform::from_xyz(ahead.x, 0.08, ahead.z));
        }
        if let Ok(mut mode) = modes.get_mut(player) {
            crate::interaction::release_claim(player, &mut mode, &mut machines);
        }
    }
}

/// Everything that happens the moment a chemist goes down.
///
/// Takes the panel off the screen of a chemist who has just gone down.
///
/// The server released their claim in [`handle_collapse`]; this is the other
/// half, and it has to be local because `InteractionMode` is. Runs everywhere
/// rather than under authority, so a collapsed client is not left reading the
/// dispenser from the floor.
fn close_panel_on_collapse(
    mut players: Query<
        (
            &Body,
            &Bloodstream,
            &mut crate::interaction::InteractionMode,
        ),
        With<crate::player::LocalPlayer>,
    >,
) {
    for (body, blood, mut mode) in &mut players {
        if (body.0.collapsed || blood.0.incapacitated()) && !mode.is_roaming() {
            *mode = crate::interaction::InteractionMode::Roaming;
        }
    }
}

/// Split from the metabolism tick because a body can also collapse from a
/// blast, and both paths must let go of the machine and the beaker. Keyed off
/// `Added` rather than the tick report so there is exactly one place that
/// notices, whatever caused it.
///
/// `With<Chemist>` since M12, when crew gained `Body` too — a collapsed crew
/// member is not holding a machine claim or waiting on medical, they are
/// walking a route. See `crew::handle_crew_collapse` for their side of this.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_collapse(
    mut commands: Commands,
    mut shift: ResMut<crate::orders::Shift>,
    mut radio: ResMut<crate::radio::RadioLog>,
    bodies: Query<(Entity, &Body), (Changed<Body>, With<Chemist>)>,
    down: Query<(), With<MedbayRetrieval>>,
    mut modes: Query<&mut crate::interaction::InteractionMode>,
    mut machines: Query<&mut crate::machines::Machine>,
    held: Query<(Entity, &HeldBy)>,
    transforms: Query<&Transform>,
) {
    for (player, body) in &bodies {
        if !body.0.collapsed || down.contains(player) {
            continue;
        }

        // Let go of whatever they were holding, where they stand.
        if let Ok(transform) = transforms.get(player) {
            for (container, holder) in &held {
                if holder.0 != player {
                    continue;
                }
                let ahead = transform.translation + transform.forward() * 0.4;
                commands
                    .entity(container)
                    .remove::<HeldBy>()
                    .insert(Transform::from_xyz(ahead.x, 0.08, ahead.z));
            }
        }

        // And of whatever machine they had open, or it stays claimed forever
        // and the other chemist can never use it.
        if let Ok(mut mode) = modes.get_mut(player) {
            crate::interaction::release_claim(player, &mut mode, &mut machines);
        }

        commands
            .entity(player)
            .insert(MedbayRetrieval(MEDBAY_SECONDS));
        shift.adjust(crate::orders::Department::Medical, COLLAPSE_PENALTY);
        radio.push(
            crate::radio::RadioEntry::new(
                crate::radio::RadioChannel::Medical,
                "Chemistry, we're reading a crew member down in your lab. Someone is on the way.",
            )
            .speaker("Nurse Okonkwo")
            .negative()
            .urgent(),
        );
    }
}

/// Medical arrives, patches you up, and leaves you by the door.
fn run_medbay_retrieval(
    mut commands: Commands,
    time: Res<Time>,
    mut radio: ResMut<crate::radio::RadioLog>,
    mut down: Query<(
        Entity,
        &mut MedbayRetrieval,
        &mut Body,
        &mut Bloodstream,
        &mut Transform,
    )>,
) {
    for (player, mut retrieval, mut body, mut blood, mut transform) in &mut down {
        // Somebody got to them first — the co-op save.
        if !body.0.collapsed {
            commands.entity(player).remove::<MedbayRetrieval>();
            continue;
        }

        retrieval.0 -= time.delta_secs();
        if retrieval.0 > 0.0 {
            continue;
        }

        // Patched up, not healed: enough to stand, not enough to be careless.
        // The bloodstream is flushed, because whatever put them down is still
        // in there and would put them straight back down.
        body.0 = chem_sim::Vitals {
            damage: chem_sim::Damage {
                brute: PATCHED_UP.min(body.0.damage.brute),
                burn: PATCHED_UP.min(body.0.damage.burn),
                toxin: PATCHED_UP.min(body.0.damage.toxin),
                oxygen: Units::ZERO,
            },
            collapsed: false,
        };
        // Scale the whole body back under the recovery line in one step rather
        // than clamping each type, which for four maxed types would still leave
        // them collapsed.
        while body.0.total() >= chem_sim::RECOVER {
            for kind in DamageKind::ALL {
                let slot = body.0.damage.get_mut(kind);
                *slot = (*slot - Units::whole(5)).clamp_non_negative();
            }
        }
        *blood = Bloodstream::default();

        // Left by the door, which is where medical would have dragged them.
        transform.translation = crate::lab::MEDBAY_DROP.with_y(crate::player::EYE_HEIGHT);
        commands.entity(player).remove::<MedbayRetrieval>();
        radio.push(
            crate::radio::RadioEntry::new(
                crate::radio::RadioChannel::Medical,
                "Got them back on their feet. Try not to make a habit of it.",
            )
            .speaker("Nurse Okonkwo"),
        );
    }
}

#[cfg(test)]
mod tests {
    //! Headless: message in, body out. No window, no renderer.
    //!
    //! The arithmetic of metabolism is covered in `crates/chem_sim/tests/body.rs`
    //! with no `App` at all. What is left to test here is the wiring — the
    //! clock, the keypress path, and who is allowed to act.

    use super::*;
    use crate::containers::HeldBy;
    use bevy::app::App;
    use chem_sim::{ChemData, DamageKind, Solution};
    use std::time::Duration;

    fn test_app() -> App {
        let data = ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should load");

        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .init_resource::<MetabolismClock>()
            .init_resource::<Time>()
            .add_message::<FromClient<ConsumeRequested>>()
            .add_message::<FromClient<ApplyHeldRequested>>()
            .add_message::<ChemicalExposure>()
            .add_message::<EmitWorldSfx>()
            .add_message::<ReactionsFired>()
            .add_systems(
                Update,
                (handle_apply_held, handle_consume, run_metabolism).chain(),
            );
        app
    }

    /// A chemist holding `kind` filled with `amount` of `reagent`.
    fn chemist_holding(
        app: &mut App,
        kind: ContainerKind,
        reagent: &str,
        amount: i32,
    ) -> (Entity, Entity) {
        let id = app.world().resource::<ChemDb>().reagent(reagent);
        let player = app
            .world_mut()
            .spawn((
                Chemist {
                    client: ClientId::Server,
                },
                Body::default(),
                Bloodstream::default(),
                Transform::from_xyz(0.0, crate::player::EYE_HEIGHT, 0.0),
            ))
            .id();

        let mut container = Container::new(kind);
        let _ = container.solution.add(id, Units::whole(amount));
        let held = app.world_mut().spawn((container, HeldBy(player))).id();
        (player, held)
    }

    fn consume(app: &mut App) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ConsumeRequested,
        });
        app.update();
    }

    /// Moves the clock on without running any real time.
    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(seconds));
        app.update();
    }

    /// Puts a dose straight into the blood, skipping the container and the
    /// keypress. `Vitals` is `Copy`, so it can be taken out, handed to
    /// `receive` alongside the borrowed bloodstream, and written back.
    fn inject(app: &mut App, player: Entity, reagent: &str, amount: i32) {
        let db = app.world().resource::<ChemDb>().0.clone();
        let mut dose = Solution::unbounded();
        let _ = dose.add(db.reagent(reagent), Units::whole(amount));

        let mut entity = app.world_mut().entity_mut(player);
        let mut vitals = entity.get::<Body>().unwrap().0;
        entity.get_mut::<Bloodstream>().unwrap().0.receive(
            &mut dose,
            Route::Injected,
            &mut vitals,
            &db,
        );
        entity.get_mut::<Body>().unwrap().0 = vitals;
    }

    fn blood_of(app: &App, player: Entity) -> &chem_sim::Bloodstream {
        &app.world().get::<Bloodstream>(player).unwrap().0
    }

    #[test]
    fn drinking_takes_a_mouthful_out_of_the_beaker_and_into_the_stomach() {
        let mut app = test_app();
        let (player, beaker) = chemist_holding(&mut app, ContainerKind::Beaker, "dylovene", 40);
        let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");

        consume(&mut app);

        let left = app.world().get::<Container>(beaker).unwrap();
        assert_eq!(
            left.solution.volume_of(dylovene),
            Units::whole(35),
            "a sip, not the lot"
        );
        // 60% of a 5u sip lands, and it lands in the stomach rather than the
        // blood — that delay is the whole difference from a syringe.
        assert_eq!(
            blood_of(&app, player).stomach.volume_of(dylovene),
            Units::whole(3)
        );
        assert!(blood_of(&app, player).blood.is_empty());
    }

    #[test]
    fn a_pill_is_swallowed_whole_and_the_wrapper_goes_with_it() {
        let mut app = test_app();
        let (player, pill) = chemist_holding(&mut app, ContainerKind::Pill, "dylovene", 20);
        let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");

        consume(&mut app);

        assert!(
            app.world().get_entity(pill).is_err(),
            "an emptied pill is a wrapper, not glassware"
        );
        assert_eq!(
            blood_of(&app, player).stomach.volume_of(dylovene),
            Units::whole(12),
            "all 20u swallowed, 60% of it absorbed"
        );
    }

    #[test]
    fn drinking_an_empty_beaker_does_nothing() {
        let mut app = test_app();
        let (player, beaker) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 0);

        consume(&mut app);

        assert!(app.world().get_entity(beaker).is_ok());
        assert!(blood_of(&app, player).is_empty());
    }

    #[test]
    fn a_collapsed_chemist_cannot_drink() {
        let mut app = test_app();
        let (player, beaker) = chemist_holding(&mut app, ContainerKind::Beaker, "dylovene", 40);
        app.world_mut().get_mut::<Body>(player).unwrap().0.collapsed = true;

        consume(&mut app);

        let left = app.world().get::<Container>(beaker).unwrap();
        assert_eq!(
            left.solution.total_volume(),
            Units::whole(40),
            "someone on the floor cannot reach their own beaker"
        );
    }

    #[test]
    fn the_clock_ticks_once_every_two_seconds() {
        let mut app = test_app();
        let (player, _) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 0);
        // Plasma does 3 toxin per tick, which makes the clock readable.
        inject(&mut app, player, "plasma", 10);

        advance(&mut app, 1.0);
        assert_eq!(
            app.world().get::<Body>(player).unwrap().0.damage.toxin,
            Units::ZERO,
            "one second is not a tick"
        );

        advance(&mut app, 1.0);
        assert_eq!(
            app.world().get::<Body>(player).unwrap().0.damage.toxin,
            Units::whole(3),
            "two seconds is"
        );
    }

    #[test]
    fn a_stalled_frame_cannot_deliver_a_whole_shifts_worth_of_poison() {
        let mut app = test_app();
        let (player, _) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 0);
        inject(&mut app, player, "plasma", 50);

        // One 20-second frame — an asset load, a window drag, a client joining.
        advance(&mut app, 20.0);

        assert_eq!(
            app.world().get::<Body>(player).unwrap().0.damage.toxin,
            Units::whole(9),
            "clamped to {MAX_CATCHUP_TICKS} ticks, not the ten the clock is owed"
        );
    }

    // -----------------------------------------------------------------------
    // Syringe
    // -----------------------------------------------------------------------

    fn apply(app: &mut App, target: Option<Entity>) {
        // Gameplay entities always have transforms. Most body fixtures do not
        // care where their patient is, so place an otherwise positionless one
        // within hand reach here; forged-distance tests send their request
        // directly and retain the position they are exercising.
        if let Some(target) = target {
            if app.world().get::<Transform>(target).is_none() {
                app.world_mut()
                    .entity_mut(target)
                    .insert(Transform::default());
            }
        }
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ApplyHeldRequested {
                target,
                point: None,
            },
        });
        app.update();
    }

    /// A loose beaker on the bench holding `contents`.
    fn bench_beaker(app: &mut App, contents: &[(&str, i32)]) -> Entity {
        let db = app.world().resource::<ChemDb>().0.clone();
        let mut container = Container::new(ContainerKind::Beaker);
        for (key, amount) in contents {
            // `add` returns what did not fit. Ignoring it makes a fixture that
            // overflows its beaker look like it worked, and then the test that
            // reads it fails somewhere far less obvious.
            let overflow = container
                .solution
                .add(db.reagent(key), Units::whole(*amount));
            assert!(overflow.is_zero(), "{key} overflowed the test beaker");
        }
        app.world_mut()
            .spawn((container, Transform::from_xyz(1.0, 1.0, 0.0)))
            .id()
    }

    fn contents_of(app: &App, container: Entity) -> Solution {
        app.world()
            .get::<Container>(container)
            .unwrap()
            .solution
            .clone()
    }

    #[test]
    fn a_syringe_draws_up_to_its_capacity_and_no_further() {
        let mut app = test_app();
        let (_, syringe) = chemist_holding(&mut app, ContainerKind::Syringe, "water", 0);
        let beaker = bench_beaker(&mut app, &[("dylovene", 40)]);
        let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");

        apply(&mut app, Some(beaker));

        assert_eq!(
            contents_of(&app, syringe).volume_of(dylovene),
            Units::whole(15),
            "a syringe holds 15u"
        );
        assert_eq!(
            contents_of(&app, beaker).volume_of(dylovene),
            Units::whole(25),
            "and the rest stays in the beaker"
        );
    }

    #[test]
    fn drawing_takes_a_proportional_sample_of_a_mixture() {
        let mut app = test_app();
        let (_, syringe) = chemist_holding(&mut app, ContainerKind::Syringe, "water", 0);
        // Two reagents that do not react with each other, 2:1, inside a
        // beaker's 50u.
        let beaker = bench_beaker(&mut app, &[("sugar", 30), ("iron", 15)]);

        apply(&mut app, Some(beaker));

        let drawn = contents_of(&app, syringe);
        let sugar = app.world().resource::<ChemDb>().reagent("sugar");
        let iron = app.world().resource::<ChemDb>().reagent("iron");
        assert_eq!(drawn.volume_of(sugar), Units::whole(10));
        assert_eq!(drawn.volume_of(iron), Units::whole(5));
    }

    #[test]
    fn drawing_from_an_empty_beaker_leaves_the_syringe_empty() {
        let mut app = test_app();
        let (_, syringe) = chemist_holding(&mut app, ContainerKind::Syringe, "water", 0);
        let beaker = bench_beaker(&mut app, &[]);

        apply(&mut app, Some(beaker));

        assert!(contents_of(&app, syringe).is_empty());
    }

    #[test]
    fn injecting_empties_the_syringe_straight_into_the_blood() {
        let mut app = test_app();
        let (player, syringe) = chemist_holding(&mut app, ContainerKind::Syringe, "dylovene", 15);
        let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");

        apply(&mut app, None);

        assert!(
            contents_of(&app, syringe).is_empty(),
            "the syringe is spent"
        );
        // Injection bypasses the stomach entirely: all 15u, immediately, which
        // is exactly what drinking does not do.
        assert_eq!(
            blood_of(&app, player).blood.volume_of(dylovene),
            Units::whole(15)
        );
        assert!(blood_of(&app, player).stomach.is_empty());
    }

    #[test]
    fn injecting_beats_drinking_to_the_same_dose() {
        let dose_in_blood = |kind: ContainerKind| {
            let mut app = test_app();
            let (player, _) = chemist_holding(&mut app, kind, "dylovene", 15);
            match kind {
                ContainerKind::Syringe => apply(&mut app, None),
                _ => consume(&mut app),
            }
            let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");
            blood_of(&app, player).blood.volume_of(dylovene)
        };

        assert!(
            dose_in_blood(ContainerKind::Syringe) > dose_in_blood(ContainerKind::Beaker),
            "the needle is the whole reason to make one"
        );
    }

    #[test]
    fn a_syringe_can_be_used_on_the_other_chemist() {
        let mut app = test_app();
        let (_, _) = chemist_holding(&mut app, ContainerKind::Syringe, "dylovene", 15);
        // A second chemist, face down.
        let patient = app
            .world_mut()
            .spawn((
                Chemist {
                    client: ClientId::Client(Entity::PLACEHOLDER),
                },
                Body::default(),
                Bloodstream::default(),
            ))
            .id();

        apply(&mut app, Some(patient));

        let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");
        assert_eq!(
            blood_of(&app, patient).blood.volume_of(dylovene),
            Units::whole(15),
            "reviving the other chemist is the best reason to have one in the room"
        );
    }

    #[test]
    fn a_beaker_pours_ten_units_into_another_container() {
        let mut app = test_app();
        let (player, source) = chemist_holding(&mut app, ContainerKind::Beaker, "dylovene", 40);
        let beaker = bench_beaker(&mut app, &[("plasma", 20)]);

        apply(&mut app, Some(beaker));

        assert!(
            blood_of(&app, player).is_empty(),
            "container-to-container pouring does not dose the holder"
        );
        assert_eq!(contents_of(&app, source).total_volume(), Units::whole(30));
        assert_eq!(contents_of(&app, beaker).total_volume(), Units::whole(30));
        let sounds: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<EmitWorldSfx>>()
            .drain()
            .collect();
        assert_eq!(sounds.len(), 1);
        assert_eq!(
            sounds[0],
            EmitWorldSfx::new(Sfx::DirectPour, Vec3::new(1.0, 1.0, 0.0))
        );
    }

    #[test]
    fn a_beaker_splashes_ten_units_through_the_touch_route() {
        let mut app = test_app();
        let (actor, source) = chemist_holding(&mut app, ContainerKind::Beaker, "dylovene", 40);
        let target = app
            .world_mut()
            .spawn((Body::default(), Bloodstream::default()))
            .id();

        apply(&mut app, Some(target));

        let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");
        assert_eq!(contents_of(&app, source).total_volume(), Units::whole(30));
        assert_eq!(
            blood_of(&app, target).blood.volume_of(dylovene),
            Units::from_f64(1.5),
            "touch absorbs fifteen percent of the ten-unit splash",
        );
        let records: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ChemicalExposure>>()
            .drain()
            .collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].actor, Some(actor));
        assert_eq!(records[0].target, target);
        assert_eq!(records[0].route, Route::Touched);
        assert_eq!(records[0].source, ExposureSource::Direct);
    }

    #[test]
    fn a_clean_matching_active_order_is_authorized_even_when_the_drug_is_harsh() {
        let mut app = test_app();
        let (_, _) = chemist_holding(&mut app, ContainerKind::Beaker, "arithrazine", 10);
        let arithrazine = app.world().resource::<ChemDb>().reagent("arithrazine");
        let target = app
            .world_mut()
            .spawn((
                Body::default(),
                Bloodstream::default(),
                crate::orders::Order {
                    reagent: arithrazine,
                    specific: true,
                    amount: Units::whole(10),
                    plea: String::new(),
                    patience: 30.0,
                    waited: 0.0,
                },
            ))
            .id();

        apply(&mut app, Some(target));

        let records: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ChemicalExposure>>()
            .drain()
            .collect();
        assert_eq!(records.len(), 1);
        assert!(records[0].harmful, "arithrazine carries a brute cost");
        assert!(records[0].authorized, "the patient explicitly requested it");
    }

    #[test]
    fn helpful_crisis_care_is_authorized_without_matching_the_exact_order() {
        let mut app = test_app();
        let (_, _) = chemist_holding(&mut app, ContainerKind::Beaker, "bicaridine", 10);
        let mut injured = Body::default();
        injured.0.damage.brute = Units::whole(12);
        let target = app
            .world_mut()
            .spawn((
                injured,
                Bloodstream::default(),
                crate::crisis::CrisisResponse {
                    responders: vec!["Medical".to_string()],
                },
            ))
            .id();

        apply(&mut app, Some(target));

        let records: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ChemicalExposure>>()
            .drain()
            .collect();
        assert_eq!(records.len(), 1);
        assert!(records[0].helpful);
        assert!(records[0].authorized);
    }

    #[test]
    fn pouring_without_an_entity_target_makes_a_puddle() {
        let mut app = test_app();
        let (_, source) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 25);
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ApplyHeldRequested {
                target: None,
                point: Some(Vec3::new(1.0, 0.0, 0.0)),
            },
        });

        app.update();

        assert_eq!(contents_of(&app, source).total_volume(), Units::whole(15));
        let puddles = app
            .world_mut()
            .query::<(&crate::chem_world::ChemicalPuddle, &Transform)>()
            .iter(app.world())
            .count();
        assert_eq!(puddles, 1);
    }

    #[test]
    fn a_forged_distant_splash_is_rejected_before_liquid_or_body_mutation() {
        let mut app = test_app();
        let (_, source) = chemist_holding(&mut app, ContainerKind::Beaker, "dylovene", 40);
        let target = app
            .world_mut()
            .spawn((
                Body::default(),
                Bloodstream::default(),
                Transform::from_xyz(20.0, crate::player::EYE_HEIGHT, 0.0),
            ))
            .id();

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ApplyHeldRequested {
                target: Some(target),
                point: Some(Vec3::new(20.0, crate::player::EYE_HEIGHT, 0.0)),
            },
        });
        app.update();

        assert_eq!(contents_of(&app, source).total_volume(), Units::whole(40));
        assert!(
            app.world_mut()
                .resource_mut::<Messages<EmitWorldSfx>>()
                .drain()
                .next()
                .is_none(),
            "a rejected remote splash must remain silent"
        );
        assert!(blood_of(&app, target).is_empty());
        let records: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ChemicalExposure>>()
            .drain()
            .collect();
        assert!(
            records.is_empty(),
            "a refused splash has no exposure record"
        );
    }

    #[test]
    fn forged_distant_and_non_finite_floor_pours_do_not_spill_or_consume() {
        for point in [
            Vec3::new(20.0, 0.0, 0.0),
            Vec3::new(f32::NAN, 0.0, 0.0),
            Vec3::new(0.0, f32::INFINITY, 0.0),
        ] {
            let mut app = test_app();
            let (_, source) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 25);
            app.world_mut().write_message(FromClient {
                client_id: ClientId::Server,
                message: ApplyHeldRequested {
                    target: None,
                    point: Some(point),
                },
            });

            app.update();

            assert_eq!(
                contents_of(&app, source).total_volume(),
                Units::whole(25),
                "invalid point {point:?} consumed liquid"
            );
            let puddles = app
                .world_mut()
                .query::<&crate::chem_world::ChemicalPuddle>()
                .iter(app.world())
                .count();
            assert_eq!(puddles, 0, "invalid point {point:?} spawned a puddle");
        }
    }

    #[test]
    fn a_solid_wall_blocks_a_forged_splash_inside_the_distance_limit() {
        let mut app = test_app();
        let (_, source) = chemist_holding(&mut app, ContainerKind::Beaker, "dylovene", 40);
        let target = app
            .world_mut()
            .spawn((
                Body::default(),
                Bloodstream::default(),
                Transform::from_xyz(2.5, crate::player::EYE_HEIGHT, 0.0),
            ))
            .id();
        app.world_mut().spawn((
            Transform::from_xyz(1.25, crate::player::EYE_HEIGHT, 0.0),
            crate::lab::Solid {
                half_extents: Vec3::new(0.1, 1.0, 1.0),
            },
        ));

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ApplyHeldRequested {
                target: Some(target),
                point: None,
            },
        });
        app.update();

        assert_eq!(contents_of(&app, source).total_volume(), Units::whole(40));
        assert!(blood_of(&app, target).is_empty());
    }

    #[test]
    fn chemical_incapacitation_drops_the_hand_and_releases_the_panel() {
        let mut app = test_app();
        app.add_systems(Update, handle_chemical_incapacitation);
        let (player, beaker) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 10);
        let machine = app
            .world_mut()
            .spawn(crate::machines::Machine::new(
                crate::machines::MachineKind::MixingChamber,
            ))
            .id();
        app.world_mut().entity_mut(player).insert((
            Transform::default(),
            crate::interaction::InteractionMode::UsingMachine(machine),
        ));
        app.world_mut()
            .get_mut::<crate::machines::Machine>(machine)
            .unwrap()
            .in_use_by = Some(player);
        app.world_mut()
            .get_mut::<Bloodstream>(player)
            .unwrap()
            .0
            .add_status(chem_sim::StatusKind::Sedated, 20.0, 2.0);

        app.update();

        assert!(app.world().get::<ChemicallyIncapacitated>(player).is_some());
        assert!(app.world().get::<HeldBy>(beaker).is_none());
        assert_eq!(
            app.world()
                .get::<crate::machines::Machine>(machine)
                .unwrap()
                .in_use_by,
            None
        );
        assert_eq!(
            *app.world()
                .get::<crate::interaction::InteractionMode>(player)
                .unwrap(),
            crate::interaction::InteractionMode::Roaming
        );

        app.world_mut()
            .get_mut::<Bloodstream>(player)
            .unwrap()
            .0
            .counter_status(chem_sim::StatusKind::Sedated, 100.0, 100.0);
        app.update();
        assert!(app.world().get::<ChemicallyIncapacitated>(player).is_none());
    }

    // -----------------------------------------------------------------------
    // Collapse
    // -----------------------------------------------------------------------

    /// The collapse systems need the shift tally and the radio, which the
    /// leaner app above deliberately does without.
    fn collapse_app() -> App {
        let mut app = test_app();
        app.init_resource::<crate::orders::Shift>()
            .init_resource::<crate::radio::RadioLog>()
            .add_systems(Update, (handle_collapse, run_medbay_retrieval).chain());
        app
    }

    fn flatten(app: &mut App, player: Entity) {
        app.world_mut()
            .get_mut::<Body>(player)
            .unwrap()
            .0
            .apply(chem_sim::Damage::of(DamageKind::Brute, Units::whole(120)));
        app.update();
    }

    #[test]
    fn going_down_drops_what_you_were_holding() {
        let mut app = collapse_app();
        let (player, beaker) = chemist_holding(&mut app, ContainerKind::Beaker, "dylovene", 30);
        app.world_mut()
            .entity_mut(player)
            .insert(Transform::default());

        flatten(&mut app, player);

        assert!(
            app.world().get::<HeldBy>(beaker).is_none(),
            "an unconscious chemist is not still holding their beaker"
        );
    }

    #[test]
    fn going_down_releases_the_machine_you_had_open() {
        let mut app = collapse_app();
        let (player, _) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 0);
        let machine = app
            .world_mut()
            .spawn(crate::machines::Machine::new(
                crate::machines::MachineKind::ChemMaster5000,
            ))
            .id();
        app.world_mut().entity_mut(player).insert((
            Transform::default(),
            crate::interaction::InteractionMode::UsingMachine(machine),
        ));
        app.world_mut()
            .get_mut::<crate::machines::Machine>(machine)
            .unwrap()
            .in_use_by = Some(player);

        flatten(&mut app, player);

        assert_eq!(
            app.world()
                .get::<crate::machines::Machine>(machine)
                .unwrap()
                .in_use_by,
            None,
            "otherwise the machine stays claimed and the other chemist is locked out"
        );
        assert_eq!(
            *app.world()
                .get::<crate::interaction::InteractionMode>(player)
                .unwrap(),
            crate::interaction::InteractionMode::Roaming
        );
    }

    #[test]
    fn going_down_costs_standing_once() {
        let mut app = collapse_app();
        let (player, _) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 0);
        app.world_mut()
            .entity_mut(player)
            .insert(Transform::default());

        flatten(&mut app, player);
        let after = app
            .world()
            .resource::<crate::orders::Shift>()
            .standing(crate::orders::Department::Medical);
        assert_eq!(after, COLLAPSE_PENALTY);

        // Still down several frames later; it must not keep charging.
        app.update();
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::orders::Shift>()
                .standing(crate::orders::Department::Medical),
            after,
            "the penalty is for going down, not for staying down"
        );
    }

    #[test]
    fn medical_eventually_comes_and_gets_you() {
        // The anti-softlock: alone and unable to walk, drink or reach a
        // machine, a chemist would otherwise be stuck for the rest of the game.
        let mut app = collapse_app();
        let (player, _) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 0);
        app.world_mut()
            .entity_mut(player)
            .insert(Transform::default());
        inject(&mut app, player, "plasma", 40);

        flatten(&mut app, player);
        assert!(app.world().get::<Body>(player).unwrap().0.collapsed);

        for _ in 0..((MEDBAY_SECONDS / 0.5).ceil() as u32 + 2) {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(Duration::from_secs_f32(0.5));
            app.update();
        }

        let body = app.world().get::<Body>(player).unwrap();
        assert!(!body.0.collapsed, "back on their feet");
        assert!(body.0.total() < chem_sim::RECOVER, "and staying that way");
        assert!(
            blood_of(&app, player).is_empty(),
            "flushed, or whatever put them down would do it again immediately"
        );
    }

    #[test]
    fn a_syringe_beats_the_medbay_clock() {
        // The co-op case, and the best reason to have a second chemist.
        let mut app = collapse_app();
        let (player, _) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 0);
        app.world_mut()
            .entity_mut(player)
            .insert(Transform::default());

        flatten(&mut app, player);
        assert!(app.world().get::<MedbayRetrieval>(player).is_some());

        // Somebody gets to them first.
        app.world_mut()
            .get_mut::<Body>(player)
            .unwrap()
            .0
            .heal(chem_sim::Damage::of(DamageKind::Brute, Units::whole(60)));
        app.update();

        assert!(
            app.world().get::<MedbayRetrieval>(player).is_none(),
            "medical stand down once someone else has handled it"
        );
    }

    #[test]
    fn an_unpoisoned_body_is_left_alone_entirely() {
        let mut app = test_app();
        let (player, _) = chemist_holding(&mut app, ContainerKind::Beaker, "water", 0);

        advance(&mut app, 10.0);

        assert!(
            !app.world().get::<Body>(player).unwrap().0.is_hurt(),
            "nothing to metabolise means nothing happens"
        );
        assert_eq!(
            app.world()
                .get::<Body>(player)
                .unwrap()
                .0
                .fraction(DamageKind::Toxin),
            0.0
        );
    }
}
