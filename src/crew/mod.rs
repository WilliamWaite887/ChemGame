//! Crew who come to the lab to collect what they ordered.
//!
//! Movement is a fixed waypoint walk rather than pathfinding. The route from
//! the door to the counter is a straight, permanently clear corridor, so
//! anything cleverer would be machinery with nothing to solve.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::body::{Body, Bloodstream, COLLAPSE_PENALTY};
use crate::lab::{COUNTER_SPOT, DOOR_MAX_X, DOOR_MIN_X};
use crate::net::is_authority;
use crate::orders::{Department, Shift};
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// Walking pace, metres per second.
const WALK_SPEED: f32 = 2.1;
/// Close enough to count as arrived.
const ARRIVE_EPSILON: f32 = 0.12;

pub struct CrewPlugin;

impl Plugin for CrewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Departments>()
            .add_systems(OnEnter(AppState::Playing), load_crew_assets)
            .add_systems(
                Update,
                (
                    // The walk is simulated once, on the server; clients
                    // receive the resulting Transform. `handle_crew_collapse`
                    // reads `Body`, which only the server ever mutates
                    // (metabolism, smoke, a delivered dose), so it belongs on
                    // the same side.
                    (
                        start_crew_at_their_department,
                        // Ambient crew decide where to be, then everyone walks.
                        populate_departments
                            .run_if(resource_exists_and_changed::<Departments>),
                        ambient_behaviour,
                        walk_route,
                        handle_crew_collapse,
                    )
                        .chain()
                        .run_if(is_authority),
                    // Runs everywhere: a crew member who arrived by
                    // replication needs a body drawing just as much as one
                    // spawned locally.
                    dress_crew,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Where each department's crew belong, from the map's `department_spot`
/// markers.
///
/// Empty in a build without the map, and every read here falls back — which is
/// what keeps crew arriving out of the patch of nothing south of the lobby
/// exactly as they always have. Keyed on the `role` that is already on every
/// [`CrewMember`], so a department needs no second roster to exist.
#[derive(Resource, Default)]
pub struct Departments {
    homes: std::collections::HashMap<String, Vec3>,
}

impl Departments {
    // Only the map backend has anywhere to put a department, so a plain build
    // fills this from nothing and never calls it outside tests.
    #[cfg_attr(not(feature = "trenchbroom"), allow(dead_code))]
    pub fn set(&mut self, department: String, at: Vec3) {
        self.homes.insert(department, at);
    }

    /// Where someone of this role lives, if the station has somewhere for them.
    pub fn home(&self, role: &str) -> Option<Vec3> {
        self.homes.get(role).copied()
    }

    /// Somewhere on the station to go next — another department if there is
    /// one, otherwise their own. Keeps idle crew crossing the corridor instead
    /// of pacing a single room.
    pub fn somewhere_else(&self, role: &str) -> Option<Vec3> {
        if self.homes.is_empty() {
            return None;
        }
        let elsewhere: Vec<&Vec3> = self
            .homes
            .iter()
            .filter(|(department, _)| department.as_str() != role)
            .map(|(_, at)| at)
            .collect();

        // Two thirds of the time go visiting, otherwise stay in your own
        // department — a station where everybody is always somewhere else reads
        // as odd as one where nobody moves.
        if elsewhere.is_empty() || rand::random_range(0..3) == 0 {
            return self.home(role);
        }
        elsewhere
            .get(rand::random_range(0..elsewhere.len()))
            .map(|at| **at)
    }
}

/// A crew member as written in `assets/data/station.crew.ron`.
#[derive(Clone, Debug, Deserialize)]
pub struct CrewDef {
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
}

#[derive(Component, Clone, Serialize, Deserialize)]
pub struct CrewMember {
    pub name: String,
    pub role: String,
}

/// Where a crew member is in their visit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CrewPhase {
    Arriving,
    Waiting,
    Leaving,
}

#[derive(Component)]
pub struct CrewRoute {
    waypoints: Vec<Vec3>,
    index: usize,
    pub phase: CrewPhase,
    /// Where they are headed, before anyone has worked out how to get there.
    ///
    /// Destinations are set from `orders` and `addiction`, which have no
    /// business knowing about navigation, so the route records the *goal* and
    /// [`walk_route`] turns it into waypoints once — it is the system that can
    /// see the nav graph.
    pending: Option<Vec3>,
}

impl CrewRoute {
    /// The walk in: to their place at the counter.
    pub fn arrival(lane: f32) -> Self {
        CrewRoute {
            waypoints: Vec::new(),
            index: 0,
            phase: CrewPhase::Arriving,
            pending: Some(Vec3::new(COUNTER_SPOT.x + lane, 0.0, COUNTER_SPOT.z)),
        }
    }

    /// Sends them back out to the station.
    pub fn leave(&mut self) {
        self.waypoints.clear();
        self.index = 0;
        self.phase = CrewPhase::Leaving;
        self.pending = Some(Vec3::new(door_x(), 0.0, spawn_z()));
    }
}

fn door_x() -> f32 {
    (DOOR_MIN_X + DOOR_MAX_X) * 0.5
}

/// Far enough beyond the lobby's south wall to be out of sight, so crew are
/// never seen popping into existence.
fn spawn_z() -> f32 {
    crate::lab::ROOMS[crate::lab::LOBBY].max_z + 2.5
}

/// Shared meshes for crew bodies.
///
/// No materials here since M12 — every crew member's uniform and skin are a
/// fresh [`StandardMaterial`] per instance (see [`dress_crew`]), for the same
/// reason `player::ChemistAssets` dropped its shared `coat`/`skin` handles:
/// tinting a status onto a shared handle would tint everyone holding it.
#[derive(Resource)]
pub struct CrewAssets {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
}

fn load_crew_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(CrewAssets {
        body: meshes.add(Capsule3d::new(0.28, 0.85)),
        head: meshes.add(Sphere::new(0.19)),
    });
}

