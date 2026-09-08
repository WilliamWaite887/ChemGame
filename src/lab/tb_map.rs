//! Checks on `assets/maps/lab.map`, the hand-edited TrenchBroom lab.
//!
//! The map is authored in TrenchBroom and is its own authority — nothing
//! generates it. That is exactly why it needs tests: a `.map` has no compiler,
//! and its two characteristic failures both produce a file that loads without
//! complaint and shows nothing. A malformed face line drops a brush; an
//! inside-out brush renders as empty space you can walk through. Neither is
//! visible in a diff.
//!
//! These run in every build, feature flag or not, because the file is in the
//! repo either way.

use bevy::prelude::Vec3;
use quake_map::Entity;

use crate::lab::{Bounds, FloorProfile, WalkableAreas, COUNTER_SPOT, TB_SCALE};
use crate::machines::MachineKind;
use crate::nav::{NavGraph, NAV_RADIUS};

const MAP: &str = "assets/maps/lab.map";

/// A map entity's `classname`, if it has one.
fn classname(entity: &Entity) -> Option<String> {
    entity
        .edict
        .iter()
        .find(|(key, _)| key.to_string_lossy() == "classname")
        .map(|(_, value)| value.to_string_lossy().to_string())
}

/// A property of a map entity.
fn property(entity: &Entity, name: &str) -> Option<String> {
    entity
        .edict
        .iter()
        .find(|(key, _)| key.to_string_lossy() == name)
        .map(|(_, value)| value.to_string_lossy().to_string())
}

/// The XZ footprint of a brush, in Bevy metres.
///
/// The three points of each face are corners of the box, so the extremes over
/// all of them are the box itself — no plane intersection needed for the
/// axis-aligned brushes the blockout is made of.
///
/// TrenchBroom is z-up and scaled: `tb = (-bevy.z, -bevy.x, bevy.y) * SCALE`,
/// so coming back is `bevy.x = -tb.y / SCALE`, `bevy.z = -tb.x / SCALE`.
fn footprint(brush: &[quake_map::Surface]) -> Bounds {
    let (mut min_tx, mut max_tx) = (f64::MAX, f64::MIN);
    let (mut min_ty, mut max_ty) = (f64::MAX, f64::MIN);

    for surface in brush {
        for point in surface.half_space {
            min_tx = min_tx.min(point[0]);
            max_tx = max_tx.max(point[0]);
            min_ty = min_ty.min(point[1]);
            max_ty = max_ty.max(point[1]);
        }
    }

    let scale = TB_SCALE as f64;
    Bounds {
        min_x: (-max_ty / scale) as f32,
        max_x: (-min_ty / scale) as f32,
        min_z: (-max_tx / scale) as f32,
        max_z: (-min_tx / scale) as f32,
    }
}

fn vertical_span(brush: &[quake_map::Surface]) -> (f32, f32) {
    let mut min = f64::MAX;
    let mut max = f64::MIN;
    for surface in brush {
        for point in surface.half_space {
            min = min.min(point[2]);
            max = max.max(point[2]);
        }
    }
    (
        (min / TB_SCALE as f64) as f32,
        (max / TB_SCALE as f64) as f32,
    )
}

/// A point entity's origin converted from TrenchBroom coordinates to Bevy XZ.
fn origin_xz(entity: &Entity) -> Option<(f32, f32)> {
    let origin = property(entity, "origin")?;
    let coordinates: Vec<f32> = origin
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    (coordinates.len() == 3).then(|| (-coordinates[1] / TB_SCALE, -coordinates[0] / TB_SCALE))
}

fn bounds_are_close(actual: Bounds, expected: Bounds) -> bool {
    (actual.min_x - expected.min_x).abs() < 0.001
        && (actual.max_x - expected.max_x).abs() < 0.001
        && (actual.min_z - expected.min_z).abs() < 0.001
        && (actual.max_z - expected.max_z).abs() < 0.001
}

fn parse() -> Vec<Entity> {
    let source = std::fs::read(MAP).unwrap_or_else(|err| panic!("reading {MAP}: {err}"));
    quake_map::parse(&mut source.as_slice())
        .unwrap_or_else(|err| panic!("{MAP} is not a valid Quake map: {err}"))
        .entities
}

pub(crate) fn authored_queue_paths() -> crate::order_intake::queue::QueuePaths {
    let map = parse();
    let lane = |name: &str| {
        let mut points: Vec<_> = map
            .iter()
            .filter(|e| {
                classname(e).as_deref() == Some("queue_point")
                    && property(e, "lane").as_deref() == Some(name)
            })
            .collect();
        points.sort_by_key(|e| property(e, "sequence").unwrap().parse::<usize>().unwrap());
        points
            .into_iter()
            .map(|e| {
                let (x, z) = origin_xz(e).unwrap();
                Vec3::new(x, 0.0, z)
            })
            .collect()
    };
    let clearances = map
        .iter()
        .filter_map(|e| {
            if classname(e).as_deref() != Some("queue_point") {
                return None;
            }
            let radius = property(e, "clearance")?.parse().ok()?;
            let (x, z) = origin_xz(e)?;
            Some((Vec3::new(x, 0.0, z), radius))
        })
        .collect();
    crate::order_intake::queue::QueuePaths {
        public: lane("public"),
        medical: lane("medical"),
        clearances,
        access: ["public_access", "medical_access"]
            .into_iter()
            .flat_map(|name| {
                lane(name)
                    .windows(2)
                    .map(|pair| (pair[0], pair[1]))
                    .collect::<Vec<_>>()
            })
            .collect(),
    }
}

/// Use the actual window positions and orientations in movement regressions.
/// Default stations intentionally fall back to the public window, including
/// for Medical, so they cannot validate the second lane's approach.
pub(crate) fn authored_delivery_stations() -> crate::lab::DeliveryStations {
    let mut spots = crate::lab::MachineSpots::default();
    for entity in parse() {
        if classname(&entity).as_deref() != Some("machine_spot")
            || property(&entity, "kind").as_deref() != Some("DeliveryWindow")
        {
            continue;
        }
        let (x, z) = origin_xz(&entity).unwrap();
        let angles = property(&entity, "angles").unwrap();
        let yaw = angles
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<f32>()
            .unwrap();
        let lane = match property(&entity, "lane").as_deref() {
            Some("medical") => crate::lab::DeliveryLane::Medical,
            _ => crate::lab::DeliveryLane::Public,
        };
        spots.insert_with_lane(
            property(&entity, "id").unwrap(),
            MachineKind::DeliveryWindow,
            bevy::prelude::Transform::from_xyz(x, 0.0, z)
                .with_rotation(bevy::prelude::Quat::from_rotation_y(yaw.to_radians())),
            Some(lane),
        );
    }
    let mut stations = crate::lab::DeliveryStations::default();
    stations.rebuild_from(&spots);
    stations
}

#[test]
fn pickup_queues_fit_the_whole_cast_and_clear_authored_furniture() {
    use crate::order_intake::queue::{navigable_line, standing_points};
    let areas = authored_walkable_areas();
    let nav = NavGraph::build(&areas, NAV_RADIUS);
    let paths = authored_queue_paths();
    let solids = authored_solid_colliders();
    let roster: Vec<crate::crew::CrewDef> =
        ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
    assert!(
        !paths.access.is_empty(),
        "both windows need an authored walking aisle"
    );
    let map = parse();
    for lane in ["public_access", "medical_access"] {
        assert!(
            map.iter()
                .filter(|entity| classname(entity).as_deref() == Some("queue_point")
                    && property(entity, "lane").as_deref() == Some(lane))
                .count()
                >= 2,
            "{lane} needs a complete authored approach aisle"
        );
    }
    for (from, to) in &paths.access {
        assert!(
            crate::npc_motion::walkable_segment(
                &areas,
                *from + Vec3::Y * crate::crew::BODY_OFFSET,
                *to + Vec3::Y * crate::crew::BODY_OFFSET,
                crate::crew::BODY_OFFSET,
            ),
            "greeting access crosses unwalkable floor: {from:?} -> {to:?}"
        );
        let steps = (from.distance(*to) / 0.1).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let body = from.lerp(*to, i as f32 / steps as f32) + Vec3::Y * crate::crew::BODY_OFFSET;
            for (center, half) in &solids {
                if (body.y - center.y).abs() < half.y + 0.6 {
                    assert!(
                        (body.x - center.x).abs() - half.x >= NAV_RADIUS
                            || (body.z - center.z).abs() - half.z >= NAV_RADIUS,
                        "greeting access clips furniture/wall at {body:?}: {center:?} {half:?}"
                    );
                }
            }
        }
    }
    for (lane, anchors) in [("public", paths.public), ("medical", paths.medical)] {
        let line = navigable_line(&anchors, &nav);
        let points = standing_points(&line, &[], &paths.clearances, &paths.access);
        assert!(
            points.len() >= roster.len() + 6,
            "{lane}: only {} standing places for {} possible identities",
            points.len(),
            roster.len() + 6
        );
        for (i, point) in points.iter().enumerate() {
            assert!(
                nav.standable_goal(*point).distance(*point) < 0.05,
                "{lane} slot {i} is not standable: {point:?}"
            );
            for (center, half) in &solids {
                let body = *point + Vec3::Y * crate::crew::BODY_OFFSET;
                if (body.y - center.y).abs() >= half.y + 0.6 {
                    continue;
                }
                let dx = (body.x - center.x).abs() - half.x;
                let dz = (body.z - center.z).abs() - half.z;
                assert!(
                    dx >= NAV_RADIUS || dz >= NAV_RADIUS,
                    "{lane} slot {i} clips furniture/wall at {point:?}: {center:?} {half:?}"
                );
            }
            for other in points.iter().skip(i + 1) {
                assert!(point.distance(*other) >= 0.84);
            }
        }
        if lane == "public" {
            assert!(
                points.iter().any(|p| p.z > 10.0),
                "public queue must extend outside the lobby"
            );
            assert!(
                line.iter()
                    .any(|p| p.z >= 6.2 && p.z <= 8.5 && p.x > 3.0 && p.x < 5.0),
                "queue must cross the actual lobby doorway"
            );
        }
    }
}

#[test]
fn every_map_texture_has_a_runtime_asset() {
    for entity in parse() {
        for brush in entity.brushes {
            for surface in brush {
                let texture = surface.texture.to_string_lossy();
                let path = format!("assets/textures/{texture}.png");
                assert!(
                    std::path::Path::new(&path).is_file(),
                    "{MAP} references `{texture}`, but {path} does not exist"
                );
            }
        }
    }
}

#[test]
fn chemistry_uses_modular_furniture_instead_of_the_placeholder_bench() {
    assert!(
        !std::path::Path::new("assets/textures/bench.png").exists(),
        "the featureless placeholder bench sprite must stay retired"
    );
    assert!(
        parse()
            .iter()
            .all(|entity| entity.brushes.iter().all(|brush| {
                brush
                    .iter()
                    .all(|surface| surface.texture.to_string_lossy() != "bench")
            })),
        "lab.map must use modular Chemistry fixtures, not a brush textured `bench`"
    );
}

#[test]
fn chemistry_surface_sprites_are_small_power_of_two_pngs() {
    const CHEMISTRY_SURFACES: &[&str] = &[
        "floor_mixing_hall",
        "floor_prep_storage",
        "floor_analysis",
        "floor_reaction_bay",
        "floor_lobby",
        "wall",
        "wall_chemistry",
    ];

    for texture in CHEMISTRY_SURFACES {
        let path = format!("assets/textures/{texture}.png");
        let png = std::fs::read(&path).unwrap_or_else(|err| panic!("reading {path}: {err}"));
        assert!(
            png.len() >= 24,
            "{path} is too short to contain a PNG header"
        );
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{path} is not a PNG");

        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!((width, height), (128, 128), "{path} changed sprite scale");
        assert!(
            width.is_power_of_two() && height.is_power_of_two(),
            "{path} must repeat as a power-of-two texture"
        );
    }
}

#[test]
fn common_area_floor_sprite_is_a_small_power_of_two_png() {
    let path = "assets/textures/floor_corridor.png";
    let png = std::fs::read(path).unwrap_or_else(|err| panic!("reading {path}: {err}"));
    assert!(
        png.len() >= 24,
        "{path} is too short to contain a PNG header"
    );
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{path} is not a PNG");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), (128, 128), "{path} changed sprite scale");
    assert!(width.is_power_of_two() && height.is_power_of_two());
}

#[test]
fn chemistry_brushes_use_the_department_wall_and_one_metre_sprite_scale() {
    const FLOORS: &[&str] = &[
        "floor_mixing_hall",
        "floor_prep_storage",
        "floor_analysis",
        "floor_reaction_bay",
        "floor_lobby",
    ];

    let mut floor_faces = std::collections::HashMap::<String, usize>::new();
    let mut wall_faces = 0;
    for entity in parse() {
        for brush in entity.brushes {
            for surface in brush {
                let texture = surface.texture.to_string_lossy();
                if FLOORS.contains(&texture.as_ref()) || texture == "wall_chemistry" {
                    assert_eq!(
                        surface.alignment.scale,
                        [0.625, 0.625],
                        "{texture} must map each 64-pixel sub-panel to one metre"
                    );
                }
                if FLOORS.contains(&texture.as_ref()) {
                    *floor_faces.entry(texture.to_string()).or_default() += 1;
                } else if texture == "wall_chemistry" {
                    wall_faces += 1;
                }
            }
        }
    }

    for floor in FLOORS {
        assert_eq!(
            floor_faces.get(*floor),
            Some(&6),
            "{floor} lost its room brush"
        );
    }
    assert_eq!(
        wall_faces, 156,
        "the Chemistry shell changed material coverage"
    );
}

/// The exact walkable registry the authored map contributes at runtime.
///
/// Exposed only to crate tests so cross-system regressions can exercise the
/// station topology without duplicating a second, subtly different parser.
pub(crate) fn authored_walkable_areas() -> WalkableAreas {
    let mut areas = WalkableAreas::default();
    for entity in parse()
        .into_iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
    {
        let room = property(&entity, "room").filter(|room| !room.trim().is_empty());
        let bridge_id = property(&entity, "bridge_id").filter(|bridge| !bridge.trim().is_empty());
        let floor = match (
            property(&entity, "slope_axis").as_deref(),
            property(&entity, "floor_min").and_then(|value| value.parse::<f32>().ok()),
            property(&entity, "floor_max").and_then(|value| value.parse::<f32>().ok()),
        ) {
            (Some("x"), Some(at_min), Some(at_max)) => {
                Some(FloorProfile::LinearX { at_min, at_max })
            }
            (Some("z"), Some(at_min), Some(at_max)) => {
                Some(FloorProfile::LinearZ { at_min, at_max })
            }
            _ => None,
        };
        for brush in &entity.brushes {
            let profile = floor.unwrap_or_else(|| FloorProfile::Flat(vertical_span(brush).0));
            areas.push_surface(footprint(brush), room.clone(), bridge_id.clone(), profile);
        }
    }
    areas
}

/// The exact collider set `LabWorldspawn::spawn_brush_colliders` builds at
/// runtime, as (center, half_extents) pairs.
///
/// Mirrors that function's own `is_scenery`/`blocks_a_body` filtering exactly
/// rather than importing it, since it lives behind `feature = "trenchbroom"`
/// and this needs to run unconditionally like the rest of this module. Exposed
/// so movement regressions can exercise real collision against the real map
/// instead of an empty solids query, which would silently stop testing
/// collision at all.
pub(crate) fn authored_solid_colliders() -> Vec<(Vec3, Vec3)> {
    fn is_scenery(texture: &str) -> bool {
        texture.starts_with("floor_") || matches!(texture, "ceiling" | "stripe")
    }

    let mut colliders = Vec::new();
    for entity in parse()
        .into_iter()
        .filter(|entity| classname(entity).as_deref() == Some("worldspawn"))
    {
        for brush in &entity.brushes {
            if brush
                .iter()
                .all(|surface| is_scenery(&surface.texture.to_string_lossy()))
            {
                continue;
            }
            let bounds = footprint(brush);
            let (min_y, max_y) = vertical_span(brush);
            if max_y <= min_y {
                continue;
            }
            let min = Vec3::new(bounds.min_x, min_y, bounds.min_z);
            let max = Vec3::new(bounds.max_x, max_y, bounds.max_z);
            colliders.push(((min + max) * 0.5, (max - min) * 0.5));
        }
    }
    colliders
}

/// Position of one authored department marker, in Bevy world coordinates.
pub(crate) fn authored_department_home(role: &str) -> Vec3 {
    let entity = parse()
        .into_iter()
        .find(|entity| {
            classname(entity).as_deref() == Some("department_spot")
                && property(entity, "department").as_deref() == Some(role)
        })
        .unwrap_or_else(|| panic!("the authored map has no {role} department_spot"));
    let (x, z) = origin_xz(&entity).expect("department_spot has a valid origin");
    Vec3::new(x, 0.0, z)
}

#[test]
fn every_brush_encloses_a_volume() {
    // Reproduces `bevy_trenchbroom`'s plane maths exactly: a face's three points
    // give normal = (p3 - p1) x (p2 - p1), and the brush interior is where
    // normal . p + d < 0. Normals must therefore point *out*. Reversing a face's
    // winding — easy to do by hand, invisible in the file — turns a solid wall
    // into nothing at all.
    let mut brushes = 0;

    for (entity_index, entity) in parse().into_iter().enumerate() {
        for (brush_index, brush) in entity.brushes.iter().enumerate() {
            assert!(
                brush.len() >= 4,
                "a brush needs at least four faces to enclose anything",
            );

            // Averaging every face point lands inside any convex brush.
            let mut centroid = [0.0f64; 3];
            let mut count = 0.0;
            for surface in brush.iter() {
                for point in surface.half_space {
                    centroid[0] += point[0];
                    centroid[1] += point[1];
                    centroid[2] += point[2];
                    count += 1.0;
                }
            }
            let centroid = [
                centroid[0] / count,
                centroid[1] / count,
                centroid[2] / count,
            ];

            for surface in brush.iter() {
                let [p1, p2, p3] = surface.half_space;
                let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
                let (a, b) = (sub(p3, p1), sub(p2, p1));
                let normal = [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ];
                let length = (normal[0].powi(2) + normal[1].powi(2) + normal[2].powi(2)).sqrt();
                assert!(length > 0.0, "a face's three points are collinear");

                let distance = -(normal[0] * p1[0] + normal[1] * p1[1] + normal[2] * p1[2]);
                let side = normal[0] * centroid[0]
                    + normal[1] * centroid[1]
                    + normal[2] * centroid[2]
                    + distance;

                assert!(
                    side < 0.0,
                    "entity {entity_index} brush {brush_index} is inside out: its centre sits outside one of its own faces",
                );
            }

            brushes += 1;
        }
    }

    // A floor to catch a gutted or truncated file. Deliberately not an exact
    // count — brushes coming and going is the editor being used, not a defect.
    assert!(
        brushes >= 20,
        "only {brushes} brushes in {MAP} — it has been gutted, not edited",
    );
}

#[test]
fn collision_and_walkable_brushes_stay_axis_aligned() {
    // Runtime collision stores only an AABB per worldspawn brush, and nav does
    // the same for every func_walkable brush. An angled drag would therefore
    // create invisible blocked space or walkable space outside the drawn hull.
    for entity in parse().iter().filter(|entity| {
        matches!(
            classname(entity).as_deref(),
            Some("worldspawn" | "func_walkable")
        )
    }) {
        let class = classname(entity).unwrap();
        for (brush_index, brush) in entity.brushes.iter().enumerate() {
            for (face_index, face) in brush.iter().enumerate() {
                let points = face.half_space;
                let constant_axis = (0..3).any(|axis| {
                    (points[0][axis] - points[1][axis]).abs() < 0.000_001
                        && (points[0][axis] - points[2][axis]).abs() < 0.000_001
                });
                assert!(
                    constant_axis,
                    "{class} brush {brush_index} face {face_index} is angled; runtime uses its AABB",
                );
            }
        }
    }
}

#[test]
fn the_maps_walkable_rooms_match_the_floor_plan() {
    // The map and `ROOMS` both describe the lab's floor while the const tables
    // are still what a plain build uses, and nothing but this notices when they
    // disagree. It also checks the hand-computed TrenchBroom coordinates of the
    // seeded volumes: an axis flipped or a scale wrong here would put every room
    // somewhere plausible-looking and completely wrong.
    let plan = WalkableAreas::from_floor_plan();

    for entity in parse() {
        if classname(&entity).as_deref() != Some("func_walkable") {
            continue;
        }
        let Some(room) = property(&entity, "room").filter(|room| !room.trim().is_empty()) else {
            // A doorway bridge; it belongs to no room and has nothing to match.
            continue;
        };

        // The map is a superset: the const plan only ever described the chem
        // lab, and the station around it exists solely in the map. Rooms it has
        // never heard of are the point, not a discrepancy.
        let Some(expected) = plan
            .regions()
            .iter()
            .find(|region| region.room.as_deref() == Some(room.as_str()))
        else {
            continue;
        };

        let actual = entity
            .brushes
            .iter()
            .map(|brush| footprint(brush))
            .reduce(|left, right| Bounds {
                min_x: left.min_x.min(right.min_x),
                max_x: left.max_x.max(right.max_x),
                min_z: left.min_z.min(right.min_z),
                max_z: left.max_z.max(right.max_z),
            })
            .unwrap_or_else(|| panic!("{room} has no walkable brush"));
        for (what, got, want) in [
            ("min_x", actual.min_x, expected.bounds.min_x),
            ("max_x", actual.max_x, expected.bounds.max_x),
            ("min_z", actual.min_z, expected.bounds.min_z),
            ("max_z", actual.max_z, expected.bounds.max_z),
        ] {
            assert!(
                (got - want).abs() < 0.01,
                "{room}'s combined walkable volume has {what} = {got}, floor plan says {want}",
            );
        }
    }
}

