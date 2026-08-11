//! The chem lab: room shell, equipment placement and lighting.
//!
//! Everything is built from scaled unit cubes. No modelling required, and with
//! decent lighting it reads perfectly well as a station interior.

use bevy::prelude::*;

use chem_sim::{Solution, Units};

use crate::interaction::Interactable;
use crate::machines::{
    Buffer, ContainerSlot, DispenseAmount, Hopper, Machine, MachineKind, Thermostat,
};
use crate::net::is_authority;
use crate::AppState;

/// Room interior runs from `-HALF` to `+HALF` on each axis.
pub const ROOM_HALF_X: f32 = 7.0;
pub const ROOM_HALF_Z: f32 = 4.5;
pub const ROOM_HEIGHT: f32 = 3.2;

const WALL_THICKNESS: f32 = 0.25;

/// Doorway in the south wall, in world x.
pub const DOOR_MIN_X: f32 = 5.2;
pub const DOOR_MAX_X: f32 = 6.6;
/// Where crew queue up to collect an order.
pub const COUNTER_SPOT: Vec3 = Vec3::new(3.2, 0.0, 3.9);

pub struct LabPlugin;

impl Plugin for LabPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(AppState::Playing),
            (
                // The room itself is scenery: identical on both ends, derived
                // from constants, and nothing about it is worth a packet.
                spawn_shell,
                load_machine_assets,
                spawn_fixtures,
                // The machines are state, so only the authority creates them.
                spawn_machines.run_if(is_authority),
            )
                // Chained for the sync point: the two spawners read the
                // resource the loader inserts.
                .chain(),
        )
        // Fitting them out runs everywhere, against whatever machines exist —
        // spawned locally here, or arrived by replication there.
        .add_systems(Update, dress_machines.run_if(in_state(AppState::Playing)));
    }
}

/// An axis-aligned obstruction the player cannot walk through.
///
/// Deliberately not a physics engine: a lab is a box with boxes in it, and
/// swept-AABB resolution against a handful of statics is all that needs.
#[derive(Component)]
pub struct Solid {
    pub half_extents: Vec3,
}

/// The glowing panel on the front of a machine.
///
/// Marked so interaction raycasts can skip it: it sits fractionally proud of
/// the casing, and without this every machine would be blocked by its own
/// screen.
#[derive(Component)]
pub struct MachineScreen;

/// Spawns a scaled cube that blocks movement.
fn solid_box(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    material: &Handle<StandardMaterial>,
    center: Vec3,
    size: Vec3,
) -> Entity {
    commands
        .spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(center).with_scale(size),
            Solid {
                half_extents: size * 0.5,
            },
        ))
        .id()
}

/// Spawns a scaled cube that does not block movement (floor, ceiling, trim).
fn decor_box(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    material: &Handle<StandardMaterial>,
    center: Vec3,
    size: Vec3,
) -> Entity {
    commands
        .spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(center).with_scale(size),
        ))
        .id()
}

