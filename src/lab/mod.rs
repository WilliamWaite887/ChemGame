//! The chem lab: room shell, equipment placement and lighting.
//!
//! Everything is built from scaled unit cubes. No modelling required, and with
//! decent lighting it reads perfectly well as a station interior.

use bevy::prelude::*;

use chem_sim::{Solution, Units};

use crate::interaction::Interactable;
use crate::machines::{Buffer, ContainerSlot, DispenseAmount, Hopper, Machine, MachineKind};
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
        app.add_systems(OnEnter(AppState::Playing), (spawn_shell, spawn_equipment));
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

fn spawn_equipment(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));

    let casing = materials.add(StandardMaterial {
        base_color: Color::srgb(0.52, 0.55, 0.60),
        perceptual_roughness: 0.45,
        metallic: 0.7,
        ..default()
    });
    let bench = materials.add(StandardMaterial {
        base_color: Color::srgb(0.30, 0.33, 0.38),
        perceptual_roughness: 0.6,
        metallic: 0.3,
        ..default()
    });

    // Machines along the north wall, facing the player as they walk in.
    let north_z = -ROOM_HALF_Z + 0.45;
    let wall_machines = [
        (MachineKind::Dispenser, -4.6f32),
        (MachineKind::ChemMaster, -2.3),
        (MachineKind::Grinder, 0.0),
        (MachineKind::Analyzer, 2.3),
    ];
    for (kind, x) in wall_machines {
        spawn_machine(
            &mut commands,
            &mut materials,
            &cube,
            &casing,
            kind,
            Vec3::new(x, 0.0, north_z),
            Vec3::new(1.5, 1.7, 0.8),
            // Screen faces +Z, into the room.
            Vec3::Z,
        );
    }

    // Test bench on the west wall: free reagents, output cannot be delivered.
    spawn_machine(
        &mut commands,
        &mut materials,
        &cube,
        &bench,
        MachineKind::TestBench,
        Vec3::new(-ROOM_HALF_X + 0.55, 0.0, 1.4),
        Vec3::new(1.0, 1.1, 2.6),
        Vec3::X,
    );

    // Delivery counter, stood off the wall so crew can reach the far side of
    // it after coming through the door.
    spawn_machine(
        &mut commands,
        &mut materials,
        &cube,
        &bench,
        MachineKind::DeliveryWindow,
        Vec3::new(3.2, 0.0, 2.9),
        Vec3::new(3.0, 1.15, 0.7),
        Vec3::NEG_Z,
    );

    // A couple of plain benches so the room is not just a corridor.
    for z in [-1.2f32, 0.9] {
        solid_box(
            &mut commands,
            &cube,
            &bench,
            Vec3::new(-1.0, 0.45, z),
            Vec3::new(2.4, 0.9, 0.8),
        );
    }
}

/// Spawns a machine body plus a glowing screen on the face pointing along
/// `facing`, and makes it interactable.
#[allow(clippy::too_many_arguments)]
fn spawn_machine(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    cube: &Handle<Mesh>,
    casing: &Handle<StandardMaterial>,
    kind: MachineKind,
    base: Vec3,
    size: Vec3,
    facing: Vec3,
) -> Entity {
    let center = base + Vec3::Y * (size.y * 0.5);
    let body = commands
        .spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(casing.clone()),
            Transform::from_translation(center).with_scale(size),
            Solid {
                half_extents: size * 0.5,
            },
            Machine::new(kind),
            Interactable::new(kind.label()),
            // Occupancy and buffer contents must match for both chemists.
            bevy_replicon::prelude::Replicated,
        ))
        .id();

    // Equipment-specific fittings. The slot offset puts a loaded beaker on top
    // of the machine, nudged towards the face the player approaches from.
    let slot = ContainerSlot {
        offset: Vec3::Y * (size.y * 0.5 + 0.07) + facing * 0.18,
    };
    match kind {
        MachineKind::Dispenser | MachineKind::TestBench => {
            commands
                .entity(body)
                .insert((slot, DispenseAmount::default()));
        }
        MachineKind::ChemMaster => {
            commands
                .entity(body)
                .insert((slot, Buffer(Solution::new(Units::whole(300)))));
        }
        MachineKind::Grinder => {
            // Two loading points: produce goes in the hopper, the extract runs
            // out into whatever beaker is in the slot.
            commands.entity(body).insert((slot, Hopper::default()));
        }
        MachineKind::Analyzer => {
            commands.entity(body).insert(slot);
        }
        MachineKind::DeliveryWindow => {
            // A tray, not a machine: whatever is left here gets handed to the
            // next crew member who asked for something in it.
            commands.entity(body).insert(slot);
        }
    }

    // The screen is a separate unparented entity rather than a child: children
    // inherit the body's non-uniform scale, which would squash it.
    let screen_color = kind.screen_color();
    let screen_material = materials.add(StandardMaterial {
        base_color: Color::BLACK,
        emissive: screen_color.to_linear() * 1500.0,
        ..default()
    });

    // Inset very slightly so it does not z-fight with the casing.
    let offset = facing * (size.dot(facing.abs()) * 0.5 + 0.011);
    let screen_size = if facing.x.abs() > 0.5 {
        Vec3::new(0.02, 0.34, 0.62)
    } else {
        Vec3::new(0.62, 0.34, 0.02)
    };
    commands.spawn((
        Mesh3d(cube.clone()),
        MeshMaterial3d(screen_material),
        Transform::from_translation(center + offset + Vec3::Y * (size.y * 0.22))
            .with_scale(screen_size),
        MachineScreen,
    ));

    body
}
