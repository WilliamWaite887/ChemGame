//! An automatic sliding door at the lab's one external entrance — the
//! doorway `CREW_DOOR_X` marks on the lobby's south wall, the only way in
//! from the rest of the station. The five interior doorways stay exactly
//! what they always were: open gaps, no door object, nothing to spawn —
//! [`spawn_doors`] picks the one doorway out of `lab::doorways()` that
//! matches the crew door's position and ignores the rest.
//!
//! Placeholder geometry — two flat-shaded slabs, the same "scaled unit cube"
//! style as the rest of the lab (there is no glTF/scene-loading pipeline in
//! this codebase to load a modelled one from) — but real state: a closed
//! door blocks the player like a wall and steers crew around it, not just
//! looks shut.
//!
//! [`Door`] follows the exact shape `machines::Thermostat` already
//! established for "a bool that must read the same on every peer": a plain
//! replicated component, decided by the authority, reacted to everywhere
//! else. The four things every peer derives from it — the leaves' slide
//! animation, whether the root blocks the player, whether its `WalkableAreas`
//! bridge blocks crew, and the open/close sound — are each their own small
//! unguarded system, the same "presentation reads replicated state, nothing
//! reads it back" split `crisis::pulse_alert_lighting` and `fx` already use.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::crew::CrewMember;
use crate::lab::{
    doorways, Solid, WalkableAreas, CREW_DOOR_X, DOOR_HEIGHT, DOOR_WIDTH, LOBBY, ROOMS, WALL_THICKNESS,
};
use crate::net::is_authority;
use crate::player::Chemist;
use crate::AppState;

/// Thinner than the wall itself — a leaf this deep still clips slightly into
/// the wall it slides past when open (there is no recessed pocket to slide
/// into, just a solid box), but a slim blade reads as a door doing that; a
/// leaf as thick as the wall read as a block doing it.
const LEAF_THICKNESS: f32 = WALL_THICKNESS * 0.4;

/// How close a chemist or crew member has to be for a door to open.
///
/// Generous on purpose — this is omnidirectional, unlike `interaction`'s
/// facing-dependent reach, and it should trigger before a walking body would
/// visibly bump the still-closed leaves rather than right as they arrive.
const DOOR_PROXIMITY: f32 = 2.2;

/// How fast a leaf slides, in fractions of the way there per second.
const SLIDE_RATE: f32 = 6.0;

pub struct DoorPlugin;

impl Plugin for DoorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(AppState::Playing),
            (load_door_assets, spawn_doors.run_if(is_authority)),
        )
        .add_systems(
            Update,
            (
                dress_door,
                decide_door_state.run_if(is_authority),
                animate_door_leaves,
                toggle_door_solid,
                toggle_door_nav,
            )
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// A door's state. Registered `.replicate::<Door>()` in `net`, next to
/// `Thermostat` — see the module doc for why the two are the same shape.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Door {
    pub open: bool,
    /// Position in `lab::doorways()`'s own iteration order. Not looked up by
    /// world position anywhere: every system that needs this door's run/axis
    /// re-derives it from this index, so there is exactly one source for
    /// "which doorway is this" instead of a cached copy that could drift.
    pub doorway_index: usize,
}

/// Meshes and materials every leaf is drawn from. Its own small resource
/// rather than reaching into `lab`'s private `MachineAssets` — doors are not
/// machines, and this is one shared cube handle either way.
#[derive(Resource)]
struct DoorAssets {
    cube: Handle<Mesh>,
    leaf: Handle<StandardMaterial>,
}

fn load_door_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(DoorAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        leaf: materials.add(StandardMaterial {
            base_color: Color::srgb(0.58, 0.62, 0.68),
            perceptual_roughness: 0.35,
            metallic: 0.8,
            ..default()
        }),
    });
}

/// The lobby's south wall, and the only way into the lab from the rest of
/// the station — see `lab::CREW_DOOR_X`'s own doc. The one doorway that
/// gets an actual door; see the module doc for why the other five do not.
fn is_crew_doorway(at: Vec3) -> bool {
    (at.x - CREW_DOOR_X).abs() < 0.001 && (at.z - ROOMS[LOBBY].max_z).abs() < 0.001
}