/// A body part, and the crew member it belongs to.
///
/// The M12 twin of `player::ChemistBody`, and needed for the same reason:
/// `fx::animate_crew_body` has to wobble/tint a *child* mesh, never the root
/// `Transform`, because the root is replicated and authoritative —
/// `CrewRoute`'s movement and every interaction raycast depend on it staying
/// exactly where the server put it. `rest`/`base_color` are the part's
/// un-animated reference point, recomputed from every frame rather than
/// accumulated onto, so nothing drifts.
#[derive(Component)]
pub(crate) struct CrewBody {
    pub(crate) crew: Entity,
    pub(crate) rest: Vec3,
    pub(crate) base_color: Color,
}

/// Spawns a crew member outside the door, walking in.
///
/// No mesh: who is visiting is shared state and replicates, what they look
/// like is derived from the roster both ends already loaded. See
/// [`dress_crew`].
///
/// Carries a `Body`/`Bloodstream` since M12 — the same components a chemist
/// has, replicated the same way (`register_replication` marks the *type*,
/// not the entity, so inserting them here is all that is needed). This is
/// what lets a delivery or a smoke cloud actually land on the person it was
/// meant for, instead of a crew member being immune to their own chemistry by
/// construction.
pub fn spawn_crew_member(commands: &mut Commands, def: &CrewDef, lane: f32) -> Entity {
    let position = Vec3::new(door_x(), 0.93, spawn_z());
    commands
        .spawn((
            CrewMember {
                name: def.name.clone(),
                role: def.role.clone(),
            },
            CrewRoute::arrival(lane),
            Transform::from_translation(position),
            Body::default(),
            Bloodstream::default(),
            // The route stays server-side; clients see the resulting Transform.
            bevy_replicon::prelude::Replicated,
            crate::until_we_leave_the_lab(),
        ))
        .id()
}

