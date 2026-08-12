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
        app.add_systems(OnEnter(AppState::Playing), load_crew_assets)
            .add_systems(
                Update,
                (
                    // The walk is simulated once, on the server; clients
                    // receive the resulting Transform. `handle_crew_collapse`
                    // reads `Body`, which only the server ever mutates
                    // (metabolism, smoke, a delivered dose), so it belongs on
                    // the same side.
                    (walk_route, handle_crew_collapse).run_if(is_authority),
                    // Runs everywhere: a crew member who arrived by
                    // replication needs a body drawing just as much as one
                    // spawned locally.
                    dress_crew,
                )
                    .run_if(in_state(AppState::Playing)),
            );
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
}

impl CrewRoute {
    /// The walk in: through the door, then across to the counter.
    pub fn arrival(lane: f32) -> Self {
        CrewRoute {
            waypoints: vec![
                Vec3::new(door_x(), 0.0, COUNTER_SPOT.z),
                Vec3::new(COUNTER_SPOT.x + lane, 0.0, COUNTER_SPOT.z),
            ],
            index: 0,
            phase: CrewPhase::Arriving,
        }
    }

    /// Sends them back out the way they came.
    pub fn leave(&mut self) {
        self.waypoints = vec![
            Vec3::new(door_x(), 0.0, COUNTER_SPOT.z),
            Vec3::new(door_x(), 0.0, spawn_z()),
        ];
        self.index = 0;
        self.phase = CrewPhase::Leaving;
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
fn walk_route(
    mut commands: Commands,
    time: Res<Time>,
    mut crew: Query<(Entity, &mut Transform, &mut CrewRoute)>,
) {
    for (entity, mut transform, mut route) in &mut crew {
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
