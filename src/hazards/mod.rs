//! What chemistry does to the room.
//!
//! `ReactionEffect::Smoke` and `::Explosion` have existed in the simulation
//! since M1 and nothing ever read them. This is the consumer: it turns a line
//! in a reaction's data file into a cloud hanging in the lab or a bang that
//! takes the glassware and a chunk out of whoever was holding it.
//!
//! Everything here is server-authoritative. Clouds replicate as entities, so no
//! `*Sync` message is needed; the *feel* of being caught in a blast is a
//! separate presentation-only message, because a screen flash is not state.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use chem_sim::body::{blast_radius, explosion_damage};
use chem_sim::{ReactionEffect, Route, Solution, Units};
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::net::is_authority;

use crate::body::{Bloodstream, Body, MetabolismClock};
use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy};
use crate::machines::ReactionsFired;
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

pub struct HazardPlugin;

impl Plugin for HazardPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<HazardScript>::new(&["hazards.ron"]))
            .add_server_message::<HazardFelt>(Channel::Ordered)
            .init_resource::<IncidentSchedule>()
            .init_resource::<IncidentGrace>()
            .add_systems(Startup, start_loading_hazards)
            .add_systems(
                Update,
                (
                    (spawn_hazards, expose_to_smoke, fade_smoke)
                        .chain()
                        .run_if(is_authority),
                    // A grace period on session start, not a phase: there is
                    // no "prep" any more for this to be the safe alternative
                    // to, just a short window so a brand new chemist is not
                    // hit by an incident before they have found the door.
                    (schedule_incidents, run_incidents)
                        .chain()
                        .run_if(is_authority),
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
    pub origin: (f32, f32, f32),
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

        commands.spawn((
            ActiveHazard {
                radius: def.radius,
                remaining: def.duration,
                intensity: def.intensity,
                color: def.color,
            },
            Transform::from_xyz(def.origin.0, def.origin.1, def.origin.2),
            Visibility::default(),
            Replicated,
            crate::until_we_leave_the_lab(),
        ));
        radio.push(RadioEntry {
            channel: "LAB".to_string(),
            text: def.onset.clone(),
            good: false,
        });
        info!("hazard: {} for {}s", def.id, def.duration);
        schedule.next_in = Some(rng.random_range(script.gap_seconds.0..=script.gap_seconds.1));
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
    radio.push(RadioEntry {
        channel: "ENG".to_string(),
        text: def.warning.clone(),
        good: false,
    });
    schedule.warning_in = Some(script.warning_seconds);
    schedule.pending = Some(def.clone());
}

/// Irradiates whoever is standing in an active hazard, and expires it.
fn run_incidents(
    mut commands: Commands,
    time: Res<Time>,
    clock: Res<MetabolismClock>,
    mut felt: MessageWriter<ToClients<HazardFelt>>,
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

/// The room position of a container, following its holder if it is being
/// carried.
///
/// A held container's own `Transform` is only meaningful on the client holding
/// it — the server never moves it, because `carry_held_containers` is a local
/// presentation trick. So a beaker that goes off in your hand has to be located
/// by finding the hand.
fn container_position(
    container: Entity,
    transforms: &Query<&Transform>,
    held: &Query<&HeldBy>,
) -> Option<Vec3> {
    let holder = held.get(container).ok().map(|holder| holder.0);
    match holder {
        Some(player) => transforms.get(player).ok().map(|t| t.translation),
        None => transforms.get(container).ok().map(|t| t.translation),
    }
}

/// Turns reported effects into things in the room.
#[allow(clippy::too_many_arguments)]
fn spawn_hazards(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut reports: MessageReader<ReactionsFired>,
    mut felt: MessageWriter<ToClients<HazardFelt>>,
    mut radio: ResMut<RadioLog>,
    transforms: Query<&Transform>,
    held: Query<&HeldBy>,
    mut containers: Query<&mut Container>,
    mut bodies: Query<(Entity, &mut Body, &crate::player::Chemist)>,
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
        for effect in &report.effects {
            match effect {
                ReactionEffect::Smoke(spread) => radius = radius.max(*spread),
                ReactionEffect::Explosion(strength) => power += strength,
                ReactionEffect::Heat(_) => {}
            }
        }

        let Some(origin) = container_position(report.container, &transforms, &held) else {
            continue;
        };

        if radius > 0.0 {
            // The cloud takes a share of the batch with it.
            let payload = containers
                .get_mut(report.container)
                .map(|mut container| container.mutate(&db, |s| s.split(SMOKE_PAYLOAD)).0)
                .unwrap_or_else(|_| Solution::unbounded());

            commands.spawn((
                SmokeCloud {
                    radius,
                    remaining: SMOKE_LIFETIME,
                },
                SmokePayload(payload),
                Transform::from_translation(origin),
                Visibility::default(),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
            radio.push(RadioEntry {
                channel: "LAB".to_string(),
                text: "Something in the chem lab just started venting.".to_string(),
                good: false,
            });
        }

        if power > 0.0 {
            // The glass and everything in it are gone.
            if let Ok(mut entity) = commands.get_entity(report.container) {
                entity.despawn();
            }

            let reach = blast_radius(power);
            for (entity, mut body, chemist) in &mut bodies {
                let Ok(transform) = transforms.get(entity) else {
                    continue;
                };
                let distance = transform.translation.distance(origin);
                let damage = explosion_damage(power, distance);
                if damage.total().is_zero() {
                    continue;
                }
                body.0.apply(damage);
                felt.write(ToClients {
                    targets: SendTargets::Single(chemist.client),
                    message: HazardFelt {
                        kind: HazardKind::Blast,
                        strength: (1.0 - distance / reach).clamp(0.0, 1.0),
                    },
                });
            }
            radio.push(RadioEntry {
                channel: "LAB".to_string(),
                text: "Was that a bang? Chemistry, report.".to_string(),
                good: false,
            });
        }
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
    mut clouds: Query<(&SmokeCloud, &Transform, &mut SmokePayload)>,
    mut bodies: Query<(
        &Transform,
        &mut Body,
        &mut Bloodstream,
        &crate::player::Chemist,
    )>,
    mut crew_bodies: Query<
        (&Transform, &mut Body, &mut Bloodstream),
        (
            With<crate::crew::CrewMember>,
            Without<crate::player::Chemist>,
        ),
    >,
) {
    // The clock is ticked by `run_metabolism`, which is chained after this;
    // reading `just_finished` here means exposure lands on the same beat.
    let _ = time;
    if !clock.0.just_finished() {
        return;
    }

    for (cloud, cloud_transform, mut payload) in &mut clouds {
        if payload.0.is_empty() {
            continue;
        }
        for (body_transform, mut body, mut blood, chemist) in &mut bodies {
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
            // Touch: 15% absorbed, half contact damage. Standing in a cloud
            // should be survivable and unpleasant, not instantly fatal.
            blood.0.receive(&mut dose, Route::Touched, &mut body.0, &db);
            felt.write(ToClients {
                targets: SendTargets::Single(chemist.client),
                message: HazardFelt {
                    kind: HazardKind::Smoke,
                    strength: 0.5,
                },
            });
        }
        for (body_transform, mut body, mut blood) in &mut crew_bodies {
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
            blood.0.receive(&mut dose, Route::Touched, &mut body.0, &db);
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
/// is: the rad leak is a 4.5m sphere centred on the dispenser, so without this
/// an incident would make the dispenser — and half the hall behind it —
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
            .add_message::<ToClients<HazardFelt>>()
            .add_systems(Update, (spawn_hazards, expose_to_smoke, fade_smoke).chain());
        app
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
        for incident in &script.incidents {
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
