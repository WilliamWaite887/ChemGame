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
        for brush in &entity.brushes {
            areas.push_with_bridge(footprint(brush), room.clone(), bridge_id.clone());
        }
    }
    areas
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
            "Bridge Operations",
            Bounds {
                min_x: -83.5,
                max_x: -41.5,
                min_z: -9.0,
                max_z: -3.0,
            },
        ),
        (
            "Quarantine",
            Bounds {
                min_x: -38.7,
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
            "Atmos/Utility",
            Bounds {
                min_x: -66.0,
                max_x: -52.5,
                min_z: 15.0,
                max_z: 28.6,
            },
        ),
        (
            "Cargo Receiving",
            Bounds {
                min_x: -109.5,
                max_x: -94.0,
                min_z: 31.4,
                max_z: 51.0,
            },
        ),
        (
            "Chapel",
            Bounds {
                min_x: -38.7,
                max_x: -23.5,
                min_z: 31.4,
                max_z: 39.0,
            },
        ),
        (
            "Quiet Room",
            Bounds {
                min_x: -47.5,
                max_x: -41.5,
                min_z: 31.4,
                max_z: 39.0,
            },
        ),
        (
            "Botany Nursery",
            Bounds {
                min_x: -18.5,
                max_x: 13.5,
                min_z: 42.0,
                max_z: 51.0,
            },
        ),
    ] {
        let found = named_bounds(room);
        assert_eq!(found.len(), 1, "{room} should be one walkable rectangle");
        assert!(bounds_are_close(found[0], expected), "{room}: {found:?}");
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
            "Bridge",
            Bounds {
                min_x: -83.5,
                max_x: -41.5,
                min_z: -3.0,
                max_z: 10.0,
            },
        ),
        (
            "Medical",
            Bounds {
                min_x: -38.7,
                max_x: -16.5,
                min_z: -3.0,
                max_z: 10.0,
            },
        ),
        (
            "Engineering",
            Bounds {
                min_x: -109.5,
                max_x: -66.0,
                min_z: 15.0,
                max_z: 28.6,
            },
        ),
        (
            "Cargo",
            Bounds {
                min_x: -94.0,
                max_x: -52.5,
                min_z: 31.4,
                max_z: 51.0,
            },
        ),
    ] {
        let found = named_bounds(room);
        assert_eq!(found.len(), 1, "{room} should be one authored rectangle");
        assert!(bounds_are_close(found[0], expected), "{room}: {found:?}");
    }

    let service = named_bounds("Service");
    assert_eq!(
        service.len(),
        2,
        "the technical spine should divide Service"
    );
    for expected in [
        Bounds {
            min_x: -47.5,
            max_x: -41.5,
            min_z: 15.0,
            max_z: 28.6,
        },
        Bounds {
            min_x: -38.7,
            max_x: -23.5,
            min_z: 15.0,
            max_z: 28.6,
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
        2,
        "the maintenance spine should divide Botany"
    );
    for expected in [
        Bounds {
            min_x: -18.5,
            max_x: 13.5,
            min_z: 15.0,
            max_z: 28.6,
        },
        Bounds {
            min_x: -18.5,
            max_x: 13.5,
            min_z: 31.4,
            max_z: 42.0,
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
        6,
        "maintenance needs four ring and two cross-route segments"
    );
    for bounds in &maintenance {
        let width = (bounds.max_x - bounds.min_x).min(bounds.max_z - bounds.min_z);
        assert!(
            (width - 3.0).abs() < 0.001 || (width - 2.8).abs() < 0.001,
            "maintenance route is {width} m wide: {bounds:?}"
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
        ("Medical", "Medical", Vec3::new(-27.0, 0.0, 4.0), "0 180 0"),
        (
            "Engineering",
            "Engineering",
            Vec3::new(-80.0, 0.0, 18.0),
            "0 180 0",
        ),
        ("Cargo", "Cargo", Vec3::new(-80.0, 0.0, 35.0), "0 180 0"),
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

#[test]
fn chemistry_decoration_markers_have_known_assets_and_fit_their_rooms() {
    struct Placement {
        kind: &'static str,
        origin: &'static str,
        angles: &'static str,
        room: Bounds,
        width: f32,
        depth: f32,
    }

    const PLACEMENTS: &[Placement] = &[
        Placement {
            kind: "chem.supply_shelf",
            origin: "-176 296 0",
            angles: "0 90 0",
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
            kind: "chem.analysis_panel",
            origin: "104 -536 0",
            angles: "0 -90 0",
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
            room: Bounds {
                min_x: -16.25,
                max_x: -7.5,
                min_z: 1.5,
                max_z: 5.0,
            },
            width: 1.35,
            depth: 0.22,
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
        let bounds = match placement.angles {
            "0 0 0" => Bounds {
                min_x: x - half_width,
                max_x: x + half_width,
                min_z: z,
                max_z: z + placement.depth,
            },
            "0 90 0" => Bounds {
                min_x: x,
                max_x: x + placement.depth,
                min_z: z - half_width,
                max_z: z + half_width,
            },
            "0 180 0" => Bounds {
                min_x: x - half_width,
                max_x: x + half_width,
                min_z: z - placement.depth,
                max_z: z,
            },
            "0 -90 0" => Bounds {
                min_x: x - placement.depth,
                max_x: x,
                min_z: z - half_width,
                max_z: z + half_width,
            },
            other => panic!("unsupported decoration angle {other}"),
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
        visual_envelopes.push((placement.origin, bounds));
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
    ];
    let overlaps = |a: Bounds, b: Bounds| {
        a.min_x < b.max_x && a.max_x > b.min_x && a.min_z < b.max_z && a.max_z > b.min_z
    };
    for (origin, decoration) in visual_envelopes {
        assert!(
            blockers
                .iter()
                .all(|blocker| !overlaps(decoration, *blocker)),
            "decoration at {origin} overlaps a machine, working point, delivery window, or door",
        );
    }
}

#[test]
fn every_chemistry_decoration_kind_has_an_exported_glb() {
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
    assert_eq!(count, 24, "the station starter kit should contain 24 GLBs");
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
        "department_dressing",
        "decoration_spot",
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
fn physical_walls_match_walkable_routes_and_airlock_gaps() {
    let map = parse();
    let blocking_walls: Vec<Bounds> = map
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
        .map(|brush| footprint(brush))
        .collect();

    for area in map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
    {
        let standing = area.inset(NAV_RADIUS);
        if let Some(wall) = blocking_walls
            .iter()
            .find(|wall| wall.intersection(&standing).is_some())
        {
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
            blocking_walls.iter().all(|wall| !wall.holds(point)),
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
        "ENGINEERING / ATMOS",
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
    assert_eq!(sign_text.len(), 17, "stale or duplicate room signs remain");

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
    const EXPECTED: &[(&str, MachineKind, Vec3)] = &[
        (
            "dispenser.a",
            MachineKind::ChemMaster5000,
            Vec3::new(-5.4, 0.0, -5.05),
        ),
        (
            "mixer.a",
            MachineKind::MixingChamber,
            Vec3::new(-2.2, 0.0, -5.05),
        ),
        (
            "dispenser.b",
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
    // Bounds measured from department_chemistry_dressing.glb. The export is
    // render-only for navigation, but its fume hood and island are real visible
    // furniture: placing a machine through either still makes the workstation
    // unusable even though collision cannot catch the mistake.
    const FURNITURE_LOCAL: &[Bounds] = &[
        Bounds {
            min_x: -1.975,
            max_x: -0.525,
            min_z: -1.605,
            max_z: -0.9225,
        },
        Bounds {
            min_x: -0.7601,
            max_x: 1.9552,
            min_z: -0.1601,
            max_z: 0.7711,
        },
    ];
    const CORE_IDS: &[&str] = &["dispenser.a", "mixer.a", "dispenser.b", "mixer.b"];

    let overlaps = |a: Bounds, b: Bounds| {
        a.min_x < b.max_x && a.max_x > b.min_x && a.min_z < b.max_z && a.max_z > b.min_z
    };
    let map = parse();
    let dressing = map
        .iter()
        .find(|entity| {
            classname(entity).as_deref() == Some("department_dressing")
                && property(entity, "department").as_deref() == Some("Chemistry")
        })
        .expect("Chemistry dressing marker");
    assert_eq!(property(dressing, "angles").as_deref(), Some("0 0 0"));
    let (dress_x, dress_z) = origin_xz(dressing).expect("valid Chemistry dressing origin");
    let furniture: Vec<Bounds> = FURNITURE_LOCAL
        .iter()
        .map(|bounds| Bounds {
            min_x: bounds.min_x + dress_x,
            max_x: bounds.max_x + dress_x,
            min_z: bounds.min_z + dress_z,
            max_z: bounds.max_z + dress_z,
        })
        .collect();

    let hall = map
        .iter()
        .filter(|entity| classname(entity).as_deref() == Some("func_walkable"))
        .filter(|entity| property(entity, "room").as_deref() == Some("Mixing Hall"))
        .flat_map(|entity| entity.brushes.iter().map(|brush| footprint(brush)))
        .next()
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
            for occupied in &furniture {
                assert!(
                    !overlaps(bounds, *occupied),
                    "{id} {what} overlaps visible Chemistry furniture: {bounds:?} vs {occupied:?}",
                );
            }
        }
    }
}