/// Creates the door's state. Authority only — see `Door`'s doc and
/// `machines::spawn_machines`, the pattern this copies.
fn spawn_doors(mut commands: Commands) {
    for (index, (run, center)) in doorways().enumerate() {
        let at = run.point(center);
        if !is_crew_doorway(at) {
            continue;
        }
        commands.spawn((
            Door {
                open: false,
                doorway_index: index,
            },
            Transform::from_translation(at),
            bevy_replicon::prelude::Replicated,
            crate::until_we_leave_the_lab(),
        ));
    }
}

/// A door's leaf. Carries its own `door` reference rather than reading the
/// parent through the hierarchy — the same shape `player::ChemistBody`
/// already uses to find the chemist it belongs to.
#[derive(Component)]
struct DoorLeaf {
    door: Entity,
    closed_local: Vec3,
    open_local: Vec3,
}

/// Gives a door its two leaves. Runs on both ends against `Added<Door>` —
/// on the authority the frame `spawn_doors` runs, on a client when
/// replication delivers the entity — same reasoning as `dress_machines`.
fn dress_door(
    mut commands: Commands,
    assets: Option<Res<DoorAssets>>,
    doors: Query<(Entity, &Door), Added<Door>>,
) {
    let Some(assets) = assets else {
        return;
    };
    for (entity, door) in &doors {
        let Some((run, _)) = doorways().nth(door.doorway_index) else {
            continue;
        };
        // Meeting at the centre (closed) or slid a full leaf-width into
        // where the flanking wall segment already stands (open) — the same
        // gap `WallRun::segments()` cuts, worked out from the same axis.
        let axis = if run.along_x { Vec3::X } else { Vec3::Z };
        let size = if run.along_x {
            Vec3::new(DOOR_WIDTH * 0.5, DOOR_HEIGHT, LEAF_THICKNESS)
        } else {
            Vec3::new(LEAF_THICKNESS, DOOR_HEIGHT, DOOR_WIDTH * 0.5)
        };
        for side in [-1.0f32, 1.0] {
            let closed_local = axis * (side * DOOR_WIDTH * 0.25) + Vec3::Y * DOOR_HEIGHT * 0.5;
            let open_local = closed_local + axis * (side * DOOR_WIDTH * 0.5);
            commands.entity(entity).with_children(|parent| {
                parent.spawn((
                    Mesh3d(assets.cube.clone()),
                    MeshMaterial3d(assets.leaf.clone()),
                    Transform::from_translation(closed_local).with_scale(size),
                    DoorLeaf {
                        door: entity,
                        closed_local,
                        open_local,
                    },
                ));
            });
        }
    }
}

/// The proximity trigger. Authority only — same two-query shape as
/// `hazards::expose_to_smoke`: one for chemists, one for crew, both plain
/// `Transform` distance checks against the door's own position.
#[allow(clippy::type_complexity)]
fn decide_door_state(
    mut doors: Query<(&Transform, &mut Door)>,
    chemists: Query<&Transform, (With<Chemist>, Without<Door>)>,
    crew: Query<&Transform, (With<CrewMember>, Without<Chemist>, Without<Door>)>,
) {
    for (door_transform, mut door) in &mut doors {
        let near = |transform: &Transform| {
            transform.translation.distance(door_transform.translation) <= DOOR_PROXIMITY
        };
        let wants_open = chemists.iter().any(near) || crew.iter().any(near);
        if door.open != wants_open {
            door.open = wants_open;
        }
    }
}

/// Slides each leaf toward its door's target, and stops writing once it
/// arrives — the same approach-and-snap idiom `crisis::pulse_alert_lighting`
/// uses for the alert lights, so a settled door does not mark itself
/// `Changed` forever.
fn animate_door_leaves(
    time: Res<Time>,
    doors: Query<&Door>,
    mut leaves: Query<(&DoorLeaf, &mut Transform)>,
) {
    let t = (time.delta_secs() * SLIDE_RATE).clamp(0.0, 1.0);
    for (leaf, mut transform) in &mut leaves {
        let Ok(door) = doors.get(leaf.door) else {
            continue;
        };
        let target = if door.open { leaf.open_local } else { leaf.closed_local };
        if transform.translation == target {
            continue;
        }
        let next = transform.translation.lerp(target, t);
        transform.translation = if next.distance(target) < 0.01 { target } else { next };
    }
}