/// Gives a crew member their body, head and uniform.
///
/// The uniform colour is looked up from the roster by name rather than sent,
/// because both ends load `station.crew.ron` anyway. A visitor whose name is
/// not in the roster still gets a body, in grey — an unrecognised name should
/// read as an oddity at the counter, not an invisible person holding an order.
///
/// Both meshes are children (`CrewBody`), not — as the body mesh used to be —
/// inserted straight onto the root. A wobble applied to the root would
/// corrupt the replicated, authoritative `Transform` `CrewRoute` and every
/// interaction raycast depend on; a child can be animated freely.
fn dress_crew(
    mut commands: Commands,
    assets: Option<Res<CrewAssets>>,
    station: Option<Res<crate::orders::StationData>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    crew: Query<(Entity, &CrewMember), Added<CrewMember>>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, member) in &crew {
        let color = station
            .as_ref()
            .and_then(|station| {
                station
                    .crew
                    .iter()
                    .find(|def| def.name == member.name)
                    .map(|def| def.color)
            })
            .unwrap_or([0.55, 0.55, 0.58]);

        let [r, g, b] = color;
        let uniform_color = Color::srgb(r, g, b);

        // A replicated crew member arrives without `Visibility` — presentation
        // is not on the wire — and a parent with none cannot propagate it to
        // the children below. Mirrors `player::dress_chemists`'s identical fix.
        commands
            .entity(entity)
            .insert_if_new(Visibility::default());

        let body_rest = Vec3::ZERO;
        commands.spawn((
            Mesh3d(assets.body.clone()),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: uniform_color,
                perceptual_roughness: 0.75,
                ..default()
            })),
            Transform::from_translation(body_rest),
            Visibility::default(),
            CrewBody {
                crew: entity,
                rest: body_rest,
                base_color: uniform_color,
            },
            ChildOf(entity),
        ));

        let head_rest = Vec3::new(0.0, 0.62, 0.0);
        commands.spawn((
            Mesh3d(assets.head.clone()),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: crate::player::SKIN_COLOR,
                perceptual_roughness: 0.8,
                ..default()
            })),
            Transform::from_translation(head_rest),
            Visibility::default(),
            CrewBody {
                crew: entity,
                rest: head_rest,
                base_color: crate::player::SKIN_COLOR,
            },
            ChildOf(entity),
        ));
    }
}

/// The crew equivalent of `body::handle_collapse` — no dropped item (a crew
/// member never holds anything of the player's), no `MedbayRetrieval` (they
/// are a visitor, not staff on shift): they simply leave, early and rattled,
/// the same way the security officer's sweep already sends a crew entity back
/// out via `.leave()`.
///
/// Dinged against Medical, same as a chemist's own collapse — it is a medical
/// mishap regardless of whose lab it happened in.
fn handle_crew_collapse(
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    mut crew: Query<(&Body, &CrewMember, &mut CrewRoute), Changed<Body>>,
) {
    for (body, member, mut route) in &mut crew {
        if !body.0.collapsed || route.phase == CrewPhase::Leaving {
            continue;
        }
        route.leave();
        shift.adjust(Department::Medical, COLLAPSE_PENALTY);
        radio.push(RadioEntry {
            channel: "MED".to_string(),
            text: format!(
                "{} just went down in the chem lab. Get them out of there.",
                member.name
            ),
            good: false,
        });
    }
}

/// Advances each crew member along their waypoints, and despawns them once
/// they are back outside.
/// A crew member who lives on the station rather than visiting the lab.
///
/// Ambient crew never queue and never carry an [`Order`](crate::orders::Order):
/// they walk between their department and the corridor, and they are what makes
/// the place feel inhabited rather than a shop with a door. In every other
/// respect they are ordinary [`CrewMember`]s — `dress_crew` draws them, smoke
/// reaches them, they can get hooked — which is the point. Reacting to a crisis
/// is only interesting if the people reacting were already there.
///
/// Every query that hunts for a customer at the counter excludes them by this
/// marker; see the `Without<Ambient>` filters in `orders`, `produce`, `quack`
/// and `rogue_security`.
#[derive(Component)]
pub struct Ambient {
    /// Seconds to stand still before picking somewhere new to be.
    dwell: f32,
}

