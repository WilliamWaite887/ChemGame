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

use quake_map::Entity;

use crate::lab::{Bounds, WalkableAreas, TB_SCALE};
use crate::machines::MachineKind;

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
            let centroid = [centroid[0] / count, centroid[1] / count, centroid[2] / count];

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
        stranded, 0,
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
        "escape_pod",
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
