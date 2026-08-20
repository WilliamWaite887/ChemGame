//! The chem lab: room shell, equipment placement and lighting.
//!
//! The room shell and simple collision remain map/primitive owned; authored
//! GLB scenes provide the visible machine bodies.
//!
//! The suite is five rooms rather than one box, because eight machines along two
//! walls of a single room read as a storage cupboard rather than a workplace.
//! Each room is a rectangle in [`ROOMS`]; walls are straight runs in [`WALLS`]
//! with doorways cut out of them. Both are plain data, and that is what lets one
//! floor plan drive the geometry, the lighting *and* the player's containment: a
//! room moved in `ROOMS` takes its floor, its lights and its walkable footprint
//! with it.

// Under the `trenchbroom` feature the map builds the shell, so the builders
// below go unused and read as dead code. The floor plan is kept rather than
// cfg'd out because `contain` still reads it, and so does every test in this
// module — retiring it is Stage 1 of the station plan, once `func_walkable`
// volumes in the map can answer "where may a body stand?" on their own.
#![cfg_attr(feature = "trenchbroom", allow(dead_code))]

use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use chem_sim::{Solution, Units};

use crate::interaction::Interactable;
use crate::machines::{
    Buffer, ContainerSlot, ContainerSlotB, DispenseAmount, Facing, Hopper, Machine, MachineKind,
    Thermostat,
};
use crate::net::is_authority;
use crate::AppState;

/// Checks on the hand-edited TrenchBroom map. Test-only, and unconditional: the
/// map is in the repo whether or not the feature that loads it is on.
#[cfg(test)]
pub(crate) mod tb_map;

/// The TrenchBroom-driven station loader. It is the default build; the feature
/// gate keeps the const-table fallback available through `--no-default-features`.
#[cfg(feature = "trenchbroom")]
pub mod tb;

pub const ROOM_HEIGHT: f32 = 3.2;

/// TrenchBroom units per metre. Must match what the map was drawn at.
///
/// 40 rather than `bevy_trenchbroom`'s default 39.37 (one unit per inch): every
/// coordinate in the floor plan is a multiple of 0.025 m, so at 40 every brush
/// lands on an integer and almost all on TrenchBroom's default 8-unit grid. At
/// 39.37 the whole lab sits on fractional coordinates and is miserable to edit.
// Only the feature-gated loader reads it, so a plain build never touches it.
#[cfg_attr(not(feature = "trenchbroom"), allow(dead_code))]
pub const TB_SCALE: f32 = 40.0;

/// Semantic locations authored into the station map for crises and incidents.
///
/// Gameplay refers to a stable id (for example `hazard.rad_leak`) rather than
/// knowing a coordinate in one particular floor plan. The registry is replaced
/// atomically when the TrenchBroom world finishes instantiating; the
/// `--no-default-features` fallback seeds the same ids at their legacy spots.
#[derive(Resource, Debug, Default, Clone)]
pub struct CrisisSpots {
    spots: std::collections::HashMap<String, Transform>,
}

#[derive(Clone, Debug)]
pub struct DoorPlacement {
    pub bridge_id: String,
    pub transform: Transform,
}

/// Stable map-authored proximity-airlock placements.
#[derive(Resource, Clone, Debug, Default)]
pub struct DoorSpots {
    spots: std::collections::BTreeMap<String, DoorPlacement>,
}

impl DoorSpots {
    pub fn insert(
        &mut self,
        id: impl Into<String>,
        bridge_id: impl Into<String>,
        transform: Transform,
    ) -> Option<DoorPlacement> {
        self.spots.insert(
            id.into(),
            DoorPlacement {
                bridge_id: bridge_id.into(),
                transform,
            },
        )
    }

    pub fn get(&self, id: &str) -> Option<&DoorPlacement> {
        self.spots.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &DoorPlacement)> {
        self.spots
            .iter()
            .map(|(id, placement)| (id.as_str(), placement))
    }
}

impl CrisisSpots {
    /// Adds or replaces a semantic spot.
    ///
    /// Public so focused gameplay tests can construct the same registry the
    /// map loader supplies without having to instantiate a world asset.
    pub fn insert(&mut self, id: impl Into<String>, transform: Transform) -> Option<Transform> {
        self.spots.insert(id.into(), transform)
    }

    /// The authored transform for `id`, including its orientation.
    pub fn get(&self, id: &str) -> Option<Transform> {
        self.spots.get(id).copied()
    }
}

/// Map-authored locations for authority-owned lab machines.
///
/// The transform is the floor origin and orientation of the authored model;
/// [`reconcile_machine_spots`] raises the gameplay root to the collider centre
/// before it is replicated. IDs, rather than kinds, are the identity because a
/// lab may contain several machines of the same kind.
#[derive(Debug, Clone)]
pub struct MachinePlacement {
    pub kind: MachineKind,
    pub transform: Transform,
    pub lane: Option<DeliveryLane>,
}

#[derive(Resource, Debug, Default, Clone)]
pub struct MachineSpots {
    spots: std::collections::BTreeMap<String, MachinePlacement>,
}

impl MachineSpots {
    /// Adds or replaces a stable placement id, returning the old placement.
    pub fn insert(
        &mut self,
        id: impl Into<String>,
        kind: MachineKind,
        transform: Transform,
    ) -> Option<MachinePlacement> {
        self.insert_with_lane(id, kind, transform, None)
    }

    pub fn insert_with_lane(
        &mut self,
        id: impl Into<String>,
        kind: MachineKind,
        transform: Transform,
        lane: Option<DeliveryLane>,
    ) -> Option<MachinePlacement> {
        self.spots.insert(
            id.into(),
            MachinePlacement {
                kind,
                transform,
                lane,
            },
        )
    }

    pub fn get(&self, id: &str) -> Option<&MachinePlacement> {
        self.spots.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &MachinePlacement)> {
        self.spots
            .iter()
            .map(|(id, placement)| (id.as_str(), placement))
    }

    pub fn len(&self) -> usize {
        self.spots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spots.is_empty()
    }
}

/// Which physical handoff counter owns an arriving order.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
pub enum DeliveryLane {
    #[default]
    Public,
    Medical,
}

impl DeliveryLane {
    #[cfg(feature = "trenchbroom")]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "public" => Some(Self::Public),
            "medical" => Some(Self::Medical),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeliveryStation {
    pub transform: Transform,
}

impl DeliveryStation {
    /// The map marker's local -Z points from the machine toward waiting crew.
    pub fn queue_position(&self, lane_offset: f32) -> Vec3 {
        let toward_crew = -(self.transform.rotation * Vec3::Z);
        let across_queue = -(self.transform.rotation * Vec3::X);
        let mut point = self.transform.translation + toward_crew + across_queue * lane_offset;
        point.y = 0.0;
        point
    }

    pub fn drop_position(&self, height: f32) -> Vec3 {
        Vec3::new(
            self.transform.translation.x,
            height,
            self.transform.translation.z,
        )
    }
}

/// Delivery counters rebuilt atomically from map-authored machine spots.
#[derive(Resource, Clone, Debug, Default)]
pub struct DeliveryStations {
    stations: std::collections::BTreeMap<DeliveryLane, DeliveryStation>,
}

impl DeliveryStations {
    pub fn station(&self, preferred: DeliveryLane) -> DeliveryStation {
        self.stations
            .get(&preferred)
            .or_else(|| self.stations.get(&DeliveryLane::Public))
            .copied()
            .unwrap_or_else(|| DeliveryStation {
                transform: Transform::from_translation(Vec3::new(
                    COUNTER_SPOT.x,
                    0.0,
                    COUNTER_DROP_Z,
                ))
                .with_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
            })
    }

    fn rebuild_from(&mut self, spots: &MachineSpots) {
        self.stations.clear();
        for (_, placement) in spots.iter() {
            if placement.kind != MachineKind::DeliveryWindow {
                continue;
            }
            let lane = placement.lane.unwrap_or(DeliveryLane::Public);
            self.stations.entry(lane).or_insert(DeliveryStation {
                transform: placement.transform,
            });
        }
    }
}

#[cfg(feature = "trenchbroom")]
fn machine_kind_named(name: &str) -> Option<MachineKind> {
    let name = name.trim();
    MachineKind::ALL
        .into_iter()
        .find(|kind| format!("{kind:?}") == name)
}

/// Present only after the current station layout has been collected and its
/// navigation graph rebuilt.
///
/// This is intentionally a marker resource, not a boolean resource: systems
/// can use Bevy's `resource_exists::<MapReady>` run condition and cannot
/// accidentally treat an initialized-but-false value as ready.
#[derive(Resource, Debug, Clone, Copy)]
pub struct MapReady;

/// Semantic id of the walkable bridge through the lab's exterior door.
///
/// The powered proximity door does not collapse this bridge while shut: crew
/// need a complete route through it before they can approach and trigger its
/// sensor. The id remains map-authored so validation can require exactly one
/// entrance connection.
pub const LAB_ENTRANCE_BRIDGE_ID: &str = "lab_entrance";

pub(crate) const WALL_THICKNESS: f32 = 0.25;
/// Width of every doorway. The walkable slab through a door is this less the
/// player's diameter, so much under 1.4 becomes a funnel you have to aim at.
pub(crate) const DOOR_WIDTH: f32 = 1.8;
/// Head height of a doorway. The wall above one is filled in, so you cannot see
/// over the top into the gap between two rooms.
pub(crate) const DOOR_HEIGHT: f32 = 2.3;

/// Centre of the crew door, in world x. Lines up with [`COUNTER_SPOT`] so crew
/// walk a straight line in from the station to the counter.
pub const CREW_DOOR_X: f32 = 4.0;
/// The crew door's opening, for anything that needs to stand in or near it.
pub const DOOR_MIN_X: f32 = CREW_DOOR_X - DOOR_WIDTH * 0.5;
pub const DOOR_MAX_X: f32 = CREW_DOOR_X + DOOR_WIDTH * 0.5;

/// Where crew queue up to collect an order: the public side of the counter.
pub const COUNTER_SPOT: Vec3 = Vec3::new(4.0, 0.0, 5.6);

/// The counter's worktop height, and the middle of it in z.
///
/// Everything that arrives or leaves is set down here — incoming crates, produce,
/// sample vials — so the two live next to [`COUNTER_SPOT`] rather than being
/// re-derived. Three modules each hardcoded the height and offset from the queue
/// spot, all of which had to agree with the delivery window's own size and with
/// nothing holding them to it; moving the counter left a vial hovering in mid-air
/// a metre and a half short of it.
pub const COUNTER_TOP: f32 = 1.15;
pub const COUNTER_DROP_Z: f32 = 4.6;

/// Where a chemist starts their shift: the middle of the hall floor, facing the
/// machines along the north wall.
pub const SPAWN_SPOT: Vec3 = Vec3::new(0.0, 0.0, 0.0);

/// Where medical leave a chemist they had to carry out — just inside the crew
/// door, off to one side so they do not wake up inside the counter.
pub const MEDBAY_DROP: Vec3 = Vec3::new(DOOR_MIN_X - 0.6, 0.0, 6.3);

/// A rectangular floor area, in world XZ. The suite is the union of these.
pub struct Room {
    pub name: &'static str,
    pub min_x: f32,
    pub max_x: f32,
    pub min_z: f32,
    pub max_z: f32,
    /// Floor tint. Each room gets its own, so you can tell from the doorway
    /// which one you are looking into.
    floor: [f32; 3],
    /// Ceiling light colour, for the same reason: the reaction bay reads cold
    /// and the storeroom warm before you have identified a single machine.
    light: [f32; 3],
}

impl Room {
    pub fn center(&self) -> Vec3 {
        Vec3::new(
            (self.min_x + self.max_x) * 0.5,
            0.0,
            (self.min_z + self.max_z) * 0.5,
        )
    }