/// Query filter for crew who are *visiting* the lab, excluding the residents
/// who simply live on the station.
///
/// Every gate that counts how busy the counter is has to use this. Counting
/// bare [`CrewMember`]s instead includes the residents — already more than
/// `orders::max_active_cap` on its own — and orders stop arriving entirely,
/// with no error and nothing in the log. That is exactly how it broke the first
/// time.
pub type NotResident = Without<Ambient>;

/// How long an idle crew member lingers before moving on.
const DWELL_SECONDS: (f32, f32) = (4.0, 11.0);

/// Gives every department someone to be in it.
///
/// Runs when [`Departments`] is filled, which only ever happens under the map —
/// a build without one has nowhere for anybody to live, and gets the old
/// visit-only crew exactly as before.
fn populate_departments(
    mut commands: Commands,
    departments: Res<Departments>,
    station: Option<Res<crate::orders::StationData>>,
    resident: Query<&CrewMember, With<Ambient>>,
) {
    let Some(station) = station else {
        return;
    };

    for def in &station.crew {
        let Some(home) = departments.home(&def.role) else {
            continue;
        };
        // One resident per person on the roster, not per department: the roster
        // is already the cast, and spawning a second Dr. Vance would put the
        // same named individual in two places.
        if resident.iter().any(|member| member.name == def.name) {
            continue;
        }

        let crew = spawn_crew_member(&mut commands, def, 0.0);
        commands.entity(crew).insert(Ambient {
            dwell: rand::random_range(DWELL_SECONDS.0..=DWELL_SECONDS.1),
        });
        // Overwrite the arrival route: they are not coming to the counter.
        commands.entity(crew).insert(CrewRoute {
            waypoints: Vec::new(),
            index: 0,
            phase: CrewPhase::Arriving,
            pending: Some(home),
        });
    }
}

/// Sends idle crew somewhere new, and sends everyone to their post when a
/// casualty turns up.
///
/// The rule is the whole of the station's mood in one branch: if your
/// department could do something about it you go *to* it, and if it could not
/// you get out of the way. Nobody runs for the escape pod — that is the end of
/// a campaign, not a bad afternoon.
fn ambient_behaviour(
    time: Res<Time>,
    departments: Res<Departments>,
    crisis: Query<(&Transform, &crate::crisis::CrisisResponse)>,
    mut residents: Query<(&CrewMember, &mut Ambient, &mut CrewRoute, &Transform)>,
) {
    let emergency = crisis.iter().next();

    for (member, mut ambient, mut route, transform) in &mut residents {
        // Mid-walk. Leave them to it.
        if route.pending.is_some() || route.index < route.waypoints.len() {
            continue;
        }

        if let Some((casualty, response)) = emergency {
            let wanted = response
                .responders
                .iter()
                .any(|role| role == &member.role);
            let post = if wanted {
                Some(casualty.translation)
            } else {
                departments.home(&member.role)
            };

            if let Some(post) = post {
                // Only walk if they are not already standing there, or they
                // shuffle on the spot for the whole crisis.
                let flat = Vec3::new(post.x, transform.translation.y, post.z);
                if transform.translation.distance(flat) > 1.5 {
                    route.pending = Some(post);
                    route.phase = CrewPhase::Arriving;
                }
                continue;
            }
        }

        ambient.dwell -= time.delta_secs();
        if ambient.dwell > 0.0 {
            continue;
        }
        ambient.dwell = rand::random_range(DWELL_SECONDS.0..=DWELL_SECONDS.1);

        // Somewhere else on the station: another department, or their own.
        if let Some(next) = departments.somewhere_else(&member.role) {
            route.pending = Some(next);
            route.phase = CrewPhase::Arriving;
        }
    }
}