fn spawn_shell(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));

    let floor = materials.add(StandardMaterial {
        base_color: Color::srgb(0.22, 0.23, 0.26),
        perceptual_roughness: 0.85,
        ..default()
    });
    let wall = materials.add(StandardMaterial {
        base_color: Color::srgb(0.38, 0.40, 0.44),
        perceptual_roughness: 0.9,
        ..default()
    });
    let ceiling = materials.add(StandardMaterial {
        base_color: Color::srgb(0.16, 0.17, 0.19),
        perceptual_roughness: 0.95,
        ..default()
    });
    let stripe = materials.add(StandardMaterial {
        base_color: Color::srgb(0.72, 0.62, 0.20),
        perceptual_roughness: 0.7,
        ..default()
    });

    let span_x = ROOM_HALF_X * 2.0 + WALL_THICKNESS * 2.0;
    let span_z = ROOM_HALF_Z * 2.0 + WALL_THICKNESS * 2.0;

    decor_box(
        &mut commands,
        &cube,
        &floor,
        Vec3::new(0.0, -0.05, 0.0),
        Vec3::new(span_x, 0.1, span_z),
    );
    decor_box(
        &mut commands,
        &cube,
        &ceiling,
        Vec3::new(0.0, ROOM_HEIGHT + 0.05, 0.0),
        Vec3::new(span_x, 0.1, span_z),
    );

    // Hazard stripe along the floor by the delivery side, so the room has an
    // obvious front and back when you spin around.
    decor_box(
        &mut commands,
        &cube,
        &stripe,
        Vec3::new(0.0, 0.005, ROOM_HALF_Z - 1.6),
        Vec3::new(span_x, 0.02, 0.18),
    );

    let half_height = ROOM_HEIGHT * 0.5;
    let south_z = ROOM_HALF_Z + WALL_THICKNESS * 0.5;
    let east_edge = span_x * 0.5;

    // The south wall is split to leave a doorway; crew walk in through it to
    // collect their orders.
    let west_span = DOOR_MIN_X + east_edge;
    let east_span = east_edge - DOOR_MAX_X;

    let walls = [
        // (center, size)
        (
            Vec3::new(0.0, half_height, -ROOM_HALF_Z - WALL_THICKNESS * 0.5),
            Vec3::new(span_x, ROOM_HEIGHT, WALL_THICKNESS),
        ),
        (
            Vec3::new((DOOR_MIN_X - east_edge) * 0.5, half_height, south_z),
            Vec3::new(west_span, ROOM_HEIGHT, WALL_THICKNESS),
        ),
        (
            Vec3::new((DOOR_MAX_X + east_edge) * 0.5, half_height, south_z),
            Vec3::new(east_span, ROOM_HEIGHT, WALL_THICKNESS),
        ),
        (
            Vec3::new(-ROOM_HALF_X - WALL_THICKNESS * 0.5, half_height, 0.0),
            Vec3::new(WALL_THICKNESS, ROOM_HEIGHT, span_z),
        ),
        (
            Vec3::new(ROOM_HALF_X + WALL_THICKNESS * 0.5, half_height, 0.0),
            Vec3::new(WALL_THICKNESS, ROOM_HEIGHT, span_z),
        ),
    ];
    for (center, size) in walls {
        solid_box(&mut commands, &cube, &wall, center, size);
    }

    // Lighting. A few warm points beat one bright source for making the
    // primitive geometry read as a room.
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.62, 0.68, 0.80),
        brightness: 220.0,
        ..default()
    });

    for x in [-4.5f32, 0.0, 4.5] {
        commands.spawn((
            PointLight {
                intensity: 260_000.0,
                range: 18.0,
                shadow_maps_enabled: true,
                color: Color::srgb(1.0, 0.96, 0.90),
                ..default()
            },
            Transform::from_xyz(x, ROOM_HEIGHT - 0.35, 0.0),
        ));
    }
}

/// Where a machine stands, how big it is, and which way it faces.
///
/// Keyed on kind alone, because the lab holds exactly one of each. That is
/// what lets a client rebuild the entire fit-out from the replicated
/// `MachineKind` without a single byte of room layout crossing the wire.
struct MachineFit {
    base: Vec3,
    size: Vec3,
    /// The face the screen sits on, and the side the player walks up to.
    facing: Vec3,
    /// Machines are steel; worktops are painted.
    is_worktop: bool,
}

fn fit(kind: MachineKind) -> MachineFit {
    // Machines along the north wall, facing the player as they walk in.
    let north_z = -ROOM_HALF_Z + 0.45;
    let wall = |x: f32| MachineFit {
        base: Vec3::new(x, 0.0, north_z),
        size: Vec3::new(1.5, 1.7, 0.8),
        // Screen faces +Z, into the room.
        facing: Vec3::Z,
        is_worktop: false,
    };

    match kind {
        MachineKind::Dispenser => wall(-4.6),
        MachineKind::ChemMaster => wall(-2.3),
        MachineKind::Grinder => wall(0.0),
        MachineKind::Analyzer => wall(2.3),
        // Nearest the door, so it is the first thing you face walking in and
        // the last thing you pass on the way out.
        MachineKind::ShiftBoard => wall(4.6),
        // Test bench on the west wall: free reagents, output cannot be
        // delivered.
        MachineKind::TestBench => MachineFit {
            base: Vec3::new(-ROOM_HALF_X + 0.55, 0.0, 1.4),
            size: Vec3::new(1.0, 1.1, 2.6),
            facing: Vec3::X,
            is_worktop: true,
        },
        // Reaction chamber on the west wall too, at the dispenser end. A hot
        // batch has to be carried to the ChemMaster before it cools, so the
        // walk between the two is the deadline — putting the chamber next to
        // the ChemMaster would remove the only cost heating has.
        MachineKind::ReactionChamber => MachineFit {
            base: Vec3::new(-ROOM_HALF_X + 0.55, 0.0, -1.6),
            size: Vec3::new(1.0, 1.5, 1.4),
            facing: Vec3::X,
            is_worktop: false,
        },
        // Delivery counter, stood off the wall so crew can reach the far side
        // of it after coming through the door.
        MachineKind::DeliveryWindow => MachineFit {
            base: Vec3::new(3.2, 0.0, 2.9),
            size: Vec3::new(3.0, 1.15, 0.7),
            facing: Vec3::NEG_Z,
            is_worktop: true,
        },
    }
}