/// Blocks the player exactly while shut. `apply_move_input` already resolves
/// against every `Solid` in the world, so this is the only touch point
/// needed on the player-collision side.
fn toggle_door_solid(mut commands: Commands, doors: Query<(Entity, &Door), Changed<Door>>) {
    for (entity, door) in &doors {
        if door.open {
            commands.entity(entity).remove::<Solid>();
            continue;
        }
        let Some((run, _)) = doorways().nth(door.doorway_index) else {
            continue;
        };
        let half_extents = if run.along_x {
            Vec3::new(DOOR_WIDTH * 0.5, DOOR_HEIGHT * 0.5, WALL_THICKNESS * 0.5)
        } else {
            Vec3::new(WALL_THICKNESS * 0.5, DOOR_HEIGHT * 0.5, DOOR_WIDTH * 0.5)
        };
        commands.entity(entity).insert(Solid { half_extents });
    }
}

/// Blocks crew pathing exactly while shut, by collapsing this door's own
/// `WalkableAreas` bridge — see `WalkableAreas::set_door_blocked`. Runs on
/// both ends unguarded, same as `nav::rebuild_graph` it feeds: every peer
/// keeps its own local copy of "where may a body stand" in step with the one
/// thing that has to agree everywhere, `Door.open`.
fn toggle_door_nav(mut walkable: ResMut<WalkableAreas>, doors: Query<&Door, Changed<Door>>) {
    for door in &doors {
        walkable.set_door_blocked(door.doorway_index, !door.open);
    }
}

#[cfg(test)]
mod tests {
    use bevy_replicon::prelude::ClientId;

    use super::*;
    use crate::lab::ROOMS;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_systems(Update, decide_door_state);
        app
    }

    fn door_at(app: &mut App, doorway_index: usize) -> Entity {
        let (run, center) = doorways().nth(doorway_index).expect("a real doorway");
        app.world_mut()
            .spawn((
                Door { open: false, doorway_index },
                Transform::from_translation(run.point(center)),
            ))
            .id()
    }

    fn chemist_at(app: &mut App, position: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                Chemist { client: ClientId::Server },
                Transform::from_translation(position),
            ))
            .id()
    }

    fn crew_at(app: &mut App, position: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: "Bystander".to_string(),
                    role: "Service".to_string(),
                },
                Transform::from_translation(position),
            ))
            .id()
    }

    fn is_open(app: &App, door: Entity) -> bool {
        app.world().get::<Door>(door).unwrap().open
    }

    #[test]
    fn a_door_opens_for_a_nearby_chemist_and_closes_once_they_leave() {
        let mut app = test_app();
        let door = door_at(&mut app, 0);
        let (run, center) = doorways().next().unwrap();
        let at = run.point(center);
        let chemist = chemist_at(&mut app, at);

        app.update();
        assert!(is_open(&app, door), "a chemist standing in the doorway did not open it");

        app.world_mut().entity_mut(chemist).get_mut::<Transform>().unwrap().translation =
            at + Vec3::new(50.0, 0.0, 50.0);
        app.update();
        assert!(!is_open(&app, door), "the door stayed open once the chemist walked far away");
    }

    #[test]
    fn a_door_opens_for_a_nearby_crew_member_too() {
        let mut app = test_app();
        let door = door_at(&mut app, 0);
        let (run, center) = doorways().next().unwrap();
        crew_at(&mut app, run.point(center));

        app.update();
        assert!(is_open(&app, door), "a crew member standing in the doorway did not open it");
    }

    #[test]
    fn a_far_off_body_never_opens_a_door() {
        let mut app = test_app();
        let door = door_at(&mut app, 0);
        // Somewhere in a different room entirely.
        chemist_at(&mut app, ROOMS[crate::lab::ANALYSIS].center());

        app.update();
        assert!(!is_open(&app, door), "a door opened for nobody near it");
    }

    #[test]
    fn spawn_doors_makes_exactly_one_door_and_it_is_the_crew_entrance() {
        // Only the lab's one external doorway gets a door — the five
        // interior ones stay open gaps, same as before this module existed.
        let mut app = App::new();
        app.add_systems(Update, spawn_doors);
        app.update();

        let mut doors = app.world_mut().query::<(&Door, &Transform)>();
        let found: Vec<(&Door, &Transform)> = doors.iter(app.world()).collect();
        assert_eq!(found.len(), 1, "expected exactly one door, found {}", found.len());

        let (door, transform) = found[0];
        assert!(
            is_crew_doorway(transform.translation),
            "the one door spawned is not at the crew entrance: {:?}",
            transform.translation
        );
        let (run, center) = doorways().nth(door.doorway_index).expect("a real doorway");
        assert_eq!(
            run.point(center),
            transform.translation,
            "doorway_index does not match where the door actually is"
        );
    }
}
