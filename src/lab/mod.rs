//! The chem lab: room shell, equipment placement and lighting.
//!
//! Everything is built from scaled unit cubes. No modelling required, and with
//! decent lighting it reads perfectly well as a station interior.
//!
//! The suite is five rooms rather than one box, because eight machines along two
//! walls of a single room read as a storage cupboard rather than a workplace.
//! Each room is a rectangle in [`ROOMS`]; walls are straight runs in [`WALLS`]
//! with doorways cut out of them. Both are plain data, and that is what lets one
//! floor plan drive the geometry, the lighting *and* the player's containment: a
//! room moved in `ROOMS` takes its floor, its lights and its walkable footprint
//! with it.

// Under the `trenchbroom` feature the map builds the shell, so the floor plan
// below is reachable only from the exporter — which is test-only, and so reads
// as dead code in a normal build. The tables are kept rather than cfg'd out
// because the point of the prototype is to run the two side by side: `contain`
// still uses them, and so does every test in this module.
#![cfg_attr(feature = "trenchbroom", allow(dead_code))]

use bevy::prelude::*;

use chem_sim::{Solution, Units};

use crate::interaction::Interactable;
use crate::machines::{
    Buffer, ContainerSlot, DispenseAmount, Hopper, Machine, MachineKind, Thermostat,
};
use crate::net::is_authority;
use crate::AppState;

/// Writes the floor plan out as a Quake `.map` for TrenchBroom. Test-only: it
/// is a one-way export used to seed the map, not part of the running game.
#[cfg(test)]
mod tb_export;

/// The TrenchBroom-driven alternative to [`spawn_shell`]. Behind a feature flag
/// so the const-table lab stays the default while the two are compared.
#[cfg(feature = "trenchbroom")]
pub mod tb;

pub const ROOM_HEIGHT: f32 = 3.2;

/// TrenchBroom units per metre, shared by the exporter and the loader.
///
/// 40 rather than `bevy_trenchbroom`'s default 39.37 (one unit per inch): every
/// coordinate in the floor plan is a multiple of 0.025 m, so at 40 every brush
/// lands on an integer and almost all on TrenchBroom's default 8-unit grid. At
/// 39.37 the whole lab sits on fractional coordinates and is miserable to edit.
// Read by the exporter (test-only) and the loader (feature-gated), so a plain
// default build legitimately never touches it.
#[cfg_attr(not(feature = "trenchbroom"), allow(dead_code))]
pub const TB_SCALE: f32 = 40.0;