impl MachineFit {
    fn center(&self) -> Vec3 {
        self.base + Vec3::Y * (self.size.y * 0.5)
    }
}

/// Meshes and materials the fit-out is drawn from.
#[derive(Resource)]
pub(crate) struct MachineAssets {
    cube: Handle<Mesh>,
    casing: Handle<StandardMaterial>,
    bench: Handle<StandardMaterial>,
}

pub(crate) fn load_machine_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(MachineAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        casing: materials.add(StandardMaterial {
            base_color: Color::srgb(0.52, 0.55, 0.60),
            perceptual_roughness: 0.45,
            metallic: 0.7,
            ..default()
        }),
        bench: materials.add(StandardMaterial {
            base_color: Color::srgb(0.30, 0.33, 0.38),
            perceptual_roughness: 0.6,
            metallic: 0.3,
            ..default()
        }),
    });
}

/// Plain benches, so the room is not just a corridor.
///
/// Scenery rather than equipment: no state, nothing to replicate, and the
/// server needs them present to collide against.
fn spawn_fixtures(mut commands: Commands, assets: Res<MachineAssets>) {
    for z in [-1.2f32, 0.9] {
        solid_box(
            &mut commands,
            &assets.cube,
            &assets.bench,
            Vec3::new(-1.0, 0.45, z),
            Vec3::new(2.4, 0.9, 0.8),
        );
    }
}

/// Creates the machines and their state. Authority only.
///
/// Everything spawned here is either replicated or derivable from what is —
/// no meshes, because a client builds those itself in [`dress_machines`].
fn spawn_machines(mut commands: Commands) {
    for kind in MachineKind::ALL {
        let fit = fit(kind);
        let machine = commands
            .spawn((
                Machine::new(kind),
                Transform::from_translation(fit.center()).with_scale(fit.size),
                // Occupancy and buffer contents must match for both chemists.
                bevy_replicon::prelude::Replicated,
            ))
            .id();

        // Equipment-specific state.
        match kind {
            MachineKind::Dispenser | MachineKind::TestBench => {
                commands.entity(machine).insert(DispenseAmount::default());
            }
            MachineKind::ChemMaster => {
                commands
                    .entity(machine)
                    .insert(Buffer(Solution::new(Units::whole(300))));
            }
            MachineKind::Grinder => {
                // Two loading points: produce goes in the hopper, the extract
                // runs out into whatever beaker is in the slot.
                commands.entity(machine).insert(Hopper::default());
            }
            MachineKind::ReactionChamber => {
                commands.entity(machine).insert(Thermostat::default());
            }
            MachineKind::Analyzer | MachineKind::DeliveryWindow | MachineKind::ShiftBoard => {}
        }
    }
}