/// Puts a newly spawned crew member at their department, if the station has one
/// for them.
///
/// A system rather than an argument to [`spawn_crew_member`], which has fourteen
/// call sites across thirteen modules — none of which should have to learn about
/// the station's layout to ask for a customer. They spawn off-screen either way,
/// so moving them the same frame is invisible.
fn start_crew_at_their_department(
    departments: Res<Departments>,
    mut arriving: Query<(&CrewMember, &mut Transform), Added<CrewRoute>>,
) {
    for (member, mut transform) in &mut arriving {
        if let Some(home) = departments.home(&member.role) {
            transform.translation.x = home.x;
            transform.translation.z = home.z;
        }
    }
}

fn walk_route(
    mut commands: Commands,
    time: Res<Time>,
    nav: Res<crate::nav::NavGraph>,
    departments: Res<Departments>,
    mut crew: Query<(Entity, &mut Transform, &mut CrewRoute, Option<&CrewMember>)>,
) {
    for (entity, mut transform, mut route, member) in &mut crew {
        // Turn a new destination into a path, once, the frame it is set.
        if let Some(goal) = route.pending.take() {
            // Someone leaving heads for their own department when the station
            // has one, rather than the generic spot outside the lobby door.
            let goal = match (route.phase, member) {
                (CrewPhase::Leaving, Some(member)) => {
                    departments.home(&member.role).unwrap_or(goal)
                }
                _ => goal,
            };
            // Straight there if navigation has nothing to say. The graph is
            // empty for a frame or two while the map loads, and a crew member
            // must not stand in the doorway waiting for it — which is also what
            // kept this working before there was any pathfinding at all.
            route.waypoints = nav
                .path(transform.translation, goal)
                .unwrap_or_else(|| vec![goal]);
            route.index = 0;
        }

        let Some(target) = route.waypoints.get(route.index).copied() else {
            // Route finished. Arriving crew wait; leaving crew are done.
            if route.phase == CrewPhase::Leaving {
                commands.entity(entity).despawn();
            } else if route.phase == CrewPhase::Arriving {
                route.phase = CrewPhase::Waiting;
            }
            continue;
        };

        let flat_target = Vec3::new(target.x, transform.translation.y, target.z);
        let to_target = flat_target - transform.translation;
        if to_target.length() <= ARRIVE_EPSILON {
            route.index += 1;
            continue;
        }

        let step = to_target.normalize() * WALK_SPEED * time.delta_secs();
        transform.translation += step;
        // Face the direction of travel so they read as people rather than
        // sliding props.
        transform.rotation = Quat::from_rotation_y(to_target.x.atan2(to_target.z));
    }
}

#[cfg(test)]
mod tests {
    //! Headless: a body goes down, a route and a standing change out.

    use super::*;
    use chem_sim::{Damage, DamageKind, Units};

    fn collapse_app() -> App {
        let mut app = App::new();
        app.init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .add_systems(Update, handle_crew_collapse);
        app
    }

