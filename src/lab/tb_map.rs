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

use crate::machines::MachineKind;

const MAP: &str = "assets/maps/lab.map";

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
fn the_map_still_places_every_machine() {
    // The drift this whole approach risks: delete a machine spot in TrenchBroom
    // and nothing notices until a shift starts without a ChemMaster. Positions
    // are deliberately unchecked — moving one is the point of using an editor.
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