    fn width(&self) -> f32 {
        self.max_x - self.min_x
    }

    fn depth(&self) -> f32 {
        self.max_z - self.min_z
    }

    /// The room's floor as a walkable region, held `margin` clear of the walls.
    fn inset(&self, margin: f32) -> Bounds {
        Bounds {
            min_x: self.min_x + margin,
            max_x: self.max_x - margin,
            min_z: self.min_z + margin,
            max_z: self.max_z - margin,
        }
    }
}

/// Indices into [`ROOMS`], for the places that need a room by name.
pub const HALL: usize = 0;
pub const REACTION_BAY: usize = 1;
pub const ANALYSIS: usize = 2;
pub const PREP: usize = 3;
pub const LOBBY: usize = 4;

/// The floor plan.
///
/// Laid out around the work rather than around symmetry. The mixing hall holds
/// the core loop and every other room hangs off it; the lobby has the only
/// external door, so crew and couriers never set foot on the working floor.
pub const ROOMS: [Room; 5] = [
    // The mixing hall. ChemMaster 5000 and Mixing Chamber stay in here
    // together: they are the two halves of every single order, and a wall
    // between them would tax the common case instead of the interesting one.
    Room {
        name: "Mixing Hall",
        min_x: -7.5,
        max_x: 7.5,
        min_z: -5.5,
        max_z: 2.0,
        floor: [0.22, 0.23, 0.26],
        light: [1.00, 0.96, 0.90],
    },
    // Reaction bay, off the hall's west end, walled off deliberately: the
    // coolant vent hazard is contained by the room instead of washing over half
    // the lab, and a hot batch has to be carried out through a doorway and
    // across the hall to the Mixing Chamber. That walk is the deadline heating
    // has always needed, and now it is one you can see.
    Room {
        name: "Reaction Bay",
        min_x: -13.5,
        max_x: -7.5,
        min_z: -5.5,
        max_z: -1.5,
        floor: [0.18, 0.21, 0.26],
        light: [0.74, 0.85, 1.00],
    },
    // Analysis, off the hall's east end. The analyzer is for finding out what
    // you have made, and is not on the critical path of an order — its own
    // room means working that out never has you standing in the way of a
    // delivery. (Used to share this room with the test bench; the space it
    // left is still here, unclaimed.)
    Room {
        name: "Analysis",
        min_x: 7.5,
        max_x: 13.5,
        min_z: -5.5,
        max_z: -0.5,
        floor: [0.26, 0.28, 0.30],
        light: [0.90, 0.97, 1.00],
    },
    // Prep and storage. Opens onto both the hall and the lobby, and that second
    // door is the point: produce is dropped at the counter next door, so the
    // grinder sits a few steps from where the crate lands rather than across the
    // whole suite.
    Room {
        name: "Prep & Storage",
        min_x: -7.5,
        max_x: 0.5,
        min_z: 2.0,
        max_z: 6.0,
        floor: [0.24, 0.22, 0.19],
        light: [1.00, 0.90, 0.74],
    },
    // The lobby: the public half of the lab. Everything that arrives or leaves
    // crosses the counter in here, and the only door to the station is in its
    // south wall.
    Room {
        name: "Lobby",
        min_x: 0.5,
        max_x: 7.5,
        min_z: 2.0,
        max_z: 7.0,
        floor: [0.20, 0.24, 0.22],
        light: [0.92, 1.00, 0.95],
    },
];

/// A straight run of wall, with its doorways cut out.
///
/// Written out rather than derived from room edges, because an edge does not
/// know whether it is an outer wall, a wall shared with the next room, or a
/// shared wall with a door in it. Twelve runs is little enough to read at a
/// glance, and `every_room_is_walled_in` catches a gap left by a mistyped span.
pub(crate) struct WallRun {
    /// True if the run travels along x — that is, a north or south wall.
    pub(crate) along_x: bool,
    /// The fixed axis: z for a run along x, x for a run along z.
    fixed: f32,
    from: f32,
    to: f32,
    /// Doorway centres, measured along the run's own axis.
    doors: &'static [f32],
}

#[rustfmt::skip]
const WALLS: [WallRun; 12] = [
    // North face of the whole suite. Reaction bay, hall and analysis all start
    // at z = -5.5, so it is one unbroken run.
    WallRun { along_x: true, fixed: -5.5, from: -13.5, to: 13.5, doors: &[] },
    // Reaction bay's south face.
    WallRun { along_x: true, fixed: -1.5, from: -13.5, to: -7.5, doors: &[] },
    // Analysis' south face.
    WallRun { along_x: true, fixed: -0.5, from: 7.5, to: 13.5, doors: &[] },
    // Hall's south face, entirely internal: prep behind the west half, lobby
    // behind the east half, one door into each.
    WallRun { along_x: true, fixed: 2.0, from: -7.5, to: 7.5, doors: &[-4.0, 5.0] },
    // Prep's south face.
    WallRun { along_x: true, fixed: 6.0, from: -7.5, to: 0.5, doors: &[] },
    // Lobby's south face, and the only way in from the rest of the station.
    WallRun { along_x: true, fixed: 7.0, from: 0.5, to: 7.5, doors: &[CREW_DOOR_X] },

    // Reaction bay's west face.
    WallRun { along_x: false, fixed: -13.5, from: -5.5, to: -1.5, doors: &[] },
    // Between the hall and the reaction bay.
    WallRun { along_x: false, fixed: -7.5, from: -5.5, to: -1.5, doors: &[-3.5] },
    // West face of the hall's southern half and of prep, in one run — the
    // reaction bay stops at z = -1.5, so everything below that is outer wall.
    WallRun { along_x: false, fixed: -7.5, from: -1.5, to: 6.0, doors: &[] },
    // Between prep and the lobby, continuing as the lobby's west face past
    // where prep ends at z = 6.0.
    WallRun { along_x: false, fixed: 0.5, from: 2.0, to: 7.0, doors: &[4.0] },
    // Between the hall and analysis, continuing as outer wall for the hall and
    // the lobby past where analysis ends at z = -0.5.
    WallRun { along_x: false, fixed: 7.5, from: -5.5, to: 7.0, doors: &[-3.0] },
    // Analysis' east face.
    WallRun { along_x: false, fixed: 13.5, from: -5.5, to: -0.5, doors: &[] },
];

impl WallRun {
    /// The solid stretches of this run, as spans along its own axis.
    ///
    /// Extended half a wall thickness past each end so runs overlap where they
    /// meet, which fills the notch that would otherwise be left at every corner.
    fn segments(&self) -> Vec<(f32, f32)> {
        let start = self.from - WALL_THICKNESS * 0.5;
        let end = self.to + WALL_THICKNESS * 0.5;

        let mut gaps: Vec<(f32, f32)> = self
            .doors
            .iter()
            .map(|center| (center - DOOR_WIDTH * 0.5, center + DOOR_WIDTH * 0.5))
            .collect();
        gaps.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut segments = Vec::new();
        let mut cursor = start;
        for (gap_start, gap_end) in gaps {
            if gap_start > cursor {
                segments.push((cursor, gap_start));
            }
            cursor = cursor.max(gap_end);
        }
        if cursor < end {
            segments.push((cursor, end));
        }
        segments
    }

    /// Centre of a span on this run, in world space.
    pub(crate) fn point(&self, along: f32) -> Vec3 {
        if self.along_x {
            Vec3::new(along, 0.0, self.fixed)
        } else {
            Vec3::new(self.fixed, 0.0, along)
        }
    }

    /// A box of `length` along the run and `WALL_THICKNESS` across it.
    fn extent(&self, length: f32, height: f32) -> Vec3 {
        if self.along_x {
            Vec3::new(length, height, WALL_THICKNESS)
        } else {
            Vec3::new(WALL_THICKNESS, height, length)
        }
    }
}

/// Every doorway in the suite, as (the run it is cut into, its centre).
///
/// `pub(crate)`: `door::spawn_doors` walks this to place the fallback lab's
/// physical door. Authored navigation identifies its matching walkable bridge
/// semantically rather than depending on this iterator's order.
pub(crate) fn doorways() -> impl Iterator<Item = (&'static WallRun, f32)> {
    WALLS
        .iter()
        .flat_map(|run| run.doors.iter().map(move |center| (run, *center)))
}

pub struct LabPlugin;

impl Plugin for LabPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(feature = "trenchbroom")]
        app.add_plugins(tb::LabTrenchBroomPlugin);

        // Where a body may stand. Under the `trenchbroom` feature this is
        // filled from the map's `func_walkable` volumes by [`tb`]'s map-ready
        // collector.
        app.init_resource::<WalkableAreas>()
            .init_resource::<CrisisSpots>()
            .init_resource::<MachineSpots>()
            .init_resource::<DeliveryStations>()
            .init_resource::<DoorSpots>()
            // Readiness belongs to a single visit. `nav` inserts it only after
            // the replacement floor plan has produced a graph.
            .add_systems(OnExit(AppState::Playing), clear_map_runtime);
        #[cfg(not(feature = "trenchbroom"))]
        app.add_systems(OnEnter(AppState::Playing), seed_legacy_map_runtime);

        // The room itself is scenery: identical on both ends, derived from
        // constants, and nothing about it is worth a packet. Under the
        // `trenchbroom` feature the shell and its fixtures come out of
        // `assets/maps/lab.map` instead — see [`tb`].
        #[cfg(not(feature = "trenchbroom"))]
        app.add_systems(
            OnEnter(AppState::Playing),
            (spawn_shell, spawn_fixtures)
                .chain()
                .after(load_machine_assets),
        );

        app.add_systems(OnEnter(AppState::Playing), load_machine_assets)
            .add_systems(
                Update,
                rebuild_delivery_stations.run_if(in_state(AppState::Playing)),
            )
            // The map loads asynchronously; its collected spot registry is the
            // signal that authority-owned machine state can be reconciled.
            .add_systems(
                Update,
                reconcile_machine_spots
                    .run_if(in_state(AppState::Playing))
                    .run_if(is_authority),
            )
            // Presentation runs everywhere, using the replicated transform on
            // clients rather than deriving placement from machine kind.
            .add_systems(
                Update,
                dress_machines
                    .after(reconcile_machine_spots)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

fn clear_map_runtime(world: &mut World) {
    world.remove_resource::<MapReady>();
    world.insert_resource(WalkableAreas::default());
    world.insert_resource(CrisisSpots::default());
    world.insert_resource(MachineSpots::default());
    world.insert_resource(DeliveryStations::default());
    world.insert_resource(DoorSpots::default());
    world.insert_resource(crate::crew::Departments::default());
    world.insert_resource(crate::nav::NavGraph::default());
}

/// Gives the compact const-table fallback the same semantic marker interface
/// as the authored station.
#[cfg(not(feature = "trenchbroom"))]
fn seed_legacy_map_runtime(mut commands: Commands) {
    commands.remove_resource::<MapReady>();

    // Automatic doors stay traversable to route planning even when their
    // leaves are shut. Otherwise a visitor cannot plan close enough to trigger
    // the proximity sensor, leaving orders stranded until a chemist opens it.
    commands.insert_resource(WalkableAreas::from_floor_plan());

    let mut spots = CrisisSpots::default();
    for (id, at) in [
        ("cult.wet_chalk_sigil", Vec3::new(-2.8, 1.0, 0.2)),
        ("cult.whispering_residue", Vec3::new(-2.2, 1.0, 2.4)),
        ("cult.bleeding_offering_bowl", Vec3::new(-3.5, 1.0, 2.0)),
        ("cult.scorched_invocation", Vec3::new(-1.7, 1.0, 0.8)),
        ("cult.airless_candle", Vec3::new(-3.0, 1.0, 3.0)),
        ("cult.rift_seal_scar", Vec3::new(-1.5, 1.0, 2.9)),
        ("hazard.rad_leak", Vec3::new(-4.0, 0.0, -4.2)),
        ("hazard.coolant_vent", Vec3::new(-12.9, 0.0, -3.5)),
        ("showdown.breach", Vec3::new(-3.2, 1.0, 1.4)),
    ] {
        spots.insert(id, Transform::from_translation(at));
    }
    commands.insert_resource(spots);
    commands.insert_resource(legacy_machine_spots());

    let (run, center) = doorways()
        .find(|(run, center)| {
            let at = run.point(*center);
            (at.x - CREW_DOOR_X).abs() < 0.001 && (at.z - ROOMS[LOBBY].max_z).abs() < 0.001
        })
        .expect("legacy lab entrance doorway exists");
    let at = run.point(center);
    let rotation = if run.along_x {
        Quat::IDENTITY
    } else {
        Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)
    };
    let mut doors = DoorSpots::default();
    doors.insert(
        "door.lab.public",
        LAB_ENTRANCE_BRIDGE_ID,
        Transform::from_translation(at).with_rotation(rotation),
    );
    commands.insert_resource(doors);
}

