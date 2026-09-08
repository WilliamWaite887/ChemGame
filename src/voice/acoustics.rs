//! Deciding how loud, and from where, one chemist's voice reaches another —
//! walls, corners and closed doors.
//!
//! Pure and ECS-free on purpose, the same way `lab::resting_place` takes
//! `solids: &[(Vec3, Vec3)]` rather than a `Query`: audibility's interesting
//! cases (a door swinging shut mid-sentence, a voice arriving from around a
//! corner) are exactly the kind that are miserable to reproduce against a
//! live map and trivial to write down as a test once the geometry is plain
//! data. `net::relay_voice_frames` is the one caller, and it supplies the
//! occluders and the door lookup from the real world.
//!
//! # Attenuation ownership
//!
//! Split deliberately so nothing downstream ever attenuates the same
//! distance twice:
//! - **Bevy owns the visible segment** — panning and distance falloff from
//!   [`Audibility::emitter`] to the listener's own ears, free from
//!   `PlaybackSettings::with_spatial(true)`.
//! - **This module owns everything Bevy cannot see** — the speaker-to-emitter
//!   leg, folded into [`Audibility::gain`]; and the character of what is in
//!   the way, folded into [`Audibility::muffle`].
//!
//! So a voice coming around a corner is attenuated here for the hidden leg
//! up to the doorway, and by Bevy for the visible leg from the doorway to the
//! ear — never both for the same metre.

use bevy::prelude::*;

use crate::interaction::authority_segment_blocked;
use crate::nav::NavGraph;

use super::PROXIMITY_RANGE;

/// Muffle added per ordinary portal (room/corridor boundary) a route crosses,
/// door state aside. A voice down a corridor and through two open doorways
/// should read as more distant than one heard straight through a single open
/// doorway, even with no closed door anywhere on the route.
const MUFFLE_PER_PORTAL: f32 = 0.12;

/// *Additional* muffle per **closed** door on the route, on top of
/// [`MUFFLE_PER_PORTAL`] for that same crossing.
const MUFFLE_PER_CLOSED_DOOR: f32 = 0.4;

/// Muffle never reaches 1.0 — a closed door and three rooms of corridor are
/// meant to sound heavily muffled, never perfectly silent. Distance alone
/// (the range cull below) is what actually stops a voice being sent at all;
/// muffle only ever shapes one that is still being sent.
const MAX_MUFFLE: f32 = 0.92;

/// Floor under [`Audibility::gain`] for the same reason: a route this module
/// found *at all* is a route Bevy will still play something for, however
/// distant, rather than a hard cut a route existing failed to produce.
const MIN_GAIN: f32 = 0.05;

/// Where a listener should hear a voice from, how loud, and how muffled —
/// computed for one (speaker, listener) pair. `None` means out of range
/// entirely: nothing is sent, which is also the privacy floor — a client
/// never in range never receives the audio at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Audibility {
    pub emitter: Vec3,
    pub gain: f32,
    pub muffle: f32,
}

/// The full Phase 1 → Phase 2 decision, cheapest test first — mirroring
/// `speech::place_bubbles`'s own cheapest-first gates:
///
/// 1. Distance cull beyond [`PROXIMITY_RANGE`] (plus no margin — a voice
///    right at the edge is a voice right at the edge).
/// 2. A clear line ⇒ full gain, no muffle, `emitter` is the speaker.
/// 3. Otherwise the portal graph, if one is given and a route exists ⇒
///    attenuated by the hidden leg, muffled by what the route crosses,
///    `emitter` is the last portal before the listener.
/// 4. No graph, or no route ⇒ not audible. This is the honest answer for two
///    points in unconnected parts of the station, and — before a `NavGraph`
///    has ever been built — the safe one: nothing else here can tell whether
///    an unbuilt graph means "not occluded" or "totally disconnected", so it
///    is never treated as the former.
pub fn compute_audibility(
    speaker: Vec3,
    listener: Vec3,
    occluders: &[(Vec3, Vec3)],
    nav: Option<&NavGraph>,
    door_open: impl Fn(&str) -> bool,
) -> Option<Audibility> {
    if !speaker.is_finite() || !listener.is_finite() {
        return None;
    }
    if speaker.distance_squared(listener) > PROXIMITY_RANGE * PROXIMITY_RANGE {
        return None;
    }

    let blocked = occluders
        .iter()
        .any(|&(center, half_extents)| authority_segment_blocked(speaker, listener, center, half_extents));
    if !blocked {
        return Some(Audibility {
            emitter: speaker,
            gain: 1.0,
            muffle: 0.0,
        });
    }

    let nav = nav?;
    let path = nav.acoustic_path(speaker, listener, door_open)?;

    // Only the hidden leg — speaker up to the last portal — is this
    // module's to attenuate; the portal-to-listener leg is `emitter`'s job,
    // left to Bevy. `path.length` is the *whole* route, so the visible leg
    // is subtracted back out here rather than asked of the graph again.
    let visible_leg = path.last_portal.distance(listener);
    let hidden_leg = (path.length - visible_leg).max(0.0);
    let gain = (1.0 - hidden_leg / PROXIMITY_RANGE).clamp(MIN_GAIN, 1.0);

    let muffle = (path.portals as f32 * MUFFLE_PER_PORTAL
        + path.muffled_doors as f32 * MUFFLE_PER_CLOSED_DOOR)
        .min(MAX_MUFFLE);

    Some(Audibility {
        emitter: path.last_portal,
        gain,
        muffle,
    })
}