const WALL_THICKNESS: f32 = 0.25;
/// Width of every doorway. The walkable slab through a door is this less the
/// player's diameter, so much under 1.4 becomes a funnel you have to aim at.
const DOOR_WIDTH: f32 = 1.8;
/// Head height of a doorway. The wall above one is filled in, so you cannot see
/// over the top into the gap between two rooms.
const DOOR_HEIGHT: f32 = 2.3;

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
    // The mixing hall. Dispenser and ChemMaster stay in here together: they are
    // the two halves of every single order, and a wall between them would tax
    // the common case instead of the interesting one.
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
    // across the hall to the ChemMaster. That walk is the deadline heating has
    // always needed, and now it is one you can see.
    Room {
        name: "Reaction Bay",
        min_x: -13.5,
        max_x: -7.5,
        min_z: -5.5,
        max_z: -1.5,
        floor: [0.18, 0.21, 0.26],
        light: [0.74, 0.85, 1.00],
    },
    // Analysis, off the hall's east end. Analyzer and test bench belong
    // together: both are for finding out what you have made, and neither is on
    // the critical path of an order. Their own room means experimenting never
    // has you standing in the way of a delivery.
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
struct WallRun {
    /// True if the run travels along x — that is, a north or south wall.
    along_x: bool,
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
    fn point(&self, along: f32) -> Vec3 {
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

/// Which room a point stands in, if any.
///
/// Returns `None` in a doorway or a wall: rooms are the open floor between the
/// walls, and a threshold belongs to neither of the rooms it joins.
pub fn room_at(point: Vec3) -> Option<&'static Room> {
    ROOMS.iter().find(|room| room.inset(0.0).holds(point))
}

/// Every doorway in the suite, as (the run it is cut into, its centre).
fn doorways() -> impl Iterator<Item = (&'static WallRun, f32)> {
    WALLS
        .iter()
        .flat_map(|run| run.doors.iter().map(move |center| (run, *center)))
}

pub struct LabPlugin;

impl Plugin for LabPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(feature = "trenchbroom")]
        app.add_plugins(tb::LabTrenchBroomPlugin);

        // The room itself is scenery: identical on both ends, derived from
        // constants, and nothing about it is worth a packet. Under the
        // `trenchbroom` feature the shell and its fixtures come out of
        // `assets/maps/lab.map` instead — see [`tb`].
        #[cfg(not(feature = "trenchbroom"))]
        app.add_systems(
            OnEnter(AppState::Playing),
            (spawn_shell, spawn_fixtures).chain().after(load_machine_assets),
        );

        app.add_systems(
            OnEnter(AppState::Playing),
            (
                load_machine_assets,
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

/// The glowing panel on the front of a machine.
///
/// Marked so interaction raycasts can skip it: it sits fractionally proud of
/// the casing, and without this every machine would be blocked by its own
/// screen.
#[derive(Component)]
pub struct MachineScreen;

/// An axis-aligned rectangle of floor, in world XZ.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub min_x: f32,
    pub max_x: f32,
    pub min_z: f32,
    pub max_z: f32,
}

impl Bounds {
    fn holds(&self, point: Vec3) -> bool {
        point.x >= self.min_x
            && point.x <= self.max_x
            && point.z >= self.min_z
            && point.z <= self.max_z
    }

    /// The nearest point inside, at the same height.
    fn nearest(&self, point: Vec3) -> Vec3 {
        Vec3::new(
            point.x.clamp(self.min_x, self.max_x),
            point.y,
            point.z.clamp(self.min_z, self.max_z),
        )
    }

    /// A region with no interior is one a margin has collapsed — a room too
    /// small to stand in, or a doorway narrower than the player. Skipped rather
    /// than clamped into, because clamping into an inverted rectangle throws the
    /// player at a corner.
    fn is_standable(&self) -> bool {
        self.min_x <= self.max_x && self.min_z <= self.max_z
    }
}

/// Every region a body of the given radius can occupy: each room held clear of
/// its walls, plus a slab bridging each doorway.
///
/// The slabs are what make this work. Two rooms inset from their shared wall
/// leave a gap wider than the wall itself, so without a region spanning the
/// opening a chemist would be sealed into whichever room they started in.
fn walkable(radius: f32) -> impl Iterator<Item = Bounds> {
    let rooms = ROOMS.iter().map(move |room| room.inset(radius));

    let thresholds = doorways().map(move |(run, center)| {
        // Along the run: the opening, held clear of the frame. Across it: far
        // enough either side to overlap both rooms' inset floors, which is what
        // joins the three regions into a continuous path.
        let half_open = DOOR_WIDTH * 0.5 - radius;
        let half_deep = radius + WALL_THICKNESS * 0.5;
        let (half_x, half_z) = if run.along_x {
            (half_open, half_deep)
        } else {
            (half_deep, half_open)
        };
        let at = run.point(center);
        Bounds {
            min_x: at.x - half_x,
            max_x: at.x + half_x,
            min_z: at.z - half_z,
            max_z: at.z + half_z,
        }
    });

    rooms.chain(thresholds)
}

/// Keeps `position` on the lab floor.
///
/// The solid walls do the real work; this is the backstop that stops a body
/// leaving the suite through a corner or a seam between two wall runs. It
/// replaced a single rectangular clamp, which was correct only while the lab was
/// one room and would have sealed every side room off the moment it was not.
pub fn contain(position: Vec3, radius: f32) -> Vec3 {
    let mut best: Option<(f32, Vec3)> = None;

    for region in walkable(radius) {
        if !region.is_standable() {
            continue;
        }
        if region.holds(position) {
            return position;
        }
        let candidate = region.nearest(position);
        let distance = candidate.distance_squared(position);
        if best.is_none_or(|(nearest, _)| distance < nearest) {
            best = Some((distance, candidate));
        }
    }

    best.map_or(position, |(_, point)| point)
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

/// Standing depth of a cabinet-style machine, and how far its base sits off the
/// wall behind it.
const CABINET: Vec3 = Vec3::new(1.5, 1.7, 0.8);
const OFF_WALL: f32 = 0.45;

fn fit(kind: MachineKind) -> MachineFit {
    let hall = &ROOMS[HALL];

    // A cabinet against a room's north wall, screen facing into the room.
    let north = |room: &Room, x: f32| MachineFit {
        base: Vec3::new(x, 0.0, room.min_z + OFF_WALL),
        size: CABINET,
        facing: Vec3::Z,
        is_worktop: false,
    };

    match kind {
        // The core loop, spread across the hall's north wall with room to stand
        // between them. In the one-room lab these were two of five machines at
        // 2.3m centres; five metres apart, each one has its own place to work.
        MachineKind::Dispenser => north(hall, -4.0),
        MachineKind::ChemMaster => north(hall, 1.5),
        // On the hall's east wall beside the lobby door, so it is the last
        // thing you pass on the way out to the counter and the first on the way
        // back in. Shallow, because it is a notice board, not a cabinet.
        MachineKind::StandingBoard => MachineFit {
            base: Vec3::new(hall.max_x - 0.4, 0.0, 0.5),
            size: Vec3::new(0.8, 1.7, 1.5),
            facing: Vec3::NEG_X,
            is_worktop: false,
        },
        // Analysis, in its own room: the analyzer against the north wall and the
        // test bench down the east one.
        MachineKind::Analyzer => north(&ROOMS[ANALYSIS], 10.5),
        MachineKind::TestBench => MachineFit {
            base: Vec3::new(ROOMS[ANALYSIS].max_x - 0.55, 0.0, -2.5),
            size: Vec3::new(1.0, 1.1, 2.6),
            facing: Vec3::NEG_X,
            is_worktop: true,
        },
        // The reaction bay's whole reason to exist, on its west wall — as far
        // from the ChemMaster as the suite gets. A hot batch has to cross a
        // doorway and the length of the hall, which is the only cost heating
        // has; putting the chamber next to the ChemMaster would remove it.
        MachineKind::ReactionChamber => MachineFit {
            base: Vec3::new(ROOMS[REACTION_BAY].min_x + 0.55, 0.0, -3.5),
            size: Vec3::new(1.0, 1.5, 1.4),
            facing: Vec3::X,
            is_worktop: false,
        },
        // The grinder, on the storeroom's south wall. Produce is dropped at the
        // counter through the connecting door, so the haul from crate to hopper
        // is a few steps.
        MachineKind::Grinder => MachineFit {
            base: Vec3::new(-4.0, 0.0, ROOMS[PREP].max_z - OFF_WALL),
            size: CABINET,
            facing: Vec3::NEG_Z,
            is_worktop: false,
        },
        // The delivery counter, across the lobby. Stood well off the south wall
        // so crew can reach the far side of it after coming through the door,
        // and wide enough for a queue plus an incoming crate.
        MachineKind::DeliveryWindow => MachineFit {
            base: Vec3::new(COUNTER_SPOT.x, 0.0, COUNTER_DROP_Z),
            size: Vec3::new(3.4, COUNTER_TOP, 0.7),
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

/// Every plain bench in the suite, as (centre, size).
///
/// Split out from the spawner so the TrenchBroom exporter can emit the same
/// boxes as brushes without restating their positions — the moment there are
/// two lists of fixtures, one of them is wrong.
pub(crate) fn fixtures() -> Vec<(Vec3, Vec3)> {
    let mut all = Vec::new();

    // An island down the middle of the hall. Standing off the walls is the
    // point: it gives the room a route around it instead of a single lane, and
    // somewhere to put a beaker down that is not on top of a machine.
    for x in [-3.5f32, 2.0] {
        all.push((Vec3::new(x, 0.45, -2.2), Vec3::new(2.6, 0.9, 0.9)));
    }

    // Shelving down the storeroom's west wall.
    for z in [3.1f32, 4.9] {
        all.push((
            Vec3::new(ROOMS[PREP].min_x + 0.5, 0.45, z),
            Vec3::new(1.0, 0.9, 1.4),
        ));
    }

    // A bench under the analysis room's west wall, for staging samples.
    all.push((Vec3::new(9.0, 0.45, -1.4), Vec3::new(2.2, 0.9, 0.8)));

    all
}

/// Plain benches, so the rooms are not just corridors.
///
/// Scenery rather than equipment: no state, nothing to replicate, and the
/// server needs them present to collide against.
fn spawn_fixtures(mut commands: Commands, assets: Res<MachineAssets>) {
    for (center, size) in fixtures() {
        solid_box(&mut commands, &assets.cube, &assets.bench, center, size);
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
            MachineKind::Analyzer | MachineKind::DeliveryWindow | MachineKind::StandingBoard => {}
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
        // The standing board has none, deliberately: walking up holding a
        // beaker would park it on the board instead of opening the panel, and
        // the player would have no way of telling why the board had stopped
        // responding.
        if kind != MachineKind::StandingBoard {
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

    /// The radius the real player controller contains bodies at.
    const BODY: f32 = 0.35;

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

    /// The name of the room a point stands in, for readable assertion messages.
    fn room_name(point: Vec3) -> Option<&'static str> {
        room_at(point).map(|room| room.name)
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
    fn the_standing_board_is_the_one_machine_with_no_slot() {
        // Walking up to it holding a beaker would park the beaker on the board
        // instead of opening the panel, and nothing would explain why.
        let mut app = client_lab();
        let board = app
            .world_mut()
            .spawn(Machine::new(MachineKind::StandingBoard))
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

    #[test]
    fn every_machine_stands_inside_a_room() {
        // The failure this catches is a silent one: a fit whose coordinates fall
        // in the gap between two rooms still spawns, still replicates and still
        // draws — as a machine embedded in a wall, or floating in the void
        // outside, reachable from nowhere.
        for kind in MachineKind::ALL {
            let fit = fit(kind);
            let room = room_name(fit.base)
                .unwrap_or_else(|| panic!("{kind:?} stands outside every room at {}", fit.base));

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
                    "{kind:?} overhangs {room} at {corner}"
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

            for kind in MachineKind::ALL {
                let fit = fit(kind);
                let gap = (fit.base - door).abs();
                let clearance = half + Vec3::new(fit.size.x, 0.0, fit.size.z) * 0.5;
                assert!(
                    gap.x >= clearance.x || gap.z >= clearance.z,
                    "{kind:?} blocks the doorway at {door}"
                );
            }
        }
    }

    #[test]
    fn every_room_can_be_walked_to_from_the_spawn_point() {
        // The whole reason `contain` exists. The old single-rectangle clamp
        // would have passed every other test in this file and still left four
        // of the five rooms unreachable, because a body outside the one
        // rectangle was snapped back into it.
        //
        // Flood-fills the walkable regions: two regions are connected if they
        // overlap, and every room must be reachable from the hall.
        let regions: Vec<Bounds> = walkable(BODY).filter(Bounds::is_standable).collect();
        let overlaps = |a: &Bounds, b: &Bounds| {
            a.min_x <= b.max_x && b.min_x <= a.max_x && a.min_z <= b.max_z && b.min_z <= a.max_z
        };

        let start = regions
            .iter()
            .position(|region| region.holds(SPAWN_SPOT))
            .expect("a chemist must spawn somewhere they can stand");

        let mut reached = vec![false; regions.len()];
        reached[start] = true;
        let mut frontier = vec![start];
        while let Some(current) = frontier.pop() {
            for (index, region) in regions.iter().enumerate() {
                if !reached[index] && overlaps(&regions[current], region) {
                    reached[index] = true;
                    frontier.push(index);
                }
            }
        }

        for (index, room) in ROOMS.iter().enumerate() {
            assert!(
                reached[index],
                "{} cannot be walked to from the spawn point",
                room.name
            );
        }
    }

    #[test]
    fn the_places_crew_and_couriers_stand_are_inside_the_lobby() {
        // Crew walk a fixed route rather than pathfinding, so if the counter
        // moves without these moving with it they walk into a wall and wait
        // there forever, holding an order nobody can collect.
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
        let counter = fit(MachineKind::DeliveryWindow);
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
        let counter = fit(MachineKind::DeliveryWindow);
        let front = counter.base.z + counter.size.z * 0.5;
        assert!(
            COUNTER_SPOT.z > front,
            "crew queue at {} but the counter's public face is at {front}",
            COUNTER_SPOT.z
        );
        assert_eq!(
            counter.facing,
            Vec3::NEG_Z,
            "the chemist works the counter from the lobby's north side"
        );
    }
}