fn rebuild_delivery_stations(spots: Res<MachineSpots>, mut stations: ResMut<DeliveryStations>) {
    if spots.is_changed() {
        stations.rebuild_from(&spots);
    }
}

/// An axis-aligned obstruction the player cannot walk through.
///
/// Deliberately not a physics engine: a lab is a set of boxes with boxes in it,
/// and swept-AABB resolution against a handful of statics is all that needs.
/// `Reflect` because the TrenchBroom path spawns these inside a loaded scene,
/// and the scene spawner refuses to write any component it cannot find in the
/// type registry. Harmless for the hand-built lab, which spawns them directly.
#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct Solid {
    pub half_extents: Vec3,
}

/// The highest surface anything can be set down on, in metres.
///
/// A box whose top is above this is something you walk into rather than
/// something you put a beaker on: every wall, and the 1.7 m machine casings.
/// The benches at 0.9 and the delivery counter at 1.15 are under it, which is
/// exactly the line a chemist's hands draw anyway.
pub const SET_DOWN_REACH: f32 = 1.4;

/// Where something set down at `spot` actually comes to rest.
///
/// Deliberately not a physics drop — nothing in the lab falls. An item is
/// *placed*, so the only question is which surface is underneath the placement,
/// and the answer is the highest [`Solid`] top below [`SET_DOWN_REACH`] whose
/// footprint covers the spot. Before this, dropping was a hardcoded `y = 0.08`,
/// which put a beaker set down at a bench through the bench and onto the floor
/// inside it — visible from nowhere and reachable from nowhere.
///
/// `solids` are `(centre, half-extents)` pairs, so this is testable without a
/// world to raycast against. `fallback` is somewhere known to be clear — the
/// chemist's own feet — used when the spot itself is inside a wall or a machine,
/// which is what happens when you drop while facing one at arm's length.
pub fn resting_place(spot: Vec3, fallback: Vec3, solids: &[(Vec3, Vec3)]) -> Vec3 {
    // Straddling the reach line is what makes a box an obstruction: it is too
    // tall to rest anything on and too solid to rest anything inside. A ceiling
    // slab starts above the line and a bench top ends below it, so neither is
    // one.
    let obstructed = |point: Vec3| {
        solids.iter().any(|(center, half)| {
            center.y - half.y < SET_DOWN_REACH
                && center.y + half.y > SET_DOWN_REACH
                && (point.x - center.x).abs() < half.x
                && (point.z - center.z).abs() < half.z
        })
    };

    let spot = if obstructed(spot) { fallback } else { spot };

    let mut surface = 0.0;
    for (center, half) in solids {
        let top = center.y + half.y;
        if top > SET_DOWN_REACH || top <= surface {
            continue;
        }
        if (spot.x - center.x).abs() > half.x || (spot.z - center.z).abs() > half.z {
            continue;
        }
        surface = top;
    }

    Vec3::new(spot.x, surface, spot.z)
}

/// The glowing panel on the front of a machine.
///
/// An axis-aligned rectangle of floor, in world XZ.
///
/// `Reflect` because the map backend measures these inside a loaded scene, and
/// the scene spawner refuses any component it cannot find in the type registry.
#[derive(Clone, Copy, Debug, Default, Reflect)]
pub struct Bounds {
    pub min_x: f32,
    pub max_x: f32,
    pub min_z: f32,
    pub max_z: f32,
}

impl Bounds {
    /// An inverted rectangle — `is_standable()` is false and `holds()` is
    /// false for every point. Available for a genuinely impassable dynamic
    /// barrier; the powered lab entrance deliberately never uses it because
    /// approaching crew must be able to plan through its proximity sensor.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const EMPTY: Bounds = Bounds {
        min_x: 0.0,
        max_x: -1.0,
        min_z: 0.0,
        max_z: -1.0,
    };

    pub fn holds(&self, point: Vec3) -> bool {
        point.x >= self.min_x
            && point.x <= self.max_x
            && point.z >= self.min_z
            && point.z <= self.max_z
    }

    /// The nearest point inside, at the same height.
    pub fn nearest(&self, point: Vec3) -> Vec3 {
        Vec3::new(
            point.x.clamp(self.min_x, self.max_x),
            point.y,
            point.z.clamp(self.min_z, self.max_z),
        )
    }

    /// This rectangle held `margin` clear of its own edges.
    pub fn inset(&self, margin: f32) -> Bounds {
        Bounds {
            min_x: self.min_x + margin,
            max_x: self.max_x - margin,
            min_z: self.min_z + margin,
            max_z: self.max_z - margin,
        }
    }

    /// Whether two rectangles share any floor. Navigation treats this as an
    /// edge: if a body can be in both, it can walk from one to the other.
    pub fn overlaps(&self, other: &Bounds) -> bool {
        self.min_x <= other.max_x
            && other.min_x <= self.max_x
            && self.min_z <= other.max_z
            && other.min_z <= self.max_z
    }

    /// The rectangle two overlapping regions share, or `None` if they do not.
    /// Navigation walks a body through the middle of this.
    pub fn intersection(&self, other: &Bounds) -> Option<Bounds> {
        self.overlaps(other).then(|| Bounds {
            min_x: self.min_x.max(other.min_x),
            max_x: self.max_x.min(other.max_x),
            min_z: self.min_z.max(other.min_z),
            max_z: self.max_z.min(other.max_z),
        })
    }

    /// The middle of the rectangle, on the floor.
    pub fn center(&self) -> Vec3 {
        Vec3::new(
            (self.min_x + self.max_x) * 0.5,
            0.0,
            (self.min_z + self.max_z) * 0.5,
        )
    }

    /// A region with no interior is one a margin has collapsed — a room too
    /// small to stand in, or a doorway narrower than the player. Skipped rather
    /// than clamped into, because clamping into an inverted rectangle throws the
    /// player at a corner.
    pub fn is_standable(&self) -> bool {
        self.min_x <= self.max_x && self.min_z <= self.max_z
    }
}

/// The widest body containment has to work for. The chemist is 0.35.
const MAX_BODY_RADIUS: f32 = 0.4;

/// How far a doorway bridge reaches past a wall's centre-line into each room.
///
/// A bridge has to still overlap both rooms' floors *after* all three have been
/// inset by the body radius. Each room's floor pulls back by one radius and the
/// bridge by another, so anything under twice the radius leaves a gap and seals
/// the two rooms apart — which is exactly the bug `contain` was written to fix,
/// reintroduced from the other direction.
const DOOR_BRIDGE_REACH: f32 = 2.0 * MAX_BODY_RADIUS + WALL_THICKNESS * 0.5;

/// The walkable bridge for the doorway cut into `run` at `center`.
/// Factored out of [`WalkableAreas::from_floor_plan`] so every fallback
/// doorway is connected using exactly the same clearance calculation.
fn doorway_bridge_bounds(run: &WallRun, center: f32) -> Bounds {
    let (half_x, half_z) = if run.along_x {
        (DOOR_WIDTH * 0.5, DOOR_BRIDGE_REACH)
    } else {
        (DOOR_BRIDGE_REACH, DOOR_WIDTH * 0.5)
    };
    let at = run.point(center);
    Bounds {
        min_x: at.x - half_x,
        max_x: at.x + half_x,
        min_z: at.z - half_z,
        max_z: at.z + half_z,
    }
}

/// A rectangle of standable floor.
#[derive(Debug, Clone, Copy, PartialEq, Reflect)]
pub enum FloorProfile {
    Flat(f32),
    LinearX { at_min: f32, at_max: f32 },
    LinearZ { at_min: f32, at_max: f32 },
}

impl Default for FloorProfile {
    fn default() -> Self {
        Self::Flat(0.0)
    }
}

impl FloorProfile {
    pub fn height_at(self, bounds: Bounds, point: Vec3) -> f32 {
        let interpolate = |value: f32, min: f32, max: f32, at_min: f32, at_max: f32| {
            let span = max - min;
            if span.abs() <= f32::EPSILON {
                at_min
            } else {
                let t = ((value - min) / span).clamp(0.0, 1.0);
                at_min + (at_max - at_min) * t
            }
        };
        match self {
            Self::Flat(y) => y,
            Self::LinearX { at_min, at_max } => {
                interpolate(point.x, bounds.min_x, bounds.max_x, at_min, at_max)
            }
            Self::LinearZ { at_min, at_max } => {
                interpolate(point.z, bounds.min_z, bounds.max_z, at_min, at_max)
            }
        }
    }
}

/// One map-authored walkable brush and the height of its standing surface.
#[derive(Debug, Clone, Copy, Reflect)]
pub struct WalkableSurface {
    pub bounds: Bounds,
    pub floor: FloorProfile,
}