#[cfg(test)]
mod tests {
    use crate::lab::{Bounds, WalkableAreas};
    use crate::nav::{NavGraph, NAV_RADIUS};

    use super::*;

    /// Two rooms joined by one doorway bridge.
    ///
    /// `NavGraph::build` insets every region by `NAV_RADIUS` *before*
    /// checking which ones overlap — so adjacent regions have to overlap by
    /// more than `2 * NAV_RADIUS` in the raw, un-inset bounds, or they go
    /// from touching to disjoint the moment they are inset and the portal
    /// between them never forms. The real map's `func_walkable` volumes are
    /// authored to overlap for exactly this reason; this fixture mirrors it
    /// rather than placing rooms edge-to-edge the way it would look natural
    /// to on paper.
    ///
    /// `room_a`: x -6..0, z -3..3. `bridge`: x -1..1, z -1..1,
    /// `bridge_id: "door"`. `room_b`: x 0..6, z -3..3.
    fn two_rooms_one_door() -> NavGraph {
        let mut areas = WalkableAreas::default();
        areas.push(
            Bounds {
                min_x: -6.0,
                max_x: 0.0,
                min_z: -3.0,
                max_z: 3.0,
            },
            Some("room_a".to_string()),
        );
        areas.push_with_bridge(
            Bounds {
                min_x: -1.0,
                max_x: 1.0,
                min_z: -1.0,
                max_z: 1.0,
            },
            None,
            Some("door".to_string()),
        );
        areas.push(
            Bounds {
                min_x: 0.0,
                max_x: 6.0,
                min_z: -3.0,
                max_z: 3.0,
            },
            Some("room_b".to_string()),
        );
        NavGraph::build(&areas, NAV_RADIUS)
    }