#[test]
fn no_room_is_split_by_an_invisible_seam() {
    // A room drawn as several brushes has to have them *overlap*, not merely
    // meet. `WalkableAreas::contain_on_surface` insets every region by the
    // body radius before asking which one holds a position, so two rectangles
    // that share an edge leave a dead strip `2 * radius` wide down the middle
    // that neither inset covers. A body walking into that strip gets clamped
    // back to the edge it came from, every frame, and the seam becomes an
    // invisible wall in the middle of an apparently open floor.
    //
    // Nothing else notices: the brushes enclose volumes, they are axis
    // aligned, no wall intrudes on them, and `the_station_is_one_connected_space`
    // passes because `NavGraph` routes crew the long way round instead. Only
    // this comparison, or walking into it, finds it.
    const RADIUS: f32 = 0.35;

    let map = parse();
    let mut by_room: std::collections::BTreeMap<String, Vec<Bounds>> = Default::default();
    for entity in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
    {
        let Some(room) = property(entity, "room").filter(|room| !room.trim().is_empty()) else {
            continue;
        };
        by_room
            .entry(room)
            .or_default()
            .extend(entity.brushes.iter().map(|brush| footprint(brush)));
    }

    for (room, parts) in by_room {
        for (i, first) in parts.iter().enumerate() {
            for second in parts.iter().skip(i + 1) {
                // Only pairs that are adjacent or overlapping: two parts of a
                // room at opposite ends are separated by walls, not a seam.
                if first.intersection(second).is_none()
                    && !(first.max_x >= second.min_x - 0.001
                        && second.max_x >= first.min_x - 0.001
                        && first.max_z >= second.min_z - 0.001
                        && second.max_z >= first.min_z - 0.001)
                {
                    continue;
                }
                assert!(
                    first
                        .inset(RADIUS)
                        .intersection(&second.inset(RADIUS))
                        .is_some(),
                    "{room} is drawn as pieces that only touch: {first:?} and {second:?} \
                     leave a {} m dead strip a body cannot cross. Overlap them instead.",
                    2.0 * RADIUS,
                );
            }
        }
    }
}

#[test]
fn every_room_in_the_floor_plan_has_a_walkable_volume_drawn_for_it() {
    // The other direction: a room nobody drew a volume over is a room the map
    // backend cannot let anyone stand in, however solid its walls look.
    let drawn: Vec<String> = parse()
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .filter_map(|entity| property(entity, "room"))
        .filter(|room| !room.trim().is_empty())
        .collect();

    for region in WalkableAreas::from_floor_plan().regions() {
        let Some(room) = region.room.as_deref() else {
            continue;
        };
        assert!(
            drawn.iter().any(|found| found == room),
            "{room} has no func_walkable volume in {MAP}",
        );
    }
}

#[test]
fn selective_subrooms_are_inside_their_parent_departments() {
    let map = parse();
    let named_bounds = |room: &str| -> Vec<Bounds> {
        map.iter()
            .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
            .filter(|entity| property(entity, "room").as_deref() == Some(room))
            .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
            .collect()
    };

    for (room, expected) in [
        (
            "Security Back Room",
            Bounds {
                min_x: -109.5,
                max_x: -83.5,
                min_z: -9.0,
                max_z: -2.0,
            },
        ),
        (
            // Carved back 1.2 m for the specimen cabinet, as Medical is for
            // the ward bay.
            "Quarantine",
            Bounds {
                min_x: -37.5,
                max_x: -16.5,
                min_z: -9.0,
                max_z: -3.0,
            },
        ),
        (
            "Medical Handoff Vestibule",
            Bounds {
                min_x: -16.25,
                max_x: -7.5,
                min_z: 1.5,
                max_z: 5.0,
            },
        ),
        (
            // Shrunk to x -66..-54.5: the strip against the east wall, x
            // -54.5..-52.5, is now the gas canister racks and filtration
            // scrubber's floor rather than walkable.
            "Atmos/Utility",
            Bounds {
                min_x: -66.0,
                max_x: -54.5,
                min_z: 15.0,
                max_z: 28.6,
            },
        ),
        (
            // Stops 3.4 m short of the back wall, the way Medical stops short
            // of its ward bay: that band is the freight line's intake run, and
            // nothing on a conveyor carries a collider.
            "Cargo Receiving",
            Bounds {
                min_x: -109.5,
                max_x: -94.0,
                min_z: 31.4,
                max_z: 47.6,
            },
        ),
        (
            // Chapel and Quiet Room are the two walled compartments either
            // side of the south stairwell, where the reference layout puts
            // them. They spent a while at z 31.4..39 -- an open alcove off
            // Service with no walls of its own -- because the walls moved
            // south and these volumes did not.
            "Chapel",
            Bounds {
                min_x: -38.7,
                max_x: -23.5,
                min_z: 44.0,
                max_z: 51.0,
            },
        ),
        (
            "Quiet Room",
            Bounds {
                min_x: -47.5,
                max_x: -41.5,
                min_z: 44.0,
                max_z: 51.0,
            },
        ),
    ] {
        let found = named_bounds(room);
        assert_eq!(found.len(), 1, "{room} should be one walkable rectangle");
        assert!(bounds_are_close(found[0], expected), "{room}: {found:?}");
    }

    // The nursery's propagation racks are two carved rows, so its walkable
    // floor is the three long aisles plus a cross-aisle at each airlock.
    let nursery = named_bounds("Botany Nursery");
    assert_eq!(
        nursery.len(),
        5,
        "Botany Nursery should be three aisles joined at both airlocks"
    );
    for expected in [
        Bounds {
            min_x: -18.5,
            max_x: -8.6,
            min_z: 42.0,
            max_z: 51.0,
        },
        Bounds {
            min_x: -7.4,
            max_x: 7.4,
            min_z: 42.0,
            max_z: 51.0,
        },
        Bounds {
            min_x: 8.6,
            max_x: 13.5,
            min_z: 42.0,
            max_z: 51.0,
        },
        Bounds {
            min_x: -18.5,
            max_x: 13.5,
            min_z: 42.0,
            max_z: 43.5,
        },
        Bounds {
            min_x: -18.5,
            max_x: 13.5,
            min_z: 49.5,
            max_z: 51.0,
        },
    ] {
        assert!(
            nursery
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "Botany Nursery is missing {expected:?}: {nursery:?}",
        );
    }

    // Bridge Operations is the southern half of one band-and-aisle skeleton
    // rather than a rectangle: the mission-control dressing pass fills the
    // combined Bridge floor with furniture rows, and a floor fixture has to
    // stand in ground carved out of the walkable volume. What is left is the
    // fourth transverse concourse plus the aisle's southern leg. The aisle
    // reaches z -2.0, past the old z -3.0 dividing line and into the main
    // hall's own aisle half, because that overlap is the only thing joining
    // the two differently-named rooms now the wall between them is gone.
    let operations = named_bounds("Bridge Operations");
    assert_eq!(
        operations.len(),
        3,
        "Bridge Operations should be the fourth concourse, the viewscreen-wall \
         strip, and the aisle's southern leg",
    );
    for expected in [
        Bounds {
            min_x: -83.5,
            max_x: -41.5,
            min_z: -5.7,
            max_z: -3.5,
        },
        Bounds {
            min_x: -83.5,
            max_x: -41.5,
            min_z: -9.0,
            max_z: -7.8,
        },
        Bounds {
            min_x: -62.3,
            max_x: -57.3,
            min_z: -9.0,
            max_z: -2.0,
        },
    ] {
        assert!(
            operations
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "Bridge Operations is missing {expected:?}: {operations:?}",
        );
    }
}

#[test]
fn station_v2_keeps_its_department_and_route_footprints() {
    let map = parse();
    let named_bounds = |room: &str| -> Vec<Bounds> {
        map.iter()
            .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
            .filter(|entity| property(entity, "room").as_deref() == Some(room))
            .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
            .collect()
    };

    for (room, expected) in [
        (
            "Security",
            Bounds {
                min_x: -109.5,
                max_x: -83.5,
                min_z: -2.0,
                max_z: 10.0,
            },
        ),
        (
            // Stops 2.5 m short of the Bridge-facing wall: that strip is the
            // ward furniture, and a walkable volume that reached the wall
            // would route crew straight through two beds.
            "Medical",
            Bounds {
                min_x: -36.2,
                max_x: -16.5,
                min_z: -3.0,
                max_z: 10.0,
            },
        ),
    ] {
        let found = named_bounds(room);
        assert_eq!(found.len(), 1, "{room} should be one authored rectangle");
        assert!(bounds_are_close(found[0], expected), "{room}: {found:?}");
    }

    // Bridge is four rectangles: three transverse concourses and the central
    // aisle that crosses all of them. That skeleton is what a mission-control
    // dressing costs. Furniture rows sit in the gaps between the concourses,
    // and since crew ignore `Solid` and path from walkable area alone, a row
    // has to be ground the walkable volume does not cover. The aisle is the
    // only thing joining the concourses to each other, which is why it runs
    // the room's full depth at a full 5 m and why both doors sit on it: pinch
    // it and the room falls into three disconnected strips.
    let bridge = named_bounds("Bridge");
    assert_eq!(
        bridge.len(),
        4,
        "Bridge should be three transverse concourses plus the central aisle",
    );
    for expected in [
        Bounds {
            min_x: -83.5,
            max_x: -41.5,
            min_z: 8.2,
            max_z: 10.0,
        },
        Bounds {
            min_x: -83.5,
            max_x: -41.5,
            min_z: 3.9,
            max_z: 6.1,
        },
        Bounds {
            min_x: -83.5,
            max_x: -41.5,
            min_z: -1.4,
            max_z: 0.8,
        },
        Bounds {
            min_x: -62.3,
            max_x: -57.3,
            min_z: -3.3,
            max_z: 10.0,
        },
    ] {
        assert!(
            bridge
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "Bridge is missing {expected:?}: {bridge:?}",
        );
    }

    // Cargo is two overlapping rectangles rather than one. Both of its long
    // walls are built against now -- the freight line runs the length of the
    // north band and storage racking the south one -- so what is left walkable
    // is the working floor between them, plus one lane straight through the
    // middle. That lane is not optional: the technical and maintenance
    // airlocks sit at the same x, and nothing else reaches either of them.
    let cargo = named_bounds("Cargo");
    assert_eq!(
        cargo.len(),
        2,
        "Cargo should be the working floor plus the cross lane between its two airlocks"
    );
    for expected in [
        Bounds {
            min_x: -94.0,
            max_x: -52.5,
            min_z: 34.5,
            max_z: 47.6,
        },
        Bounds {
            min_x: -81.5,
            max_x: -78.5,
            min_z: 31.4,
            max_z: 51.0,
        },
    ] {
        assert!(
            cargo
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "Cargo is missing {expected:?}: {cargo:?}",
        );
    }

    // Engineering is three brushes: the west sliver's walkable floor (0a,
    // shrunk to z 15..23 -- the decoration pass's two floor-fixture rows,
    // z 23..28.6, take the rest of that column, the only one with a real wall
    // behind it), the rest of the room east of it (0b, full depth, untouched
    // -- that's a through-route to the arm, not a wall recess, so nothing was
    // carved from it), and the strip over the roofed maintenance arm that is
    // the room's southern floor. The arm stops at Atmos/Utility's west wall --
    // the band behind Atmos is filled solid, so there is nothing to stand on
    // between there and the Public Loop leg.
    let engineering = named_bounds("Engineering");
    assert_eq!(
        engineering.len(),
        3,
        "Engineering should be the west sliver, the rest of the room, and the band strip west of Atmos"
    );
    for expected in [
        Bounds {
            min_x: -109.5,
            max_x: -101.0,
            min_z: 15.0,
            max_z: 23.0,
        },
        Bounds {
            min_x: -102.0,
            max_x: -66.0,
            min_z: 15.0,
            max_z: 28.6,
        },
        Bounds {
            min_x: -102.0,
            max_x: -66.0,
            min_z: 27.6,
            max_z: 31.4,
        },
    ] {
        assert!(
            engineering
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "Engineering is missing {expected:?}: {engineering:?}",
        );
    }

    // Service and Botany each used to be cut in two by the old at-grade
    // maintenance hallway; now that it's underground, both are one
    // continuous room again, stitched across the old gap by a third,
    // connector brush (the two original halves plus the bridge between them).
    let service = named_bounds("Service");
    assert_eq!(
        service.len(),
        3,
        "Service should be its two original halves plus the connector across the old spine gap"
    );
    // All three now run the full depth of the room, to the wall at z 39. The
    // halves used to stop at z 31.4 because Chapel and Quiet Room claimed the
    // band beyond; those rooms have since moved to their own walls at z 44..51.
    for expected in [
        Bounds {
            min_x: -47.5,
            max_x: -41.5,
            min_z: 15.0,
            max_z: 39.0,
        },
        Bounds {
            min_x: -38.7,
            max_x: -23.625,
            min_z: 15.0,
            max_z: 39.0,
        },
        Bounds {
            min_x: -43.4,
            max_x: -37.2,
            min_z: 15.0,
            max_z: 39.0,
        },
    ] {
        assert!(
            service
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "Service is missing {expected:?}: {service:?}",
        );
    }

    let botany = named_bounds("Botany");
    assert_eq!(
        botany.len(),
        14,
        "Botany should be greenhouse aisles, workroom aisles, and the connector across the old spine gap"
    );
    for expected in [
        Bounds {
            min_x: -18.5,
            max_x: -9.2,
            min_z: 15.0,
            max_z: 28.6,
        },
        Bounds {
            min_x: -6.8,
            max_x: 6.8,
            min_z: 15.0,
            max_z: 28.6,
        },
        Bounds {
            min_x: 9.2,
            max_x: 13.5,
            min_z: 15.0,
            max_z: 28.6,
        },
        Bounds {
            min_x: -18.5,
            max_x: 13.5,
            min_z: 15.0,
            max_z: 17.0,
        },
        Bounds {
            min_x: -18.5,
            max_x: 13.5,
            min_z: 26.5,
            max_z: 28.6,
        },
        Bounds {
            min_x: -18.5,
            max_x: -14.0,
            min_z: 31.4,
            max_z: 42.0,
        },
        Bounds {
            min_x: -4.5,
            max_x: 9.9,
            min_z: 31.4,
            max_z: 42.0,
        },
        Bounds {
            min_x: 11.1,
            max_x: 13.5,
            min_z: 31.4,
            max_z: 42.0,
        },
        Bounds {
            min_x: -18.5,
            max_x: 9.9,
            min_z: 31.4,
            max_z: 33.3,
        },
        Bounds {
            min_x: -18.5,
            max_x: 9.9,
            min_z: 35.7,
            max_z: 37.8,
        },
        Bounds {
            min_x: -6.8,
            max_x: -3.5,
            min_z: 38.2,
            max_z: 41.2,
        },
        Bounds {
            min_x: -6.8,
            max_x: 9.9,
            min_z: 39.6,
            max_z: 41.2,
        },
        Bounds {
            min_x: -18.5,
            max_x: 13.5,
            min_z: 40.4,
            max_z: 42.0,
        },
        // Deliberately overhangs both halves by 1.5 m rather than butting up
        // against them — see `no_room_is_split_by_an_invisible_seam`.
        Bounds {
            min_x: -18.5,
            max_x: 6.0,
            min_z: 27.1,
            max_z: 32.9,
        },
    ] {
        assert!(
            botany
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "Botany is missing {expected:?}: {botany:?}",
        );
    }

    let public = named_bounds("Public Loop");
    assert_eq!(public.len(), 5, "public loop lost a circulation segment");
    assert!(
        public.iter().any(|bounds| bounds_are_close(
            *bounds,
            Bounds {
                min_x: -109.5,
                max_x: 13.5,
                min_z: 10.0,
                max_z: 15.0,
            }
        )),
        "the five-metre northern gallery is missing"
    );

    let maintenance = named_bounds("Maintenance V2");
    assert_eq!(
        maintenance.len(),
        4,
        "maintenance should be the four-segment outer service ring"
    );
    for bounds in &maintenance {
        let width = (bounds.max_x - bounds.min_x).min(bounds.max_z - bounds.min_z);
        assert!(
            (width - 3.0).abs() < 0.001 || (width - 2.8).abs() < 0.001,
            "maintenance route is {width} m wide: {bounds:?}"
        );
    }

    let branches = named_bounds("Maintenance Lower");
    assert_eq!(
        branches.len(),
        2,
        "the lower maintenance deck needs two cross-routes"
    );
    for expected in [
        Bounds {
            min_x: -41.5,
            max_x: -38.7,
            min_z: -1.5,
            max_z: 43.5,
        },
        Bounds {
            min_x: -102.0,
            max_x: 6.0,
            min_z: 28.6,
            max_z: 31.4,
        },
    ] {
        assert!(
            branches
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "lower maintenance route is missing {expected:?}: {branches:?}",
        );
    }

    for entity in map
        .iter()
        .filter(|entity| property(entity, "room").as_deref() == Some("Maintenance Lower"))
    {
        for brush in &entity.brushes {
            let (min_y, max_y) = vertical_span(brush);
            assert!((min_y + 3.6).abs() < 0.001 && (max_y + 3.1).abs() < 0.001);
        }
    }

    for (room, axis, floor_min, floor_max) in [
        ("Maintenance Stair North", "z", "0", "-3.6"),
        ("Maintenance Stair South", "z", "-3.6", "0"),
        ("Maintenance Stair West", "x", "0", "-3.6"),
        ("Maintenance Stair East", "x", "-3.6", "0"),
    ] {
        let stair = map
            .iter()
            .find(|entity| property(entity, "room").as_deref() == Some(room))
            .unwrap_or_else(|| panic!("missing {room}"));
        assert_eq!(property(stair, "slope_axis").as_deref(), Some(axis));
        assert_eq!(property(stair, "floor_min").as_deref(), Some(floor_min));
        assert_eq!(property(stair, "floor_max").as_deref(), Some(floor_max));
    }
}

#[test]
fn maintenance_cross_has_visible_lower_floors_and_four_twelve_step_descents() {
    let map = parse();
    let world = map.first().expect("worldspawn");
    let uses = |brush: &[quake_map::Surface], texture: &str| {
        brush
            .iter()
            .all(|surface| surface.texture.to_string_lossy() == texture)
    };

    for expected in [
        Bounds {
            min_x: -41.5,
            max_x: -38.7,
            min_z: -1.5,
            max_z: 43.5,
        },
        Bounds {
            min_x: -102.0,
            max_x: 6.0,
            min_z: 28.6,
            max_z: 31.4,
        },
    ] {
        let floor = world.brushes.iter().find(|brush| {
            uses(brush, "floor_corridor") && bounds_are_close(footprint(brush), expected) && {
                let (min_y, max_y) = vertical_span(brush);
                (min_y + 4.0).abs() < 0.001 && (max_y + 3.6).abs() < 0.001
            }
        });
        assert!(
            floor.is_some(),
            "lower route has no visible floor: {expected:?}"
        );
    }

    for (name, envelope) in [
        (
            "north",
            Bounds {
                min_x: -41.5,
                max_x: -38.7,
                min_z: -8.1,
                max_z: -1.5,
            },
        ),
        (
            "south",
            Bounds {
                min_x: -41.5,
                max_x: -38.7,
                min_z: 43.5,
                max_z: 50.1,
            },
        ),
        (
            "west",
            Bounds {
                min_x: -108.6,
                max_x: -102.0,
                min_z: 28.6,
                max_z: 31.4,
            },
        ),
        (
            "east",
            Bounds {
                min_x: 6.0,
                max_x: 12.6,
                min_z: 28.6,
                max_z: 31.4,
            },
        ),
    ] {
        let steps = world
            .brushes
            .iter()
            .filter(|brush| uses(brush, "floor_corridor"))
            .filter(|brush| {
                let bounds = footprint(brush);
                bounds.min_x >= envelope.min_x - 0.001
                    && bounds.max_x <= envelope.max_x + 0.001
                    && bounds.min_z >= envelope.min_z - 0.001
                    && bounds.max_z <= envelope.max_z + 0.001
            })
            .filter(|brush| {
                let (min_y, max_y) = vertical_span(brush);
                ((max_y - min_y) - 0.2).abs() < 0.001 && max_y <= 0.001
            })
            .count();
        assert_eq!(steps, 12, "{name} maintenance stair is incomplete");
    }
}