/// A rectangle of standable floor.
#[derive(Debug, Clone)]
pub struct Region {
    pub bounds: Bounds,
    /// The authored/open shape. A dynamic blocker may collapse `bounds`, but
    /// reopening never has to reconstruct map geometry from Rust constants.
    #[cfg_attr(not(test), allow(dead_code))]
    pub open_bounds: Bounds,
    /// Which room this is part of, or `None` for a doorway bridge: a threshold
    /// belongs to neither of the rooms it joins.
    pub room: Option<String>,
    /// Stable map-authored identity for a dynamic bridge, if this is one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub bridge_id: Option<String>,
    /// The elevation of this floor. Sloped profiles are used by stair runs;
    /// ordinary rooms and corridors remain flat.
    pub floor: FloorProfile,
}

impl Region {
    pub fn floor_at(&self, point: Vec3) -> f32 {
        self.floor.height_at(self.bounds, point)
    }
}

/// Every place a body may stand.
///
/// The single answer to "may I be here?", and what navigation is built from.
/// Filled by whichever backend owns the floor plan — [`ROOMS`]/[`WALLS`] in a
/// plain build, `func_walkable` volumes out of the map under the `trenchbroom`
/// feature — and read by code that knows about neither.
///
/// Regions are stored *raw*, un-inset. Callers pass the radius of the body they
/// are asking about, because the chemist and a crew member are not the same
/// size and the floor plan should not have to be rebuilt to say so.
#[derive(Resource, Default)]
pub struct WalkableAreas {
    regions: Vec<Region>,
}

impl WalkableAreas {
    /// Builds the areas from the const floor plan.
    pub fn from_floor_plan() -> Self {
        let mut areas = Self::default();

        for room in &ROOMS {
            areas.push(room.inset(0.0), Some(room.name.to_string()));
        }

        // A slab bridging each doorway. These are what make the suite one
        // space: two rooms inset from their shared wall leave a gap wider than
        // the wall itself, so without a region spanning the opening a chemist
        // would be sealed into whichever room they started in.
        for (index, (run, center)) in doorways().enumerate() {
            let at = run.point(center);
            let bridge_id = if (at.x - CREW_DOOR_X).abs() < 0.001
                && (at.z - ROOMS[LOBBY].max_z).abs() < 0.001
            {
                LAB_ENTRANCE_BRIDGE_ID.to_string()
            } else {
                format!("legacy_doorway_{index}")
            };
            areas.push_with_bridge(doorway_bridge_bounds(run, center), None, Some(bridge_id));
        }

        areas
    }

    pub fn push(&mut self, bounds: Bounds, room: Option<String>) {
        self.push_with_bridge(bounds, room, None);
    }

    /// Adds an authored region, optionally naming the dynamic bridge it forms.
    pub fn push_with_bridge(
        &mut self,
        bounds: Bounds,
        room: Option<String>,
        bridge_id: Option<String>,
    ) {
        self.push_surface(bounds, room, bridge_id, FloorProfile::default());
    }

    /// Adds an authored region at a particular elevation or along a stair.
    pub fn push_surface(
        &mut self,
        bounds: Bounds,
        room: Option<String>,
        bridge_id: Option<String>,
        floor: FloorProfile,
    ) {
        self.regions.push(Region {
            bounds,
            open_bounds: bounds,
            room,
            bridge_id,
            floor,
        });
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// Compatibility helper for a genuinely impassable fallback barrier that
    /// still identifies a doorway through the const floor-plan iterator.
    /// Automatic proximity doors must remain traversable and must not call it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_door_blocked(&mut self, doorway_index: usize, blocked: bool) {
        let Some((run, center)) = doorways().nth(doorway_index) else {
            return;
        };
        let at = run.point(center);
        let id = if (at.x - CREW_DOOR_X).abs() < 0.001 && (at.z - ROOMS[LOBBY].max_z).abs() < 0.001
        {
            LAB_ENTRANCE_BRIDGE_ID.to_string()
        } else {
            format!("legacy_doorway_{doorway_index}")
        };
        self.set_bridge_blocked(&id, blocked);
    }

    /// Opens or closes every authored bridge with this semantic id.
    ///
    /// Returning the number matched makes a missing/mistyped id observable to
    /// callers and tests without making a temporarily unloaded map an error.
    /// This is reserved for barriers actors cannot open by approaching; using
    /// it for a proximity door creates a pathfinding/sensor deadlock.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_bridge_blocked(&mut self, bridge_id: &str, blocked: bool) -> usize {
        let mut matched = 0;
        for region in &mut self.regions {
            if region.bridge_id.as_deref() != Some(bridge_id) {
                continue;
            }
            region.bounds = if blocked {
                Bounds::EMPTY
            } else {
                region.open_bounds
            };
            matched += 1;
        }
        matched
    }

    /// Keeps `position` on the floor.
    ///
    /// The solid walls do the real work; this is the backstop that stops a body
    /// leaving through a corner or a seam between two wall runs. It replaced a
    /// single rectangular clamp, which was correct only while the lab was one
    /// room and would have sealed every side room off the moment it was not.
    ///
    /// An empty set of areas returns `position` untouched rather than dragging
    /// the body to the origin — under the map backend there is a frame or two
    /// before the scene has loaded, and a chemist must not be flung across the
    /// station while waiting for it.
    /// Keeps a body on the vertically nearest authored floor.
    ///
    /// `body_offset` is the distance from the standing surface to the entity's
    /// origin (eye height for a player, root height for a crew model). It lets
    /// ground-floor and underfloor regions overlap in XZ without teleporting a
    /// body between them, while a stair profile provides the continuous link.
    pub fn contain_on_surface(&self, position: Vec3, radius: f32, body_offset: f32) -> Vec3 {
        // Two tiers, not one flat contest. A region whose inset genuinely
        // *holds* (x, z) is somewhere the body is actually standing; a region
        // that only offers its *nearest boundary point* is a fallback for
        // when nothing holds at all (the dead strip between two regions'
        // insets, narrower than `radius` on each side). Held candidates must
        // always beat clamped ones regardless of raw distance.
        //
        // The bug this tier split fixes: on a stair mid-slope, the stair's
        // own region holds (x, z) exactly, but a merely-clamped neighbour —
        // a flat landing or a stray flat tile — can still report a *smaller*
        // 3D distance, because clamping never has to pay for a height
        // change and the slope does. Comparing flat/held candidates as one
        // pool let the flat one win every time, permanently pinning a
        // descending body at its entry height. See
        // `player::tests::a_chemist_can_walk_down_every_maintenance_stair`.
        //
        // The clamped tier compares *horizontal* distance only, for the same
        // reason: it exists for the dead strip between two regions' insets
        // (up to `radius` on each side, so up to `2 * radius` wide) where
        // neither one holds yet, and nothing has been entered there for a
        // height to matter to.
        //
        // The held tier has a subtler version of the same problem: two
        // regions both holding the same (x, z) is a real, wanted case (a
        // sloped landing sitting on top of the corridor it bridges to), and
        // *there* a flat region's "zero height change" is a permanent,
        // structural advantage over a sloped one's "correct, non-zero
        // change" — not a coincidence of one frame's numbers. So slope beats
        // flat outright when both hold; height distance from the current
        // position is the tiebreaker only when both candidates are equally
        // flat or equally sloped, which is what keeps a body on the floor it
        // is already standing near instead of snapping through it into an
        // underfloor region that overlaps the same (x, z) — the case the
        // body_offset doc comment above describes.
        let mut best_held: Option<(f32, bool, Vec3)> = None;
        let mut best_clamped: Option<(f32, Vec3)> = None;

        for region in &self.regions {
            let inset = region.bounds.inset(radius);
            if !inset.is_standable() {
                continue;
            }
            let holds = inset.holds(position);
            let horizontal = if holds { position } else { inset.nearest(position) };
            let floor_y = region.floor_at(horizontal);
            let candidate = Vec3::new(horizontal.x, floor_y + body_offset, horizontal.z);
            if holds {
                let sloped = !matches!(region.floor, FloorProfile::Flat(_));
                let distance = candidate.distance_squared(position);
                let better = match best_held {
                    None => true,
                    Some((_, nearest_sloped, _)) if sloped != nearest_sloped => sloped,
                    Some((nearest, _, _)) => distance < nearest,
                };
                if better {
                    best_held = Some((distance, sloped, candidate));
                }
            } else {
                let distance = (horizontal.x - position.x).powi(2) + (horizontal.z - position.z).powi(2);
                if best_clamped.is_none_or(|(nearest, _)| distance < nearest) {
                    best_clamped = Some((distance, candidate));
                }
            }
        }

        best_held
            .map(|(_, _, point)| point)
            .or(best_clamped.map(|(_, point)| point))
            .unwrap_or(position)
    }

    /// Which room a point stands in, if any.
    ///
    /// Returns `None` in a doorway or a wall: rooms are the open floor between
    /// the walls, and a threshold belongs to neither of the rooms it joins.
    pub fn room_at(&self, point: Vec3) -> Option<&str> {
        self.regions
            .iter()
            .filter(|region| region.room.is_some() && region.bounds.holds(point))
            .min_by(|a, b| {
                (a.floor_at(point) - point.y)
                    .abs()
                    .total_cmp(&(b.floor_at(point) - point.y).abs())
            })
            .and_then(|region| region.room.as_deref())
    }
}

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
            crate::until_we_leave_the_lab(),
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
            crate::until_we_leave_the_lab(),
        ))
        .id()
}

fn spawn_shell(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));

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

    // Floors and ceilings, one slab per room, overhanging by a wall thickness so
    // no seam shows where a floor meets the wall standing on it.
    for room in &ROOMS {
        let [r, g, b] = room.floor;
        let floor = materials.add(StandardMaterial {
            base_color: Color::srgb(r, g, b),
            perceptual_roughness: 0.85,
            ..default()
        });

        let span = Vec3::new(
            room.width() + WALL_THICKNESS * 2.0,
            0.1,
            room.depth() + WALL_THICKNESS * 2.0,
        );
        decor_box(
            &mut commands,
            &cube,
            &floor,
            room.center() + Vec3::Y * -0.05,
            span,
        );
        decor_box(
            &mut commands,
            &cube,
            &ceiling,
            room.center() + Vec3::Y * (ROOM_HEIGHT + 0.05),
            span,
        );
    }

    // Walls, cut around their doorways.
    for run in &WALLS {
        for (from, to) in run.segments() {
            let length = to - from;
            if length <= 0.0 {
                continue;
            }
            solid_box(
                &mut commands,
                &cube,
                &wall,
                run.point((from + to) * 0.5) + Vec3::Y * (ROOM_HEIGHT * 0.5),
                run.extent(length, ROOM_HEIGHT),
            );
        }
    }

    for (run, center) in doorways() {
        // Fill the wall above the opening. Without this you see straight over
        // the top of every internal wall into the next room's ceiling void.
        let header = ROOM_HEIGHT - DOOR_HEIGHT;
        decor_box(
            &mut commands,
            &cube,
            &wall,
            run.point(center) + Vec3::Y * (DOOR_HEIGHT + header * 0.5),
            run.extent(DOOR_WIDTH, header),
        );

        // A painted threshold, so a doorway reads as a doorway from across the
        // room. This replaces the single hazard stripe the one-room lab had:
        // with five rooms, what a floor stripe is for is telling you where the
        // exits are, and now every doorway says so.
        let threshold = if run.along_x {
            Vec3::new(DOOR_WIDTH, 0.02, WALL_THICKNESS + 0.4)
        } else {
            Vec3::new(WALL_THICKNESS + 0.4, 0.02, DOOR_WIDTH)
        };
        decor_box(
            &mut commands,
            &cube,
            &stripe,
            run.point(center) + Vec3::Y * 0.005,
            threshold,
        );
    }

    // Lighting. A few warm points beat one bright source for making the
    // primitive geometry read as a room.
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.62, 0.68, 0.80),
        brightness: 220.0,
        ..default()
    });

    for (index, room) in ROOMS.iter().enumerate() {
        let [r, g, b] = room.light;
        let base_color = Color::srgb(r, g, b);
        for spot in light_spots(room) {
            commands.spawn((
                PointLight {
                    intensity: 260_000.0,
                    range: 18.0,
                    // Only the hall casts shadows. Eleven shadow-mapped points
                    // is a lot of render target for a lab you can cross in six
                    // seconds, and the side rooms read fine on their tint alone.
                    shadow_maps_enabled: index == HALL,
                    color: base_color,
                    ..default()
                },
                Transform::from_translation(spot),
                LabLight { base_color },
                crate::until_we_leave_the_lab(),
            ));
        }
    }
}