    /// A wall standing exactly on the doorway, so any straight line between
    /// the two rooms below is genuinely blocked and the portal graph is what
    /// has to answer instead.
    fn wall_across_the_doorway() -> Vec<(Vec3, Vec3)> {
        vec![(Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.05, 2.0, 3.0))]
    }

    const ALWAYS_OPEN: fn(&str) -> bool = |_| true;
    const ALWAYS_CLOSED: fn(&str) -> bool = |_| false;

    #[test]
    fn beyond_proximity_range_nothing_is_sent_at_all() {
        let far = Vec3::new(PROXIMITY_RANGE + 1.0, 0.0, 0.0);
        let result = compute_audibility(Vec3::ZERO, far, &[], None, ALWAYS_OPEN);
        assert!(result.is_none(), "out of range must not be sent, not just quiet");
    }

    #[test]
    fn a_clear_line_is_full_volume_from_the_speakers_own_position() {
        let speaker = Vec3::new(-2.0, 1.0, 0.0);
        let listener = Vec3::new(2.0, 1.0, 0.0);
        let audibility = compute_audibility(speaker, listener, &[], None, ALWAYS_OPEN)
            .expect("in range with nothing blocking it");
        assert_eq!(audibility.emitter, speaker);
        assert_eq!(audibility.gain, 1.0);
        assert_eq!(audibility.muffle, 0.0);
    }

    #[test]
    fn a_bench_between_two_speakers_does_not_muffle() {
        // Not `AcousticOccluder`-filtered here — this pure function only
        // ever sees what its caller already filtered to. The behaviour under
        // test is that furniture passed in as an occluder still only blocks
        // when it geometrically blocks; the *filtering itself* is
        // `lab::tb::only_real_walls_can_occlude_a_voice`'s job, proven at the
        // texture level, not this function's.
        let speaker = Vec3::new(-2.0, 1.0, 0.0);
        let listener = Vec3::new(2.0, 1.0, 0.0);
        let bench_out_of_the_way = vec![(Vec3::new(10.0, 1.0, 10.0), Vec3::new(1.0, 1.0, 1.0))];
        let audibility = compute_audibility(speaker, listener, &bench_out_of_the_way, None, ALWAYS_OPEN)
            .expect("the bench is nowhere near the line between them");
        assert_eq!(audibility.muffle, 0.0);
    }

    #[test]
    fn without_a_nav_graph_a_blocked_line_is_not_audible() {
        // Before the graph has ever been built. Treated as "not audible"
        // rather than "clear" — the safe direction to be wrong in.
        let speaker = Vec3::new(-2.0, 1.0, 0.0);
        let listener = Vec3::new(2.0, 1.0, 0.0);
        let occluders = wall_across_the_doorway();
        assert!(compute_audibility(speaker, listener, &occluders, None, ALWAYS_OPEN).is_none());
    }

    #[test]
    fn a_voice_through_a_closed_door_is_muffled_but_never_silent() {
        let nav = two_rooms_one_door();
        let speaker = Vec3::new(-2.0, 0.0, 0.0);
        let listener = Vec3::new(2.0, 0.0, 0.0);
        let occluders = wall_across_the_doorway();

        let audibility = compute_audibility(speaker, listener, &occluders, Some(&nav), ALWAYS_CLOSED)
            .expect("a closed door is muffled, never absent — the Bounds::EMPTY trap this guards");
        assert!(audibility.muffle > 0.0, "a closed door must muffle something");
        assert!(
            audibility.muffle < MAX_MUFFLE + f32::EPSILON,
            "muffle must stay short of total silence"
        );
        assert!(audibility.gain >= MIN_GAIN);
    }

    #[test]
    fn the_same_route_through_an_open_door_is_audibly_louder_than_closed() {
        let nav = two_rooms_one_door();
        let speaker = Vec3::new(-2.0, 0.0, 0.0);
        let listener = Vec3::new(2.0, 0.0, 0.0);
        let occluders = wall_across_the_doorway();

        let open = compute_audibility(speaker, listener, &occluders, Some(&nav), ALWAYS_OPEN)
            .expect("a route exists");
        let closed = compute_audibility(speaker, listener, &occluders, Some(&nav), ALWAYS_CLOSED)
            .expect("a route exists");

        assert!(
            open.muffle < closed.muffle,
            "open {} should muffle less than closed {}",
            open.muffle,
            closed.muffle
        );
    }

    #[test]
    fn an_occluded_voice_is_heard_from_the_doorway_not_through_the_wall() {
        let nav = two_rooms_one_door();
        let speaker = Vec3::new(-2.0, 0.0, 0.0);
        let listener = Vec3::new(2.0, 0.0, 0.0);
        let occluders = wall_across_the_doorway();

        let audibility = compute_audibility(speaker, listener, &occluders, Some(&nav), ALWAYS_OPEN)
            .expect("a route exists");
        assert_ne!(
            audibility.emitter, speaker,
            "the emitter must move to the doorway, not stay at the true position \
             the listener cannot see"
        );
        // The doorway's bridge region is centred on the origin.
        assert!(audibility.emitter.distance(Vec3::ZERO) < 1.5);
    }

    #[test]
    fn an_occluded_voice_never_gets_a_gain_boost_over_a_clear_line() {
        let nav = two_rooms_one_door();
        let speaker = Vec3::new(-2.0, 0.0, 0.0);
        let listener = Vec3::new(2.0, 0.0, 0.0);
        let occluders = wall_across_the_doorway();

        let occluded = compute_audibility(speaker, listener, &occluders, Some(&nav), ALWAYS_OPEN)
            .expect("a route exists");
        assert!(occluded.gain <= 1.0);
    }

    #[test]
    fn an_l_shaped_route_reports_the_last_of_its_two_portals() {
        // Three rooms in an L: a -> bridge_1 -> b -> bridge_2 -> c. The
        // emitter for a voice in `a` heard from `c` must be `bridge_2`'s
        // position, the doorway nearest the listener, not `bridge_1`'s.
        let mut areas = WalkableAreas::default();
        areas.push(
            Bounds { min_x: -6.0, max_x: 0.0, min_z: -3.0, max_z: 3.0 },
            Some("a".to_string()),
        );
        areas.push_with_bridge(
            Bounds { min_x: -1.0, max_x: 1.0, min_z: -1.0, max_z: 1.0 },
            None,
            Some("door_1".to_string()),
        );
        areas.push(
            Bounds { min_x: 0.0, max_x: 8.0, min_z: -3.0, max_z: 3.0 },
            Some("b".to_string()),
        );
        areas.push_with_bridge(
            Bounds { min_x: 6.0, max_x: 8.0, min_z: -1.0, max_z: 1.0 },
            None,
            Some("door_2".to_string()),
        );
        areas.push(
            Bounds { min_x: 7.0, max_x: 13.0, min_z: -3.0, max_z: 3.0 },
            Some("c".to_string()),
        );
        let nav = NavGraph::build(&areas, NAV_RADIUS);

        let speaker = Vec3::new(-3.0, 0.0, 0.0);
        let listener = Vec3::new(10.0, 0.0, 0.0);
        // A wall spanning the full route so no straight line is ever clear —
        // only the portal graph can answer this one.
        let occluders = vec![(Vec3::new(3.5, 1.0, 0.0), Vec3::new(6.0, 2.0, 0.1))];

        let audibility = compute_audibility(speaker, listener, &occluders, Some(&nav), ALWAYS_OPEN)
            .expect("a -> b -> c is a real route");
        assert!(
            audibility.emitter.distance(Vec3::new(7.0, audibility.emitter.y, 0.0)) < 1.5,
            "expected the second doorway (~x=7), got {:?}",
            audibility.emitter
        );
    }
}