#[test]
fn primary_department_doors_open_onto_the_ground_floor_public_loop() {
    let map = parse();
    let main_gallery = Bounds {
        min_x: -109.5,
        max_x: 13.5,
        min_z: 10.0,
        max_z: 15.0,
    };
    let public_loop: Vec<Bounds> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .filter(|entity| property(entity, "room").as_deref() == Some("Public Loop"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
        .collect();

    for department in ["medical", "bridge", "engineering", "service", "botany"] {
        let id = format!("door.{department}.public");
        let door = map
            .iter()
            .find(|entity| {
                classname(entity).as_deref() == Some("door_spot")
                    && property(entity, "id").as_deref() == Some(id.as_str())
            })
            .unwrap_or_else(|| panic!("missing relocated primary door `{id}`"));
        let (x, z) = origin_xz(door).expect("primary door has a valid origin");
        let point = Vec3::new(x, 0.0, z);
        assert!(
            main_gallery.holds(point) && public_loop.iter().any(|bounds| bounds.holds(point)),
            "{id} is not served by the ground-floor northern gallery: {point}",
        );
    }
}

#[test]
fn the_map_authors_two_semantic_delivery_windows() {
    let map = parse();
    for (id, lane) in [
        ("delivery.public", "public"),
        ("delivery.medical", "medical"),
    ] {
        let matches: Vec<_> = map
            .iter()
            .filter(|entity| classname(entity).as_deref() == Some("machine_spot"))
            .filter(|entity| property(entity, "id").as_deref() == Some(id))
            .collect();
        assert_eq!(matches.len(), 1, "expected one `{id}` window");
        assert_eq!(
            property(matches[0], "kind").as_deref(),
            Some("DeliveryWindow")
        );
        assert_eq!(property(matches[0], "lane").as_deref(), Some(lane));
    }
}

#[test]
fn medical_delivery_is_a_shared_wall_window_with_a_floored_chemistry_vestibule() {
    let map = parse();
    let marker = map
        .iter()
        .find(|entity| {
            classname(entity).as_deref() == Some("machine_spot")
                && property(entity, "id").as_deref() == Some("delivery.medical")
        })
        .expect("delivery.medical marker");
    let (x, z) = origin_xz(marker).expect("delivery.medical has a valid origin");
    let window = Vec3::new(x, 0.0, z);
    assert!(
        window.distance(Vec3::new(-16.5, 0.0, 3.25)) < 0.001,
        "clinical delivery must sit in the Medical/Chemistry partition: {window}",
    );
    assert_eq!(
        property(marker, "angles").as_deref(),
        Some("0 90 0"),
        "the clinical face must look into Medical while its back opens to Chemistry",
    );

    let world = map
        .iter()
        .find(|entity| classname(entity).as_deref() == Some("worldspawn"))
        .expect("worldspawn");
    let is_texture = |brush: &[quake_map::Surface], expected: &str| {
        brush
            .first()
            .is_some_and(|surface| surface.texture.to_string_lossy() == expected)
    };
    assert!(
        world.brushes.iter().any(|brush| {
            is_texture(brush, "wall")
                && vertical_span(brush).0 >= 2.34
                && footprint(brush).holds(window)
        }),
        "the delivery model has no shared-wall header above it",
    );
    assert!(
        world.brushes.iter().all(|brush| {
            !is_texture(brush, "wall")
                || vertical_span(brush).0 > 0.01
                || !footprint(brush).holds(window)
        }),
        "a floor-height wall still seals the clinical delivery opening",
    );

    let has_floor = |point: Vec3| {
        world.brushes.iter().any(|brush| {
            brush
                .first()
                .is_some_and(|surface| surface.texture.to_string_lossy().starts_with("floor_"))
                && vertical_span(brush).0 <= -0.01
                && vertical_span(brush).1 >= -0.01
                && footprint(brush).holds(point)
        })
    };
    for sample_x in [-17.0, -16.0, -14.0, -12.0, -10.0, -8.0, -7.0] {
        let sample = Vec3::new(sample_x, 0.0, 3.25);
        assert!(
            has_floor(sample),
            "the Medical-to-Prep handoff has a floor gap at {sample}",
        );
    }

    let vestibule: Vec<Bounds> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .filter(|entity| property(entity, "room").as_deref() == Some("Medical Handoff Vestibule"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
        .collect();
    assert_eq!(vestibule.len(), 1);
    assert!(bounds_are_close(
        vestibule[0],
        Bounds {
            min_x: -16.25,
            max_x: -7.5,
            min_z: 1.5,
            max_z: 5.0,
        }
    ));
}

#[test]
fn chemistry_public_door_has_enclosed_wall_returns_on_both_sides() {
    let map = parse();
    let door = map
        .iter()
        .find(|entity| {
            classname(entity).as_deref() == Some("door_spot")
                && property(entity, "id").as_deref() == Some("door.chemistry.public")
        })
        .expect("door.chemistry.public marker");
    let (door_x, door_z) = origin_xz(door).expect("Chemistry door has a valid origin");
    assert!((door_x - 4.0).abs() < 0.001 && (door_z - 7.1).abs() < 0.001);

    let world = map
        .iter()
        .find(|entity| classname(entity).as_deref() == Some("worldspawn"))
        .expect("worldspawn");
    let walls: Vec<Bounds> = world
        .brushes
        .iter()
        .filter(|brush| {
            brush
                .first()
                .is_some_and(|surface| surface.texture.to_string_lossy() == "wall")
                && vertical_span(brush).0 <= 0.01
                && vertical_span(brush).1 >= 3.19
        })
        .map(|brush| footprint(brush))
        .collect();

    for (side, expected) in [
        (
            "west",
            Bounds {
                min_x: 2.125,
                max_x: 2.375,
                min_z: 7.0,
                max_z: 10.0,
            },
        ),
        (
            "east",
            Bounds {
                min_x: 5.625,
                max_x: 5.875,
                min_z: 7.0,
                max_z: 10.0,
            },
        ),
    ] {
        assert!(
            walls
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "the {side} side of the Chemistry entrance has no enclosing wall return",
        );
    }

    let throat_points = [Vec3::new(4.0, 0.0, 7.5), Vec3::new(4.0, 0.0, 9.5)];
    for point in throat_points {
        assert!(
            world.brushes.iter().any(|brush| {
                brush
                    .first()
                    .is_some_and(|surface| surface.texture.to_string_lossy() == "floor_corridor")
                    && footprint(brush).holds(point)
            }),
            "the enclosed Chemistry entrance lost its floor at {point}",
        );
        assert!(
            walls.iter().all(|wall| !wall.holds(point)),
            "a new wall return blocks the Chemistry entrance at {point}",
        );
    }
}

#[test]
fn station_v2_lighting_stays_inside_the_shell_and_reaches_every_room() {
    let map = parse();
    let lights: Vec<(&Entity, Vec3)> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("light_point"))
        .map(|entity| {
            let (x, z) = origin_xz(entity).expect("a light_point with a valid origin");
            (entity, Vec3::new(x, 0.0, z))
        })
        .collect();

    for (_, point) in &lights {
        assert!(
            (-112.5..=16.5).contains(&point.x) && (-12.0..=54.0).contains(&point.z),
            "light at {point} sits outside the V2 station shell",
        );
    }

    let named_bounds = |room: &str| -> Vec<Bounds> {
        map.iter()
            .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
            .filter(|entity| property(entity, "room").as_deref() == Some(room))
            .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
            .collect()
    };
    for room in [
        "Security",
        "Security Back Room",
        "Bridge",
        "Bridge Operations",
        "Medical",
        "Quarantine",
        "Engineering",
        "Atmos/Utility",
        "Cargo",
        "Cargo Receiving",
        "Service",
        "Chapel",
        "Quiet Room",
        "Botany",
        "Botany Nursery",
        "Public Loop",
        "Maintenance V2",
    ] {
        let bounds = named_bounds(room);
        assert!(!bounds.is_empty(), "{room} has no walkable floor");
        assert!(
            lights
                .iter()
                .any(|(_, light)| bounds.iter().any(|area| area.holds(*light))),
            "{room} has no authored light",
        );
    }

    for (entity, point) in &lights {
        if point.x < -14.0 || point.z > 8.0 || point.z < -8.0 {
            assert_eq!(
                property(entity, "shadows_enabled").as_deref(),
                Some("false"),
                "station blockout light at {point} must stay non-shadowed",
            );
        }
    }
}

/// A department's gathering point must not sit in its own doorway.
///
/// `crew::Departments::home` is the *single* point everyone of a role walks
/// back to, and `somewhere_else` sends idle crew visiting another department
/// two thirds of the time — so this one spot is where a department's entire
/// floating population piles up. Five of the eight sat exactly 1.50 m inside
/// their primary door. A doorway is 1.6 m deep, so that is barely past its
/// inner mouth, and with `npc_motion::CLEARANCE` holding bodies 0.72 m apart a
/// few arrivals fill the opening and everyone behind them wedges against it.
///
/// Reported from play, in these words: "the place where the medical people
/// gather is right inside the door, so people are getting stuck." Engineering
/// (5.40 m) and Botany (5.00 m) were already clear of theirs, and are where
/// this threshold comes from — it is the authored norm, not a new invention.
///
/// Deliberately checked against *every* door rather than the department's own:
/// a gathering point that has been nudged clear of its front door and into a
/// maintenance one is the same bug wearing a different hat.
#[test]
fn a_department_gathering_point_is_clear_of_every_doorway() {
    /// Room for a body (0.70 m) plus a full clearance gap (0.72 m) beyond the
    /// far mouth of a 1.6 m-deep doorway, rounded up. Below this, a queue at
    /// the gathering point reaches back into the opening.
    const CLEAR_OF_DOOR: f32 = 4.5;

    let map = parse();
    let doors: Vec<(String, Vec3)> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("door_spot"))
        .filter_map(|entity| {
            let id = property(entity, "id")?;
            let (x, z) = origin_xz(entity)?;
            Some((id, Vec3::new(x, 0.0, z)))
        })
        .collect();
    assert!(
        !doors.is_empty(),
        "the map authors no doors to check against"
    );

    let mut checked = 0;
    for spot in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("department_spot"))
    {
        let department = property(spot, "department").unwrap_or_else(|| "<unnamed>".into());
        let (x, z) = origin_xz(spot).expect("department_spot has a valid origin");
        let at = Vec3::new(x, 0.0, z);
        for (id, door) in &doors {
            let gap = at.distance(*door);
            assert!(
                gap >= CLEAR_OF_DOOR,
                "{department}'s gathering point is {gap:.2}m from `{id}`, inside \
                 the {CLEAR_OF_DOOR}m a crowd needs to not block it — every idle \
                 visitor to {department} walks to this exact point",
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 6,
        "only {checked} department gathering points were checked; the map should \
         have one per department and this test is silently covering nothing",
    );
}

#[test]
fn every_department_on_the_crew_roster_has_somewhere_to_live() {
    // The station's wings and the crew roster have to agree, or a Botanist
    // spawns with nowhere to walk back to. `station.crew.ron` is the authority
    // on which departments exist — the map has to keep up with it, not the
    // other way round.
    let roster = std::fs::read_to_string("assets/data/station.crew.ron")
        .expect("assets/data/station.crew.ron");

    let map = parse();
    let spots: Vec<String> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("department_spot"))
        .filter_map(|entity| property(entity, "department"))
        .collect();

    for department in [
        "Medical",
        "Security",
        "Engineering",
        "Cargo",
        "Service",
        "Botany",
    ] {
        assert!(
            roster.contains(&format!("role: \"{department}\"")),
            "{department} is not on the crew roster any more; the map still has a wing for it",
        );
        assert!(
            spots.iter().any(|spot| spot == department),
            "{department} is on the crew roster but has no department_spot in {MAP}",
        );
    }

    assert!(
        map.iter()
            .any(|entity| classname(entity).as_deref() == Some("escape_pod")),
        "{MAP} has no escape pod",
    );
}

#[test]
fn every_named_crew_member_has_a_work_post() {
    // A missing post is optional by design — `crew::CrewPosts::work` falls
    // back to the shared department point exactly as before Phase 3
    // existed — but once authoring is meant to be complete, a silently
    // missing one is worth catching here rather than only noticing someone
    // still stacked on their department-mate in play.
    let map = parse();
    let work_posts: Vec<String> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("crew_post"))
        .filter(|entity| property(entity, "kind").as_deref() == Some("work"))
        .filter_map(|entity| property(entity, "occupant"))
        .collect();

    for name in [
        "Dr. Vance",
        "Nurse Okonkwo",
        "Officer Reyes",
        "Warden Bex",
        "Tech Lindqvist",
        "Miner Sato",
        "Quartermaster Rhee",
        "Botanist Ivy",
        "Chef Dubois",
    ] {
        assert!(
            work_posts.iter().any(|occupant| occupant == name),
            "{name} has no work-kind crew_post in {MAP}",
        );
    }
}

#[test]
fn department_dressing_markers_fit_their_authored_rooms() {
    // Each shell-free set keeps the starter bay's 4.6 x 3.6 m authoring
    // envelope. These origins put the five public departments against their
    // rear walls while retaining the full south-side route to the corridor.
    // Chemistry deliberately uses modular fixtures instead of a starter bay.
    const HALF_WIDTH: f32 = 2.3;
    const HALF_DEPTH: f32 = 1.8;
    const EXPECTED: &[(&str, &str, Vec3, &str)] = &[
        ("Medical", "Medical", Vec3::new(-27.0, 0.0, 4.0), "0 180 0"),
        (
            "Engineering",
            "Engineering",
            Vec3::new(-80.0, 0.0, 18.0),
            "0 180 0",
        ),
        ("Cargo", "Cargo", Vec3::new(-70.0, 0.0, 45.0), "0 180 0"),
        (
            "Security",
            "Security",
            Vec3::new(-96.0, 0.0, 4.0),
            "0 180 0",
        ),
        ("Service", "Service", Vec3::new(-35.0, 0.0, 18.0), "0 180 0"),
    ];

    let map = parse();
    let markers: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("department_dressing"))
        .collect();
    assert_eq!(
        markers.len(),
        EXPECTED.len(),
        "only departments still using a starter bay should have dressing markers",
    );
    assert!(
        markers
            .iter()
            .all(|marker| property(marker, "department").as_deref() != Some("Chemistry")),
        "Chemistry's overlapping starter dressing must stay replaced by modular fixtures",
    );

    for (department, room, expected_origin, expected_angles) in EXPECTED {
        let matches: Vec<&Entity> = markers
            .iter()
            .copied()
            .filter(|entity| property(entity, "department").as_deref() == Some(*department))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "{department} should have exactly one department_dressing marker",
        );

        let marker = matches[0];
        let (x, z) = origin_xz(marker).expect("a dressing marker with a valid origin");
        let origin = Vec3::new(x, 0.0, z);
        assert!(
            origin.distance(*expected_origin) < 0.001,
            "{department} dressing moved to {origin}; expected {expected_origin}",
        );
        assert_eq!(
            property(marker, "angles").as_deref(),
            Some(*expected_angles),
            "{department} dressing no longer faces into its room",
        );

        let room_bounds: Vec<Bounds> = map
            .iter()
            .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
            .filter(|entity| property(entity, "room").as_deref() == Some(*room))
            .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
            .collect();
        assert!(
            room_bounds.iter().any(|bounds| {
                origin.x - HALF_WIDTH >= bounds.min_x - 0.001
                    && origin.x + HALF_WIDTH <= bounds.max_x + 0.001
                    && origin.z - HALF_DEPTH >= bounds.min_z - 0.001
                    && origin.z + HALF_DEPTH <= bounds.max_z + 0.001
            }),
            "{department}'s 4.6 x 3.6 m dressing envelope leaves {room}: {origin} in {room_bounds:?}",
        );
    }
}

#[test]
fn every_department_dressing_marker_has_an_exported_glb() {
    for (department, path) in [
        (
            "Chemistry",
            "assets/3dassets/station_starter_kit/glb/department_chemistry_dressing.glb",
        ),
        (
            "Medical",
            "assets/3dassets/station_starter_kit/glb/department_medical_dressing.glb",
        ),
        (
            "Engineering",
            "assets/3dassets/station_starter_kit/glb/department_engineering_dressing.glb",
        ),
        (
            "Cargo",
            "assets/3dassets/station_starter_kit/glb/department_cargo_dressing.glb",
        ),
        (
            "Security",
            "assets/3dassets/station_starter_kit/glb/department_security_dressing.glb",
        ),
        (
            "Service",
            "assets/3dassets/station_starter_kit/glb/department_service_dressing.glb",
        ),
    ] {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|error| panic!("{department} dressing is missing at {path}: {error}"));
        bevy::gltf::gltf::Gltf::from_slice(&bytes).unwrap_or_else(|error| {
            panic!("{department} dressing at {path} is not Bevy-compatible glTF: {error}")
        });
    }
}

const ASSET_SESSION_KINDS: &[&str] = &[
    "hall.waiting_bench",
    "hall.wall_light",
    "hall.planter",
    "hall.waste_station",
    "chapel.pew",
    "chapel.plinth",
    "chapel.votive_stand",
    "chapel.lectern",
    "quiet.armchair",
    "quiet.reading_shelf",
    "quiet.side_table_lamp",
    "quiet.acoustic_panel",
    "med.patient_bay",
    "med.nurse_station",
    "med.supply_shelf",
    "med.crash_cart",
    "med.examination_couch",
    "med.diagnostic_stand",
    "med.privacy_screen",
    "med.hygiene_cabinet",
    "sec.dispatch_console",
    "sec.equipment_locker_bank",
    "sec.interrogation_table",
    "sec.wall_camera",
    "sec.radio_charger",
    "sec.restraint_display",
    "sec.report_shelf",
    "sec.personal_effects",
    "eng.generator_turbine",
    "eng.smes_bank",
    "eng.parts_workbench",
    "eng.pump_assembly",
    "eng.tool_trolley",
    "eng.cable_junction",
    "eng.hose_reel",
    "eng.parts_shelf",
];

const ASSET_SESSION_FLOOR_KINDS: &[&str] = &[
    "hall.waiting_bench",
    "hall.planter",
    "hall.waste_station",
    "chapel.pew",
    "chapel.plinth",
    "chapel.votive_stand",
    "chapel.lectern",
    "quiet.armchair",
    "quiet.reading_shelf",
    "quiet.side_table_lamp",
    "med.patient_bay",
    "med.nurse_station",
    "med.crash_cart",
    "med.examination_couch",
    "med.diagnostic_stand",
    "med.privacy_screen",
    "sec.dispatch_console",
    "sec.equipment_locker_bank",
    "sec.interrogation_table",
    "eng.generator_turbine",
    "eng.smes_bank",
    "eng.parts_workbench",
    "eng.pump_assembly",
    "eng.tool_trolley",
    "med.waiting_row",
    "med.cryo_pod",
    "med.cryo_pod_occupied",
    "med.cryo_monitor_bay",
];