/// Marks a ceiling light as one `crisis::pulse_alert_lighting` can pull
/// toward red and back. `base_color` is the room's own tint, captured at
/// spawn time — the lerp target to return to once a crisis clears, so red
/// alert never permanently overwrites what a room actually looks like.
#[derive(Component)]
pub struct LabLight {
    pub base_color: Color,
}

/// Ceiling light positions for a room: one every few metres along its long
/// axis, so a fifteen-metre hall is not lit by a single hotspot.
fn light_spots(room: &Room) -> Vec<Vec3> {
    const SPACING: f32 = 5.5;

    let center = room.center();
    let count = (room.width() / SPACING).ceil().max(1.0);
    let step = room.width() / count;
    let first = room.min_x + step * 0.5;

    (0..count as usize)
        .map(|index| Vec3::new(first + index as f32 * step, ROOM_HEIGHT - 0.35, center.z))
        .collect()
}

/// Collision and authored-model geometry at one concrete placement.
///
/// Kind owns dimensions and sockets; a map spot owns position and orientation.
/// The split permits repeated kinds without a second placement table.
struct MachineFit {
    base: Vec3,
    size: Vec3,
    rotation: Quat,
    /// The face the screen sits on, and the side the player walks up to.
    facing: Vec3,
}

/// Standing depth of a cabinet-style machine, and how far its base sits off the
/// wall behind it.
const CABINET: Vec3 = Vec3::new(1.5, 1.7, 0.8);
const OFF_WALL: f32 = 0.45;

/// Model-space dimensions. Width is local X and working depth is local Z;
/// placement rotation turns these into the world-space AABB used by `Solid`.
fn machine_local_size(kind: MachineKind) -> Vec3 {
    match kind {
        MachineKind::ChemMaster5000
        | MachineKind::MixingChamber
        | MachineKind::Grinder
        | MachineKind::Analyzer
        | MachineKind::Locker
        | MachineKind::StandingBoard => CABINET,
        MachineKind::ReactionChamber => Vec3::new(1.4, 1.5, 1.0),
        MachineKind::DeliveryWindow => Vec3::new(3.4, COUNTER_TOP, 0.7),
    }
}

fn placement_transform(base: Vec3, facing: Vec3) -> Transform {
    Transform::from_translation(base).with_rotation(Quat::from_rotation_y(facing.x.atan2(facing.z)))
}

/// The supported no-map layout, expressed through the same stable-id registry
/// populated by TrenchBroom in the default build.
fn legacy_machine_spots() -> MachineSpots {
    let hall = &ROOMS[HALL];
    let north = |room: &Room, x: f32| {
        placement_transform(Vec3::new(x, 0.0, room.min_z + OFF_WALL), Vec3::Z)
    };
    let mut spots = MachineSpots::default();
    for (id, kind, transform) in [
        (
            "dispenser.a",
            MachineKind::ChemMaster5000,
            north(hall, -5.4),
        ),
        ("mixer.a", MachineKind::MixingChamber, north(hall, -2.2)),
        ("dispenser.b", MachineKind::ChemMaster5000, north(hall, 1.4)),
        // Nudged east of the nominal 4.6 m lane centre so its casing and
        // standing point clear the authored Chemistry fume hood and island.
        ("mixer.b", MachineKind::MixingChamber, north(hall, 5.4)),
        (
            "grinder.main",
            MachineKind::Grinder,
            placement_transform(
                Vec3::new(-4.0, 0.0, ROOMS[PREP].max_z - OFF_WALL),
                Vec3::NEG_Z,
            ),
        ),
        (
            "analyzer.main",
            MachineKind::Analyzer,
            north(&ROOMS[ANALYSIS], 10.5),
        ),
        (
            "delivery.public",
            MachineKind::DeliveryWindow,
            placement_transform(Vec3::new(COUNTER_SPOT.x, 0.0, COUNTER_DROP_Z), Vec3::NEG_Z),
        ),
        (
            "board.main",
            MachineKind::StandingBoard,
            placement_transform(Vec3::new(hall.max_x - 0.4, 0.0, 0.5), Vec3::NEG_X),
        ),
        (
            "reactor.main",
            MachineKind::ReactionChamber,
            placement_transform(
                Vec3::new(ROOMS[REACTION_BAY].min_x + 0.55, 0.0, -3.5),
                Vec3::X,
            ),
        ),
        (
            "locker.main",
            MachineKind::Locker,
            north(&ROOMS[PREP], -1.5),
        ),
    ] {
        spots.insert(id, kind, transform);
    }
    let delivery = spots
        .get("delivery.public")
        .expect("legacy delivery window exists")
        .clone();
    spots.insert_with_lane(
        "delivery.public",
        delivery.kind,
        delivery.transform,
        Some(DeliveryLane::Public),
    );
    debug_assert!(
        MachineKind::ALL
            .into_iter()
            .all(|kind| spots.iter().any(|(_, placement)| placement.kind == kind)),
        "the fallback registry must represent every machine kind",
    );
    spots
}

impl MachineFit {
    fn from_placement(kind: MachineKind, placement: Transform) -> Self {
        let local_size = machine_local_size(kind);
        let right = placement.rotation * Vec3::X;
        let up = placement.rotation * Vec3::Y;
        let forward = placement.rotation * Vec3::Z;
        MachineFit {
            base: placement.translation,
            size: right.abs() * local_size.x
                + up.abs() * local_size.y
                + forward.abs() * local_size.z,
            rotation: placement.rotation,
            facing: forward.normalize_or_zero(),
        }
    }

    fn from_root(kind: MachineKind, root: Transform) -> Self {
        let mut placement = root;
        placement.translation -= root.rotation * Vec3::Y * (machine_local_size(kind).y * 0.5);
        Self::from_placement(kind, placement)
    }

    fn root_transform(&self) -> Transform {
        Transform::from_translation(self.center()).with_rotation(self.rotation)
    }

    fn center(&self) -> Vec3 {
        self.base + Vec3::Y * (self.size.y * 0.5)
    }

    /// GLBs are authored floor-first with local `+Z` as their working face.
    /// The gameplay root stays at the collider centre and carries the authored
    /// rotation, so the child only drops by half the collider height.
    fn visual_transform(&self) -> Transform {
        Transform::from_translation(Vec3::NEG_Y * (self.size.y * 0.5))
    }

    /// Maps a floor-origin point from an authored GLB into an offset from the
    /// gameplay root at the collider centre. This is the same transform used by
    /// the visual child, so a code-owned slot and its visible dock cannot drift.
    fn visual_point_offset(&self, point: Vec3) -> Vec3 {
        let visual = self.visual_transform();
        self.rotation * (visual.translation + point)
    }
}

/// Container dock points copied from `station_starter_kit_manifest.json`'s
/// `gltf_xyz_m` entries. GLBs use floor-origin local coordinates with `+Z` as
/// their working face; [`MachineFit::visual_point_offset`] performs the one
/// conversion needed by both north/south and east/west placements.
fn authored_container_sockets(kind: MachineKind) -> (Option<Vec3>, Option<Vec3>) {
    let a = |point| (Some(point), None);

    match kind {
        MachineKind::ChemMaster5000 => a(Vec3::new(0.0, 1.77, 0.10)),
        MachineKind::MixingChamber => (
            Some(Vec3::new(-0.28, 1.77, 0.08)),
            Some(Vec3::new(0.28, 1.77, 0.08)),
        ),
        MachineKind::Grinder => a(Vec3::new(0.30, 1.14, 0.08)),
        MachineKind::Analyzer => a(Vec3::new(-0.16, 1.43, 0.04)),
        MachineKind::ReactionChamber => a(Vec3::new(0.0, 1.58, 0.13)),
        MachineKind::DeliveryWindow => a(Vec3::new(0.0, 1.27, 0.14)),
        MachineKind::StandingBoard | MachineKind::Locker => (None, None),
    }
}

/// Local, render-only child of a replicated machine entity.
#[derive(Component)]
struct MachineVisual;

const STATION_KIT: &str = "3dassets/station_starter_kit/glb";

/// Primitive fixture assets plus the eight authored machine scenes.
#[derive(Resource)]
pub(crate) struct MachineAssets {
    cube: Handle<Mesh>,
    bench: Handle<StandardMaterial>,
    chem_master: Handle<WorldAsset>,
    mixing_chamber: Handle<WorldAsset>,
    grinder: Handle<WorldAsset>,
    analyzer: Handle<WorldAsset>,
    delivery_window: Handle<WorldAsset>,
    standing_board: Handle<WorldAsset>,
    reaction_chamber: Handle<WorldAsset>,
    locker: Handle<WorldAsset>,
}

impl MachineAssets {
    fn model(&self, kind: MachineKind) -> Handle<WorldAsset> {
        match kind {
            MachineKind::ChemMaster5000 => self.chem_master.clone(),
            MachineKind::MixingChamber => self.mixing_chamber.clone(),
            MachineKind::Grinder => self.grinder.clone(),
            MachineKind::Analyzer => self.analyzer.clone(),
            MachineKind::DeliveryWindow => self.delivery_window.clone(),
            MachineKind::StandingBoard => self.standing_board.clone(),
            MachineKind::ReactionChamber => self.reaction_chamber.clone(),
            MachineKind::Locker => self.locker.clone(),
        }
    }
}

pub(crate) fn load_machine_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let model = |file: &'static str| {
        asset_server.load(GltfAssetLabel::Scene(0).from_asset(format!("{STATION_KIT}/{file}")))
    };

    commands.insert_resource(MachineAssets {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        bench: materials.add(StandardMaterial {
            base_color: Color::srgb(0.30, 0.33, 0.38),
            perceptual_roughness: 0.6,
            metallic: 0.3,
            ..default()
        }),
        chem_master: model("machine_chem_master_5000.glb"),
        mixing_chamber: model("machine_mixing_chamber.glb"),
        grinder: model("machine_reagent_grinder.glb"),
        analyzer: model("machine_sample_analyzer.glb"),
        delivery_window: model("machine_delivery_window.glb"),
        standing_board: model("machine_standing_board.glb"),
        reaction_chamber: model("machine_reaction_chamber.glb"),
        locker: model("machine_storage_locker.glb"),
    });
}

