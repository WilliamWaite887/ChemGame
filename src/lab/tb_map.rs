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

use crate::lab::{Bounds, WalkableAreas, COUNTER_SPOT, TB_SCALE};
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

#[test]
fn every_brush_encloses_a_volume() {
    // Reproduces `bevy_trenchbroom`'s plane maths exactly: a face's three points
    // give normal = (p3 - p1) x (p2 - p1), and the brush interior is where
    // normal . p + d < 0. Normals must therefore point *out*. Reversing a face's
    // winding — easy to do by hand, invisible in the file — turns a solid wall
    // into nothing at all.
    let mut brushes = 0;

    for entity in parse() {
        for brush in &entity.brushes {
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
                    "a brush is inside out: its centre sits outside one of its own faces",
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

        for brush in &entity.brushes {
            let actual = footprint(brush);
            for (what, got, want) in [
                ("min_x", actual.min_x, expected.bounds.min_x),
                ("max_x", actual.max_x, expected.bounds.max_x),
                ("min_z", actual.min_z, expected.bounds.min_z),
                ("max_z", actual.max_z, expected.bounds.max_z),
            ] {
                assert!(
                    (got - want).abs() < 0.01,
                    "{room}'s walkable volume has {what} = {got}, floor plan says {want}",
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
fn the_expansion_keeps_its_authored_room_and_loop_footprints() {
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
            "Maintenance",
            Bounds {
                min_x: -37.0,
                max_x: -31.0,
                min_z: 5.0,
                max_z: 11.0,
            },
        ),
        (
            "Chapel",
            Bounds {
                min_x: -31.0,
                max_x: -25.0,
                min_z: 5.0,
                max_z: 11.0,
            },
        ),
        (
            "Quarantine",
            Bounds {
                min_x: -25.0,
                max_x: -19.0,
                min_z: 5.0,
                max_z: 11.0,
            },
        ),
        (
            "Atmos/Utility",
            Bounds {
                min_x: -19.0,
                max_x: -13.0,
                min_z: 5.0,
                max_z: 11.0,
            },
        ),
    ] {
        let found = named_bounds(room);
        assert_eq!(found.len(), 1, "{room} should be one walkable rectangle");
        assert!(
            bounds_are_close(found[0], expected),
            "{room} moved or changed size: {:?}, expected {:?}",
            found[0],
            expected,
        );
    }

    let corridor = named_bounds("Main Corridor");
    assert_eq!(corridor.len(), 1, "Main Corridor should be one rectangle");
    assert!(
        (corridor[0].min_x + 40.0).abs() < 0.001,
        "Main Corridor no longer reaches the expansion's west edge: {:?}",
        corridor[0],
    );

    let loop_bounds = named_bounds("Maintenance Loop");
    let expected_loop = [
        Bounds {
            min_x: -40.0,
            max_x: -37.0,
            min_z: 4.2,
            max_z: 11.0,
        },
        Bounds {
            min_x: -40.0,
            max_x: -10.0,
            min_z: 2.0,
            max_z: 5.8,
        },
        Bounds {
            min_x: -13.0,
            max_x: -10.0,
            min_z: 4.2,
            max_z: 11.0,
        },
    ];
    assert_eq!(
        loop_bounds.len(),
        expected_loop.len(),
        "Maintenance Loop lost one of its three U-shaped components",
    );
    for expected in expected_loop {
        assert!(
            loop_bounds
                .iter()
                .any(|actual| bounds_are_close(*actual, expected)),
            "Maintenance Loop is missing component {expected:?}; found {loop_bounds:?}",
        );
    }
}

#[test]
fn the_expansion_lighting_matches_the_blockout_budget() {
    let map = parse();
    let lights: Vec<(&Entity, Vec3)> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("light_point"))
        .map(|entity| {
            let (x, z) = origin_xz(entity).expect("a light_point with a valid origin");
            (entity, Vec3::new(x, 0.0, z))
        })
        .collect();

    for (room, bounds) in [
        (
            "Maintenance",
            Bounds {
                min_x: -37.0,
                max_x: -31.0,
                min_z: 5.0,
                max_z: 11.0,
            },
        ),
        (
            "Chapel",
            Bounds {
                min_x: -31.0,
                max_x: -25.0,
                min_z: 5.0,
                max_z: 11.0,
            },
        ),
        (
            "Quarantine",
            Bounds {
                min_x: -25.0,
                max_x: -19.0,
                min_z: 5.0,
                max_z: 11.0,
            },
        ),
        (
            "Atmos/Utility",
            Bounds {
                min_x: -19.0,
                max_x: -13.0,
                min_z: 5.0,
                max_z: 11.0,
            },
        ),
    ] {
        let count = lights
            .iter()
            .filter(|(_, point)| bounds.holds(*point))
            .count();
        assert_eq!(count, 2, "{room} should have two authored light fixtures");
    }

    let expansion_lights = lights.iter().filter(|(_, point)| {
        point.x >= -40.0 && point.x <= -10.0 && point.z >= 2.0 && point.z <= 14.0
    });
    for (entity, _) in expansion_lights {
        assert_eq!(
            property(entity, "shadows_enabled").as_deref(),
            Some("false"),
            "new station lights must stay non-shadowed",
        );
    }

    let assert_even_spacing =
        |label: &str, mut positions: Vec<f32>, start: f32, end: f32, max_gap: f32| {
            positions.sort_by(f32::total_cmp);
            assert!(!positions.is_empty(), "{label} has no lights");
            assert!(
                positions[0] - start <= max_gap * 0.5,
                "{label}'s first light is too far from its start: {positions:?}",
            );
            assert!(
                end - positions[positions.len() - 1] <= max_gap * 0.5,
                "{label}'s last light is too far from its end: {positions:?}",
            );
            for pair in positions.windows(2) {
                assert!(
                    pair[1] - pair[0] <= max_gap,
                    "{label} has a lighting gap wider than {max_gap} m: {positions:?}",
                );
            }
        };

    assert_even_spacing(
        "Main Corridor",
        lights
            .iter()
            .filter(|(_, point)| {
                point.x >= -40.0 && point.x <= 26.0 && (point.z - 12.5).abs() < 0.1
            })
            .map(|(_, point)| point.x)
            .collect(),
        -40.0,
        26.0,
        5.3,
    );
    assert_even_spacing(
        "rear maintenance passage",
        lights
            .iter()
            .filter(|(_, point)| {
                point.x >= -40.0 && point.x <= -10.0 && (point.z - 3.5).abs() < 0.1
            })
            .map(|(_, point)| point.x)
            .collect(),
        -40.0,
        -10.0,
        5.3,
    );
    for expected in [Vec3::new(-38.5, 0.0, 8.0), Vec3::new(-11.5, 0.0, 8.0)] {
        assert!(
            lights
                .iter()
                .any(|(_, point)| point.distance(expected) < 0.1),
            "maintenance leg has no light near {expected}",
        );
    }
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

    for department in ["Medical", "Security", "Engineering", "Cargo", "Service"] {
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
fn department_dressing_markers_fit_their_authored_rooms() {
    // Each shell-free set keeps the starter bay's 4.6 x 3.6 m authoring
    // envelope. These origins put the five public departments against their
    // rear walls while retaining the full south-side route to the corridor;
    // Chemistry occupies the clear east side of the Mixing Hall.
    const HALF_WIDTH: f32 = 2.3;
    const HALF_DEPTH: f32 = 1.8;
    const EXPECTED: &[(&str, &str, Vec3, &str)] = &[
        (
            "Chemistry",
            "Mixing Hall",
            Vec3::new(5.0, 0.0, -3.7),
            "0 0 0",
        ),
        ("Medical", "Medical", Vec3::new(-21.0, 0.0, 20.2), "0 180 0"),
        (
            "Engineering",
            "Engineering",
            Vec3::new(-13.0, 0.0, 20.2),
            "0 180 0",
        ),
        ("Cargo", "Cargo", Vec3::new(-5.0, 0.0, 20.2), "0 180 0"),
        ("Security", "Security", Vec3::new(3.0, 0.0, 20.2), "0 180 0"),
        ("Service", "Service", Vec3::new(11.0, 0.0, 20.2), "0 180 0"),
    ];

    let map = parse();
    let markers: Vec<&Entity> = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("department_dressing"))
        .collect();
    assert_eq!(
        markers.len(),
        EXPECTED.len(),
        "the station needs exactly one dressing marker per starter department",
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
        assert_eq!(room_bounds.len(), 1, "{room} should be one rectangle");
        let bounds = room_bounds[0];
        assert!(
            origin.x - HALF_WIDTH >= bounds.min_x - 0.001
                && origin.x + HALF_WIDTH <= bounds.max_x + 0.001
                && origin.z - HALF_DEPTH >= bounds.min_z - 0.001
                && origin.z + HALF_DEPTH <= bounds.max_z + 0.001,
            "{department}'s 4.6 x 3.6 m dressing envelope leaves {room}: {origin} in {bounds:?}",
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
    assert_eq!(count, 20, "the station starter kit should contain 20 GLBs");
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
        "chemist_start",
        "department_spot",
        "department_dressing",
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
    for expected in ["MAINTENANCE", "CHAPEL", "QUARANTINE", "ATMOS / UTILITY"] {
        assert_eq!(
            sign_text
                .iter()
                .filter(|text| text.as_str() == expected)
                .count(),
            1,
            "public sign `{expected}` should agree with one named walkable room",
        );
    }
    assert_eq!(
        sign_text
            .iter()
            .filter(|text| text.as_str() == "MAINTENANCE ACCESS")
            .count(),
        2,
        "both loop entrances need a maintenance access sign",
    );

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
fn the_map_still_places_every_machine() {
    // The drift this whole approach risks: delete a machine spot in TrenchBroom
    // and nothing notices until a shift starts without a Mixing Chamber.
    // Positions are deliberately unchecked — moving one is the point of using
    // an editor.
    let map = std::fs::read_to_string(MAP).unwrap_or_else(|err| panic!("reading {MAP}: {err}"));

    for kind in MachineKind::ALL {
        assert!(
            map.contains(&format!("\"kind\" \"{kind:?}\"")),
            "{kind:?} has no machine_spot in {MAP}",
        );
    }

    assert!(
        map.contains("\"classname\" \"chemist_start\""),
        "{MAP} has nowhere for a chemist to start",
    );
    assert!(
        map.contains("\"classname\" \"worldspawn\""),
        "{MAP} has no worldspawn, so it has no world",
    );
}