#[test]
fn decoration_markers_have_known_assets_and_fit_their_rooms() {
    /// How a module's envelope relates to its marker origin.
    ///
    /// The two categories in the starter kit place their origin differently, so
    /// the same `origin`/`angles` pair means different things depending on
    /// which one a `kind` names.
    #[derive(PartialEq)]
    enum Mount {
        /// Origin on the wall face, geometry entirely in front of it.
        Wall,
        /// Origin at the centre of the footprint, geometry all around it. A
        /// fixture stands on the floor, so it must also sit in ground the map
        /// has carved *out* of the walkable volume — decorations carry no
        /// collider, and crew path by walkable area alone.
        Floor,
    }

    struct Placement {
        kind: &'static str,
        origin: &'static str,
        angles: &'static str,
        mount: Mount,
        /// For a wall module, the walkable room the envelope must stay inside.
        /// For a floor fixture, the room's *full* floor area including the
        /// carved-out strip.
        room: Bounds,
        width: f32,
        depth: f32,
    }

    // The two rooms whose walkable rectangle is deliberately smaller than their
    // floor, so a fixture has somewhere to stand that nothing walks through.
    const MEDICAL_WALKABLE: Bounds = Bounds {
        min_x: -36.2,
        max_x: -16.5,
        min_z: -3.0,
        max_z: 10.0,
    };
    const MEDICAL_FLOOR: Bounds = Bounds {
        min_x: -38.7,
        max_x: -16.5,
        min_z: -3.0,
        max_z: 10.0,
    };
    const QUARANTINE_WALKABLE: Bounds = Bounds {
        min_x: -37.5,
        max_x: -16.5,
        min_z: -9.0,
        max_z: -3.0,
    };
    const QUARANTINE_FLOOR: Bounds = Bounds {
        min_x: -38.7,
        max_x: -16.5,
        min_z: -9.0,
        max_z: -3.0,
    };

    // Security and Service retain continuous floors with runtime furniture
    // collision. Engineering's original floor
    // fixtures sit in the west sliver, a corner its walkable volume no
    // longer covers (world x -109.5..-102, z 23..28.6), so this is the
    // room's full floor — matching how `MEDICAL_FLOOR` differs from
    // `MEDICAL_WALKABLE` above.
    const ENGINEERING: Bounds = Bounds {
        min_x: -109.5,
        max_x: -66.0,
        min_z: 15.0,
        max_z: 28.6,
    };
    // The underfloor arm, Engineering's southern floor over what used to be
    // an open trench. The high-voltage sign mounts on its north wall, shared
    // with Cargo's cross lane.
    const ENGINEERING_ARM: Bounds = Bounds {
        min_x: -102.0,
        max_x: -66.0,
        min_z: 27.6,
        max_z: 31.4,
    };
    // Shares Engineering's "ENGINEERING / ATMOS" signage and decoration
    // materials, but is its own room rectangle.
    const ATMOS_UTILITY: Bounds = Bounds {
        min_x: -66.0,
        max_x: -52.5,
        min_z: 15.0,
        max_z: 28.6,
    };
    const CARGO: Bounds = Bounds {
        min_x: -94.0,
        max_x: -52.5,
        min_z: 31.4,
        max_z: 51.0,
    };
    const SECURITY: Bounds = Bounds {
        min_x: -109.5,
        max_x: -83.5,
        min_z: -2.0,
        max_z: 10.0,
    };
    const SECURITY_BACK: Bounds = Bounds {
        min_x: -109.5,
        max_x: -83.5,
        min_z: -9.0,
        max_z: -2.0,
    };
    // Service is drawn as three brushes; every module here sits in the wide
    // eastern one.
    //
    // These bounds have twice been narrower than the room. `max_z` used to stop
    // at 28.6 and `min_x` at -38.7 — neither because the floor did, but because
    // nothing had ever been placed further out. The walkable brush is
    // x -47.5..-23.62, z 15..39 (see `station_v2_rooms_are_where_the_plan_says`),
    // so the old bounds admitted 365 m² of a 573 m² room and quietly made the
    // western third unbuildable. That is most of why Service measured 0.8
    // decorations per 100 m² against a station norm of 2.4-4.1: the validation
    // that was supposed to keep props inside the room was also keeping them out
    // of two thirds of it.
    const SERVICE_EAST: Bounds = Bounds {
        min_x: -47.5,
        max_x: -23.5,
        min_z: 15.0,
        max_z: 39.0,
    };
    // Botany is one continuous greenhouse across the old maintenance spine.
    // Its floor fixtures are carved out of the walkable brushes, but placement
    // validation uses the complete architectural floor they stand on.
    const BOTANY: Bounds = Bounds {
        min_x: -18.5,
        max_x: 13.5,
        min_z: 15.0,
        max_z: 42.0,
    };
    const BOTANY_NURSERY: Bounds = Bounds {
        min_x: -18.5,
        max_x: 13.5,
        min_z: 42.0,
        max_z: 51.0,
    };
    const CHAPEL: Bounds = Bounds {
        min_x: -38.7,
        max_x: -23.5,
        min_z: 44.0,
        max_z: 51.0,
    };
    // Bridge's two halves. The wall that used to divide them is gone, so
    // these are names for the north and south ends of one 42 x 19 m floor
    // rather than separate rooms -- and the mission-control pass's third
    // furniture row straddles the old z -3.0 dividing line, which is why the
    // main hall's floor is quoted down to the far edge of that row.
    const BRIDGE: Bounds = Bounds {
        min_x: -83.5,
        max_x: -41.5,
        min_z: -3.5,
        max_z: 10.0,
    };
    const BRIDGE_OPERATIONS: Bounds = Bounds {
        min_x: -83.5,
        max_x: -41.5,
        min_z: -9.0,
        max_z: -3.0,
    };

    const MAIN_HALL: Bounds = Bounds {
        min_x: -80.0,
        max_x: -40.0,
        min_z: 10.0,
        max_z: 15.0,
    };
    const QUIET_ROOM: Bounds = Bounds {
        min_x: -47.5,
        max_x: -41.5,
        min_z: 44.0,
        max_z: 51.0,
    };

    // Metre measurements, including the 3.14m waiting row, are not angles.
    #[allow(clippy::approx_constant)]
    const PLACEMENTS: &[Placement] = &[
        Placement {
            kind: "chem.supply_shelf",
            origin: "-232 254 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: Bounds {
                min_x: -7.5,
                max_x: 0.5,
                min_z: 2.0,
                max_z: 6.0,
            },
            width: 1.65,
            depth: 0.38,
        },
        Placement {
            kind: "chem.sink_island",
            origin: "88 140 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: Bounds {
                min_x: -7.5,
                max_x: 7.5,
                min_z: -5.5,
                max_z: 2.0,
            },
            width: 2.60,
            depth: 0.90,
        },
        Placement {
            kind: "chem.clean_workbench",
            origin: "88 -80 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: Bounds {
                min_x: -7.5,
                max_x: 7.5,
                min_z: -5.5,
                max_z: 2.0,
            },
            width: 2.60,
            depth: 0.90,
        },
        Placement {
            kind: "chem.sample_bench",
            origin: "56 -360 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: Bounds {
                min_x: 7.5,
                max_x: 13.5,
                min_z: -5.5,
                max_z: -0.5,
            },
            width: 2.20,
            depth: 0.80,
        },
        Placement {
            kind: "chem.fume_hood",
            origin: "204 416 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: Bounds {
                min_x: -13.5,
                max_x: -7.5,
                min_z: -5.5,
                max_z: -1.5,
            },
            width: 1.50,
            depth: 0.75,
        },
        Placement {
            kind: "chem.analysis_panel",
            origin: "104 -536 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: Bounds {
                min_x: 7.5,
                max_x: 13.5,
                min_z: -5.5,
                max_z: -0.5,
            },
            width: 1.55,
            depth: 0.24,
        },
        Placement {
            kind: "chem.emergency_station",
            origin: "64 416 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: Bounds {
                min_x: -13.5,
                max_x: -7.5,
                min_z: -5.5,
                max_z: -1.5,
            },
            width: 1.25,
            depth: 0.42,
        },
        Placement {
            kind: "chem.service_board",
            origin: "-216 -296 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: Bounds {
                min_x: 0.5,
                max_x: 7.5,
                min_z: 2.0,
                max_z: 7.0,
            },
            width: 1.35,
            depth: 0.22,
        },
        Placement {
            kind: "chem.service_board",
            origin: "-64 480 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: Bounds {
                min_x: -16.25,
                max_x: -7.5,
                min_z: 1.5,
                max_z: 5.0,
            },
            width: 1.35,
            depth: 0.22,
        },
        Placement {
            kind: "med.supply_shelf",
            origin: "-260 665 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: MEDICAL_WALKABLE,
            width: 1.65,
            depth: 0.38,
        },
        Placement {
            kind: "med.vitals_panel",
            origin: "-20 665 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: MEDICAL_WALKABLE,
            width: 1.55,
            depth: 0.24,
        },
        Placement {
            kind: "med.crash_station",
            origin: "115 1320 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: MEDICAL_WALKABLE,
            width: 1.25,
            depth: 0.42,
        },
        Placement {
            kind: "med.triage_board",
            origin: "-395 840 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: MEDICAL_WALKABLE,
            width: 1.35,
            depth: 0.22,
        },
        Placement {
            kind: "med.quarantine_seal",
            origin: "125 1200 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: QUARANTINE_WALKABLE,
            width: 1.15,
            depth: 0.30,
        },
        Placement {
            kind: "med.waiting_row",
            origin: "98 890 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MEDICAL_FLOOR,
            width: 3.14,
            depth: 0.59,
        },
        Placement {
            kind: "med.specimen_cold",
            origin: "240 1529 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: QUARANTINE_FLOOR,
            width: 1.00,
            depth: 0.70,
        },
        Placement {
            kind: "eng.breaker_panel",
            origin: "-1251 3800 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: ENGINEERING_ARM,
            width: 1.55,
            depth: 0.24,
        },
        Placement {
            kind: "eng.tool_board",
            origin: "-1251 3520 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: ENGINEERING_ARM,
            width: 1.65,
            depth: 0.38,
        },
        Placement {
            kind: "eng.pipe_manifold",
            origin: "-605 2800 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: ENGINEERING,
            width: 1.45,
            depth: 0.35,
        },
        Placement {
            kind: "eng.safety_station",
            origin: "-1000 4375 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: ENGINEERING,
            width: 1.25,
            depth: 0.42,
        },
        // Floor fixtures in the north band brush 0's carve opened up. Big
        // machinery (SMES bank, generator) sits west of the cross lane, where
        // the arm's own floor does not reach; tools and suits sit east of it.
        Placement {
            kind: "eng.smes_bank",
            origin: "-1116 4160 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: ENGINEERING,
            width: 2.20,
            depth: 1.00,
        },
        Placement {
            kind: "eng.generator_turbine",
            origin: "-1096 4292 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: ENGINEERING,
            width: 2.60,
            depth: 2.00,
        },
        Placement {
            kind: "eng.hardsuit_locker",
            origin: "-948 4324 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: ENGINEERING,
            width: 1.80,
            depth: 0.85,
        },
        Placement {
            kind: "eng.parts_workbench",
            origin: "-948 4220 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: ENGINEERING,
            width: 1.90,
            depth: 0.95,
        },
        Placement {
            kind: "eng.cable_spool_rack",
            origin: "-948 4120 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: ENGINEERING,
            width: 1.50,
            depth: 0.90,
        },
        Placement {
            kind: "eng.power_monitor_console",
            origin: "-1251 3360 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: ENGINEERING_ARM,
            width: 1.60,
            depth: 0.32,
        },
        Placement {
            kind: "eng.solar_readout",
            origin: "-605 4000 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: ENGINEERING,
            width: 1.10,
            depth: 0.20,
        },
        // On the arm's north wall, shared with Cargo's cross lane, a few
        // metres from the technical airlock both sides now dress around.
        Placement {
            kind: "eng.hv_warning_sign",
            origin: "-1251 3600 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: ENGINEERING_ARM,
            width: 0.70,
            depth: 0.16,
        },
        // Atmos/Utility's floor fixtures, in the strip its own walkable
        // brush shrink opened up against the room's east wall.
        Placement {
            kind: "eng.gas_canister_rack",
            origin: "-760 2140 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: ATMOS_UTILITY,
            width: 1.60,
            depth: 0.80,
        },
        Placement {
            kind: "eng.filtration_scrubber",
            origin: "-880 2140 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: ATMOS_UTILITY,
            width: 1.20,
            depth: 1.20,
        },
        Placement {
            kind: "eng.gas_canister_rack",
            origin: "-1000 2140 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: ATMOS_UTILITY,
            width: 1.60,
            depth: 0.80,
        },
        // ------------------------------------------------------------------
        // Bridge, dressed as mission control.
        //
        // The first pass put eleven modules against the walls of a 42 x 19 m
        // room: 32 m^2 of furniture, 4% of the floor, nothing in the middle.
        // This is the same room filled the way its shape wants -- ranks of
        // console arcs, a command dais centred in the west wing, and a
        // viewscreen wall along the south end for the ranks to face.
        //
        // Every floor fixture is "0 180 0", facing away from the public door,
        // so walking in you see the backs of the chairs and the crew are
        // looking at the screens beyond them. `bridge.duty_station_bank` is
        // deliberately absent: it is built back-to-back, three desks facing
        // each way, so in a rank of single-sided consoles half its chairs
        // always point the wrong way.
        //
        // Rows sit in the gaps between the walkable concourses, and each row
        // is filled end to end on purpose. A floor fixture must stand in
        // ground carved out of the walkable volume, so the carve follows the
        // rows; a gap left inside a row is not open floor, it is an invisible
        // wall a body cannot walk into and nothing on screen explains.
        // ------------------------------------------------------------------
        // Row 1 -- the front rank, hard up against the viewscreen wall.
        Placement {
            kind: "bridge.console_arc",
            origin: "-286 3214 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "-286 2974 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "-286 2734 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.tactical_rail",
            origin: "-286 2554 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 3.00,
            depth: 1.60,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "-286 2170 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "-286 1930 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.tactical_rail",
            origin: "-286 1750 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 3.00,
            depth: 1.60,
        },
        // Row 2 -- the command row: the dais centred in the west wing, ops desks east.
        Placement {
            kind: "bridge.holomap_island",
            origin: "-94 3290 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 2.20,
            depth: 2.20,
        },
        Placement {
            kind: "bridge.tactical_rail",
            origin: "-94 3186 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 3.00,
            depth: 1.60,
        },
        Placement {
            kind: "bridge.briefing_table",
            origin: "-94 3082 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 2.20,
            depth: 1.70,
        },
        Placement {
            kind: "bridge.command_dais",
            origin: "-94 2920 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 5.00,
            depth: 3.00,
        },
        Placement {
            kind: "bridge.captains_chair",
            origin: "-94 2808 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 0.60,
            depth: 0.55,
        },
        Placement {
            kind: "bridge.astrogation_pillar",
            origin: "-94 2784 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 0.60,
            depth: 0.60,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "-94 2652 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.astrogation_pillar",
            origin: "-94 2520 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 0.60,
            depth: 0.60,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "-94 2170 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.briefing_table",
            origin: "-94 2006 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 2.20,
            depth: 1.70,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "-94 1842 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        // Row 3 -- the second rank.
        Placement {
            kind: "bridge.tactical_rail",
            origin: "98 3274 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 3.00,
            depth: 1.60,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "98 3094 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "98 2854 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "98 2614 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "98 1788 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "98 2028 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.tactical_rail",
            origin: "98 2208 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE,
            width: 3.00,
            depth: 1.60,
        },
        // Row 4 -- back of house, along the maintenance wall.
        Placement {
            kind: "bridge.tactical_rail",
            origin: "270 3274 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE_OPERATIONS,
            width: 3.00,
            depth: 1.60,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "270 3094 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE_OPERATIONS,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.briefing_table",
            origin: "270 2930 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE_OPERATIONS,
            width: 2.20,
            depth: 1.70,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "270 2766 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE_OPERATIONS,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.tactical_rail",
            origin: "270 2586 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE_OPERATIONS,
            width: 3.00,
            depth: 1.60,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "270 2170 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE_OPERATIONS,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.console_arc",
            origin: "270 1930 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE_OPERATIONS,
            width: 6.00,
            depth: 2.00,
        },
        Placement {
            kind: "bridge.briefing_table",
            origin: "270 1766 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: BRIDGE_OPERATIONS,
            width: 2.20,
            depth: 1.70,
        },
        // Wall modules.
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 3260 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 3140 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 3020 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 2900 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 2780 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 2660 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 2540 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 2260 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 2140 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 2020 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 1900 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.viewscreen",
            origin: "360 1780 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 2.40,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.alert_panel",
            origin: "360 3320 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 0.60,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.alert_panel",
            origin: "360 1680 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: BRIDGE_OPERATIONS,
            width: 0.60,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.nav_desk",
            origin: "-400 3200 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.80,
            depth: 0.60,
        },
        Placement {
            kind: "bridge.comms_console",
            origin: "-400 3040 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.40,
            depth: 0.30,
        },
        Placement {
            kind: "bridge.crew_roster_board",
            origin: "-400 2880 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.20,
            depth: 0.12,
        },
        Placement {
            kind: "bridge.alert_panel",
            origin: "-400 2720 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 0.60,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.nav_desk",
            origin: "-400 2580 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.80,
            depth: 0.60,
        },
        Placement {
            kind: "bridge.alert_panel",
            origin: "-400 2300 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 0.60,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.crew_roster_board",
            origin: "-400 2040 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.20,
            depth: 0.12,
        },
        Placement {
            kind: "bridge.alert_panel",
            origin: "-400 1740 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 0.60,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.alert_panel",
            origin: "-364 3340 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 0.60,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.comms_console",
            origin: "-200 3340 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.40,
            depth: 0.30,
        },
        Placement {
            kind: "bridge.crew_roster_board",
            origin: "12 3340 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.20,
            depth: 0.12,
        },
        Placement {
            kind: "bridge.alert_panel",
            origin: "-364 1660 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 0.60,
            depth: 0.15,
        },
        Placement {
            kind: "bridge.crew_roster_board",
            origin: "-200 1660 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.20,
            depth: 0.12,
        },
        Placement {
            kind: "bridge.comms_console",
            origin: "12 1660 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: BRIDGE,
            width: 1.40,
            depth: 0.30,
        },
        // Both were on the north wall until the freight line took it; a wall
        // module behind a running conveyor is a wall module nobody sees.
        Placement {
            kind: "cargo.manifest_board",
            origin: "-1520 2105 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: CARGO,
            width: 1.35,
            depth: 0.22,
        },
        Placement {
            kind: "cargo.parcel_shelf",
            origin: "-1760 2105 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: CARGO,
            width: 1.65,
            depth: 0.38,
        },
        Placement {
            kind: "cargo.dispatch_panel",
            origin: "-1261 2640 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: CARGO,
            width: 1.55,
            depth: 0.24,
        },
        Placement {
            kind: "cargo.weigh_station",
            origin: "-1840 2105 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: CARGO,
            width: 1.25,
            depth: 0.40,
        },
        // The freight bay's floor fixtures, down the south wall and past the
        // end of the belt. Every one stands in a band the map carves out of
        // Cargo's walkable volume; the floor-fixture assertion below is what
        // proves that, rather than these coordinates being trusted.
        Placement {
            kind: "cargo.storage_rack",
            origin: "-1292 3680 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 3.36,
            depth: 1.26,
        },
        Placement {
            kind: "cargo.storage_rack",
            origin: "-1292 3540 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 3.36,
            depth: 1.26,
        },
        Placement {
            kind: "cargo.storage_rack",
            origin: "-1292 3400 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 3.36,
            depth: 1.26,
        },
        Placement {
            kind: "cargo.storage_rack",
            origin: "-1292 3060 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 3.36,
            depth: 1.26,
        },
        Placement {
            kind: "cargo.crate_stack",
            origin: "-1292 2928 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 1.34,
            depth: 1.12,
        },
        Placement {
            kind: "cargo.crate_stack",
            origin: "-1292 2840 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 1.34,
            depth: 1.12,
        },
        Placement {
            kind: "cargo.pallet_row",
            origin: "-1292 2740 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 2.66,
            depth: 1.10,
        },
        Placement {
            kind: "cargo.storage_rack",
            origin: "-1292 2500 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 3.36,
            depth: 1.26,
        },
        Placement {
            kind: "cargo.storage_rack",
            origin: "-1292 2360 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 3.36,
            depth: 1.26,
        },
        Placement {
            kind: "cargo.forklift_bay",
            origin: "-1320 2220 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 2.42,
            depth: 2.62,
        },
        Placement {
            kind: "cargo.requisitions_desk",
            origin: "-1936 2220 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: CARGO,
            width: 2.20,
            depth: 0.88,
        },
        Placement {
            kind: "sec.notice_board",
            origin: "-395 4120 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: SECURITY,
            width: 1.35,
            depth: 0.22,
        },
        Placement {
            kind: "sec.camera_bank",
            origin: "-395 3560 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: SECURITY,
            width: 1.30,
            depth: 0.26,
        },
        Placement {
            kind: "sec.armory_rack",
            origin: "-200 3345 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: SECURITY,
            width: 1.55,
            depth: 0.36,
        },
        Placement {
            kind: "sec.evidence_wall",
            origin: "-80 4375 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: SECURITY,
            width: 1.45,
            depth: 0.34,
        },
        Placement {
            kind: "sec.dispatch_console",
            origin: "0 3400 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: SECURITY,
            width: 2.40,
            depth: 1.20,
        },
        Placement {
            kind: "sec.officer_desk_bank",
            origin: "-250 4320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: SECURITY,
            width: 3.40,
            depth: 1.70,
        },
        Placement {
            kind: "sec.evidence_locker_bank",
            origin: "290 4320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: SECURITY_BACK,
            width: 2.20,
            depth: 0.90,
        },
        Placement {
            kind: "sec.equipment_locker_bank",
            origin: "290 3400 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: SECURITY_BACK,
            width: 2.20,
            depth: 0.90,
        },
        Placement {
            kind: "sec.brig_bunk",
            origin: "300 4080 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: SECURITY_BACK,
            width: 1.05,
            depth: 2.20,
        },
        Placement {
            kind: "sec.interrogation_table",
            origin: "300 3630 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: SECURITY_BACK,
            width: 2.20,
            depth: 1.90,
        },
        Placement {
            kind: "sec.mugshot_board",
            origin: "85 4120 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: SECURITY_BACK,
            width: 1.80,
            depth: 0.24,
        },
        Placement {
            kind: "sec.alert_panel",
            origin: "355 3560 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: SECURITY_BACK,
            width: 1.20,
            depth: 0.26,
        },
        Placement {
            kind: "svc.menu_board",
            origin: "-1139 1200 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: SERVICE_EAST,
            width: 1.40,
            depth: 0.22,
        },
        Placement {
            kind: "svc.crockery_shelf",
            origin: "-1139 1060 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: SERVICE_EAST,
            width: 1.55,
            depth: 0.36,
        },
        Placement {
            kind: "svc.drinks_board",
            origin: "-760 945 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: SERVICE_EAST,
            width: 1.35,
            depth: 0.28,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1156 1400 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1146 1362 0",
            angles: "0 30.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1118 1334 0",
            angles: "0 60.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1080 1324 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1042 1334 0",
            angles: "0 120.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1014 1362 0",
            angles: "0 150.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1014 1438 0",
            angles: "0 210.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1042 1466 0",
            angles: "0 240.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1080 1476 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1118 1466 0",
            angles: "0 300.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bar_counter",
            origin: "-1146 1438 0",
            angles: "0 330.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.02,
            depth: 0.68,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1186 1372 0",
            angles: "0 15.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1158 1322 0",
            angles: "0 45.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1108 1294 0",
            angles: "0 75.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1052 1294 0",
            angles: "0 105.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1002 1322 0",
            angles: "0 135.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1052 1506 0",
            angles: "0 255.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1108 1506 0",
            angles: "0 285.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1158 1478 0",
            angles: "0 315.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.bench",
            origin: "-1186 1428 0",
            angles: "0 345.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-760 1800 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-789 1800 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-760 1771 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-731 1800 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-760 1829 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-760 1660 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-789 1660 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-760 1631 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-731 1660 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-760 1689 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1080 1800 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1109 1800 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1080 1771 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1051 1800 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1080 1829 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1240 1660 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1269 1660 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1240 1631 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1211 1660 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1240 1689 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1400 1800 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1429 1800 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1400 1771 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1371 1800 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1400 1829 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1400 1660 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1429 1660 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1400 1631 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1371 1660 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1400 1689 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-760 1140 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-789 1140 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-760 1111 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-731 1140 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-760 1169 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1080 1140 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1109 1140 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1080 1111 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1051 1140 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1080 1169 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1240 1260 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1269 1260 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1240 1231 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1211 1260 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1240 1289 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1400 1140 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1429 1140 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1400 1111 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1371 1140 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1400 1169 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1400 1260 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1429 1260 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1400 1231 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1371 1260 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1400 1289 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_table",
            origin: "-1240 1540 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 1.10,
            depth: 1.10,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1269 1540 0",
            angles: "0 0.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1240 1511 0",
            angles: "0 90.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1211 1540 0",
            angles: "0 180.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.dining_chair",
            origin: "-1240 1569 0",
            angles: "0 270.0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.44,
            depth: 0.46,
        },
        Placement {
            kind: "svc.pass_hatch",
            origin: "-605 1120 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: SERVICE_EAST,
            width: 1.60,
            depth: 0.44,
        },
        // Both benches sit at the exact same raw origins as the two "relax"
        // `crew_post` markers in the Service hall (`assets/maps/lab.map`),
        // so the seat a resident's `Sitting` animation settles onto is
        // physically where the game says they are standing.
        Placement {
            kind: "svc.bench",
            origin: "-840 1400 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        // --------------------------------------------------------------
        // Botany greenhouse. Two planted lanes frame the public entrance;
        // the deeper room divides into mature crops, a research island, and
        // hydroponic support; the nursery is a paired propagation promenade.
        // --------------------------------------------------------------
        Placement {
            kind: "bot.grow_plot",
            origin: "-760 320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 4.00,
            depth: 2.40,
        },
        Placement {
            kind: "bot.grow_plot",
            origin: "-920 320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 4.00,
            depth: 2.40,
        },
        Placement {
            kind: "bot.planter_row",
            origin: "-1030 320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 1.50,
            depth: 2.40,
        },
        Placement {
            kind: "bot.grow_plot",
            origin: "-760 -320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 4.00,
            depth: 2.40,
        },
        Placement {
            kind: "bot.grow_plot",
            origin: "-920 -320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 4.00,
            depth: 2.40,
        },
        Placement {
            kind: "bot.planter_row",
            origin: "-1030 -320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 1.50,
            depth: 2.40,
        },
        Placement {
            kind: "bot.grow_plot",
            origin: "-1380 480 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 4.00,
            depth: 2.40,
        },
        Placement {
            kind: "bot.grow_plot",
            origin: "-1380 320 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 4.00,
            depth: 2.40,
        },
        Placement {
            kind: "bot.planter_row",
            origin: "-1380 210 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 1.50,
            depth: 2.40,
        },
        Placement {
            kind: "bot.research_desk",
            origin: "-1560 512 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 2.40,
            depth: 1.60,
        },
        Placement {
            kind: "bot.research_desk",
            origin: "-1560 416 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 2.40,
            depth: 1.60,
        },
        Placement {
            kind: "bot.research_desk",
            origin: "-1560 320 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 2.40,
            depth: 1.60,
        },
        Placement {
            kind: "bot.hydro_rack",
            origin: "-1340 -420 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 3.00,
            depth: 1.20,
        },
        Placement {
            kind: "bot.hydro_rack",
            origin: "-1460 -420 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 3.00,
            depth: 1.20,
        },
        Placement {
            kind: "bot.nutrient_tank",
            origin: "-1544 -420 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 1.20,
            depth: 1.20,
        },
        Placement {
            kind: "bot.nutrient_tank",
            origin: "-1592 -420 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY,
            width: 1.20,
            depth: 1.20,
        },
        Placement {
            kind: "bot.hydro_rack",
            origin: "-1800 320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY_NURSERY,
            width: 3.00,
            depth: 1.20,
        },
        Placement {
            kind: "bot.hydro_rack",
            origin: "-1920 320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY_NURSERY,
            width: 3.00,
            depth: 1.20,
        },
        Placement {
            kind: "bot.hydro_rack",
            origin: "-1800 -320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY_NURSERY,
            width: 3.00,
            depth: 1.20,
        },
        Placement {
            kind: "bot.hydro_rack",
            origin: "-1920 -320 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: BOTANY_NURSERY,
            width: 3.00,
            depth: 1.20,
        },
        Placement {
            kind: "bot.seed_vault",
            origin: "-720 740 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: BOTANY,
            width: 1.30,
            depth: 0.38,
        },
        Placement {
            kind: "bot.tool_rack",
            origin: "-940 740 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: BOTANY,
            width: 1.20,
            depth: 0.32,
        },
        Placement {
            kind: "bot.sample_board",
            origin: "-1340 740 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: BOTANY,
            width: 1.30,
            depth: 0.16,
        },
        Placement {
            kind: "bot.irrigation_panel",
            origin: "-1640 740 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: BOTANY,
            width: 1.10,
            depth: 0.34,
        },
        Placement {
            kind: "bot.irrigation_panel",
            origin: "-720 -540 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: BOTANY,
            width: 1.10,
            depth: 0.34,
        },
        Placement {
            kind: "bot.sample_board",
            origin: "-1080 -540 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: BOTANY,
            width: 1.30,
            depth: 0.16,
        },
        Placement {
            kind: "bot.seed_vault",
            origin: "-1360 -540 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: BOTANY,
            width: 1.30,
            depth: 0.38,
        },
        Placement {
            kind: "bot.tool_rack",
            origin: "-1880 -540 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: BOTANY_NURSERY,
            width: 1.20,
            depth: 0.32,
        },
        Placement {
            kind: "chapel.plinth",
            origin: "-1900 970 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.35,
            depth: 0.65,
        },
        Placement {
            kind: "chapel.runner",
            origin: "-1900 1072 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.20,
            depth: 3.40,
        },
        Placement {
            kind: "chapel.runner",
            origin: "-1900 1208 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.20,
            depth: 3.40,
        },
        Placement {
            kind: "chapel.pew",
            origin: "-1975 1088 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.45,
            depth: 0.55,
        },
        Placement {
            kind: "chapel.pew",
            origin: "-1825 1088 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.45,
            depth: 0.55,
        },
        Placement {
            kind: "chapel.pew",
            origin: "-1975 1152 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.45,
            depth: 0.55,
        },
        Placement {
            kind: "chapel.pew",
            origin: "-1825 1152 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.45,
            depth: 0.55,
        },
        Placement {
            kind: "chapel.pew",
            origin: "-1975 1216 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.45,
            depth: 0.55,
        },
        Placement {
            kind: "chapel.pew",
            origin: "-1825 1216 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.45,
            depth: 0.55,
        },
        Placement {
            kind: "chapel.pew",
            origin: "-1975 1280 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.45,
            depth: 0.55,
        },
        Placement {
            kind: "chapel.pew",
            origin: "-1825 1280 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 1.45,
            depth: 0.55,
        },
        Placement {
            kind: "chapel.memorial_panel",
            origin: "-1900 942 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: CHAPEL,
            width: 1.60,
            depth: 0.12,
        },
        Placement {
            kind: "svc.bench",
            origin: "-900 1350 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: SERVICE_EAST,
            width: 0.42,
            depth: 0.42,
        },
        // The canteen rebuild: a bench beside each dining table, and boards on
        // the west and north walls, which were bare floor with nothing to look
        // at. Service measured 0.8 decorations per 100 m² against a station
        // norm of 2.4-4.1 — the least dressed room, and the one every resident
        // has a reason to visit.
        // Station expansion; repeat placements do not increase the asset count.
        Placement {
            kind: "sec.wall_camera",
            origin: "20 3345 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: SECURITY,
            width: 0.64,
            depth: 0.58,
        },
        Placement {
            kind: "sec.radio_charger",
            origin: "75 3500 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: SECURITY,
            width: 1.45,
            depth: 0.36,
        },
        Placement {
            kind: "sec.restraint_display",
            origin: "85 4000 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: SECURITY_BACK,
            width: 1.50,
            depth: 0.32,
        },
        Placement {
            kind: "sec.report_shelf",
            origin: "-395 3680 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: SECURITY,
            width: 1.70,
            depth: 0.44,
        },
        Placement {
            kind: "sec.personal_effects",
            origin: "355 3740 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: SECURITY_BACK,
            width: 1.80,
            depth: 0.46,
        },
        Placement {
            kind: "med.patient_bay",
            origin: "-288 1448 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: MEDICAL_FLOOR,
            width: 2.70,
            depth: 2.40,
        },
        Placement {
            kind: "med.patient_bay",
            origin: "-8 1448 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: MEDICAL_FLOOR,
            width: 2.70,
            depth: 2.40,
        },
        Placement {
            kind: "med.nurse_station",
            origin: "-304 820 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: MEDICAL_FLOOR,
            width: 3.00,
            depth: 2.40,
        },
        Placement {
            kind: "med.crash_cart",
            origin: "80 1240 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MEDICAL_FLOOR,
            width: 0.92,
            depth: 0.70,
        },
        Placement {
            kind: "med.examination_couch",
            origin: "24 900 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: MEDICAL_FLOOR,
            width: 0.90,
            depth: 1.95,
        },
        Placement {
            kind: "med.diagnostic_stand",
            origin: "70 1000 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: MEDICAL_FLOOR,
            width: 0.68,
            depth: 0.62,
        },
        Placement {
            kind: "med.privacy_screen",
            origin: "-32 900 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MEDICAL_FLOOR,
            width: 1.40,
            depth: 0.62,
        },
        Placement {
            kind: "med.hygiene_cabinet",
            origin: "115 740 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: MEDICAL_FLOOR,
            width: 1.10,
            depth: 0.32,
        },
        Placement {
            kind: "med.cryo_pod",
            origin: "308 1472 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: QUARANTINE_FLOOR,
            width: 1.30,
            depth: 1.30,
        },
        Placement {
            kind: "med.cryo_pod_occupied",
            origin: "164 1472 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: QUARANTINE_FLOOR,
            width: 1.30,
            depth: 1.30,
        },
        Placement {
            kind: "med.cryo_monitor_bay",
            origin: "240 1456 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: QUARANTINE_FLOOR,
            width: 1.70,
            depth: 1.30,
        },
        Placement {
            kind: "eng.pump_assembly",
            origin: "-1080 2160 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: ATMOS_UTILITY,
            width: 1.55,
            depth: 0.90,
        },
        Placement {
            kind: "eng.tool_trolley",
            origin: "-1012 4220 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: ENGINEERING,
            width: 0.95,
            depth: 0.65,
        },
        Placement {
            kind: "eng.cable_junction",
            origin: "-605 3760 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: ENGINEERING,
            width: 1.20,
            depth: 0.24,
        },
        Placement {
            kind: "eng.hose_reel",
            origin: "-680 2105 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: ATMOS_UTILITY,
            width: 0.95,
            depth: 0.42,
        },
        Placement {
            kind: "eng.parts_shelf",
            origin: "-700 4375 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: ENGINEERING,
            width: 1.55,
            depth: 0.38,
        },
        Placement {
            kind: "hall.waiting_bench",
            origin: "-420 2960 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MAIN_HALL,
            width: 2.10,
            depth: 0.65,
        },
        Placement {
            kind: "hall.waiting_bench",
            origin: "-420 2600 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MAIN_HALL,
            width: 2.10,
            depth: 0.65,
        },
        Placement {
            kind: "hall.waiting_bench",
            origin: "-420 2000 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MAIN_HALL,
            width: 2.10,
            depth: 0.65,
        },
        Placement {
            kind: "hall.planter",
            origin: "-420 3032 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MAIN_HALL,
            width: 0.90,
            depth: 0.65,
        },
        Placement {
            kind: "hall.planter",
            origin: "-420 2672 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MAIN_HALL,
            width: 0.90,
            depth: 0.65,
        },
        Placement {
            kind: "hall.planter",
            origin: "-420 2072 0",
            angles: "0 0 0",
            mount: Mount::Floor,
            room: MAIN_HALL,
            width: 0.90,
            depth: 0.65,
        },
        Placement {
            kind: "hall.waste_station",
            origin: "-582 2800 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: MAIN_HALL,
            width: 1.15,
            depth: 0.55,
        },
        Placement {
            kind: "hall.waste_station",
            origin: "-582 2200 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: MAIN_HALL,
            width: 1.15,
            depth: 0.55,
        },
        Placement {
            kind: "hall.wall_light",
            origin: "-405 3080 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: MAIN_HALL,
            width: 0.95,
            depth: 0.18,
        },
        Placement {
            kind: "hall.wall_light",
            origin: "-405 2760 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: MAIN_HALL,
            width: 0.95,
            depth: 0.18,
        },
        Placement {
            kind: "hall.wall_light",
            origin: "-405 2120 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: MAIN_HALL,
            width: 0.95,
            depth: 0.18,
        },
        Placement {
            kind: "hall.wall_light",
            origin: "-405 1800 0",
            angles: "0 0 0",
            mount: Mount::Wall,
            room: MAIN_HALL,
            width: 0.95,
            depth: 0.18,
        },
        Placement {
            kind: "hall.wall_light",
            origin: "-595 3040 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: MAIN_HALL,
            width: 0.95,
            depth: 0.18,
        },
        Placement {
            kind: "hall.wall_light",
            origin: "-595 2560 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: MAIN_HALL,
            width: 0.95,
            depth: 0.18,
        },
        Placement {
            kind: "hall.wall_light",
            origin: "-595 2260 0",
            angles: "0 180 0",
            mount: Mount::Wall,
            room: MAIN_HALL,
            width: 0.95,
            depth: 0.18,
        },
        Placement {
            kind: "chapel.votive_stand",
            origin: "-1992 980 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 0.90,
            depth: 0.58,
        },
        Placement {
            kind: "chapel.votive_stand",
            origin: "-1796 980 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 0.90,
            depth: 0.58,
        },
        Placement {
            kind: "chapel.lectern",
            origin: "-1832 1012 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: CHAPEL,
            width: 0.80,
            depth: 0.62,
        },
        Placement {
            kind: "quiet.armchair",
            origin: "-1972 1864 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: QUIET_ROOM,
            width: 0.92,
            depth: 0.86,
        },
        Placement {
            kind: "quiet.armchair",
            origin: "-1972 1696 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: QUIET_ROOM,
            width: 0.92,
            depth: 0.86,
        },
        Placement {
            kind: "quiet.side_table_lamp",
            origin: "-1916 1866 0",
            angles: "0 90 0",
            mount: Mount::Floor,
            room: QUIET_ROOM,
            width: 0.62,
            depth: 0.55,
        },
        Placement {
            kind: "quiet.side_table_lamp",
            origin: "-1916 1694 0",
            angles: "0 -90 0",
            mount: Mount::Floor,
            room: QUIET_ROOM,
            width: 0.62,
            depth: 0.55,
        },
        Placement {
            kind: "quiet.reading_shelf",
            origin: "-2026 1780 0",
            angles: "0 180 0",
            mount: Mount::Floor,
            room: QUIET_ROOM,
            width: 1.35,
            depth: 0.38,
        },
        Placement {
            kind: "quiet.acoustic_panel",
            origin: "-1840 1895 0",
            angles: "0 90 0",
            mount: Mount::Wall,
            room: QUIET_ROOM,
            width: 1.05,
            depth: 0.13,
        },
        Placement {
            kind: "quiet.acoustic_panel",
            origin: "-1840 1670 0",
            angles: "0 -90 0",
            mount: Mount::Wall,
            room: QUIET_ROOM,
            width: 1.05,
            depth: 0.13,
        },
    ];

    let map = parse();
    let markers: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("decoration_spot"))
        .collect();
    assert_eq!(markers.len(), PLACEMENTS.len());

    let mut visual_envelopes = Vec::new();
    for placement in PLACEMENTS {
        let matches: Vec<&Entity> = markers
            .iter()
            .copied()
            .filter(|entity| property(entity, "kind").as_deref() == Some(placement.kind))
            .filter(|entity| property(entity, "origin").as_deref() == Some(placement.origin))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected one {} marker at {}",
            placement.kind,
            placement.origin,
        );
        assert_eq!(
            property(matches[0], "angles").as_deref(),
            Some(placement.angles),
        );

        let (x, z) = origin_xz(matches[0]).expect("decoration_spot has a valid origin");
        let half_width = placement.width * 0.5;
        // A wall module grows forwards out of its mounting plane; a floor
        // fixture grows both ways out of its footprint centre.
        let (behind, ahead) = match placement.mount {
            Mount::Wall => (0.0, placement.depth),
            Mount::Floor => (placement.depth * 0.5, placement.depth * 0.5),
        };
        let bounds = match placement.angles {
            "0 0 0" => Bounds {
                min_x: x - half_width,
                max_x: x + half_width,
                min_z: z - behind,
                max_z: z + ahead,
            },
            "0 90 0" => Bounds {
                min_x: x - behind,
                max_x: x + ahead,
                min_z: z - half_width,
                max_z: z + half_width,
            },
            "0 180 0" => Bounds {
                min_x: x - half_width,
                max_x: x + half_width,
                min_z: z - ahead,
                max_z: z + behind,
            },
            "0 -90 0" => Bounds {
                min_x: x - ahead,
                max_x: x + behind,
                min_z: z - half_width,
                max_z: z + half_width,
            },
            // Any other yaw, for fixtures arranged on a curve rather than
            // against an axis — the galley's bar ring is eleven segments at
            // thirty-degree steps, and its stools and chairs face inward.
            //
            // The four cases above stay written out rather than folded into
            // this one: they are exact for an axis-aligned fixture, while this
            // takes the rotated footprint's axis-aligned extent, which is
            // correct but slightly generous on the diagonal.
            other => {
                let yaw = other
                    .strip_prefix("0 ")
                    .and_then(|rest| rest.strip_suffix(" 0"))
                    .and_then(|degrees| degrees.parse::<f32>().ok())
                    .unwrap_or_else(|| panic!("unsupported decoration angle {other}"));
                let (sin, cos) = yaw.to_radians().sin_cos();
                let depth = behind + ahead;
                let centre_z = z + (ahead - depth * 0.5) * cos;
                let centre_x = x + (ahead - depth * 0.5) * sin;
                let extent_x = half_width * cos.abs() + depth * 0.5 * sin.abs();
                let extent_z = half_width * sin.abs() + depth * 0.5 * cos.abs();
                Bounds {
                    min_x: centre_x - extent_x,
                    max_x: centre_x + extent_x,
                    min_z: centre_z - extent_z,
                    max_z: centre_z + extent_z,
                }
            }
        };
        assert!(
            bounds.min_x >= placement.room.min_x
                && bounds.max_x <= placement.room.max_x
                && bounds.min_z >= placement.room.min_z
                && bounds.max_z <= placement.room.max_z,
            "{} leaves its authored room: {bounds:?} vs {:?}",
            placement.kind,
            placement.room,
        );

        let session_asset = ASSET_SESSION_KINDS.contains(&placement.kind);
        if session_asset || (placement.mount == Mount::Wall && placement.kind.starts_with("chem."))
        {
            let doorway_bridges: Vec<Bounds> = map
                .iter()
                .filter(|entity| {
                    classname(entity).as_deref() == Some("func_walkable")
                        && property(entity, "bridge_id").is_some()
                })
                .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
                .collect();
            assert!(
                doorway_bridges
                    .iter()
                    .all(|bridge| bridge.intersection(&bounds).is_none()),
                "fixture {} at {bounds:?} obstructs a doorway bridge",
                placement.kind,
            );

            if placement.mount == Mount::Wall {
                // Three samples across the mount width must land on tall physical
                // world geometry immediately behind the model. This catches a
                // shelf placed across an opening even when its centre happens to
                // be close to a remaining wall return.
                let (back, lateral) = match placement.angles {
                    "0 0 0" => (Vec3::NEG_Z, Vec3::X),
                    "0 90 0" => (Vec3::NEG_X, Vec3::Z),
                    "0 180 0" => (Vec3::Z, Vec3::X),
                    "0 -90 0" => (Vec3::X, Vec3::Z),
                    other => panic!("unsupported wall angle {other}"),
                };
                let world = map
                    .iter()
                    .find(|entity| classname(entity).as_deref() == Some("worldspawn"))
                    .expect("worldspawn");
                for offset in [-half_width + 0.05, 0.0, half_width - 0.05] {
                    let sample = Vec3::new(x, 0.0, z) + back * 0.20 + lateral * offset;
                    assert!(
                        world.brushes.iter().any(|brush| {
                            let (bottom, top) = vertical_span(brush);
                            top - bottom > 1.0 && footprint(brush).holds(sample)
                        }),
                        "wall fixture {} has no backing wall at {sample}",
                        placement.kind,
                    );
                }
            }
        }

        // The invariant that makes a floor fixture safe. A wall module can
        // overhang walkable floor — you brush past a shelf. A bed cannot: with
        // no collider on the scene and no `Solid` for crew to consult, standing
        // in walkable ground is the same as not being there.
        // Security deliberately keeps one continuous walkable floor, and its
        // fixtures have matching runtime `Solid` envelopes, so their visible
        // geometry -- not a hidden floor boundary -- stops the player.
        // `svc.bench` is the same shape for a different reason: it must
        // coincide exactly with a `crew_post` relax marker a resident's
        // `NavGraph` route has to reach, so it cannot be carved out of the
        // walkable volume the way an ordinary fixture is.
        let collider_backed_walkable_fixture = matches!(
            placement.kind,
            "sec.dispatch_console"
                | "sec.officer_desk_bank"
                | "sec.evidence_locker_bank"
                | "sec.equipment_locker_bank"
                | "sec.brig_bunk"
                | "sec.interrogation_table"
                | "svc.bench"
                // The galley furniture, for the same reason as `svc.bench` and
                // Security's fixtures: each has a matching runtime `Solid`
                // envelope in `tb.rs`, so its visible geometry is what stops the
                // player. Carving the bar ring and every chair out of the floor
                // would also cut the room into a maze of holes that the crew's
                // `NavGraph` has to squeeze between — and the seats are places
                // residents are explicitly routed *to*.
                | "svc.bar_counter"
                | "svc.dining_table"
                | "svc.dining_chair"
                | "chapel.pew"
                | "chapel.plinth"
                | "chapel.runner"
        ) || ASSET_SESSION_FLOOR_KINDS
            .contains(&placement.kind);
        if placement.mount == Mount::Floor && !collider_backed_walkable_fixture {
            let walkable: Vec<Bounds> = map
                .iter()
                .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
                .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
                .collect();
            if let Some(overlap) = walkable
                .iter()
                // A fixture may meet the carved floor exactly at its edge.
                // `Bounds::intersection` intentionally treats touching edges
                // as connected for navigation, but edge contact is not floor
                // area a body can stand on inside the visible fixture.
                .find(|area| {
                    area.min_x < bounds.max_x
                        && area.max_x > bounds.min_x
                        && area.min_z < bounds.max_z
                        && area.max_z > bounds.min_z
                })
            {
                panic!(
                    "floor fixture {} at {bounds:?} stands in walkable floor {overlap:?}; \
                     carve the walkable volume around it or bodies will walk through it",
                    placement.kind,
                );
            }
        }

        visual_envelopes.push((placement.kind, placement.origin, bounds));
    }

    let blockers = [
        Bounds {
            min_x: -5.0,
            max_x: -3.0,
            min_z: 4.4,
            max_z: 6.0,
        },
        Bounds {
            min_x: -2.3,
            max_x: -0.7,
            min_z: 2.0,
            max_z: 3.5,
        },
        Bounds {
            min_x: 9.6,
            max_x: 11.4,
            min_z: -5.5,
            max_z: -4.0,
        },
        Bounds {
            min_x: -13.5,
            max_x: -12.0,
            min_z: -4.3,
            max_z: -2.7,
        },
        Bounds {
            min_x: 2.3,
            max_x: 5.7,
            min_z: 4.1,
            max_z: 5.2,
        },
        Bounds {
            min_x: 3.1,
            max_x: 4.9,
            min_z: 6.5,
            max_z: 7.5,
        },
        Bounds {
            min_x: -16.25,
            max_x: -14.5,
            min_z: 2.0,
            max_z: 4.5,
        },
        // `delivery.medical`: the Medical side of the shared handoff window,
        // and the one thing in this room a decoration must never sit on.
        Bounds {
            min_x: -16.85,
            max_x: -16.15,
            min_z: 1.55,
            max_z: 4.95,
        },
        // The three Medical/Quarantine airlocks and the crew standing spot,
        // each a doorway's width of floor that has to stay clear.
        Bounds {
            min_x: -28.0,
            max_x: -26.0,
            min_z: 9.2,
            max_z: 10.8,
        },
        Bounds {
            min_x: -28.0,
            max_x: -26.0,
            min_z: -3.8,
            max_z: -2.2,
        },
        Bounds {
            min_x: -28.0,
            max_x: -26.0,
            min_z: -9.8,
            max_z: -8.2,
        },
        Bounds {
            min_x: -27.8,
            max_x: -26.2,
            min_z: 7.7,
            max_z: 9.3,
        },
        // The four remaining department dressing bays and the crew standing
        // spot in each: a module dropped on top of either would look placed
        // and read as a bug.
        Bounds {
            min_x: -82.3,
            max_x: -77.7,
            min_z: 16.2,
            max_z: 19.8,
        },
        Bounds {
            min_x: -80.8,
            max_x: -79.2,
            min_z: 25.2,
            max_z: 26.8,
        },
        Bounds {
            min_x: -82.3,
            max_x: -77.7,
            min_z: 33.2,
            max_z: 36.8,
        },
        Bounds {
            min_x: -54.8,
            max_x: -53.2,
            min_z: 41.2,
            max_z: 42.8,
        },
        Bounds {
            min_x: -98.3,
            max_x: -93.7,
            min_z: 2.2,
            max_z: 5.8,
        },
        Bounds {
            min_x: -96.8,
            max_x: -95.2,
            min_z: 7.7,
            max_z: 9.3,
        },
        Bounds {
            min_x: -37.3,
            max_x: -32.7,
            min_z: 16.2,
            max_z: 19.8,
        },
        Bounds {
            min_x: -35.8,
            max_x: -34.2,
            min_z: 15.7,
            max_z: 17.3,
        },
        // Botany's central home marker plus its public, west, nursery, and
        // perimeter airlocks. Cultivation rows deliberately frame these
        // clearances instead of turning a doorway into a slalom.
        Bounds {
            min_x: -1.0,
            max_x: 1.0,
            min_z: 19.0,
            max_z: 21.0,
        },
        Bounds {
            min_x: -1.0,
            max_x: 1.0,
            min_z: 14.2,
            max_z: 15.8,
        },
        Bounds {
            min_x: -19.3,
            max_x: -17.7,
            min_z: 37.0,
            max_z: 39.0,
        },
        Bounds {
            min_x: -1.0,
            max_x: 1.0,
            min_z: 41.2,
            max_z: 42.8,
        },
        Bounds {
            min_x: -1.0,
            max_x: 1.0,
            min_z: 50.2,
            max_z: 51.8,
        },
    ];
    let overlaps = |a: Bounds, b: Bounds| {
        a.min_x < b.max_x && a.max_x > b.min_x && a.min_z < b.max_z && a.max_z > b.min_z
    };
    for (kind, origin, decoration) in &visual_envelopes {
        assert!(
            blockers
                .iter()
                .all(|blocker| !overlaps(*decoration, *blocker)),
            "decoration at {origin} overlaps a machine, working point, delivery window, or door",
        );
        if !ASSET_SESSION_KINDS.contains(kind) {
            continue;
        }
        for spot in map.iter().filter(|entity| {
            classname(entity).as_deref() == Some("utility_spot")
                || classname(entity).as_deref() == Some("crew_post")
        }) {
            let (x, z) = origin_xz(spot).expect("work point origin");
            // Crew need body-width clearance around work and treatment
            // approaches, including where the point itself is just outside a
            // furniture envelope. This catches a bedside cart across a route.
            let approach = Bounds {
                min_x: x - NAV_RADIUS,
                max_x: x + NAV_RADIUS,
                min_z: z - NAV_RADIUS,
                max_z: z + NAV_RADIUS,
            };
            assert!(
                !overlaps(*decoration, approach),
                "{kind} at {origin} obstructs work approach {:?} at ({x}, {z})",
                property(spot, "id").or_else(|| property(spot, "occupant")),
            );
        }
    }

    for (index, (kind, origin, bounds)) in visual_envelopes.iter().enumerate() {
        for (other_kind, other_origin, other_bounds) in &visual_envelopes[index + 1..] {
            if !(ASSET_SESSION_KINDS.contains(kind)
                || ASSET_SESSION_KINDS.contains(other_kind))
                // Flat chapel runners are intentional floor layering.
                || *kind == "chapel.runner"
                || *other_kind == "chapel.runner"
            {
                continue;
            }
            assert!(
                !overlaps(*bounds, *other_bounds),
                "{kind} at {origin} overlaps {other_kind} at {other_origin}",
            );
        }
    }
}