/// Plain benches, so the rooms are not just corridors.
///
/// Scenery rather than equipment: no state, nothing to replicate, and the
/// server needs them present to collide against.
fn spawn_fixtures(mut commands: Commands, assets: Res<MachineAssets>) {
    // An island down the middle of the hall. Standing off the walls is the
    // point: it gives the room a route around it instead of a single lane, and
    // somewhere to put a beaker down that is not on top of a machine.
    for x in [-3.5f32, 2.0] {
        solid_box(
            &mut commands,
            &assets.cube,
            &assets.bench,
            Vec3::new(x, 0.45, -2.2),
            Vec3::new(2.6, 0.9, 0.9),
        );
    }

    // Shelving down the storeroom's west wall.
    for z in [3.1f32, 4.9] {
        solid_box(
            &mut commands,
            &assets.cube,
            &assets.bench,
            Vec3::new(ROOMS[PREP].min_x + 0.5, 0.45, z),
            Vec3::new(1.0, 0.9, 1.4),
        );
    }

    // A bench under the analysis room's west wall, for staging samples.
    solid_box(
        &mut commands,
        &assets.cube,
        &assets.bench,
        Vec3::new(9.0, 0.45, -1.4),
        Vec3::new(2.2, 0.9, 0.8),
    );
}

/// Authority-local identity used to reconcile hot-reloaded map markers.
#[derive(Component, Debug)]
struct MachineSpotId(String);

/// Creates one machine and its equipment-specific state. Authority only.
/// Presentation remains local to each peer through [`dress_machines`].
fn spawn_machine_at(commands: &mut Commands, id: &str, placement: &MachinePlacement) -> Entity {
    let kind = placement.kind;
    let fit = MachineFit::from_placement(kind, placement.transform);
    let machine = commands
        .spawn((
            Name::new(format!("{} [{id}]", kind.label())),
            MachineSpotId(id.to_string()),
            Machine::new(kind),
            fit.root_transform(),
            bevy_replicon::prelude::Replicated,
            crate::until_we_leave_the_lab(),
        ))
        .id();

    if let Some(lane) = placement.lane {
        commands.entity(machine).insert(lane);
    }

    match kind {
        MachineKind::ChemMaster5000 => {
            commands.entity(machine).insert(DispenseAmount::default());
        }
        MachineKind::MixingChamber => {
            commands
                .entity(machine)
                .insert(Buffer(Solution::new(Units::whole(300))));
        }
        MachineKind::Grinder => {
            commands.entity(machine).insert(Hopper::default());
        }
        MachineKind::ReactionChamber => {
            commands.entity(machine).insert(Thermostat::default());
        }
        MachineKind::Analyzer
        | MachineKind::DeliveryWindow
        | MachineKind::StandingBoard
        | MachineKind::Locker => {}
    }
    machine
}

/// Reconciles authority-owned state with the current map marker registry.
///
/// Repeated kinds are expected; only the stable spot id is unique. Reusing an
/// id during map hot reload preserves that machine's state while applying its
/// newly authored transform.
fn reconcile_machine_spots(
    mut commands: Commands,
    spots: Res<MachineSpots>,
    existing: Query<(Entity, &MachineSpotId, &Machine)>,
) {
    if !spots.is_changed() {
        return;
    }
    if spots.is_empty() {
        for (entity, ..) in &existing {
            commands.entity(entity).despawn();
        }
        return;
    }
    debug!("reconciling {} map-authored machine spots", spots.len());

    let mut found = std::collections::HashSet::new();
    for (entity, id, machine) in &existing {
        let Some(placement) = spots.get(&id.0) else {
            commands.entity(entity).despawn();
            continue;
        };
        found.insert(id.0.clone());
        if placement.kind != machine.kind {
            commands.entity(entity).despawn();
            spawn_machine_at(&mut commands, &id.0, placement);
            continue;
        }

        let fit = MachineFit::from_placement(placement.kind, placement.transform);
        let mut entity_commands = commands.entity(entity);
        entity_commands.insert(fit.root_transform());
        if let Some(lane) = placement.lane {
            entity_commands.insert(lane);
        } else {
            entity_commands.remove::<DeliveryLane>();
        }
    }

    for (id, placement) in spots.iter() {
        if !found.contains(id) {
            spawn_machine_at(&mut commands, id, placement);
        }
    }
}

/// Gives a machine its authored visual scene, simple collider and slot.
///
/// Runs on both ends. Position and orientation are replicated authority state;
/// only presentation, collision and socket offsets are derived locally.
#[derive(Component)]
pub(crate) struct MachineDressed;

fn fallback_machine_fit(kind: MachineKind) -> MachineFit {
    let spots = legacy_machine_spots();
    let placement = spots
        .iter()
        .find_map(|(_, placement)| (placement.kind == kind).then_some(placement))
        .expect("every machine kind has a legacy placement");
    MachineFit::from_placement(kind, placement.transform)
}