    /// A crew member walking a real lab, with the nav graph they route on.
    fn walking_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Departments>()
            .insert_resource(crate::nav::NavGraph::build(
                &crate::lab::WalkableAreas::from_floor_plan(),
                crate::nav::NAV_RADIUS,
            ))
            .add_systems(
                Update,
                (start_crew_at_their_department, walk_route).chain(),
            );
        app
    }

    fn walker(app: &mut App, at: Vec3, route: CrewRoute) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: "Tester".into(),
                    role: "Medical".into(),
                },
                Transform::from_translation(at),
                route,
            ))
            .id()
    }

    fn tick(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    #[test]
    fn a_crew_member_walks_all_the_way_to_the_counter() {
        // The end-to-end of the whole navigation change: set a destination,
        // walk, arrive. Before pathfinding this was two hardcoded waypoints; the
        // observable behaviour must not have changed.
        let mut app = walking_app();
        let crew = walker(
            &mut app,
            Vec3::new(door_x(), 0.93, spawn_z()),
            CrewRoute::arrival(0.0),
        );

        for _ in 0..400 {
            tick(&mut app, 0.05);
            if app.world().get::<CrewRoute>(crew).unwrap().phase == CrewPhase::Waiting {
                break;
            }
        }

        let route = app.world().get::<CrewRoute>(crew).unwrap();
        assert_eq!(
            route.phase,
            CrewPhase::Waiting,
            "never finished the walk to the counter",
        );

        let at = app.world().get::<Transform>(crew).unwrap().translation;
        let counter = Vec3::new(COUNTER_SPOT.x, at.y, COUNTER_SPOT.z);
        assert!(
            at.distance(counter) < 0.5,
            "stopped {:.2}m short of the counter, at {at:?}",
            at.distance(counter),
        );
    }

    #[test]
    fn a_walk_across_the_suite_is_routed_rather_than_straight_through_walls() {
        // What pathfinding buys that the hardcoded route could not: someone
        // standing in the reaction bay is two rooms and two doorways from the
        // counter. A straight line there crosses three walls, so the route must
        // contain intermediate waypoints — and none of them may be the goal.
        let mut app = walking_app();
        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let crew = walker(
            &mut app,
            Vec3::new(from.x, 0.93, from.z),
            CrewRoute::arrival(0.0),
        );

        tick(&mut app, 0.01);

        let route = app.world().get::<CrewRoute>(crew).unwrap();
        assert!(
            route.waypoints.len() >= 3,
            "expected a route through the hall and lobby, got {:?}",
            route.waypoints,
        );
        assert!(
            route.pending.is_none(),
            "the destination should have been resolved on the first update",
        );
    }

    #[test]
    fn crew_come_from_and_return_to_their_own_department() {
        // What the station's wings are for. A Medical crew member starts in
        // Medical rather than the void south of the lobby, and heads back there
        // when they are done — the whole visible difference between a lab with
        // a corridor attached and a station with people in it.
        let mut app = walking_app();
        let medical = Vec3::new(-21.0, 0.0, 18.0);
        app.world_mut()
            .resource_mut::<Departments>()
            .set("Medical".into(), medical);

        let crew = walker(
            &mut app,
            Vec3::new(door_x(), 0.93, spawn_z()),
            CrewRoute::arrival(0.0),
        );
        tick(&mut app, 0.01);

        // Loose on purpose: they are placed and then immediately take their
        // first step towards the counter in the same tick. Medical is thirty
        // metres from the old spawn spot, so a quarter of a metre still tells
        // the two apart unambiguously.
        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert!(
            at.distance(Vec3::new(medical.x, at.y, medical.z)) < 0.25,
            "started at {at:?}, not in Medical",
        );

        app.world_mut().get_mut::<CrewRoute>(crew).unwrap().leave();
        tick(&mut app, 0.01);

        let heading_for = *app
            .world()
            .get::<CrewRoute>(crew)
            .unwrap()
            .waypoints
            .last()
            .expect("a route home");
        assert!(
            (heading_for.x - medical.x).abs() < 0.01
                && (heading_for.z - medical.z).abs() < 0.01,
            "left towards {heading_for:?} instead of back to Medical",
        );
    }

    #[test]
    fn a_department_the_station_has_no_room_for_changes_nothing() {
        // Every build without the map has an empty `Departments`, and so does a
        // map missing a wing. Crew must keep arriving and leaving the way they
        // did before any of this existed.
        let mut app = walking_app();
        let start = Vec3::new(door_x(), 0.93, spawn_z());
        let crew = walker(&mut app, start, CrewRoute::arrival(0.0));

        tick(&mut app, 0.01);
        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert!(
            (at.x - start.x).abs() < 0.5,
            "moved to a department that does not exist",
        );

        app.world_mut().get_mut::<CrewRoute>(crew).unwrap().leave();
        tick(&mut app, 0.01);

        let heading_for = *app
            .world()
            .get::<CrewRoute>(crew)
            .unwrap()
            .waypoints
            .last()
            .expect("a route out");
        assert!(
            (heading_for.z - spawn_z()).abs() < 0.01,
            "left towards {heading_for:?} instead of off-station",
        );
    }

    /// A station with two departments and residents idling in them.
    fn station_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Departments>()
            .insert_resource(crate::nav::NavGraph::build(
                &crate::lab::WalkableAreas::from_floor_plan(),
                crate::nav::NAV_RADIUS,
            ))
            .add_systems(Update, (ambient_behaviour, walk_route).chain());

        let mut departments = app.world_mut().resource_mut::<Departments>();
        departments.set("Medical".into(), Vec3::new(-21.0, 0.0, 18.0));
        departments.set("Cargo".into(), Vec3::new(-5.0, 0.0, 18.0));
        app
    }

    /// Someone standing around in the corridor, with nothing to do.
    fn resident(app: &mut App, role: &str, at: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: format!("{role} Resident"),
                    role: role.into(),
                },
                Transform::from_translation(at),
                CrewRoute {
                    waypoints: Vec::new(),
                    index: 0,
                    phase: CrewPhase::Waiting,
                    pending: None,
                },
                // Long dwell, so any movement in these tests is the crisis
                // talking and never the idle wander.
                Ambient { dwell: 1_000.0 },
            ))
            .id()
    }

    fn casualty(app: &mut App, at: Vec3, responders: &[&str]) {
        app.world_mut().spawn((
            Transform::from_translation(at),
            crate::crisis::CrisisResponse {
                responders: responders.iter().map(|role| role.to_string()).collect(),
            },
        ));
    }

    fn destination(app: &App, crew: Entity) -> Vec3 {
        *app.world()
            .get::<CrewRoute>(crew)
            .unwrap()
            .waypoints
            .last()
            .expect("somewhere to be")
    }

    #[test]
    fn residents_never_count_towards_how_busy_the_counter_is() {
        // The regression this exists to stop happening twice: `generate_orders`
        // counts crew to decide whether there is room for another customer, and
        // `max_active_cap` is 5. Eight residents living on the station is
        // already over it, so every order-spawning gate in the game closed
        // permanently — no error, nothing in the log, orders simply stopped.
        let mut app = App::new();

        for role in ["Medical", "Cargo", "Security"] {
            app.world_mut().spawn((
                CrewMember {
                    name: format!("{role} Resident"),
                    role: role.into(),
                },
                Ambient { dwell: 1.0 },
            ));
        }
        let visitor = app.world_mut().spawn(CrewMember {
            name: "Customer".into(),
            role: "Service".into(),
        });
        let visitor = visitor.id();

        let mut all = app.world_mut().query::<&CrewMember>();
        assert_eq!(all.iter(app.world()).count(), 4, "four crew exist");

        let mut visiting = app.world_mut().query_filtered::<Entity, (
            With<CrewMember>,
            NotResident,
        )>();
        let counted: Vec<Entity> = visiting.iter(app.world()).collect();
        assert_eq!(
            counted,
            vec![visitor],
            "only the customer may count towards the counter being busy",
        );
    }

    #[test]
    fn a_department_that_can_help_walks_towards_the_casualty() {
        // The user's rule, first half: if your department would be any use, you
        // go to it.
        let mut app = station_app();
        let medic = resident(&mut app, "Medical", Vec3::new(-21.0, 0.93, 18.0));
        let hurt = Vec3::new(4.0, 0.0, 5.6);
        casualty(&mut app, hurt, &["Medical"]);

        tick(&mut app, 0.01);

        let heading_for = destination(&app, medic);
        assert!(
            heading_for.distance(hurt) < 0.01,
            "a medic headed for {heading_for:?} instead of the casualty at {hurt:?}",
        );
    }

    #[test]
    fn a_department_that_cannot_help_goes_home_instead() {
        // The second half: if you would be no use, you get out of the way. Note
        // *not* the escape pod — that is the end of a campaign, not a bad shift.
        let mut app = station_app();
        let hauler = resident(&mut app, "Cargo", Vec3::new(0.0, 0.93, 0.0));
        casualty(&mut app, Vec3::new(4.0, 0.0, 5.6), &["Medical"]);

        tick(&mut app, 0.01);

        let heading_for = destination(&app, hauler);
        let home = Vec3::new(-5.0, 0.0, 18.0);
        assert!(
            heading_for.distance(home) < 0.01,
            "a hauler headed for {heading_for:?} instead of home to Cargo",
        );
    }

    #[test]
    fn a_responder_already_at_the_casualty_does_not_shuffle_on_the_spot() {
        // Without the "close enough" check, a responder standing over the
        // casualty re-routes to their own position every frame, which reads as
        // twitching and never stops.
        let mut app = station_app();
        let hurt = Vec3::new(4.0, 0.0, 5.6);
        let medic = resident(&mut app, "Medical", Vec3::new(hurt.x, 0.93, hurt.z));
        casualty(&mut app, hurt, &["Medical"]);

        tick(&mut app, 0.01);

        assert!(
            app.world()
                .get::<CrewRoute>(medic)
                .unwrap()
                .waypoints
                .is_empty(),
            "re-routed despite already standing on the casualty",
        );
    }

    #[test]
    fn with_no_casualty_nobody_is_summoned_anywhere() {
        // Idle crew wander on their own clock; a long dwell means they stay put.
        let mut app = station_app();
        let medic = resident(&mut app, "Medical", Vec3::new(0.0, 0.93, 0.0));

        tick(&mut app, 0.01);

        assert!(
            app.world()
                .get::<CrewRoute>(medic)
                .unwrap()
                .waypoints
                .is_empty(),
            "went somewhere with no crisis to go to",
        );
    }

    #[test]
    fn a_crew_member_still_walks_when_the_station_has_no_nav_graph() {
        // Under the map backend the graph is empty for a frame or two while the
        // scene loads. Crew asking for a route in that window must head for the
        // door anyway rather than stand still forever.
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Departments>()
            .init_resource::<crate::nav::NavGraph>()
            .add_systems(Update, walk_route);

        let start = Vec3::new(door_x(), 0.93, spawn_z());
        let crew = walker(&mut app, start, CrewRoute::arrival(0.0));

        for _ in 0..20 {
            tick(&mut app, 0.05);
        }

        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert!(
            at.distance(start) > 0.5,
            "stood still with an empty nav graph",
        );
    }

    fn flatten(app: &mut App, crew: Entity) {
        app.world_mut()
            .get_mut::<Body>(crew)
            .unwrap()
            .0
            .apply(Damage::of(DamageKind::Brute, Units::whole(120)));
        app.update();
    }

    #[test]
    fn a_collapsed_crew_member_leaves_early_and_costs_medical_standing() {
        let mut app = collapse_app();
        let crew = app
            .world_mut()
            .spawn((
                Body::default(),
                CrewMember {
                    name: "Test Subject".to_string(),
                    role: "Medical".to_string(),
                },
                CrewRoute::arrival(0.0),
            ))
            .id();

        flatten(&mut app, crew);

        assert_eq!(
            app.world().get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Leaving,
            "a crew member who goes down should be sent for the door, not left standing"
        );
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Medical),
            COLLAPSE_PENALTY
        );

        // Still down several frames later — must not keep charging, exactly
        // like the player's own `going_down_costs_standing_once`.
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Medical),
            COLLAPSE_PENALTY,
            "the penalty is for going down, not for staying down"
        );
    }

    #[test]
    fn an_unhurt_crew_member_is_left_alone() {
        let mut app = collapse_app();
        let crew = app
            .world_mut()
            .spawn((
                Body::default(),
                CrewMember {
                    name: "Fine".to_string(),
                    role: "Medical".to_string(),
                },
                CrewRoute::arrival(0.0),
            ))
            .id();

        app.update();

        assert_eq!(
            app.world().get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Arriving,
            "nothing collapsed, so nothing should have sent them for the door"
        );
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Medical),
            0
        );
    }
}