#[test]
fn station_asset_session_places_all_36_distinct_deliverables() {
    let map = parse();
    let placed: std::collections::HashSet<String> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("decoration_spot"))
        .filter_map(|entity| property(entity, "kind"))
        .collect();
    assert_eq!(ASSET_SESSION_KINDS.len(), 36);
    assert_eq!(
        ASSET_SESSION_KINDS
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        36,
        "repeated placements and recolors do not count as new assets",
    );
    for kind in ASSET_SESSION_KINDS {
        assert!(
            placed.contains(*kind),
            "approved asset {kind} is not placed"
        );
    }
    for kind in [
        "med.cryo_pod",
        "med.cryo_pod_occupied",
        "med.cryo_monitor_bay",
    ] {
        assert!(placed.contains(kind), "Quarantine lost its reused {kind}");
    }
}

#[test]
fn every_decoration_kind_has_an_exported_glb() {
    // Written out again rather than read from `tb::DECORATION_KINDS`: that
    // table only exists behind the `trenchbroom` feature, and a test that
    // shares its list with the code under test proves nothing about it.
    for (kind, path) in [
        (
            "chem.supply_shelf",
            "assets/3dassets/station_starter_kit/glb/decor_chem_supply_shelf.glb",
        ),
        (
            "chem.analysis_panel",
            "assets/3dassets/station_starter_kit/glb/decor_chem_analysis_panel.glb",
        ),
        (
            "chem.emergency_station",
            "assets/3dassets/station_starter_kit/glb/decor_chem_emergency_station.glb",
        ),
        (
            "chem.service_board",
            "assets/3dassets/station_starter_kit/glb/decor_chem_service_board.glb",
        ),
        (
            "med.supply_shelf",
            "assets/3dassets/station_starter_kit/glb/decor_med_supply_shelf.glb",
        ),
        (
            "med.vitals_panel",
            "assets/3dassets/station_starter_kit/glb/decor_med_vitals_panel.glb",
        ),
        (
            "med.crash_station",
            "assets/3dassets/station_starter_kit/glb/decor_med_crash_station.glb",
        ),
        (
            "med.triage_board",
            "assets/3dassets/station_starter_kit/glb/decor_med_triage_board.glb",
        ),
        (
            "med.quarantine_seal",
            "assets/3dassets/station_starter_kit/glb/decor_med_quarantine_seal.glb",
        ),
        (
            "med.ward_bay",
            "assets/3dassets/station_starter_kit/glb/decor_med_ward_bay.glb",
        ),
        (
            "med.waiting_row",
            "assets/3dassets/station_starter_kit/glb/decor_med_waiting_row.glb",
        ),
        (
            "med.specimen_cold",
            "assets/3dassets/station_starter_kit/glb/decor_med_specimen_cold.glb",
        ),
        (
            "eng.breaker_panel",
            "assets/3dassets/station_starter_kit/glb/decor_eng_breaker_panel.glb",
        ),
        (
            "eng.tool_board",
            "assets/3dassets/station_starter_kit/glb/decor_eng_tool_board.glb",
        ),
        (
            "eng.pipe_manifold",
            "assets/3dassets/station_starter_kit/glb/decor_eng_pipe_manifold.glb",
        ),
        (
            "eng.safety_station",
            "assets/3dassets/station_starter_kit/glb/decor_eng_safety_station.glb",
        ),
        (
            "eng.smes_bank",
            "assets/3dassets/station_starter_kit/glb/decor_eng_smes_bank.glb",
        ),
        (
            "eng.generator_turbine",
            "assets/3dassets/station_starter_kit/glb/decor_eng_generator_turbine.glb",
        ),
        (
            "eng.hardsuit_locker",
            "assets/3dassets/station_starter_kit/glb/decor_eng_hardsuit_locker.glb",
        ),
        (
            "eng.parts_workbench",
            "assets/3dassets/station_starter_kit/glb/decor_eng_parts_workbench.glb",
        ),
        (
            "eng.cable_spool_rack",
            "assets/3dassets/station_starter_kit/glb/decor_eng_cable_spool_rack.glb",
        ),
        (
            "eng.gas_canister_rack",
            "assets/3dassets/station_starter_kit/glb/decor_eng_gas_canister_rack.glb",
        ),
        (
            "eng.filtration_scrubber",
            "assets/3dassets/station_starter_kit/glb/decor_eng_filtration_scrubber.glb",
        ),
        (
            "eng.power_monitor_console",
            "assets/3dassets/station_starter_kit/glb/decor_eng_power_monitor_console.glb",
        ),
        (
            "eng.hv_warning_sign",
            "assets/3dassets/station_starter_kit/glb/decor_eng_hv_warning_sign.glb",
        ),
        (
            "eng.solar_readout",
            "assets/3dassets/station_starter_kit/glb/decor_eng_solar_readout.glb",
        ),
        (
            "cargo.manifest_board",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_manifest_board.glb",
        ),
        (
            "cargo.parcel_shelf",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_parcel_shelf.glb",
        ),
        (
            "cargo.dispatch_panel",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_dispatch_panel.glb",
        ),
        (
            "cargo.weigh_station",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_weigh_station.glb",
        ),
        (
            "cargo.storage_rack",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_storage_rack.glb",
        ),
        (
            "cargo.crate_stack",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_crate_stack.glb",
        ),
        (
            "cargo.pallet_row",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_pallet_row.glb",
        ),
        (
            "cargo.forklift_bay",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_forklift_bay.glb",
        ),
        (
            "cargo.requisitions_desk",
            "assets/3dassets/station_starter_kit/glb/decor_cargo_requisitions_desk.glb",
        ),
        (
            "sec.notice_board",
            "assets/3dassets/station_starter_kit/glb/decor_sec_notice_board.glb",
        ),
        (
            "sec.camera_bank",
            "assets/3dassets/station_starter_kit/glb/decor_sec_camera_bank.glb",
        ),
        (
            "sec.armory_rack",
            "assets/3dassets/station_starter_kit/glb/decor_sec_armory_rack.glb",
        ),
        (
            "sec.evidence_wall",
            "assets/3dassets/station_starter_kit/glb/decor_sec_evidence_wall.glb",
        ),
        (
            "sec.booking_desk",
            "assets/3dassets/station_starter_kit/glb/decor_sec_booking_desk.glb",
        ),
        (
            "sec.dispatch_console",
            "assets/3dassets/station_starter_kit/glb/decor_sec_dispatch_console.glb",
        ),
        (
            "sec.officer_desk_bank",
            "assets/3dassets/station_starter_kit/glb/decor_sec_officer_desk_bank.glb",
        ),
        (
            "sec.evidence_locker_bank",
            "assets/3dassets/station_starter_kit/glb/decor_sec_evidence_locker_bank.glb",
        ),
        (
            "sec.equipment_locker_bank",
            "assets/3dassets/station_starter_kit/glb/decor_sec_equipment_locker_bank.glb",
        ),
        (
            "sec.brig_bunk",
            "assets/3dassets/station_starter_kit/glb/decor_sec_brig_bunk.glb",
        ),
        (
            "sec.interrogation_table",
            "assets/3dassets/station_starter_kit/glb/decor_sec_interrogation_table.glb",
        ),
        (
            "sec.processing_scanner",
            "assets/3dassets/station_starter_kit/glb/decor_sec_processing_scanner.glb",
        ),
        (
            "sec.mugshot_board",
            "assets/3dassets/station_starter_kit/glb/decor_sec_mugshot_board.glb",
        ),
        (
            "sec.alert_panel",
            "assets/3dassets/station_starter_kit/glb/decor_sec_alert_panel.glb",
        ),
        (
            "svc.menu_board",
            "assets/3dassets/station_starter_kit/glb/decor_svc_menu_board.glb",
        ),
        (
            "svc.crockery_shelf",
            "assets/3dassets/station_starter_kit/glb/decor_svc_crockery_shelf.glb",
        ),
        (
            "svc.pass_hatch",
            "assets/3dassets/station_starter_kit/glb/decor_svc_pass_hatch.glb",
        ),
        (
            "svc.drinks_board",
            "assets/3dassets/station_starter_kit/glb/decor_svc_drinks_board.glb",
        ),
        (
            "svc.bench",
            "assets/3dassets/station_starter_kit/glb/decor_svc_bench.glb",
        ),
        (
            "bridge.holomap_island",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_holomap_island.glb",
        ),
        (
            "bridge.captains_chair",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_captains_chair.glb",
        ),
        (
            "bridge.duty_station",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_duty_station.glb",
        ),
        (
            "bridge.duty_station_bank",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_duty_station_bank.glb",
        ),
        (
            "bridge.astrogation_pillar",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_astrogation_pillar.glb",
        ),
        (
            "bridge.briefing_table",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_briefing_table.glb",
        ),
        (
            "bridge.nav_desk",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_nav_desk.glb",
        ),
        (
            "bridge.alert_panel",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_alert_panel.glb",
        ),
        (
            "bridge.viewscreen",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_viewscreen.glb",
        ),
        (
            "bridge.comms_console",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_comms_console.glb",
        ),
        (
            "bridge.crew_roster_board",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_crew_roster_board.glb",
        ),
        (
            "bridge.console_arc",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_console_arc.glb",
        ),
        (
            "bridge.command_dais",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_command_dais.glb",
        ),
        (
            "bridge.tactical_rail",
            "assets/3dassets/station_starter_kit/glb/decor_bridge_tactical_rail.glb",
        ),
        (
            "bot.grow_plot",
            "assets/3dassets/station_starter_kit/glb/decor_bot_grow_plot.glb",
        ),
        (
            "bot.planter_row",
            "assets/3dassets/station_starter_kit/glb/decor_bot_planter_row.glb",
        ),
        (
            "bot.hydro_rack",
            "assets/3dassets/station_starter_kit/glb/decor_bot_hydro_rack.glb",
        ),
        (
            "bot.research_desk",
            "assets/3dassets/station_starter_kit/glb/decor_bot_research_desk.glb",
        ),
        (
            "bot.nutrient_tank",
            "assets/3dassets/station_starter_kit/glb/decor_bot_nutrient_tank.glb",
        ),
        (
            "bot.seed_vault",
            "assets/3dassets/station_starter_kit/glb/decor_bot_seed_vault.glb",
        ),
        (
            "bot.tool_rack",
            "assets/3dassets/station_starter_kit/glb/decor_bot_tool_rack.glb",
        ),
        (
            "bot.sample_board",
            "assets/3dassets/station_starter_kit/glb/decor_bot_sample_board.glb",
        ),
        (
            "bot.irrigation_panel",
            "assets/3dassets/station_starter_kit/glb/decor_bot_irrigation_panel.glb",
        ),
        (
            "hall.waiting_bench",
            "assets/3dassets/station_starter_kit/glb/decor_hall_waiting_bench.glb",
        ),
        (
            "hall.wall_light",
            "assets/3dassets/station_starter_kit/glb/decor_hall_wall_light.glb",
        ),
        (
            "hall.planter",
            "assets/3dassets/station_starter_kit/glb/decor_hall_planter.glb",
        ),
        (
            "hall.waste_station",
            "assets/3dassets/station_starter_kit/glb/decor_hall_waste_station.glb",
        ),
        (
            "chapel.votive_stand",
            "assets/3dassets/station_starter_kit/glb/decor_chapel_votive_stand.glb",
        ),
        (
            "chapel.lectern",
            "assets/3dassets/station_starter_kit/glb/decor_chapel_lectern.glb",
        ),
        (
            "quiet.armchair",
            "assets/3dassets/station_starter_kit/glb/decor_quiet_armchair.glb",
        ),
        (
            "quiet.reading_shelf",
            "assets/3dassets/station_starter_kit/glb/decor_quiet_reading_shelf.glb",
        ),
        (
            "quiet.side_table_lamp",
            "assets/3dassets/station_starter_kit/glb/decor_quiet_side_table_lamp.glb",
        ),
        (
            "quiet.acoustic_panel",
            "assets/3dassets/station_starter_kit/glb/decor_quiet_acoustic_panel.glb",
        ),
        (
            "med.crash_cart",
            "assets/3dassets/station_starter_kit/glb/decor_med_crash_cart.glb",
        ),
        (
            "med.examination_couch",
            "assets/3dassets/station_starter_kit/glb/decor_med_examination_couch.glb",
        ),
        (
            "med.diagnostic_stand",
            "assets/3dassets/station_starter_kit/glb/decor_med_diagnostic_stand.glb",
        ),
        (
            "med.privacy_screen",
            "assets/3dassets/station_starter_kit/glb/decor_med_privacy_screen.glb",
        ),
        (
            "med.hygiene_cabinet",
            "assets/3dassets/station_starter_kit/glb/decor_med_hygiene_cabinet.glb",
        ),
        (
            "sec.wall_camera",
            "assets/3dassets/station_starter_kit/glb/decor_sec_wall_camera.glb",
        ),
        (
            "sec.radio_charger",
            "assets/3dassets/station_starter_kit/glb/decor_sec_radio_charger.glb",
        ),
        (
            "sec.restraint_display",
            "assets/3dassets/station_starter_kit/glb/decor_sec_restraint_display.glb",
        ),
        (
            "sec.report_shelf",
            "assets/3dassets/station_starter_kit/glb/decor_sec_report_shelf.glb",
        ),
        (
            "sec.personal_effects",
            "assets/3dassets/station_starter_kit/glb/decor_sec_personal_effects.glb",
        ),
        (
            "eng.pump_assembly",
            "assets/3dassets/station_starter_kit/glb/decor_eng_pump_assembly.glb",
        ),
        (
            "eng.tool_trolley",
            "assets/3dassets/station_starter_kit/glb/decor_eng_tool_trolley.glb",
        ),
        (
            "eng.cable_junction",
            "assets/3dassets/station_starter_kit/glb/decor_eng_cable_junction.glb",
        ),
        (
            "eng.hose_reel",
            "assets/3dassets/station_starter_kit/glb/decor_eng_hose_reel.glb",
        ),
        (
            "eng.parts_shelf",
            "assets/3dassets/station_starter_kit/glb/decor_eng_parts_shelf.glb",
        ),
    ] {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|error| panic!("{kind} decoration is missing at {path}: {error}"));
        bevy::gltf::gltf::Gltf::from_slice(&bytes).unwrap_or_else(|error| {
            panic!("{kind} decoration at {path} is not Bevy-compatible glTF: {error}")
        });
    }
}