pub(crate) fn dress_machines(
    mut commands: Commands,
    assets: Option<Res<MachineAssets>>,
    machines: Query<(
        Entity,
        &Machine,
        Option<Ref<Transform>>,
        Option<&MachineDressed>,
    )>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, machine, transform, dressed) in &machines {
        if dressed.is_some()
            && transform
                .as_ref()
                .is_none_or(|transform| !transform.is_changed())
        {
            continue;
        }

        let kind = machine.kind;
        let fit = transform
            .as_deref()
            .map(|transform| MachineFit::from_root(kind, *transform))
            .unwrap_or_else(|| fallback_machine_fit(kind));

        let visual_transform = fit.visual_transform();
        commands
            .entity(entity)
            .insert((
                Solid {
                    half_extents: fit.size * 0.5,
                },
                // Which way the machine's working face points. Static geometry
                // like the slot below, so it is derived here on both ends
                // rather than replicated — and needed on the authority,
                // because "in front of this machine" is where anything it
                // hands back has to end up.
                Facing(fit.facing),
                Interactable::new(kind.label()),
                MachineDressed,
                // The authored visual is a child. Visibility propagation in
                // Bevy requires every parent in that hierarchy to carry the
                // visibility components too; Mesh3d used to add them for us.
                Visibility::default(),
            ))
            .with_children(|parent| {
                if dressed.is_none() {
                    parent.spawn((
                        Name::new(format!("{} visual", kind.label())),
                        MachineVisual,
                        WorldAssetRoot(assets.model(kind)),
                        visual_transform,
                    ));
                }
            });
        if transform.is_none() {
            commands.entity(entity).insert(fit.root_transform());
        }

        // Slot positions are authored in the GLB and copied into the manifest.
        // They are static geometry, so they are derived rather than replicated.
        //
        // The standing board has none, deliberately: walking up holding a
        // beaker would park it on the board instead of opening the panel, and
        // the player would have no way of telling why the board had stopped
        // responding. The locker has none for the opposite reason — it takes
        // what is in your hand *inside*, and a slot would catch the first
        // beaker on the roof instead.
        match authored_container_sockets(kind) {
            (Some(slot_a), Some(slot_b)) => {
                commands.entity(entity).insert((
                    ContainerSlot {
                        offset: fit.visual_point_offset(slot_a),
                    },
                    ContainerSlotB {
                        offset: fit.visual_point_offset(slot_b),
                    },
                ));
            }
            (Some(slot_a), None) => {
                commands.entity(entity).insert(ContainerSlot {
                    offset: fit.visual_point_offset(slot_a),
                });
            }
            (None, None) => {}
            (None, Some(_)) => unreachable!("slot B cannot exist without slot A"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The radius the real player controller contains bodies at.
    const BODY: f32 = 0.35;

    /// A lab as a joining client builds one: no machines of its own, and the
    /// asset handles ready for whatever the server sends.
    fn client_lab() -> App {
        let mut app = App::new();
        app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_asset::<WorldAsset>()
            .add_systems(Startup, load_machine_assets)
            .add_systems(Update, dress_machines);
        app
    }

    /// The name of the room a point stands in, for readable assertion messages.
    ///
    /// Reads the const plan directly rather than going through
    /// [`WalkableAreas`], so these tests keep asserting against the floor plan
    /// as written even once the map is what fills the resource at runtime.
    fn room_name(point: Vec3) -> Option<&'static str> {
        ROOMS
            .iter()
            .find(|room| room.inset(0.0).holds(point))
            .map(|room| room.name)
    }

    #[test]
    fn a_machine_that_arrives_over_the_wire_is_built_out_in_full() {
        // Replication carries the machine's state and nothing else — no visual,
        // collider or label. Before this split the client spawned its own
        // machines instead, which raycast fine and were then meaningless to the
        // server: interacting sent it an entity id it had never issued.
        let mut app = client_lab();
        let arrived = app
            .world_mut()
            .spawn(Machine::new(MachineKind::ChemMaster5000))
            .id();

        app.update();

        let world = app.world();
        let visual = world
            .get::<Children>(arrived)
            .and_then(|children| {
                children
                    .iter()
                    .find(|child| world.get::<MachineVisual>(*child).is_some())
            })
            .expect("an undressed machine is an invisible one");
        assert!(world.get::<WorldAssetRoot>(visual).is_some());
        assert_eq!(world.get::<Transform>(arrived).unwrap().scale, Vec3::ONE);
        assert!(
            world.get::<Visibility>(arrived).is_some(),
            "the GLB child needs a visible hierarchy root"
        );
        assert_eq!(
            world.get::<Transform>(visual).unwrap().translation.y,
            -machine_local_size(MachineKind::ChemMaster5000).y * 0.5
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
    fn the_two_machines_that_must_not_catch_a_beaker_have_no_slot() {
        // The board, because parking a beaker on it instead of opening the
        // panel would leave nothing to explain why it had stopped responding.
        // The locker, because what you are carrying belongs *inside* it, and a
        // slot would catch the first beaker on the roof.
        let mut app = client_lab();
        let slotless: Vec<Entity> = [MachineKind::StandingBoard, MachineKind::Locker]
            .into_iter()
            .map(|kind| app.world_mut().spawn(Machine::new(kind)).id())
            .collect();

        app.update();

        for machine in slotless {
            assert!(app.world().get::<ContainerSlot>(machine).is_none());
        }
    }

    #[test]
    fn container_slots_land_on_every_authored_glb_socket() {
        let mut app = client_lab();
        let authored = [
            (
                MachineKind::ChemMaster5000,
                Vec3::new(0.0, 1.77, 0.10),
                None,
            ),
            (
                MachineKind::MixingChamber,
                Vec3::new(-0.28, 1.77, 0.08),
                Some(Vec3::new(0.28, 1.77, 0.08)),
            ),
            (MachineKind::Grinder, Vec3::new(0.30, 1.14, 0.08), None),
            (MachineKind::Analyzer, Vec3::new(-0.16, 1.43, 0.04), None),
            (
                MachineKind::ReactionChamber,
                Vec3::new(0.0, 1.58, 0.13),
                None,
            ),
            (
                MachineKind::DeliveryWindow,
                Vec3::new(0.0, 1.27, 0.14),
                None,
            ),
        ];
        let machines: Vec<_> = authored
            .iter()
            .map(|(kind, _, _)| (*kind, app.world_mut().spawn(Machine::new(*kind)).id()))
            .collect();

        app.update();

        for ((kind, slot_a, slot_b), (_, entity)) in authored.into_iter().zip(machines) {
            let fit = fallback_machine_fit(kind);
            let yaw = Quat::from_rotation_y(fit.facing.x.atan2(fit.facing.z));
            let root = app.world().get::<Transform>(entity).unwrap().translation;
            let actual_a = root + app.world().get::<ContainerSlot>(entity).unwrap().offset;
            let expected_a = fit.base + yaw * slot_a;
            assert!(
                actual_a.distance(expected_a) < 0.000_01,
                "{kind:?} slot A is at {actual_a}, expected authored socket {expected_a}"
            );

            match slot_b {
                Some(slot_b) => {
                    let actual_b = root + app.world().get::<ContainerSlotB>(entity).unwrap().offset;
                    let expected_b = fit.base + yaw * slot_b;
                    assert!(
                        actual_b.distance(expected_b) < 0.000_01,
                        "{kind:?} slot B is at {actual_b}, expected authored socket {expected_b}"
                    );
                }
                None => assert!(app.world().get::<ContainerSlotB>(entity).is_none()),
            }
        }
    }

    #[test]
    fn every_machine_knows_which_way_it_faces() {
        // What ejecting reads to decide where a beaker goes. The old fixed
        // `+Z` was right for the two machines on the hall's north wall and
        // wrong for every other one — the grinder faces `-Z`, so its beaker
        // went through the storeroom wall.
        let mut app = client_lab();
        let machines: Vec<(MachineKind, Entity)> = MachineKind::ALL
            .into_iter()
            .map(|kind| (kind, app.world_mut().spawn(Machine::new(kind)).id()))
            .collect();

        app.update();

        for (kind, machine) in machines {
            let facing = app
                .world()
                .get::<Facing>(machine)
                .unwrap_or_else(|| panic!("{kind:?} has no facing"));
            assert_eq!(
                facing.0,
                fallback_machine_fit(kind).facing,
                "{kind:?} is dressed facing a different way from its fit"
            );
        }
    }

    #[test]
    fn a_beaker_set_down_at_a_bench_lands_on_it() {
        // The bug this replaced a constant to fix: the drop height was a
        // hardcoded 0.08, so a beaker set down at a bench went *through* the
        // bench and came to rest inside it — invisible, and unreachable by the
        // crosshair that would have picked it back up.
        let bench = (Vec3::new(0.0, 0.45, 0.0), Vec3::new(1.3, 0.45, 0.45));

        let on_top = resting_place(Vec3::new(0.2, 1.7, 0.1), Vec3::ZERO, &[bench]);
        assert_eq!(on_top.y, 0.9, "the bench top is 0.9 m up");
        assert_eq!((on_top.x, on_top.z), (0.2, 0.1), "and it stays where put");

        // Just past the end of the bench is floor, not bench.
        let beside_it = resting_place(Vec3::new(2.0, 1.7, 0.1), Vec3::ZERO, &[bench]);
        assert_eq!(beside_it.y, 0.0);
    }

    #[test]
    fn nothing_is_ever_set_down_inside_a_wall() {
        // Dropping while facing a wall at arm's length puts the spot inside it.
        // Without the fallback the beaker would sit in the masonry; with a
        // naive "highest surface" rule and no reach limit it would sit on top
        // of the wall, three metres up. Neither is somewhere a chemist can
        // reach, which is the only thing setting something down is for.
        let wall = (
            Vec3::new(0.0, ROOM_HEIGHT * 0.5, 3.0),
            Vec3::new(6.0, ROOM_HEIGHT * 0.5, WALL_THICKNESS * 0.5),
        );
        let feet = Vec3::new(0.5, 1.7, 2.0);

        let resting = resting_place(Vec3::new(0.5, 1.7, 3.0), feet, &[wall]);
        assert_eq!(
            resting,
            Vec3::new(0.5, 0.0, 2.0),
            "a blocked spot falls back to the chemist's own feet, on the floor"
        );
    }

    #[test]
    fn a_ceiling_is_neither_a_surface_nor_an_obstruction() {
        // Both halves of `resting_place` key off the reach line, and a slab
        // entirely above it has to fail both tests: counted as an obstruction
        // it would make every square metre of the lab undroppable, and counted
        // as a surface it would put beakers on the roof.
        let ceiling = (
            Vec3::new(0.0, ROOM_HEIGHT + 0.05, 0.0),
            Vec3::new(6.0, 0.05, 6.0),
        );
        let spot = Vec3::new(1.0, 1.7, 1.0);

        let resting = resting_place(spot, Vec3::ZERO, &[ceiling]);
        assert_eq!(resting, Vec3::new(1.0, 0.0, 1.0));
    }

    #[test]
    fn dressing_preserves_the_replicated_machine_transform() {
        let mut app = client_lab();
        let authored = Transform::from_xyz(23.0, 0.85, -17.0)
            .with_rotation(Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2));
        let machine = app
            .world_mut()
            .spawn((Machine::new(MachineKind::ChemMaster5000), authored))
            .id();

        app.update();

        assert_eq!(app.world().get::<Transform>(machine), Some(&authored));
        assert!(
            app.world()
                .get::<Facing>(machine)
                .unwrap()
                .0
                .distance(Vec3::NEG_X)
                < 0.000_01,
            "facing must come from the replicated rotation",
        );
    }

    #[test]
    fn no_two_machines_are_stacked_in_the_same_spot() {
        let spots = legacy_machine_spots();
        let fits: Vec<_> = spots
            .iter()
            .map(|(id, placement)| {
                (
                    id,
                    MachineFit::from_placement(placement.kind, placement.transform),
                )
            })
            .collect();
        for (index, (id, fit)) in fits.iter().enumerate() {
            for (other_id, other) in &fits[index + 1..] {
                assert_ne!(
                    fit.center(),
                    other.center(),
                    "{id} and {other_id} occupy the same place"
                );
            }
        }
    }

    #[test]
    fn the_fallback_registry_spawns_two_independent_core_lanes() {
        let spots = legacy_machine_spots();
        assert_eq!(spots.len(), 10);

        let mut app = App::new();
        app.insert_resource(spots)
            .add_systems(Update, reconcile_machine_spots);
        app.update();

        let mut machines = app
            .world_mut()
            .query::<(Entity, &MachineSpotId, &Machine, &Transform)>();
        let placed: Vec<_> = machines
            .iter(app.world())
            .map(|(entity, id, machine, transform)| {
                (entity, id.0.clone(), machine.kind, *transform)
            })
            .collect();
        assert_eq!(placed.len(), 10);
        for kind in MachineKind::ALL {
            let expected = if matches!(
                kind,
                MachineKind::ChemMaster5000 | MachineKind::MixingChamber
            ) {
                2
            } else {
                1
            };
            assert_eq!(
                placed
                    .iter()
                    .filter(|(_, _, found, _)| *found == kind)
                    .count(),
                expected,
                "wrong fallback count for {kind:?}",
            );
        }

        let (original, _, _, _) = placed
            .iter()
            .find(|(_, id, _, _)| id == "dispenser.a")
            .expect("first dispenser");
        let moved = placement_transform(Vec3::new(-6.2, 0.0, -5.05), Vec3::X);
        app.world_mut().resource_mut::<MachineSpots>().insert(
            "dispenser.a",
            MachineKind::ChemMaster5000,
            moved,
        );
        app.update();

        let same = app
            .world_mut()
            .query::<(Entity, &MachineSpotId, &Transform)>()
            .iter(app.world())
            .find(|(_, id, _)| id.0 == "dispenser.a")
            .map(|(entity, _, transform)| (entity, *transform))
            .expect("moved dispenser");
        assert_eq!(
            same.0, *original,
            "moving a spot should preserve machine state"
        );
        assert_eq!(
            same.1,
            MachineFit::from_placement(MachineKind::ChemMaster5000, moved).root_transform(),
        );
    }

    #[test]
    fn every_machine_stands_inside_a_room() {
        // The failure this catches is a silent one: a fit whose coordinates fall
        // in the gap between two rooms still spawns, still replicates and still
        // draws — as a machine embedded in a wall, or floating in the void
        // outside, reachable from nowhere.
        for (id, placement) in legacy_machine_spots().iter() {
            let kind = placement.kind;
            let fit = MachineFit::from_placement(kind, placement.transform);
            let room = room_name(fit.base)
                .unwrap_or_else(|| panic!("{id} stands outside every room at {}", fit.base));

            // And its casing must fit, not just its centre line.
            let corners = [
                fit.base + Vec3::new(fit.size.x, 0.0, fit.size.z) * 0.5,
                fit.base - Vec3::new(fit.size.x, 0.0, fit.size.z) * 0.5,
                fit.base + Vec3::new(fit.size.x, 0.0, -fit.size.z) * 0.5,
                fit.base + Vec3::new(-fit.size.x, 0.0, fit.size.z) * 0.5,
            ];
            for corner in corners {
                assert_eq!(
                    room_name(corner),
                    Some(room),
                    "{id} ({kind:?}) overhangs {room} at {corner}"
                );
            }
        }
    }

    /// Whether the wall line through `from..to` is unbroken, counting every run
    /// lying on it. An edge may take more than one run to cover: the hall's west
    /// side is a shared wall down to the reaction bay's corner and an outer wall
    /// from there on, which is two runs on the same line.
    fn line_is_covered(along_x: bool, fixed: f32, from: f32, to: f32) -> bool {
        let mut spans: Vec<(f32, f32)> = WALLS
            .iter()
            .filter(|run| run.along_x == along_x && (run.fixed - fixed).abs() < 0.001)
            .map(|run| (run.from, run.to))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut reached = from;
        for (start, end) in spans {
            if start > reached + 0.001 {
                break;
            }
            reached = reached.max(end);
        }
        reached >= to - 0.001
    }

    #[test]
    fn every_room_is_walled_in() {
        // Walls are hand-written spans, so a mistyped coordinate leaves a hole
        // rather than a compile error. Every edge of every room has to be walled
        // along its whole length or the suite leaks into the void — doorways
        // excepted, which `every_doorway_is_a_door_to_somewhere` covers.
        for room in &ROOMS {
            let edges = [
                (true, room.min_z, room.min_x, room.max_x),
                (true, room.max_z, room.min_x, room.max_x),
                (false, room.min_x, room.min_z, room.max_z),
                (false, room.max_x, room.min_z, room.max_z),
            ];

            for (along_x, fixed, from, to) in edges {
                let axis = if along_x { "z" } else { "x" };
                assert!(
                    line_is_covered(along_x, fixed, from, to),
                    "{}'s {axis} = {fixed} edge from {from} to {to} is not walled all the way",
                    room.name
                );
            }
        }
    }

    #[test]
    fn every_doorway_is_a_door_to_somewhere() {
        // A door cut into the wrong wall is a hole to space; a door cut past the
        // end of the neighbouring room opens onto the gap between two rooms.
        // Both look exactly like walls until you try to walk through one.
        //
        // The lobby's crew door is the deliberate exception, and pinning that
        // there is only one of them is the point: a second external door would
        // be a way into the lab that skips the counter entirely.
        let step = WALL_THICKNESS * 0.5 + 0.05;
        let mut external = Vec::new();

        for (run, center) in doorways() {
            let at = run.point(center);
            let across = if run.along_x { Vec3::Z } else { Vec3::X };

            match (room_name(at + across * step), room_name(at - across * step)) {
                (Some(near), Some(far)) => assert_ne!(
                    near, far,
                    "the door at {at} has the same room on both sides"
                ),
                (Some(_), None) | (None, Some(_)) => external.push(at),
                (None, None) => panic!("the door at {at} opens onto nothing at all"),
            }
        }

        assert_eq!(
            external.len(),
            1,
            "the lab has exactly one way in from the station, found {external:?}"
        );
        let station_door = external[0];
        assert!(
            (station_door.x - CREW_DOOR_X).abs() < 0.001
                && (station_door.z - ROOMS[LOBBY].max_z).abs() < 0.001,
            "the only external door must be the lobby's crew door, not {station_door}"
        );
    }

    #[test]
    fn no_doorway_is_blocked_by_a_machine() {
        // A machine parked across an opening seals a room off. It would still
        // draw and still work, and the room behind it would simply be
        // unreachable.
        for (run, center) in doorways() {
            let door = run.point(center);
            let half = if run.along_x {
                Vec3::new(DOOR_WIDTH * 0.5, 0.0, WALL_THICKNESS * 0.5 + BODY)
            } else {
                Vec3::new(WALL_THICKNESS * 0.5 + BODY, 0.0, DOOR_WIDTH * 0.5)
            };

            for (id, placement) in legacy_machine_spots().iter() {
                let fit = MachineFit::from_placement(placement.kind, placement.transform);
                let gap = (fit.base - door).abs();
                let clearance = half + Vec3::new(fit.size.x, 0.0, fit.size.z) * 0.5;
                assert!(
                    gap.x >= clearance.x || gap.z >= clearance.z,
                    "{id} blocks the doorway at {door}"
                );
            }
        }
    }

    #[test]
    fn every_room_has_somewhere_to_stand() {
        // That the rooms are *connected* is `nav`'s to prove, against the graph
        // crew actually walk — this only checks the floor plan gives each room a
        // region wide enough for a body once its walls are held clear.
        let areas = WalkableAreas::from_floor_plan();

        for room in &ROOMS {
            let region = areas
                .regions()
                .iter()
                .find(|region| region.room.as_deref() == Some(room.name))
                .unwrap_or_else(|| panic!("{} has no walkable region at all", room.name));
            assert!(
                region.bounds.inset(BODY).is_standable(),
                "{} is too narrow for a chemist to stand in",
                room.name
            );
        }
    }

    #[test]
    fn blocking_a_door_seals_it_and_reopening_recovers_the_original_bridge() {
        // Genuinely impassable dynamic barriers may still collapse a bridge;
        // this exercises the floor-plan math without needing a full app. The
        // powered proximity entrance deliberately does not use this operation.
        for index in 0..doorways().count() {
            let mut areas = WalkableAreas::from_floor_plan();
            let healthy = areas.regions()[ROOMS.len() + index].bounds;

            areas.set_door_blocked(index, true);
            let blocked = areas.regions()[ROOMS.len() + index].bounds;
            assert!(
                !blocked.is_standable(),
                "doorway {index} still stands after being blocked"
            );

            areas.set_door_blocked(index, false);
            let reopened = areas.regions()[ROOMS.len() + index].bounds;
            assert!(
                (reopened.min_x - healthy.min_x).abs() < 0.001
                    && (reopened.max_x - healthy.max_x).abs() < 0.001
                    && (reopened.min_z - healthy.min_z).abs() < 0.001
                    && (reopened.max_z - healthy.max_z).abs() < 0.001,
                "doorway {index} did not recover its original bridge: {reopened:?} vs {healthy:?}"
            );
        }
    }

    #[test]
    fn a_semantic_bridge_reopens_to_its_authored_bounds() {
        // Map bridges are not derivable from WALLS. Their own open shape is the
        // only correct thing to restore after a door moves out of the way.
        let open = Bounds {
            min_x: 31.25,
            max_x: 34.75,
            min_z: -9.0,
            max_z: -7.25,
        };
        let room = Bounds {
            min_x: -4.0,
            max_x: 4.0,
            min_z: -3.0,
            max_z: 3.0,
        };
        let other_bridge = Bounds {
            min_x: 12.0,
            max_x: 13.0,
            min_z: 6.0,
            max_z: 8.0,
        };
        let mut areas = WalkableAreas::default();
        areas.push(room, Some("Unrelated Room".to_string()));
        areas.push_with_bridge(open, None, Some(LAB_ENTRANCE_BRIDGE_ID.to_string()));
        areas.push_with_bridge(other_bridge, None, Some("some_other_door".to_string()));

        let same = |actual: Bounds, expected: Bounds| {
            (actual.min_x - expected.min_x).abs() < 0.001
                && (actual.max_x - expected.max_x).abs() < 0.001
                && (actual.min_z - expected.min_z).abs() < 0.001
                && (actual.max_z - expected.max_z).abs() < 0.001
        };

        assert_eq!(areas.set_bridge_blocked(LAB_ENTRANCE_BRIDGE_ID, true), 1);
        assert!(same(areas.regions()[0].bounds, room));
        assert!(!areas.regions()[1].bounds.is_standable());
        assert!(same(areas.regions()[2].bounds, other_bridge));
        assert_eq!(areas.set_bridge_blocked(LAB_ENTRANCE_BRIDGE_ID, false), 1);
        let reopened = areas.regions()[1].bounds;
        assert!((reopened.min_x - open.min_x).abs() < 0.001);
        assert!((reopened.max_x - open.max_x).abs() < 0.001);
        assert!((reopened.min_z - open.min_z).abs() < 0.001);
        assert!((reopened.max_z - open.max_z).abs() < 0.001);
    }

    #[test]
    fn crisis_spots_are_seedable_by_semantic_id() {
        let transform =
            Transform::from_xyz(8.0, 1.25, -13.0).with_rotation(Quat::from_rotation_y(0.75));
        let mut spots = CrisisSpots::default();
        assert!(spots.insert("test.incident", transform).is_none());
        let found = spots.get("test.incident").expect("inserted marker");
        assert_eq!(found.translation, transform.translation);
        assert_eq!(found.rotation, transform.rotation);
        assert!(spots.get("test.missing").is_none());
    }

    #[test]
    fn leaving_play_clears_every_map_derived_resource() {
        let mut areas = WalkableAreas::default();
        areas.push(
            Bounds {
                min_x: -2.0,
                max_x: 2.0,
                min_z: -2.0,
                max_z: 2.0,
            },
            Some("Old Session".to_string()),
        );
        let graph = crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS);
        let mut spots = CrisisSpots::default();
        spots.insert("old.incident", Transform::from_xyz(1.0, 0.0, 1.0));
        let mut departments = crate::crew::Departments::default();
        departments.set("Engineering".to_string(), Vec3::new(1.0, 0.0, 1.0));
        let mut machines = MachineSpots::default();
        machines.insert(
            "old.machine",
            MachineKind::Analyzer,
            Transform::from_xyz(2.0, 0.0, 2.0),
        );

        let mut world = World::new();
        world.insert_resource(MapReady);
        world.insert_resource(areas);
        world.insert_resource(graph);
        world.insert_resource(spots);
        world.insert_resource(machines);
        world.insert_resource(departments);

        clear_map_runtime(&mut world);

        assert!(!world.contains_resource::<MapReady>());
        assert!(world.resource::<WalkableAreas>().regions().is_empty());
        assert!(world
            .resource::<CrisisSpots>()
            .get("old.incident")
            .is_none());
        assert!(world.resource::<MachineSpots>().is_empty());
        assert!(world
            .resource::<crate::crew::Departments>()
            .home("Engineering")
            .is_none());
        assert!(world
            .resource::<crate::nav::NavGraph>()
            .path(Vec3::ZERO, Vec3::ONE)
            .is_none());
    }

    #[test]
    fn the_places_crew_and_couriers_stand_are_inside_the_lobby() {
        // These destinations are coupled to the lobby workflow. If the counter
        // moves without them, couriers wait in the wrong place with orders the
        // player cannot collect cleanly.
        let lobby = &ROOMS[LOBBY];
        for (what, spot) in [
            ("the counter queue", COUNTER_SPOT),
            ("the medbay drop", MEDBAY_DROP),
            // The produce courier unloads a lane over from the order queue.
            ("the courier lane", COUNTER_SPOT - Vec3::X * 1.1),
        ] {
            assert!(
                lobby.inset(BODY).holds(spot),
                "{what} at {spot} is not standable in the lobby"
            );
        }

        assert!(
            (CREW_DOOR_X - COUNTER_SPOT.x).abs() < 0.001,
            "crew walk straight in from the door to the counter"
        );
    }

    #[test]
    fn everything_set_down_on_the_counter_lands_on_the_counter() {
        // This drop point used to be spelled out separately in three modules, as
        // a literal height plus a metre back from the queue spot. Moving the
        // counter left sample vials hovering in mid-air a metre and a half short
        // of it, and nothing failed: a floating bottle is still pickable, so it
        // reads as a graphics glitch rather than a stale coordinate.
        let placement = legacy_machine_spots()
            .get("delivery.public")
            .expect("delivery window placement")
            .clone();
        let counter = MachineFit::from_placement(placement.kind, placement.transform);
        assert_eq!(
            COUNTER_TOP, counter.size.y,
            "things are set down at COUNTER_TOP, so it must be the counter's height"
        );

        let half_depth = counter.size.z * 0.5;
        assert!(
            (COUNTER_DROP_Z - counter.base.z).abs() <= half_depth,
            "the drop at z = {COUNTER_DROP_Z} misses the counter, which spans {} to {}",
            counter.base.z - half_depth,
            counter.base.z + half_depth
        );
    }

    #[test]
    fn the_counter_separates_the_chemist_from_the_crew() {
        // The delivery window is the one machine crew touch, and the whole point
        // of it is that they touch the other side. If the queue spot ended up
        // north of the counter, crew would walk through the lab to reach it.
        let placement = legacy_machine_spots()
            .get("delivery.public")
            .expect("delivery window placement")
            .clone();
        let counter = MachineFit::from_placement(placement.kind, placement.transform);
        let front = counter.base.z + counter.size.z * 0.5;
        assert!(
            COUNTER_SPOT.z > front,
            "crew queue at {} but the counter's public face is at {front}",
            COUNTER_SPOT.z
        );
        assert!(
            counter.facing.distance(Vec3::NEG_Z) < 0.000_01,
            "the chemist works the counter from the lobby's north side"
        );
    }

    #[test]
    fn fallback_maps_resolve_the_missing_medical_lane_to_public() {
        let spots = legacy_machine_spots();
        let mut stations = DeliveryStations::default();
        stations.rebuild_from(&spots);

        assert_eq!(
            stations.station(DeliveryLane::Medical),
            stations.station(DeliveryLane::Public),
        );
    }
}