/// Gives a machine its casing, its screen and its slot.
///
/// Runs on both ends, against `Added<Machine>`. On the authority that fires
/// the frame [`spawn_machines`] runs; on a client it fires when the machine
/// arrives by replication. Everything here is derived from the kind, so the
/// two ends cannot disagree about where a machine is or how big it is.
pub(crate) fn dress_machines(
    mut commands: Commands,
    assets: Option<Res<MachineAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    machines: Query<(Entity, &Machine), Added<Machine>>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, machine) in &machines {
        let kind = machine.kind;
        let fit = fit(kind);
        let casing = if fit.is_worktop {
            &assets.bench
        } else {
            &assets.casing
        };

        commands.entity(entity).insert((
            Mesh3d(assets.cube.clone()),
            MeshMaterial3d(casing.clone()),
            Solid {
                half_extents: fit.size * 0.5,
            },
            Interactable::new(kind.label()),
            // Position is replicated, but a client that dresses a machine the
            // same frame it arrives may not have received the transform yet.
            // Setting it from the fit costs nothing and removes the flicker.
            Transform::from_translation(fit.center()).with_scale(fit.size),
        ));

        // The slot offset puts a loaded beaker on top of the machine, nudged
        // towards the face the player approaches from. Static geometry, so it
        // is derived rather than replicated.
        //
        // The shift board has none, deliberately: walking up holding a beaker
        // would park it on the board instead of opening the panel, and the
        // player would have no way of telling why the board had stopped
        // responding.
        if kind != MachineKind::ShiftBoard {
            commands.entity(entity).insert(ContainerSlot {
                offset: Vec3::Y * (fit.size.y * 0.5 + 0.07) + fit.facing * 0.18,
            });
        }

        // The screen is a separate unparented entity rather than a child:
        // children inherit the body's non-uniform scale, which would squash it.
        let screen_material = materials.add(StandardMaterial {
            base_color: Color::BLACK,
            emissive: kind.screen_color().to_linear() * 1500.0,
            ..default()
        });

        // Inset very slightly so it does not z-fight with the casing.
        let offset = fit.facing * (fit.size.dot(fit.facing.abs()) * 0.5 + 0.011);
        let screen_size = if fit.facing.x.abs() > 0.5 {
            Vec3::new(0.02, 0.34, 0.62)
        } else {
            Vec3::new(0.62, 0.34, 0.02)
        };
        commands.spawn((
            Mesh3d(assets.cube.clone()),
            MeshMaterial3d(screen_material),
            Transform::from_translation(fit.center() + offset + Vec3::Y * (fit.size.y * 0.22))
                .with_scale(screen_size),
            MachineScreen,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lab as a joining client builds one: no machines of its own, and the
    /// asset handles ready for whatever the server sends.
    fn client_lab() -> App {
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_systems(Startup, load_machine_assets)
            .add_systems(Update, dress_machines);
        app
    }

    #[test]
    fn a_machine_that_arrives_over_the_wire_is_built_out_in_full() {
        // Replication carries the machine's state and nothing else — no mesh,
        // no collider, no label. Before this split the client spawned its own
        // machines instead, which raycast fine and were then meaningless to
        // the server: interacting sent it an entity id it had never issued.
        let mut app = client_lab();
        let arrived = app
            .world_mut()
            .spawn(Machine::new(MachineKind::Dispenser))
            .id();

        app.update();

        let world = app.world();
        assert!(
            world.get::<Mesh3d>(arrived).is_some(),
            "an undressed machine is an invisible one"
        );
        assert!(
            world.get::<Interactable>(arrived).is_some(),
            "without this the crosshair passes straight through it"
        );
        assert!(
            world.get::<Solid>(arrived).is_some(),
            "the chemist would walk through the dispenser"
        );
        assert!(
            world.get::<ContainerSlot>(arrived).is_some(),
            "a dispenser with no slot cannot be loaded with a beaker"
        );
    }

    #[test]
    fn the_shift_board_is_the_one_machine_with_no_slot() {
        // Walking up to it holding a beaker would park the beaker on the board
        // instead of opening the panel, and nothing would explain why.
        let mut app = client_lab();
        let board = app
            .world_mut()
            .spawn(Machine::new(MachineKind::ShiftBoard))
            .id();

        app.update();

        assert!(app.world().get::<ContainerSlot>(board).is_none());
    }

    #[test]
    fn both_ends_agree_on_where_every_machine_stands() {
        // The client derives position and size from the kind alone. If that
        // ever drifted from what the authority spawns, the second chemist
        // would see machines floating away from their casings.
        for kind in MachineKind::ALL {
            let placed = fit(kind);
            let dressed = fit(kind);
            assert_eq!(placed.center(), dressed.center(), "{kind:?} moved");
            assert_eq!(placed.size, dressed.size, "{kind:?} resized");
        }
    }

    #[test]
    fn no_two_machines_are_stacked_in_the_same_spot() {
        // `ALL` drives both the spawner and the fit-out, so a machine added to
        // the enum but forgotten in `fit` would silently land on top of
        // another one rather than failing to compile.
        for (index, kind) in MachineKind::ALL.iter().enumerate() {
            for other in &MachineKind::ALL[index + 1..] {
                assert_ne!(
                    fit(*kind).center(),
                    fit(*other).center(),
                    "{kind:?} and {other:?} occupy the same place"
                );
            }
        }
    }
}