#[test]
fn every_station_kit_glb_parses_with_bevys_gltf_parser() {
    let directory = "assets/3dassets/station_starter_kit/glb";
    let mut count = 0;
    for entry in
        std::fs::read_dir(directory).unwrap_or_else(|error| panic!("reading {directory}: {error}"))
    {
        let path = entry.expect("a readable GLB directory entry").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("glb") {
            continue;
        }
        count += 1;
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        bevy::gltf::gltf::Gltf::from_slice(&bytes).unwrap_or_else(|error| {
            panic!("{} is not Bevy-compatible glTF: {error}", path.display())
        });
    }
    assert_eq!(
        count, 154,
        "the station starter kit should contain 154 GLBs, including the 25 new department and shared-space assets"
    );
}

/// The band both Cargo rooms carve out of their walkable volumes for the
/// freight line, in world coordinates.
///
/// In the hall the carve is only the north-west alcove, `x -94..-83`; east of
/// that the room still reaches the wall so the maintenance airlock stays
/// reachable. Receiving carves the band across its full width.
const FREIGHT_BAND_MIN_Z: f32 = 47.6;
const FREIGHT_BAND_MAX_Z: f32 = 51.0;

#[test]
fn conveyor_markers_form_continuous_lines() {
    // The map decides the layout and `freight` only knows how a belt behaves,
    // so everything that could be wrong about the layout has to be wrong here
    // rather than at runtime, where a mis-ordered run is a belt that teleports
    // parcels and a missing chute is boxes riding off the end into the floor.
    let map = parse();
    let markers: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("conveyor_spot"))
        .collect();
    assert!(!markers.is_empty(), "{MAP} authors no conveyor at all");

    let mut lines: std::collections::BTreeMap<String, Vec<&Entity>> = Default::default();
    for marker in &markers {
        let line = property(marker, "line").unwrap_or_default();
        assert!(!line.trim().is_empty(), "a conveyor_spot names no line");
        lines.entry(line).or_default().push(marker);
    }

    for (line, mut pieces) in lines {
        pieces.sort_by_key(|piece| {
            property(piece, "index")
                .and_then(|index| index.parse::<i32>().ok())
                .expect("every conveyor_spot has an integer index")
        });

        let role = |piece: &Entity| property(piece, "role").unwrap_or_default();
        let count = |what: &str| pieces.iter().filter(|piece| role(piece) == what).count();
        assert_eq!(count("intake"), 1, "line '{line}' needs exactly one intake");
        assert!(
            count("chute") >= 1,
            "line '{line}' has nowhere to send anything"
        );
        assert!(count("run") >= 1, "line '{line}' has no belt");
        for piece in &pieces {
            let role = role(piece);
            assert!(
                matches!(role.as_str(), "intake" | "run" | "sorter" | "chute"),
                "line '{line}' has a piece with unknown role '{role}'",
            );
            if role == "chute" {
                assert!(
                    !property(piece, "label")
                        .unwrap_or_default()
                        .trim()
                        .is_empty(),
                    "a chute on line '{line}' has no destination label",
                );
            }
        }

        // Runs have to meet end to end. `build_conveyor_lines` joins them into
        // one path and only collapses ends within 5 cm of each other; a gap
        // wider than that becomes an invisible extra segment a parcel slides
        // along sideways through open air.
        let mut previous_end: Option<(f32, f32)> = None;
        for piece in pieces.iter().filter(|piece| role(piece) == "run") {
            let (x, z) = origin_xz(piece).expect("a run with a valid origin");
            let length: f32 = property(piece, "length")
                .and_then(|length| length.parse().ok())
                .expect("every run declares a length");
            assert!(length > 0.0, "a run on line '{line}' has no length");
            let angles = property(piece, "angles").unwrap_or_default();
            // `+Z` local, as every other point class in the map reads it.
            let (dx, dz) = match angles.as_str() {
                "0 0 0" => (0.0, 1.0),
                "0 90 0" => (1.0, 0.0),
                "0 180 0" => (0.0, -1.0),
                "0 -90 0" => (-1.0, 0.0),
                other => panic!("unsupported conveyor angle {other}"),
            };
            if let Some((px, pz)) = previous_end {
                let gap = ((x - px).powi(2) + (z - pz).powi(2)).sqrt();
                assert!(
                    gap <= 0.05,
                    "line '{line}' has a {gap} m gap between runs at ({px}, {pz}) and ({x}, {z})",
                );
            }
            previous_end = Some((x + dx * length, z + dz * length));
        }

        // Every diverter has to sit downstream of the scanner, or parcels are
        // sorted after they have already been sent somewhere.
        if let Some(sorter) = pieces.iter().find(|piece| role(piece) == "sorter") {
            let (sorter_x, _) = origin_xz(sorter).expect("a sorter with a valid origin");
            for chute in pieces.iter().filter(|piece| role(piece) == "chute") {
                let (chute_x, _) = origin_xz(chute).expect("a chute with a valid origin");
                assert!(
                    chute_x > sorter_x,
                    "a chute on line '{line}' at x {chute_x} sits upstream of the sorter \
                     at x {sorter_x}",
                );
            }
        }
    }
}

#[test]
fn a_conveyor_crossing_walkable_floor_clears_head_height() {
    // Cargo's belt runs the length of the north wall, and the one strip of
    // floor that has to run wall to wall crosses it. The belt goes *over* that
    // lane rather than stopping short of it, which is only acceptable while it
    // is genuinely overhead: let the deck sag toward walking height and the
    // hall's only route between its two airlocks becomes something you walk
    // face-first into. Nothing else notices -- the props carry no collider, so
    // the game would happily draw a conveyor through somebody's head.
    const CLEARANCE: f32 = 2.2;

    let map = parse();
    let walkable: Vec<Bounds> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
        .collect();

    let mut runs: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("conveyor_spot"))
        .filter(|entity| property(entity, "role").as_deref() == Some("run"))
        .collect();
    runs.sort_by_key(|run| {
        property(run, "index")
            .and_then(|index| index.parse::<i32>().ok())
            .expect("every run has an index")
    });

    // The same chaining `freight::Line::assemble` does: a run starts at
    // whatever height the one before it finished at.
    let mut deck = 0.95;
    for run in runs {
        let (x, z) = origin_xz(run).expect("a run with a valid origin");
        let length: f32 = property(run, "length")
            .and_then(|value| value.parse().ok())
            .expect("every run declares a length");
        let rise: f32 = property(run, "rise")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0.0);
        let (dx, dz) = match property(run, "angles").unwrap_or_default().as_str() {
            "0 0 0" => (0.0, 1.0),
            "0 90 0" => (1.0, 0.0),
            "0 180 0" => (0.0, -1.0),
            "0 -90 0" => (-1.0, 0.0),
            other => panic!("unsupported conveyor angle {other}"),
        };

        for step in 0..=40 {
            let along = step as f32 / 40.0;
            let point = Vec3::new(x + dx * length * along, 0.0, z + dz * length * along);
            let height = deck + rise * along;
            if height >= CLEARANCE {
                continue;
            }
            if let Some(area) = walkable.iter().find(|area| area.holds(point)) {
                panic!(
                    "a conveyor run passes over walkable floor {area:?} at ({}, {}) \
                     only {height} m up; either lift it clear or carve the floor",
                    point.x, point.z,
                );
            }
        }
        deck += rise;
    }

    assert!(
        (deck - 0.95).abs() < 0.001,
        "the line finishes {deck} m up: every metre climbed has to be given back, \
         or the belt ends in mid-air",
    );
}

#[test]
fn no_conveyor_marker_stands_in_walkable_floor() {
    // The same invariant floor-standing decorations already assert, and for
    // the same reason: nothing here carries a collider, crew path from
    // `func_walkable` alone, and `contain_on_surface` confines the player to it
    // too. Carving the band is the *only* thing that stops a body walking
    // through a running belt, so un-carving it has to fail here rather than in
    // a playtest.
    let map = parse();
    let walkable: Vec<Bounds> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
        .collect();

    for marker in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("conveyor_spot"))
    {
        let (x, z) = origin_xz(marker).expect("a conveyor_spot with a valid origin");
        let role = property(marker, "role").unwrap_or_default();
        assert!(
            (FREIGHT_BAND_MIN_Z..=FREIGHT_BAND_MAX_Z).contains(&z),
            "conveyor_spot '{role}' at ({x}, {z}) is outside the carved freight band",
        );
        let point = Vec3::new(x, 0.0, z);
        if let Some(area) = walkable.iter().find(|area| area.holds(point)) {
            panic!(
                "conveyor_spot '{role}' at ({x}, {z}) stands in walkable floor {area:?}; \
                 carve the band back or bodies will walk through the belt",
            );
        }
    }
}

#[test]
fn department_and_escape_markers_are_on_floor_and_route_to_gameplay() {
    let map = parse();
    let mut areas = WalkableAreas::default();
    for bounds in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
    {
        areas.push(bounds, None);
    }
    let graph = NavGraph::build(&areas, NAV_RADIUS);
    let on_floor = |point: Vec3| {
        const TOLERANCE: f32 = 0.25;
        areas.regions().iter().any(|region| {
            point.x >= region.bounds.min_x - TOLERANCE
                && point.x <= region.bounds.max_x + TOLERANCE
                && point.z >= region.bounds.min_z - TOLERANCE
                && point.z <= region.bounds.max_z + TOLERANCE
        })
    };
    let marker_position = |entity: &Entity| {
        let (x, z) = origin_xz(entity).expect("a valid marker origin");
        Vec3::new(x, 0.0, z)
    };

    let pods: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("escape_pod"))
        .collect();
    assert_eq!(
        pods.len(),
        1,
        "the station should expose one escape_pod marker"
    );
    let pod = marker_position(pods[0]);
    assert!(
        on_floor(pod),
        "escape_pod at {pod} is not on walkable floor"
    );

    for department in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("department_spot"))
    {
        let role = property(department, "department").unwrap_or_else(|| "<missing>".into());
        let home = marker_position(department);
        assert!(
            on_floor(home),
            "department_spot `{role}` at {home} is not on walkable floor",
        );
        assert!(
            graph.path(home, COUNTER_SPOT).is_some(),
            "department_spot `{role}` cannot route to the lab counter",
        );
        assert!(
            graph.path(home, pod).is_some(),
            "department_spot `{role}` cannot route to the escape pod",
        );
    }
}

/// Every kind of `crew_post` the loader will accept.
///
/// `lab::tb`'s own `match` only `warn!`s on anything else, which is invisible
/// in play: a mistyped kind silently authors nothing at all.
const CREW_POST_KINDS: [&str; 5] = ["work", "relax", "loiter", "visit", "duty"];

#[test]
fn every_crew_post_is_on_walkable_floor_and_routes_to_the_counter() {
    // `department_spot` has had this check since the station was laid out;
    // `crew_post` never did, which was tolerable at eleven hand-verified
    // markers and is not at forty. The Bridge is the reason: its walkable
    // aisles are 72-88 units wide with furniture carved out between them, so a
    // post authored twenty units off is in a console rather than beside it,
    // and the only symptom in play is one crew member who never arrives.
    let map = parse();
    let areas = authored_walkable_areas();
    let graph = NavGraph::build(&areas, NAV_RADIUS);
    let on_floor = |point: Vec3| {
        const TOLERANCE: f32 = 0.25;
        areas.regions().iter().any(|region| {
            point.x >= region.bounds.min_x - TOLERANCE
                && point.x <= region.bounds.max_x + TOLERANCE
                && point.z >= region.bounds.min_z - TOLERANCE
                && point.z <= region.bounds.max_z + TOLERANCE
        })
    };

    for post in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("crew_post"))
    {
        let kind = property(post, "kind").unwrap_or_else(|| "<missing>".into());
        let occupant = property(post, "occupant").unwrap_or_default();
        let label = if occupant.is_empty() {
            format!("{kind} post")
        } else {
            format!("{kind} post for {occupant}")
        };
        let (x, z) = origin_xz(post).expect("a valid crew_post origin");
        let at = Vec3::new(x, 0.0, z);
        assert!(on_floor(at), "{label} at {at} is not on walkable floor");
        assert!(
            graph.path(at, COUNTER_SPOT).is_some(),
            "{label} at {at} cannot route to the lab counter",
        );
    }
}

#[test]
fn every_crew_post_kind_is_one_the_loader_handles() {
    let map = parse();
    for post in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("crew_post"))
    {
        let kind = property(post, "kind").unwrap_or_else(|| "<missing>".into());
        assert!(
            CREW_POST_KINDS.contains(&kind.as_str()),
            "crew_post has kind '{kind}', which `lab::tb` only warns about",
        );
    }
}

#[test]
fn utility_spots_are_unique_walkable_and_routable() {
    let map = parse();
    let areas = authored_walkable_areas();
    let graph = NavGraph::build(&areas, NAV_RADIUS);
    let spots: Vec<_> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("utility_spot"))
        .collect();
    let expected = [
        "cargo.manifest",
        "cargo.weigh",
        "cargo.sort",
        "cargo.dispatch",
        "cargo.requisition",
        "medical.bed.1",
        "medical.bed.2",
        "botany.plot.inspect",
        "botany.irrigation",
        "botany.plot.tend",
        "botany.harvest.process",
        "botany.output.shelf",
        "engineering.generator.inspect",
        "engineering.breaker.maintenance",
        "engineering.coolant.manifold",
        "engineering.power.monitor",
        "service.kitchen.prep",
        "service.meal.pass",
        "service.table.host",
        "service.cleanup",
        "service.lounge.seat.1",
        "service.lounge.seat.2",
        "service.lounge.gather",
        // Dining tables, capacity 4 apiece. Service is the one room the whole
        // station visits and it had two seats for thirty residents.
        "service.table.a",
        "service.table.b",
        "service.table.c",
        "service.table.d",
        "service.table.e",
        "service.table.f",
        "service.table.g",
        "service.table.h",
        "service.table.i",
        "service.table.j",
        "service.table.k",
        "service.table.l",
        "security.dispatch",
        "security.desk",
        "security.evidence",
        "security.interview.room",
        "bridge.helm",
        "bridge.comms",
        "bridge.station.monitor",
        "bridge.briefing",
        // One voluntary-aid intake per department — see `utility_ai::aid`.
        // A player must stand within reach of these to donate, and workers
        // walk to them to assess, so both halves need the walkable and
        // routable checks below.
        "medical.aid_intake",
        "security.aid_intake",
        "engineering.aid_intake",
        "cargo.aid_intake",
        "service.aid_intake",
        "botany.aid_intake",
        "bridge.aid_intake",
    ];
    assert_eq!(spots.len(), expected.len());

    let mut ids = std::collections::HashSet::new();
    for spot in spots {
        let id = property(spot, "id").unwrap_or_default();
        assert!(
            expected.contains(&id.as_str()),
            "unexpected utility spot '{id}'"
        );
        assert!(ids.insert(id.clone()), "duplicate utility spot '{id}'");
        let capacity = property(spot, "capacity")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        assert!(capacity > 0, "utility spot '{id}' has no capacity");
        let (x, z) = origin_xz(spot).expect("utility_spot has a valid origin");
        let at = Vec3::new(x, 0.0, z);
        assert!(
            areas.regions().iter().any(|region| region.bounds.holds(at)),
            "utility spot '{id}' at {at} is not on walkable floor",
        );
        assert!(
            graph.path(at, COUNTER_SPOT).is_some(),
            "utility spot '{id}' at {at} cannot route to the counter",
        );
    }
}

/// The galley bar is concentric with the room's wayfinding hub.
///
/// The hub is not decoration: `dress_wayfinding_hubs` spawns a disc with the
/// department route lines radiating out of it, so the deck itself draws a
/// starburst converging on one point. The first galley put its bar 5 m away from
/// that point, and the room read as two competing centres — the floor pointing
/// at nothing and a bar sitting off to one side of it.
///
/// Concentric, the routes run under the counter and out between the stools, and
/// the thing the floor already pointed at is the thing you walk to.
///
/// Falsifies by construction: move either the hub or the ring and the centres
/// separate. The tolerance is tight because there is no reason for these to be
/// nearly aligned — they are either the same point or the bug is back.
#[test]
fn the_galley_bar_is_built_around_the_wayfinding_hub() {
    /// The hub disc's own outer radius (`HUB_PORT_OUTER_RADIUS`). A ring that
    /// cleared this would sit on top of the routes instead of around them.
    const HUB_RADIUS: f32 = 1.17;
    const TOLERANCE: f32 = 0.05;

    let map = parse();
    let hub = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("wayfinding_hub"))
        .filter_map(origin_xz)
        .find(|(x, z)| (-47.5..=-23.5).contains(x) && (15.0..=39.0).contains(z))
        .expect("Service has a wayfinding hub");

    let segments: Vec<(f32, f32)> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("decoration_spot"))
        .filter(|entity| property(entity, "kind").as_deref() == Some("svc.bar_counter"))
        .filter_map(origin_xz)
        .collect();
    assert!(
        segments.len() >= 8,
        "the bar is {} segments; too few to read as a ring",
        segments.len(),
    );

    let centre = (
        segments.iter().map(|(x, _)| x).sum::<f32>() / segments.len() as f32,
        segments.iter().map(|(_, z)| z).sum::<f32>() / segments.len() as f32,
    );
    // The omitted entrance segment biases the mean toward the far side, so
    // compare radii per segment rather than trusting the centroid alone.
    let offset = ((centre.0 - hub.0).powi(2) + (centre.1 - hub.1).powi(2)).sqrt();
    assert!(
        offset <= 0.35,
        "the bar's centre is {offset:.2} m from the wayfinding hub at \
         ({:.1}, {:.1}); the deck's route lines converge somewhere the bar is not",
        hub.0,
        hub.1,
    );

    let radii: Vec<f32> = segments
        .iter()
        .map(|(x, z)| ((x - hub.0).powi(2) + (z - hub.1).powi(2)).sqrt())
        .collect();
    let (min, max) = radii
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), r| (lo.min(*r), hi.max(*r)));
    assert!(
        max - min <= TOLERANCE,
        "bar segment radii from the hub span {min:.2}..{max:.2} m; \
         the ring is not centred on the hub",
    );
    assert!(
        min > HUB_RADIUS,
        "the bar ring sits at {min:.2} m, inside the hub disc's {HUB_RADIUS} m \
         radius; the counter would cover the route lines instead of framing them",
    );
}

/// Service must be able to seat a real fraction of the station.
///
/// It is the one room the whole crew has a reason to visit, and it had **two**
/// seats — `service.lounge.seat.1` and `.2`, capacity 1 each — for thirty
/// residents. Anything that drives people there (hunger, a break, a meal) would
/// have sent a crowd to a room that could seat two, and the occupancy filter
/// would have bounced the rest back to standing at their posts: a queue at a
/// bench, which reads worse than the empty room it replaced.
///
/// Falsifies the rebuild: delete the `service.table.*` markers from the map and
/// the room drops back to four places.
#[test]
fn the_service_room_seats_a_real_fraction_of_the_crew() {
    /// Enough that a shift change or a mealtime looks like a canteen rather
    /// than a queue. Deliberately well under thirty — not everyone eats at
    /// once, and a room with a seat per resident would be mostly empty chairs.
    const ENOUGH_SEATS: usize = 12;

    let map = parse();
    let seats: usize = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("utility_spot"))
        .filter(|entity| {
            property(entity, "id").is_some_and(|id| {
                id.starts_with("service.table.") || id.starts_with("service.lounge.seat")
            })
        })
        .filter_map(|entity| property(entity, "capacity")?.parse::<usize>().ok())
        .sum();

    assert!(
        seats >= ENOUGH_SEATS,
        "Service seats {seats}; a station of thirty needs at least \
         {ENOUGH_SEATS} places or everyone sent there stands up",
    );
}

/// Every seat must be reachable from the counter people collect food at.
///
/// A table walled off by its own furniture is the failure the collision-extent
/// registration makes easy, and it would present as a resident who selects a
/// meal, walks, fails, and re-selects for ever.
#[test]
fn every_service_seat_is_reachable_from_the_meal_pass() {
    let map = parse();
    let areas = authored_walkable_areas();
    let graph = NavGraph::build(&areas, NAV_RADIUS);

    let spot = |wanted: &str| {
        map.iter()
            .filter(|entity| classname(entity).as_deref() == Some("utility_spot"))
            .find(|entity| property(entity, "id").as_deref() == Some(wanted))
            .and_then(origin_xz)
            .map(|(x, z)| Vec3::new(x, 0.0, z))
    };
    let pass = spot("service.meal.pass").expect("Service authors a meal pass");

    let mut checked = 0;
    for entity in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("utility_spot"))
    {
        let Some(id) = property(entity, "id") else {
            continue;
        };
        if !(id.starts_with("service.table.") || id.starts_with("service.lounge.seat")) {
            continue;
        }
        let (x, z) = origin_xz(entity).expect("utility_spot has a valid origin");
        assert!(
            graph.path(pass, Vec3::new(x, 0.0, z)).is_some(),
            "'{id}' cannot be walked to from the meal pass, so anyone sent \
             there to eat never arrives",
        );
        checked += 1;
    }
    assert!(
        checked >= 6,
        "only {checked} seats were checked; this test is silently covering \
         almost nothing",
    );
}

#[test]
fn work_posts_name_a_roster_member_and_communal_posts_do_not() {
    // Both halves are silent failures otherwise. A `work` post with no
    // occupant is dropped by the loader with a warning; an occupant on a
    // communal post is a copy-paste slip that reads as authored intent.
    let map = parse();
    let roster: Vec<crate::crew::CrewDef> =
        ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();

    for post in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("crew_post"))
    {
        let kind = property(post, "kind").unwrap_or_default();
        let occupant = property(post, "occupant").unwrap_or_default();
        if kind == "work" {
            assert!(
                roster.iter().any(|member| member.name == occupant),
                "work crew_post names '{occupant}', who is not on the roster",
            );
        } else {
            assert!(
                occupant.is_empty(),
                "{kind} crew_post is communal but names an occupant '{occupant}'",
            );
        }
    }
}

/// Every resident must have somewhere the utility AI can send them.
///
/// `MaintainPost` is the Routine-bucket floor: when a department's board is
/// momentarily empty it is the only thing standing between a resident and
/// idling for the rest of the shift. It needs a target, and a missing target
/// does not lower its score — `HasTarget` is a multiplicative consideration,
/// so zero deletes the candidate outright.
///
/// The map authors only nine `work` posts against a cast of thirty, so
/// twenty-one residents depend entirely on reaching a communal `duty` post.
/// This asserts every one of them can, by route rather than by straight line.
///
/// Falsifies the whole arrangement: delete the `duty` markers from the map and
/// this names the twenty-one people who would have nothing to do.
#[test]
fn the_whole_authored_cast_can_maintain_a_post() {
    let map = parse();
    let areas = authored_walkable_areas();
    let graph = NavGraph::build(&areas, NAV_RADIUS);

    let posts: Vec<(String, String, Vec3)> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("crew_post"))
        .filter_map(|entity| {
            let kind = property(entity, "kind")?;
            let (x, z) = origin_xz(entity)?;
            Some((
                kind,
                property(entity, "occupant").unwrap_or_default(),
                Vec3::new(x, 0.0, z),
            ))
        })
        .collect();
    let duty: Vec<Vec3> = posts
        .iter()
        .filter(|(kind, _, _)| kind == "duty")
        .map(|(_, _, at)| *at)
        .collect();
    assert!(!duty.is_empty(), "the map authors no communal duty posts");

    // Where each resident starts the shift: their department's gathering point.
    let homes: Vec<(String, Vec3)> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("department_spot"))
        .filter_map(|entity| {
            let department = property(entity, "department")?;
            let (x, z) = origin_xz(entity)?;
            Some((department, Vec3::new(x, 0.0, z)))
        })
        .collect();

    let roster: Vec<crate::crew::CrewDef> =
        ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
    let cast: Vec<(String, String)> = roster
        .iter()
        .map(|member| (member.name.clone(), member.role.clone()))
        .chain(
            crate::crew::fluff::support_crew()
                .iter()
                .map(|(name, role)| ((*name).to_string(), (*role).to_string())),
        )
        .collect();
    assert_eq!(
        cast.len(),
        30,
        "the station is authored for thirty residents; this test is covering {} \
         and would silently miss the rest",
        cast.len(),
    );

    for (name, role) in &cast {
        if posts
            .iter()
            .any(|(kind, occupant, _)| kind == "work" && occupant == name)
        {
            continue;
        }
        let Some((_, home)) = homes.iter().find(|(department, _)| department == role) else {
            panic!("{name} has role '{role}', which has no department_spot to start from");
        };
        assert!(
            duty.iter().any(|at| graph.path(*home, *at).is_some()),
            "{name} ({role}) has no personal work post and cannot walk to any \
             communal duty post from their department point — they would idle \
             for the whole shift the moment their department's board emptied",
        );
    }
}

#[test]
fn the_bridge_has_somewhere_to_pretend_to_work() {
    // The Bridge is the most heavily furnished room in the map and had nobody
    // in it until duty posts existed. Without this, tidying the map back to
    // zero duty posts would silently return it to being an empty set — the
    // fluff crew would still spawn, on their department point, doing nothing.
    const BRIDGE_MIN_X: f32 = -83.5;
    const BRIDGE_MAX_X: f32 = -41.5;

    let map = parse();
    let on_the_bridge = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("crew_post"))
        .filter(|entity| property(entity, "kind").as_deref() == Some("duty"))
        .filter_map(origin_xz)
        .filter(|(x, _)| *x >= BRIDGE_MIN_X && *x <= BRIDGE_MAX_X)
        .count();

    assert!(
        on_the_bridge >= 4,
        "the Bridge has {on_the_bridge} duty posts; it needs enough to look staffed",
    );
}

#[test]
fn station_v2_routes_hit_the_pacing_distance_budget() {
    let map = parse();
    let areas = authored_walkable_areas();
    let graph = NavGraph::build(&areas, NAV_RADIUS);
    let route_length = |from: Vec3, to: Vec3| {
        let path = graph
            .path(from, to)
            .unwrap_or_else(|| panic!("no route from {from} to {to}"));
        let mut length = 0.0;
        let mut previous = from;
        for waypoint in path {
            length += previous.distance(waypoint);
            previous = waypoint;
        }
        length
    };

    let entrances: Vec<(String, Vec3)> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("department_spot"))
        .filter_map(|entity| {
            let role = property(entity, "department")?;
            let (x, z) = origin_xz(entity)?;
            Some((role, Vec3::new(x, 0.0, z)))
        })
        .filter(|(role, _)| role != "Chemistry")
        .collect();
    let (role, farthest) = entrances
        .iter()
        .map(|(role, point)| (role, route_length(COUNTER_SPOT, *point)))
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .expect("department entrances");
    assert!(
        (90.0..=115.0).contains(&farthest),
        "Chemistry to farthest entrance ({role}) is {farthest:.1} m",
    );

    let deepest_cargo = Vec3::new(-106.0, 0.0, 48.0);
    let deep = route_length(COUNTER_SPOT, deepest_cargo);
    assert!(
        (125.0..=155.0).contains(&deep),
        "Chemistry to deep Cargo event space is {deep:.1} m",
    );
}

#[test]
fn maintenance_cross_routes_create_meaningful_optional_shortcuts() {
    let map = parse();
    let mut public_areas = WalkableAreas::default();
    for entity in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .filter(|entity| {
            !property(entity, "room")
                .unwrap_or_default()
                .starts_with("Maintenance")
        })
    {
        for brush in &entity.brushes {
            public_areas.push(footprint(brush), property(entity, "room"));
        }
    }
    let public = NavGraph::build(&public_areas, NAV_RADIUS);
    let full = NavGraph::build(&authored_walkable_areas(), NAV_RADIUS);
    let length = |graph: &NavGraph, from: Vec3, to: Vec3| -> Option<f32> {
        let mut previous = from;
        let mut total = 0.0;
        for point in graph.path(from, to)? {
            total += previous.distance(point);
            previous = point;
        }
        Some(total)
    };
    let mut improvements = Vec::new();
    let mut measured = Vec::new();
    for (a, b, from, to) in [
        (
            "Security back room",
            "Engineering utility",
            Vec3::new(-107.0, 0.0, -7.0),
            Vec3::new(-107.0, 0.0, 22.0),
        ),
        (
            "Security back room",
            "Cargo deep storage",
            Vec3::new(-107.0, 0.0, -7.0),
            Vec3::new(-107.0, 0.0, 42.0),
        ),
        (
            "Engineering utility",
            "Cargo deep storage",
            Vec3::new(-107.0, 0.0, 22.0),
            Vec3::new(-107.0, 0.0, 42.0),
        ),
    ] {
        let normal = length(&public, from, to).expect("public route");
        let shortcut = length(&full, from, to).expect("maintenance route");
        measured.push((a, b, normal, shortcut));
        if shortcut <= normal * 0.8 {
            improvements.push((a, b, normal, shortcut));
        }
    }
    assert!(
        improvements.len() >= 3,
        "only {} representative pairs gained a 20% shortcut: {measured:?}",
        improvements.len(),
    );
}

#[test]
fn the_station_is_one_connected_space() {
    // The failure this catches is a wing whose doorway bridge was forgotten:
    // the room is drawn, lit and walkable, and simply cannot be reached. Uses
    // the same overlap rule `nav` builds its graph from, so agreeing here means
    // agreeing there.
    let mut regions: Vec<Bounds> = Vec::new();
    for entity in parse() {
        if classname(&entity).as_deref() != Some("func_walkable") {
            continue;
        }
        for brush in &entity.brushes {
            regions.push(footprint(brush));
        }
    }
    assert!(regions.len() > 15, "expected the whole station's floor");

    // Inset as `nav` does, so a bridge too narrow for a body counts as absent.
    let inset: Vec<Bounds> = regions
        .iter()
        .map(|bounds| bounds.inset(crate::nav::NAV_RADIUS))
        .filter(|bounds| bounds.is_standable())
        .collect();

    let mut reached = vec![false; inset.len()];
    reached[0] = true;
    let mut frontier = vec![0usize];
    while let Some(current) = frontier.pop() {
        for (index, region) in inset.iter().enumerate() {
            if !reached[index] && inset[current].overlaps(region) {
                reached[index] = true;
                frontier.push(index);
            }
        }
    }

    let stranded = reached.iter().filter(|seen| !**seen).count();
    assert_eq!(
        stranded,
        0,
        "{stranded} of {} walkable regions cannot be walked to from the first",
        inset.len(),
    );
}

#[test]
fn the_lower_cross_is_reached_through_a_stair_not_a_vertical_teleport() {
    let graph = NavGraph::build(&authored_walkable_areas(), NAV_RADIUS);
    let ground = Vec3::new(-40.1, 0.0, -10.5);
    let lower = Vec3::new(-40.1, -3.6, 30.0);
    let path = graph
        .path(ground, lower)
        .expect("the ground ring should connect to the lower maintenance cross");

    assert!(
        path.iter().any(|point| point.y < -0.01 && point.y > -3.59),
        "route skipped the stair's intermediate elevations: {path:?}",
    );
    assert!(
        path.last()
            .is_some_and(|point| (point.y + 3.6).abs() < 0.01),
        "route did not arrive on the lower deck: {path:?}",
    );
}

#[test]
fn every_entity_in_the_map_is_a_class_the_game_registers() {
    // `bevy_trenchbroom` resolves each entity's classname against the registered
    // Quake classes as the scene is built, so an entity placed in TrenchBroom
    // that Rust has never heard of fails at load — after the menu, on the way
    // into a shift. Cheaper to notice here.
    //
    // Keep in step with the `register_type` calls in `lab::tb`.
    const KNOWN: &[&str] = &[
        "worldspawn",
        "light_point",
        "func_walkable",
        "machine_spot",
        "door_spot",
        "chemist_start",
        "department_spot",
        "crew_post",
        "utility_spot",
        "queue_point",
        "department_dressing",
        "decoration_spot",
        "conveyor_spot",
        "wayfinding_hub",
        "wayfinding_sign",
        "escape_pod",
        "crisis_spot",
        "room_sign",
    ];

    for entity in parse() {
        let name = classname(&entity).expect("every entity needs a classname");
        assert!(
            KNOWN.contains(&name.as_str()),
            "{MAP} has a `{name}` entity, which no class in lab::tb registers",
        );
    }
}

#[test]
fn every_airlock_has_unique_semantic_ids_and_a_matching_nav_bridge() {
    let map = parse();
    let bridges: std::collections::HashSet<String> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .filter_map(|entity| property(entity, "bridge_id"))
        .collect();
    let mut ids = std::collections::HashSet::new();
    let mut door_bridges = std::collections::HashSet::new();
    let mut count = 0;

    for door in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("door_spot"))
    {
        count += 1;
        let id = property(door, "id").unwrap_or_default();
        let bridge = property(door, "bridge_id").unwrap_or_default();
        assert!(!id.trim().is_empty(), "a door_spot has no stable id");
        assert!(
            !bridge.trim().is_empty(),
            "door_spot `{id}` has no bridge_id"
        );
        assert!(ids.insert(id.clone()), "duplicate door_spot id `{id}`");
        assert!(
            door_bridges.insert(bridge.clone()),
            "two door_spots claim bridge `{bridge}`",
        );
        assert!(
            bridges.contains(&bridge),
            "door_spot `{id}` references missing nav bridge `{bridge}`",
        );
    }

    assert!(
        count >= 8,
        "the station needs authored department/crossover airlocks"
    );
}

#[test]
fn every_airlock_id_names_a_department_the_door_sprites_cover() {
    // `door::DoorSkin` reads the department straight out of the id rather than
    // keeping a second registry beside the map, so a misspelled or brand-new
    // department does not fail loudly -- it quietly paints the door neutral.
    // This is what makes that visible.
    let map = parse();
    for door in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("door_spot"))
    {
        let id = property(door, "id").unwrap_or_default();
        let neutral_by_name = id.split('.').nth(2) == Some("maintenance");
        assert!(
            crate::door::DoorSkin::from_spot_id(&id) != crate::door::DoorSkin::Maintenance
                || neutral_by_name,
            "door_spot `{id}` falls back to the neutral maintenance skin: either its \
             department is misspelled, or `door::DoorSkin` needs a variant for it and \
             `tools/gen_door_sprites.py` a color",
        );
    }
}

#[test]
fn physical_walls_match_walkable_routes_and_airlock_gaps() {
    let map = parse();
    let blocking_walls: Vec<(Bounds, (f32, f32))> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("worldspawn"))
        .flat_map(|entity| &entity.brushes)
        .filter(|brush| {
            brush
                .first()
                .is_some_and(|surface| surface.texture.to_string_lossy() == "wall")
                && vertical_span(brush).0 <= 0.01
                && vertical_span(brush).1 >= 1.0
        })
        .map(|brush| (footprint(brush), vertical_span(brush)))
        .collect();

    // Compared in three dimensions, not two. The station has two decks now:
    // the ground floor runs directly over the lower maintenance tunnel for
    // most of its length, so every ground-floor wall shares a footprint with
    // some stretch of tunnel below it. Only a wall standing in the same band
    // of height a body occupies on that floor is actually in the way.
    for (area, floor) in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .flat_map(|entity| {
            entity
                .brushes
                .iter()
                .map(|brush| (footprint(brush), vertical_span(brush)))
        })
    {
        let standing = area.inset(NAV_RADIUS);
        let head = floor.0 + 1.0;
        if let Some((wall, _)) = blocking_walls.iter().find(|(wall, span)| {
            wall.intersection(&standing).is_some() && span.1 > floor.0 + 0.01 && span.0 < head
        }) {
            panic!("floor-height wall {wall:?} intrudes into walkable area {area:?}");
        }
    }

    for door in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("door_spot"))
    {
        let id = property(door, "id").unwrap_or_else(|| "<missing>".into());
        let bridge_id = property(door, "bridge_id").unwrap_or_default();
        let (x, z) = origin_xz(door).expect("door_spot with a valid origin");
        let point = Vec3::new(x, 0.0, z);
        assert!(
            blocking_walls.iter().all(|(wall, _)| !wall.holds(point)),
            "door_spot `{id}` is embedded in a floor-height wall",
        );

        let matching: Vec<Bounds> = map
            .iter()
            .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
            .filter(|entity| property(entity, "bridge_id").as_deref() == Some(&bridge_id))
            .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "door_spot `{id}` should own exactly one bridge footprint",
        );
        assert!(
            matching[0].holds(point),
            "door_spot `{id}` at {point} is not centred on bridge {bridge_id}",
        );
    }

    for stale in [
        "Legacy Medical Spur",
        "Legacy Engineering Spur",
        "Legacy Cargo Spur",
        "Legacy Security Spur",
        "Legacy Service Spur",
        "Maintenance Loop",
    ] {
        assert!(
            map.iter()
                .all(|entity| property(entity, "room").as_deref() != Some(stale)),
            "stale pre-V2 walkable region `{stale}` remains in {MAP}",
        );
    }
}

#[test]
fn every_runtime_crisis_location_has_one_unique_map_marker() {
    // These ids are the interface between content and layout. Duplicates are
    // ambiguous and a missing id would make the corresponding event silently
    // skip instead of appearing at some accidental legacy coordinate.
    const REQUIRED: &[&str] = &[
        "cult.wet_chalk_sigil",
        "cult.whispering_residue",
        "cult.bleeding_offering_bowl",
        "cult.scorched_invocation",
        "cult.airless_candle",
        "cult.rift_seal_scar",
        "hazard.rad_leak",
        "hazard.coolant_vent",
        "showdown.breach",
    ];

    let mut ids = std::collections::HashSet::new();
    for entity in parse()
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("crisis_spot"))
    {
        let id = property(entity, "id").unwrap_or_default();
        assert!(!id.trim().is_empty(), "a crisis_spot in {MAP} has no id");
        assert!(
            ids.insert(id.clone()),
            "duplicate crisis_spot id `{id}` in {MAP}"
        );
    }

    for required in REQUIRED {
        assert!(
            ids.contains(*required),
            "{MAP} has no crisis_spot with id `{required}`",
        );
    }

    // Content is allowed to grow without updating this test's fixed list. Any
    // newly authored RON `spot` reference must nevertheless have a map marker.
    for data_file in [
        "assets/data/station.cult.ron",
        "assets/data/station.hazards.ron",
        "assets/data/station.arc.ron",
    ] {
        let source = std::fs::read_to_string(data_file)
            .unwrap_or_else(|error| panic!("reading {data_file}: {error}"));
        for line in source.lines() {
            let line = line.trim();
            let Some(id) = line
                .strip_prefix("spot: \"")
                .and_then(|rest| rest.strip_suffix("\","))
            else {
                continue;
            };
            assert!(
                ids.contains(id),
                "{data_file} references crisis spot `{id}`, but {MAP} has no such marker",
            );
        }
    }
}

#[test]
fn every_crisis_marker_touches_reachable_floor() {
    // `the_station_is_one_connected_space` proves this complete footprint set
    // is reachable. Here we prove each semantic event origin actually lands on
    // that set instead of inside a wall or outside the hull.
    const TOLERANCE: f32 = 0.25;
    let map = parse();
    let floor: Vec<Bounds> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
        .collect();

    for marker in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("crisis_spot"))
    {
        let id = property(marker, "id").unwrap_or_else(|| "<missing>".to_string());
        let (x, z) = origin_xz(marker)
            .unwrap_or_else(|| panic!("crisis_spot `{id}` has no valid three-number origin"));
        assert!(
            floor.iter().any(|bounds| {
                x >= bounds.min_x - TOLERANCE
                    && x <= bounds.max_x + TOLERANCE
                    && z >= bounds.min_z - TOLERANCE
                    && z <= bounds.max_z + TOLERANCE
            }),
            "crisis_spot `{id}` at ({x}, {z}) is not on reachable walkable floor",
        );
    }
}

#[test]
fn room_signs_have_visible_text_and_the_lab_entrance_has_one_bridge() {
    let map = parse();
    let signs: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("room_sign"))
        .collect();
    assert!(!signs.is_empty(), "{MAP} has no room_sign markers");
    for sign in signs {
        assert!(
            property(sign, "text").is_some_and(|text| !text.trim().is_empty()),
            "a room_sign in {MAP} has no visible text",
        );
    }

    let sign_text: Vec<String> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("room_sign"))
        .filter_map(|entity| property(entity, "text"))
        .collect();
    for expected in [
        "SECURITY",
        "BRIDGE",
        "MEDICAL / QUARANTINE",
        "CARGO / RECEIVING",
        "SERVICE / CHAPEL",
        "BOTANY",
        "SECURITY BACK ROOM",
        "BRIDGE OPERATIONS",
        "QUARANTINE",
        "ATMOS / UTILITY",
        "CARGO RECEIVING",
        "CHAPEL / QUIET ROOM",
        "BOTANY NURSERY",
        "QUIET ROOM",
        "CHEMISTRY / DISPENSARY",
        "MEDICAL HANDOFF",
    ] {
        assert_eq!(
            sign_text
                .iter()
                .filter(|text| text.as_str() == expected)
                .count(),
            1,
            "public sign `{expected}` should agree with one named walkable room",
        );
    }
    // 16, not 17: Engineering's entrance placard ("ENGINEERING / ATMOS") was
    // removed during manual map edits and has not been re-authored. The room
    // and its department marker still exist; only the signage is gone.
    assert_eq!(sign_text.len(), 16, "stale or duplicate room signs remain");

    let entrance_bridges = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .filter(|entity| property(entity, "bridge_id").as_deref() == Some("lab_entrance"))
        .count();
    assert_eq!(
        entrance_bridges, 1,
        "expected exactly one func_walkable bridge_id=lab_entrance in {MAP}",
    );
}

#[test]
fn common_area_wayfinding_marks_the_crossroads_and_every_department() {
    let map = parse();
    let hubs: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("wayfinding_hub"))
        .collect();
    assert_eq!(hubs.len(), 1, "the station needs one unambiguous route hub");
    assert_eq!(
        origin_xz(hubs[0]),
        Some((-35.0, 27.0)),
        "the route hub moved out of the middle of Service",
    );

    // No Service entry: the hub stands in Service, so that department has no
    // route of its own and no plaque pointing along one. No Engineering entry
    // either: its wayfinding sign was removed along with the room_sign during
    // manual map edits to the arm's north wall (see the missing
    // "ENGINEERING / ATMOS" case in `room_signs_have_visible_text_and_the_lab_
    // entrance_has_one_bridge`) and has not been re-authored.
    let expected: std::collections::HashSet<&str> = [
        "Chemistry",
        "Medical",
        "Cargo",
        "Security",
        "Bridge",
        "Botany",
    ]
    .into_iter()
    .collect();
    let signs: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("wayfinding_sign"))
        .collect();
    assert_eq!(signs.len(), expected.len(), "stale wayfinding signs remain");

    let mut found = std::collections::HashSet::new();
    for sign in signs {
        let department = property(sign, "department").unwrap_or_default();
        assert!(
            expected.contains(department.as_str()),
            "unknown wayfinding department `{department}`",
        );
        assert!(
            found.insert(department.clone()),
            "duplicate wayfinding sign for {department}",
        );
        assert!(
            property(sign, "text").is_some_and(|text| !text.trim().is_empty()),
            "{department} wayfinding sign has no readable text",
        );
        assert!(
            property(sign, "direction")
                .is_some_and(|direction| matches!(direction.as_str(), "left" | "right")),
            "{department} wayfinding sign needs a left/right arrow",
        );
    }
    assert_eq!(found.len(), expected.len());
}

#[test]
fn the_map_still_places_every_machine() {
    const EXPECTED: &[(&str, MachineKind, Vec3)] = &[
        (
            "chemmaster5000.a",
            MachineKind::ChemMaster5000,
            Vec3::new(-5.4, 0.0, -5.05),
        ),
        (
            "mixer.a",
            MachineKind::MixingChamber,
            Vec3::new(-2.2, 0.0, -5.05),
        ),
        (
            "chemmaster5000.b",
            MachineKind::ChemMaster5000,
            Vec3::new(1.4, 0.0, -5.05),
        ),
        (
            "mixer.b",
            MachineKind::MixingChamber,
            Vec3::new(5.4, 0.0, -5.05),
        ),
        (
            "grinder.main",
            MachineKind::Grinder,
            Vec3::new(-4.0, 0.0, 5.55),
        ),
        (
            "analyzer.main",
            MachineKind::Analyzer,
            Vec3::new(10.5, 0.0, -5.05),
        ),
        (
            "delivery.public",
            MachineKind::DeliveryWindow,
            Vec3::new(4.0, 0.0, 4.6),
        ),
        (
            "delivery.medical",
            MachineKind::DeliveryWindow,
            Vec3::new(-16.5, 0.0, 3.25),
        ),
        (
            "board.main",
            MachineKind::StandingBoard,
            Vec3::new(7.1, 0.0, 0.5),
        ),
        (
            "reactor.main",
            MachineKind::ReactionChamber,
            Vec3::new(-12.95, 0.0, -3.5),
        ),
        (
            "locker.main",
            MachineKind::Locker,
            Vec3::new(-1.5, 0.0, 2.45),
        ),
    ];

    let fallback = super::legacy_machine_spots();
    assert_eq!(fallback.len() + 1, EXPECTED.len());
    for (id, kind, expected) in EXPECTED {
        if *id == "delivery.medical" {
            assert!(fallback.get(id).is_none());
            continue;
        }
        let placement = fallback
            .get(id)
            .unwrap_or_else(|| panic!("fallback has no machine spot `{id}`"));
        assert_eq!(placement.kind, *kind, "fallback `{id}` changed kind");
        assert!(
            placement.transform.translation.distance(*expected) < 0.001,
            "fallback `{id}` and authored map placement disagree",
        );
        let expected_facing = match *id {
            "grinder.main" | "delivery.public" => Vec3::NEG_Z,
            "board.main" => Vec3::NEG_X,
            "reactor.main" => Vec3::X,
            _ => Vec3::Z,
        };
        assert!(
            (placement.transform.rotation * Vec3::Z).distance(expected_facing) < 0.000_01,
            "fallback `{id}` and authored map orientation disagree",
        );
    }

    let map = parse();
    let markers: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("machine_spot"))
        .collect();
    assert_eq!(markers.len(), EXPECTED.len());

    let mut ids = std::collections::HashSet::new();
    for marker in &markers {
        let id = property(marker, "id").unwrap_or_default();
        assert!(!id.trim().is_empty(), "a machine_spot in {MAP} has no id");
        assert!(ids.insert(id.clone()), "duplicate machine_spot id `{id}`");
    }

    for (id, kind, expected) in EXPECTED {
        let matches: Vec<_> = markers
            .iter()
            .copied()
            .filter(|marker| property(marker, "id").as_deref() == Some(*id))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one machine_spot id `{id}`"
        );
        let expected_kind = format!("{kind:?}");
        assert_eq!(
            property(matches[0], "kind").as_deref(),
            Some(expected_kind.as_str()),
            "machine_spot `{id}` changed kind",
        );
        let expected_angles = match *id {
            "grinder.main" | "delivery.public" => "0 180 0",
            "board.main" => "0 -90 0",
            "delivery.medical" | "reactor.main" => "0 90 0",
            _ => "0 0 0",
        };
        assert_eq!(
            property(matches[0], "angles").as_deref(),
            Some(expected_angles),
            "machine_spot `{id}` changed orientation",
        );
        let (x, z) = origin_xz(matches[0]).expect("machine spot with a valid origin");
        let actual = Vec3::new(x, 0.0, z);
        assert!(
            actual.distance(*expected) < 0.001,
            "machine_spot `{id}` moved to {actual}; expected {expected}",
        );
    }

    for kind in MachineKind::ALL {
        let expected = if matches!(
            kind,
            MachineKind::ChemMaster5000 | MachineKind::MixingChamber | MachineKind::DeliveryWindow
        ) {
            2
        } else {
            1
        };
        let kind_name = format!("{kind:?}");
        let actual = markers
            .iter()
            .filter(|marker| property(marker, "kind").as_deref() == Some(kind_name.as_str()))
            .count();
        assert_eq!(actual, expected, "wrong number of {kind:?} machine spots");
    }

    assert!(
        map.iter()
            .any(|entity| classname(entity).as_deref() == Some("chemist_start")),
        "{MAP} has nowhere for a chemist to start",
    );
    assert!(
        map.iter()
            .any(|entity| classname(entity).as_deref() == Some("worldspawn")),
        "{MAP} has no worldspawn, so it has no world",
    );
}

#[test]
fn core_lanes_clear_the_authored_chemistry_furniture() {
    // The two islands preserve a separate physical lane for each machine pair.
    // Keep their authored envelopes literal here rather than sharing the map
    // placement table: moving either side without updating this test should be
    // a deliberate workflow decision.
    const FURNITURE: &[Bounds] = &[
        Bounds {
            min_x: -4.8,
            max_x: -2.2,
            min_z: -2.65,
            max_z: -1.75,
        },
        Bounds {
            min_x: 0.7,
            max_x: 3.3,
            min_z: -2.65,
            max_z: -1.75,
        },
    ];
    const CORE_IDS: &[&str] = &["chemmaster5000.a", "mixer.a", "chemmaster5000.b", "mixer.b"];

    let overlaps = |a: Bounds, b: Bounds| {
        a.min_x < b.max_x && a.max_x > b.min_x && a.min_z < b.max_z && a.max_z > b.min_z
    };
    let map = parse();
    let hall = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .filter(|entity| property(entity, "room").as_deref() == Some("Mixing Hall"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
        .reduce(|left, right| Bounds {
            min_x: left.min_x.min(right.min_x),
            max_x: left.max_x.max(right.max_x),
            min_z: left.min_z.min(right.min_z),
            max_z: left.max_z.max(right.max_z),
        })
        .expect("Mixing Hall floor");

    for id in CORE_IDS {
        let marker = map
            .iter()
            .find(|entity| {
                classname(entity).as_deref() == Some("machine_spot")
                    && property(entity, "id").as_deref() == Some(*id)
            })
            .unwrap_or_else(|| panic!("machine spot `{id}`"));
        let (x, z) = origin_xz(marker).expect("valid machine origin");
        // Both core models are 1.5 m wide and 0.8 m deep. Their local +Z
        // working point matches machines::front_of: casing depth + 0.35 m.
        let casing = Bounds {
            min_x: x - 0.75,
            max_x: x + 0.75,
            min_z: z - 0.40,
            max_z: z + 0.40,
        };
        let standing = Bounds {
            min_x: x - NAV_RADIUS,
            max_x: x + NAV_RADIUS,
            min_z: z + 0.75 - NAV_RADIUS,
            max_z: z + 0.75 + NAV_RADIUS,
        };

        for (what, bounds) in [("casing", casing), ("standing footprint", standing)] {
            assert!(
                bounds.min_x >= hall.min_x
                    && bounds.max_x <= hall.max_x
                    && bounds.min_z >= hall.min_z
                    && bounds.max_z <= hall.max_z,
                "{id} {what} leaves the Mixing Hall: {bounds:?}",
            );
            for occupied in FURNITURE {
                assert!(
                    !overlaps(bounds, *occupied),
                    "{id} {what} overlaps visible Chemistry furniture: {bounds:?} vs {occupied:?}",
                );
            }
        }
    }
}
