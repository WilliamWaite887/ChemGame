//! Walking from one part of the station to another.
//!
//! A portal graph over [`WalkableAreas`]: every standable rectangle is a node,
//! two nodes are joined when they overlap, and the waypoint for crossing between
//! them is the middle of the floor they share. That is the whole idea, and it
//! works because the floor plan is already a set of rectangles — rooms, doorway
//! bridges, stretches of corridor — rather than arbitrary geometry.
//!
//! The connectivity half of this used to live inside
//! `lab::tests::every_room_can_be_walked_to_from_the_spawn_point`, which
//! flood-filled the same overlaps to prove no room was sealed off. The test was
//! right about the shape of the problem; it just had nowhere to put the answer.
//!
//! Nothing here replicates. `crew::walk_route` runs on the authority only and
//! clients receive the resulting `Transform`, so a path is server-side
//! scratch work that never crosses the wire.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use bevy::prelude::*;

use crate::lab::{Bounds, FloorProfile, MapReady, WalkableAreas};
use crate::AppState;

/// The body radius paths are planned for.
///
/// One graph serves everyone, so this is the *widest* thing that walks — the
/// chemist at 0.35, rather than the 0.28 crew capsule. Planning for the widest
/// means a route is never too tight for whoever takes it; the cost is that a
/// crew member gives doorframes a few more centimetres than they need.
pub const NAV_RADIUS: f32 = 0.35;

/// Maximum mismatch between two surfaces at a portal. Stair runs meet their
/// flat landings within this tolerance; stacked decks remain separate.
const MAX_PORTAL_STEP: f32 = 0.45;

/// How long a body may cover no ground before [`ProgressWatch`] calls it wedged.
const STALL_WINDOW: f32 = 2.0;

/// How much route has to be walked off inside that window to count as headway.
///
/// Deliberately far below anything a walking body covers — the slowest crew
/// member the chemistry can produce still manages a third of a metre a second —
/// so this fires for bodies that are genuinely getting nowhere and not for ones
/// merely having a bad day.
const STALL_PROGRESS: f32 = 0.2;

/// How far a recovery point has to be from the body it is meant to rescue.
///
/// Walking to a spot you are already standing on unwedges nothing, so a
/// candidate nearer than this is skipped in favour of the next one out.
const MIN_RECOVERY_STEP: f32 = 0.75;

/// A walkable rectangle and the ways out of it.
struct Node {
    bounds: Bounds,
    profile_bounds: Bounds,
    floor: FloorProfile,
    edges: Vec<Edge>,
    /// The doorway this region *is*, if it is a doorway bridge rather than a
    /// room — mirrors `lab::Region::bridge_id`. `None` for an ordinary room
    /// or corridor. Read by [`Self::acoustic_path`] to know which crossings
    /// along a route have a `Door` to ask about.
    bridge_id: Option<String>,
}

impl Node {
    fn floor_at(&self, point: Vec3) -> f32 {
        self.floor.height_at(self.profile_bounds, point)
    }
}

struct Edge {
    to: usize,
    /// Middle of the floor the two regions share: where a body crosses over.
    portal: Vec3,
}

/// What [`NavGraph::acoustic_path`] found between a speaker and a listener
/// with no clear line between them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AcousticPath {
    /// Physical distance along the route, portal to portal plus the final
    /// leg into the listener's region — not straight-line.
    pub length: f32,
    /// How many doorway/room boundaries the route crosses.
    pub portals: usize,
    /// The last portal before the listener: where an occluded voice should
    /// sound like it is coming from, not the speaker's true position, which
    /// the listener may not even be able to see.
    pub last_portal: Vec3,
    /// How many of those crossings were through a **closed** door.
    pub muffled_doors: usize,
}

/// The station's walkable regions, joined up.
///
/// Rebuilt whenever [`WalkableAreas`] changes, which in practice means once,
/// when the floor plan is seeded or the map finishes loading.
#[derive(Resource, Default)]
pub struct NavGraph {
    nodes: Vec<Node>,
}

impl NavGraph {
    /// Builds the graph from the walkable floor, held clear of the walls by
    /// `radius`.
    pub fn build(areas: &WalkableAreas, radius: f32) -> Self {
        // Inset first, then join. Doing it the other way round would connect two
        // regions that only touch along an edge a body cannot actually fit
        // through — a path that exists on paper and wedges someone in a doorway.
        let mut nodes: Vec<Node> = areas
            .regions()
            .iter()
            .filter_map(|region| {
                let bounds = region.bounds.inset(radius);
                bounds.is_standable().then_some(Node {
                    bounds,
                    profile_bounds: region.open_bounds,
                    floor: region.floor,
                    edges: Vec::new(),
                    bridge_id: region.bridge_id.clone(),
                })
            })
            .collect();

        for i in 0..nodes.len() {
            for j in (i + 1)..nodes.len() {
                let Some(shared) = nodes[i].bounds.intersection(&nodes[j].bounds) else {
                    continue;
                };
                let mut portal = shared.center();
                let a_y = nodes[i].floor_at(portal);
                let b_y = nodes[j].floor_at(portal);
                if (a_y - b_y).abs() > MAX_PORTAL_STEP {
                    continue;
                }
                portal.y = (a_y + b_y) * 0.5;
                nodes[i].edges.push(Edge { to: j, portal });
                nodes[j].edges.push(Edge { to: i, portal });
            }
        }

        Self { nodes }
    }

    /// The region a point genuinely stands in, if any.
    fn held_by(&self, point: Vec3) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.bounds.holds(point))
            .min_by(|(_, a), (_, b)| {
                (a.floor_at(point) - point.y)
                    .abs()
                    .total_cmp(&(b.floor_at(point) - point.y).abs())
            })
            .map(|(index, _)| index)
    }

    /// The region nearest a point that is not in one.
    fn nearest_to(&self, point: Vec3) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let a_xz = a.bounds.nearest(point);
                let b_xz = b.bounds.nearest(point);
                let a = Vec3::new(a_xz.x, a.floor_at(a_xz), a_xz.z).distance_squared(point);
                let b = Vec3::new(b_xz.x, b.floor_at(b_xz), b_xz.z).distance_squared(point);
                a.total_cmp(&b)
            })
            .map(|(index, _)| index)
    }

    /// The region a point is in, or failing that the nearest one — and which
    /// of the two it was.
    ///
    /// The fallback matters: crew spawn outside the station and walk in, and a
    /// body nudged a few centimetres into a wall by a collision still has to be
    /// able to ask for a route home. But it used to be *silent*, and a caller
    /// that cannot tell "standing here" from "nearest to here" will happily
    /// emit a waypoint outside the walkable floor and walk a body through a
    /// wall to reach it. Every caller now has to look at the `bool`.
    fn locate_or_nearest(&self, point: Vec3) -> Option<(usize, bool)> {
        if let Some(index) = self.held_by(point) {
            return Some((index, true));
        }
        self.nearest_to(point).map(|index| (index, false))
    }

    /// A point pulled onto floor the given region can actually stand on.
    fn standable_in(&self, node: usize, point: Vec3) -> Vec3 {
        self.nodes[node].bounds.nearest(point)
    }

    /// Waypoints from `from` to `to`, ending on `to`.
    ///
    /// `None` when the two are in parts of the station with no route between
    /// them, or when the graph has not been built yet. Callers should wait or
    /// stop; walking straight to the destination can cross station walls.
    pub fn path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        let (start, start_held) = self.locate_or_nearest(from)?;
        let (goal, goal_held) = self.locate_or_nearest(to)?;
        let body_offset = from.y - self.nodes[start].floor_at(from);
        // The goal is deliberately *not* pulled onto walkable floor. Leaving
        // the station is a real destination outside it — that final leg out
        // through the door is what despawns a crew member — so clamping here
        // would strand every leaver at the threshold. Keeping a body on the
        // floor is `crew::walk_route`'s job, which knows which leg it is on;
        // this function only says where to walk.
        let normalized_goal = Vec3::new(to.x, self.nodes[goal].floor_at(to) + body_offset, to.z);
        // Only a short-circuit when both ends are genuinely inside the same
        // region: a rectangle is convex, so a straight line between two points
        // it holds stays inside it. Two points that merely *fall back* to the
        // same region prove nothing about the floor between them.
        if start == goal && start_held && goal_held {
            return Some(vec![normalized_goal]);
        }

        // Dijkstra over regions, measuring the walk portal-to-portal rather than
        // centre-to-centre. Centres would send a body into the middle of a room
        // it is only passing through the corner of.
        let mut cheapest = vec![f32::INFINITY; self.nodes.len()];
        let start_floor = Vec3::new(from.x, self.nodes[start].floor_at(from), from.z);
        let mut entered_at = vec![start_floor; self.nodes.len()];
        let mut came_from: Vec<Option<(usize, Vec3)>> = vec![None; self.nodes.len()];
        let mut queue = BinaryHeap::new();

        cheapest[start] = 0.0;
        queue.push(Step {
            cost: 0.0,
            node: start,
        });

        while let Some(Step { cost, node }) = queue.pop() {
            if node == goal {
                break;
            }
            if cost > cheapest[node] {
                continue;
            }

            for edge in &self.nodes[node].edges {
                let walked = cost + entered_at[node].distance(edge.portal);
                if walked < cheapest[edge.to] {
                    cheapest[edge.to] = walked;
                    entered_at[edge.to] = edge.portal;
                    came_from[edge.to] = Some((node, edge.portal));
                    queue.push(Step {
                        cost: walked,
                        node: edge.to,
                    });
                }
            }
        }

        if cheapest[goal].is_infinite() {
            return None;
        }

        let mut waypoints = vec![normalized_goal];
        let mut current = goal;
        while let Some((previous, portal)) = came_from[current] {
            waypoints.push(portal);
            current = previous;
            if current == start {
                break;
            }
        }
        waypoints.reverse();
        let portals = waypoints.len().saturating_sub(1);
        for waypoint in &mut waypoints[..portals] {
            waypoint.y += body_offset;
        }
        // A body that is not standing anywhere yet — crew spawn outside the
        // station, by the door — gets an explicit leg onto the floor before
        // the route proper. Without it the first waypoint is a portal deep
        // inside the building and the walk there is a straight line through
        // the outside wall.
        if !start_held {
            let entry = self.standable_in(start, from);
            let entry = Vec3::new(
                entry.x,
                self.nodes[start].floor_at(entry) + body_offset,
                entry.z,
            );
            if waypoints
                .first()
                .is_none_or(|first| first.distance(entry) > 0.01)
            {
                waypoints.insert(0, entry);
            }
        }
        Some(waypoints)
    }

    /// How far a sound travels between two points that do not have a clear
    /// line between them, following the same rooms-and-doorways graph a body
    /// would walk.
    ///
    /// Deliberately its own method rather than a `path()` variant: `path()`
    /// measures a body's footsteps and pulls the goal onto standable floor
    /// "close enough" to walk to; this measures a sound's travel and must
    /// not — a listener standing right at a doorway threshold is a real
    /// position sound has to reach exactly, not one to be nudged off of.
    ///
    /// `door_open(bridge_id)` lets the caller supply live `Door` state
    /// without this module reaching into the ECS itself — the same reason
    /// `build`'s own `radius` is a plain parameter rather than a query. A
    /// caller should only call this after finding the direct line blocked;
    /// it does not check that itself, so calling it on a clear line just
    /// costs a graph search for no reason.
    pub fn acoustic_path(
        &self,
        from: Vec3,
        to: Vec3,
        door_open: impl Fn(&str) -> bool,
    ) -> Option<AcousticPath> {
        let start = self.held_by(from).or_else(|| self.nearest_to(from))?;
        let goal = self.held_by(to).or_else(|| self.nearest_to(to))?;

        if start == goal {
            // Occluded by something other than a wall between two nav
            // regions — a pillar inside one big room, say. Still a real
            // distance; just not one this graph has portals for.
            return Some(AcousticPath {
                length: from.distance(to),
                portals: 0,
                last_portal: to,
                muffled_doors: 0,
            });
        }

        // Same Dijkstra `path()` runs — portal-to-portal physical distance —
        // reused rather than duplicated, because a sound and a body should
        // agree on what "the way there" means. What differs is entirely in
        // what gets read back out of it below.
        let mut cheapest = vec![f32::INFINITY; self.nodes.len()];
        let mut entered_at = vec![from; self.nodes.len()];
        let mut came_from: Vec<Option<usize>> = vec![None; self.nodes.len()];
        let mut queue = BinaryHeap::new();

        cheapest[start] = 0.0;
        queue.push(Step {
            cost: 0.0,
            node: start,
        });

        while let Some(Step { cost, node }) = queue.pop() {
            if node == goal {
                break;
            }
            if cost > cheapest[node] {
                continue;
            }
            for edge in &self.nodes[node].edges {
                let walked = cost + entered_at[node].distance(edge.portal);
                if walked < cheapest[edge.to] {
                    cheapest[edge.to] = walked;
                    entered_at[edge.to] = edge.portal;
                    came_from[edge.to] = Some(node);
                    queue.push(Step {
                        cost: walked,
                        node: edge.to,
                    });
                }
            }
        }

        if cheapest[goal].is_infinite() {
            return None;
        }

        let mut portals = 0usize;
        let mut muffled_doors = 0usize;
        let mut current = goal;
        while let Some(previous) = came_from[current] {
            portals += 1;
            if let Some(bridge_id) = self.nodes[current].bridge_id.as_deref() {
                if !door_open(bridge_id) {
                    muffled_doors += 1;
                }
            }
            if previous == start {
                break;
            }
            current = previous;
        }

        Some(AcousticPath {
            // Portal-to-portal to the goal *node*, plus the last, short leg
            // from that node's entry portal to the listener's exact position
            // inside it — `path()` never needs this last leg because its
            // waypoints already end wherever the caller wants; a scalar
            // total has nowhere else to put it.
            length: cheapest[goal] + entered_at[goal].distance(to),
            portals,
            last_portal: entered_at[goal],
            muffled_doors,
        })
    }

    /// A destination moved onto floor the route to it actually ends on.
    ///
    /// The counterpart to [`NavGraph::path`] leaving the goal alone, and the
    /// answer to why it can: a caller that is *not* walking off the station
    /// deliberately has to say so, and gets a goal it can stand on.
    ///
    /// This is where a destination authored a few centimetres inside a wall or
    /// a counter stops being fatal. `path` faithfully returns such a point as
    /// the final waypoint, containment then holds the body off it, and it
    /// presses at the edge forever without ever coming within
    /// `ARRIVE_EPSILON` — a crew member who never arrives, with an order that
    /// times out at a window they are standing a metre from.
    ///
    /// Only the horizontal is touched. `path` normalizes height itself, from
    /// the region the walker starts in.
    pub fn standable_goal(&self, to: Vec3) -> Vec3 {
        let Some((node, held)) = self.locate_or_nearest(to) else {
            return to;
        };
        if held {
            return to;
        }
        // The node the route ends in, not the nearest floor outright: clamping
        // against the whole station could pull a goal just inside a wall
        // through to the room on the other side of it.
        let on_floor = self.standable_in(node, to);
        Vec3::new(on_floor.x, to.y, on_floor.z)
    }

    /// Open floor to send a wedged body to before it tries its goal again.
    ///
    /// The escape hatch for the failure a portal graph cannot see: the graph
    /// says a route exists and it does, but the body walking it is pinned by
    /// `contain_on_surface` against a wall it is trying to walk through — a
    /// corner cut too fine, a destination authored a few centimetres inside a
    /// counter — and heads straight back at the same waypoint every frame,
    /// forever. Nothing about the route is wrong, so replanning it changes
    /// nothing; what breaks the deadlock is walking somewhere *else* first.
    ///
    /// The candidates are the middle of the region they are in and the middles
    /// of the regions next door — points as far from any wall as that part of
    /// the station allows, which is exactly what a body stuck against one
    /// needs. `attempt` walks outward through them, so a body that wedges
    /// again after being rescued is not handed the same useless point twice.
    pub fn recovery_point(&self, from: Vec3, attempt: usize) -> Option<Vec3> {
        let (node, _) = self.locate_or_nearest(from)?;
        let body_offset = from.y - self.nodes[node].floor_at(from);
        let mut candidates: Vec<Vec3> = std::iter::once(node)
            .chain(self.nodes[node].edges.iter().map(|edge| edge.to))
            .map(|index| {
                let center = self.nodes[index].bounds.center();
                Vec3::new(
                    center.x,
                    self.nodes[index].floor_at(center) + body_offset,
                    center.z,
                )
            })
            .filter(|point| flat_distance(*point, from) >= MIN_RECOVERY_STEP)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by(|a, b| flat_distance(*a, from).total_cmp(&flat_distance(*b, from)));
        Some(candidates[attempt % candidates.len()])
    }

    /// The candidate reachable on foot by the shortest *route*, and how far a
    /// body would actually walk to reach it.
    ///
    /// Deliberately not "the nearest by straight line". The two disagree
    /// exactly where it matters: a beaker a metre away through a wall is a
    /// twenty-metre walk around, and picking it by proximity sends a body
    /// pressing hopelessly against the partition. Candidates with no route at
    /// all are skipped rather than returned with an infinite cost, so an empty
    /// return means "nowhere to go" and never "somewhere unreachable".
    pub fn nearest_reachable<T>(
        &self,
        from: Vec3,
        candidates: impl IntoIterator<Item = (T, Vec3)>,
    ) -> Option<(T, f32)> {
        candidates
            .into_iter()
            .filter_map(|(item, at)| {
                let path = self.path(from, at)?;
                Some((item, floor_length(from, &path)))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
    }
}

/// Distance between two points on the floor plane.
pub(crate) fn flat_distance(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

/// Length of a route, measured on the floor plane.
///
/// Height is dropped on purpose: a stair's rise is not distance a body has to
/// spend, and counting it would bias every comparison against routes that
/// happen to change deck.
pub(crate) fn floor_length(from: Vec3, path: &[Vec3]) -> f32 {
    let mut previous = from;
    let mut total = 0.0;
    for waypoint in path {
        let mut leg = *waypoint - previous;
        leg.y = 0.0;
        total += leg.length();
        previous = *waypoint;
    }
    total
}

// ---------------------------------------------------------------------------
// Getting nowhere
// ---------------------------------------------------------------------------

/// Watches a body that is walking a route and reports when it stops getting
/// anywhere.
///
/// The measure is *route left to walk*, not distance travelled and not distance
/// to the next waypoint. Distance travelled says a body scraping sideways along
/// a wall is fine; distance to the next waypoint jumps up every time one is
/// reached, so a long leg would read as a stall. Route left to walk only ever
/// falls, and a body that is not making it fall is not going anywhere,
/// whichever way it happens to be facing while it does so.
pub struct ProgressWatch {
    /// Route left when the current window opened.
    mark: f32,
    /// Seconds left in that window.
    window: f32,
}

impl Default for ProgressWatch {
    fn default() -> Self {
        // An infinite mark means the first observation always counts as
        // headway, so a fresh watch never accuses a body that has only just
        // started walking.
        Self {
            mark: f32::INFINITY,
            window: STALL_WINDOW,
        }
    }
}

impl ProgressWatch {
    /// Starts the window again, forgetting whatever the body was doing.
    ///
    /// For every moment the measure stops being comparable with itself: a new
    /// route (longer than the old one, through no fault of the walker) and a
    /// body that has stopped walking for a legitimate reason and will resume.
    pub fn restart(&mut self) {
        *self = Self::default();
    }

    /// Whether a whole window has passed without the route getting shorter.
    ///
    /// Reporting `true` also opens a fresh window, so a caller that cannot act
    /// on the answer is told again a window later rather than every frame.
    pub fn stalled(&mut self, dt: f32, remaining: f32) -> bool {
        if self.mark - remaining >= STALL_PROGRESS {
            self.mark = remaining;
            self.window = STALL_WINDOW;
            return false;
        }
        self.window -= dt;
        if self.window > 0.0 {
            return false;
        }
        self.mark = remaining;
        self.window = STALL_WINDOW;
        true
    }
}

// ---------------------------------------------------------------------------
// Walking a route
// ---------------------------------------------------------------------------

/// A cached portal route and the walking of it.
///
/// Extracted from `showdown::Pursuit`, which had the only copy: a path, an
/// index into it, and a repath clock, plus the loop that follows waypoints
/// while staying on the walkable floor. `crate::crew::Errand` needs all four
/// and none of the hitting, so the four move here.
///
/// [`crate::crew::CrewRoute`] deliberately does **not** ride on this. It walks
/// to a destination fixed the moment it is set, through `walk_route`'s phases,
/// lanes and department fallbacks; a `Trail` re-plans toward something that can
/// move or vanish. Merging the two would mean one struct with two disjoint
/// halves and a flag saying which is live.
#[derive(Default)]
pub struct Trail {
    path: Vec<Vec3>,
    waypoint: usize,
    /// Seconds until the goal is sampled again and the route rebuilt.
    replan_in: f32,
}

impl Trail {
    /// Whether the replan cadence has come round, ticking its clock.
    ///
    /// A fresh `Trail` starts at zero, so the first call always says yes and
    /// nothing has to plan an opening route by hand.
    pub fn due_for_replan(&mut self, dt: f32, every: f32) -> bool {
        self.replan_in -= dt;
        if self.replan_in > 0.0 {
            return false;
        }
        self.replan_in = every;
        true
    }

    /// Replaces the cached route.
    ///
    /// An unreachable goal — or a graph that has not finished building —
    /// leaves the route *empty*, which [`Trail::walk`] then treats as "do not
    /// move". Never a straight line: that fallback is precisely how bodies
    /// used to walk through station walls.
    pub fn plan(&mut self, nav: Option<&NavGraph>, from: Vec3, to: Vec3) {
        self.waypoint = 0;
        self.path = nav
            .and_then(|graph| graph.path(from, to))
            .unwrap_or_default();
    }

    /// No route: nowhere to go, and nothing should move.
    pub fn is_empty(&self) -> bool {
        self.path.is_empty()
    }

    /// Where the cached route finishes, if there is one.
    pub fn end(&self) -> Option<Vec3> {
        self.path.last().copied()
    }

    /// Whether the cached route actually ends on `point`, on the floor plane.
    ///
    /// The guard against acting on a stale route: a goal that has moved since
    /// the last replan leaves a path ending somewhere it no longer is, and a
    /// body that treats "I reached the end of my route" as "I reached my
    /// target" acts on thin air.
    pub fn ends_at(&self, point: Vec3) -> bool {
        self.end().is_some_and(|end| {
            Vec2::new(end.x - point.x, end.z - point.z).length_squared() <= 0.0001
        })
    }

    /// Length of the unwalked part, on the floor plane.
    pub fn remaining(&self, from: Vec3) -> Option<f32> {
        if self.path.is_empty() {
            return None;
        }
        Some(floor_length(
            from,
            &self.path[self.waypoint.min(self.path.len())..],
        ))
    }

    /// Walks up to `distance` along the route, kept on the walkable floor.
    ///
    /// Returns the direction of the last step taken, for a caller that wants
    /// to face its body that way — `None` when nothing moved. Containment is
    /// applied per step rather than per frame for the same reason
    /// `crew::walk_route` does it: a long step across a doorway can pass
    /// through geometry the endpoints are both clear of.
    pub fn walk_avoiding(
        &mut self,
        transform: &mut Transform,
        areas: Option<&WalkableAreas>,
        distance: f32,
        body_offset: f32,
        mut motion: Option<(Entity, &mut crate::npc_motion::NpcMotion)>,
    ) -> Option<Vec3> {
        let mut remaining = distance;
        let mut heading = None;
        while remaining > 0.0 {
            if let Some(areas) = areas {
                if self.path.last().is_some_and(|last| {
                    flat_distance(transform.translation, *last) < 0.02
                        && crate::npc_motion::walkable_segment(
                            areas,
                            transform.translation,
                            *last,
                            body_offset,
                        )
                }) {
                    self.waypoint = self.path.len();
                }
                while self.path.get(self.waypoint + 1).is_some_and(|next| {
                    crate::npc_motion::walkable_segment(
                        areas,
                        transform.translation,
                        *next,
                        body_offset,
                    )
                }) {
                    self.waypoint += 1;
                }
            }
            let Some(waypoint) = self.path.get(self.waypoint).copied() else {
                break;
            };
            // Horizontal, for the reason `crew::walk_route` spells out at
            // length: `contain_on_surface` rewrites y every step, so a body
            // cannot close a vertical gap, and a portal joining two floors up
            // to [`MAX_PORTAL_STEP`] apart carries exactly such a gap into its
            // waypoint. Measured in 3D, an errand-runner or a pursuer standing
            // on the portal in XZ never reaches it and walks on the spot until
            // its deadline writes it off.
            let step = waypoint - transform.translation;
            let waypoint_distance = Vec2::new(step.x, step.z).length();
            if waypoint_distance <= 0.02
                && areas.is_none_or(|areas| {
                    crate::npc_motion::walkable_segment(
                        areas,
                        transform.translation,
                        self.path
                            .get(self.waypoint + 1)
                            .copied()
                            .unwrap_or(waypoint),
                        body_offset,
                    )
                })
            {
                self.waypoint += 1;
                continue;
            }
            if waypoint_distance <= f32::EPSILON {
                // An invalid onward portal needs replanning, not a zero-length
                // step that divides by zero or loops without using distance.
                break;
            }
            let walked = remaining.min(waypoint_distance).min(0.12);
            let direction = Vec3::new(step.x, 0.0, step.z) / waypoint_distance;
            let candidate = transform.translation + direction * walked;
            transform.translation = if let Some((entity, motion)) = motion.as_mut() {
                motion.advance(
                    *entity,
                    transform.translation,
                    waypoint,
                    *self.path.last().unwrap_or(&waypoint),
                    walked,
                    areas,
                    body_offset,
                )
            } else {
                areas.map_or(candidate, |areas| {
                    areas.contain_on_surface(candidate, NAV_RADIUS, body_offset)
                })
            };
            if motion
                .as_ref()
                .is_some_and(|(entity, m)| m.waiting(*entity))
            {
                return (transform.translation.distance_squared(candidate) < walked * walked)
                    .then_some(direction);
            }
            heading = Some(direction);
            remaining -= walked;
            // `walked` is the requested distance. Containment or avoidance
            // may have moved the body elsewhere, so only the actual-position
            // arrival check on the next iteration can consume the waypoint.
        }
        heading
    }
}

/// A node waiting to be expanded, cheapest first.
///
/// `BinaryHeap` is a max-heap, so the ordering is deliberately reversed. `f32`
/// is only `PartialOrd`, hence `total_cmp` — and hence writing this by hand
/// rather than deriving it.
struct Step {
    cost: f32,
    node: usize,
}

impl Ord for Step {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| self.node.cmp(&other.node))
    }
}

impl PartialOrd for Step {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Step {
    fn eq(&self, other: &Self) -> bool {
        self.cost == other.cost && self.node == other.node
    }
}

impl Eq for Step {}

pub struct NavPlugin;

impl Plugin for NavPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NavGraph>().add_systems(
            Update,
            rebuild_graph
                .run_if(resource_exists_and_changed::<WalkableAreas>)
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// Rebuilds the graph when the floor plan changes.
///
/// Runs on both ends. It is derived data, identical either side, and a client
/// that ever wants to reason about the station's shape should not have to ask
/// the server for something it can work out from the map it already loaded.
fn rebuild_graph(mut commands: Commands, areas: Res<WalkableAreas>, mut graph: ResMut<NavGraph>) {
    *graph = NavGraph::build(&areas, NAV_RADIUS);
    if areas.regions().is_empty() {
        commands.remove_resource::<MapReady>();
    } else {
        // Deferred until after this system completes, so every system gated on
        // the marker observes the finished graph, never a half-loaded map.
        commands.insert_resource(MapReady);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab::{ROOMS, SPAWN_SPOT};

    fn lab_graph() -> NavGraph {
        NavGraph::build(&WalkableAreas::from_floor_plan(), NAV_RADIUS)
    }

    #[test]
    fn clamped_final_step_does_not_consume_an_unreached_waypoint() {
        let mut areas = WalkableAreas::default();
        areas.push(
            Bounds {
                min_x: 0.0,
                max_x: 1.0,
                min_z: 0.0,
                max_z: 1.0,
            },
            None,
        );
        // A target moved behind the floor boundary after this route was
        // cached. The requested last step is 0.1 m, but containment permits
        // only 0.05 m. A stale route must remain visibly unfinished.
        let target = Vec3::new(0.70, 0.93, 0.5);
        let mut trail = Trail {
            path: vec![target],
            ..Default::default()
        };
        let mut at = Transform::from_xyz(0.60, 0.93, 0.5);
        for _ in 0..3 {
            trail.walk_avoiding(&mut at, Some(&areas), 0.12, 0.93, None);
            assert_eq!(trail.waypoint, 0);
            assert!(trail.remaining(at.translation).unwrap() > 0.04);
        }
        assert!((at.translation.x - 0.65).abs() < 0.001);
    }

    #[test]
    fn every_room_can_be_reached_from_the_spawn_point() {
        // The check that used to live in `lab::tests`, now asserting against the
        // graph crew actually walk rather than a copy of the algorithm kept
        // alongside it. A room the graph cannot reach is a room no NPC will ever
        // visit, however open the doorway looks.
        let graph = lab_graph();

        for room in &ROOMS {
            let path = graph
                .path(SPAWN_SPOT, room.center())
                .unwrap_or_else(|| panic!("no route from the spawn point to {}", room.name));
            assert!(
                !path.is_empty(),
                "the route to {} has no waypoints",
                room.name
            );
        }
    }

    #[test]
    fn every_waypoint_is_somewhere_a_body_can_stand() {
        // A portal in the middle of a wall is the characteristic failure of this
        // kind of graph, and it looks like an NPC walking into a doorframe and
        // stopping. Every waypoint must sit inside the walkable floor, inset for
        // the body being routed.
        let areas = WalkableAreas::from_floor_plan();
        let graph = NavGraph::build(&areas, NAV_RADIUS);

        let inset: Vec<Bounds> = areas
            .regions()
            .iter()
            .map(|region| region.bounds.inset(NAV_RADIUS))
            .filter(|bounds| bounds.is_standable())
            .collect();

        for room in &ROOMS {
            let path = graph.path(SPAWN_SPOT, room.center()).expect("a route");
            for waypoint in path {
                assert!(
                    inset.iter().any(|bounds| bounds.holds(waypoint)),
                    "waypoint {waypoint:?} on the way to {} is inside a wall",
                    room.name,
                );
            }
        }
    }

    #[test]
    fn a_route_across_the_suite_goes_through_the_rooms_between() {
        // Reaction bay to analysis is the longest walk in the lab: west room,
        // through a door, the length of the hall, through another door. A
        // straight line between them crosses two walls, so a single-waypoint
        // answer would mean the graph had quietly given up and gone direct.
        let graph = lab_graph();
        let from = ROOMS[crate::lab::REACTION_BAY].center();
        let to = ROOMS[crate::lab::ANALYSIS].center();

        let path = graph.path(from, to).expect("a route across the suite");
        assert!(
            path.len() >= 3,
            "expected a route through the hall, got {path:?}",
        );
    }

    #[test]
    fn a_recovery_point_is_open_floor_a_body_is_not_already_standing_on() {
        // What a wedged body is asking for: somewhere with room around it,
        // far enough away to actually be a walk. A recovery point inside a
        // wall would replace one trap with another, and one under the body's
        // own feet would be no rescue at all.
        let areas = WalkableAreas::from_floor_plan();
        let graph = NavGraph::build(&areas, NAV_RADIUS);
        let inset: Vec<Bounds> = areas
            .regions()
            .iter()
            .map(|region| region.bounds.inset(NAV_RADIUS))
            .filter(|bounds| bounds.is_standable())
            .collect();

        for room in &ROOMS {
            // A corner of the room, which is where bodies actually wedge.
            let corner = Vec3::new(room.min_x + NAV_RADIUS, 0.0, room.min_z + NAV_RADIUS);
            let recovery = graph
                .recovery_point(corner, 0)
                .unwrap_or_else(|| panic!("nowhere to send a body stuck in {}", room.name));

            assert!(
                inset.iter().any(|bounds| bounds.holds(recovery)),
                "{}: recovery point {recovery:?} is inside a wall",
                room.name,
            );
            assert!(
                flat_distance(recovery, corner) >= MIN_RECOVERY_STEP,
                "{}: recovery point is under the body's own feet",
                room.name,
            );
        }
    }

    #[test]
    fn a_second_attempt_is_sent_somewhere_the_first_one_was_not() {
        // A body that wedges again after being rescued has already proved the
        // first point did not help. Handing it the same one forever is how a
        // stuck NPC becomes a pacing one.
        let graph = lab_graph();
        let from = ROOMS[crate::lab::LOBBY].center();

        let first = graph.recovery_point(from, 0).expect("a first candidate");
        let second = graph.recovery_point(from, 1).expect("a second candidate");
        assert!(
            first.distance(second) > 0.01,
            "both attempts sent the body to {first:?}",
        );
    }

    #[test]
    fn progress_is_only_a_stall_when_the_route_stops_getting_shorter() {
        let mut watch = ProgressWatch::default();
        // Walking: the route shrinks by a stride every tick.
        let mut remaining = 20.0;
        for _ in 0..200 {
            remaining -= 0.1;
            assert!(
                !watch.stalled(0.05, remaining),
                "a body covering ground was called stuck at {remaining:.2}m left",
            );
        }

        // Wedged: the route stops shrinking, and one window later it is
        // reported — once, not every frame after.
        let stuck_at = remaining;
        let ticks = (STALL_WINDOW / 0.05).ceil() as usize;
        let reports = (0..ticks + 1)
            .filter(|_| watch.stalled(0.05, stuck_at))
            .count();
        assert_eq!(reports, 1, "expected exactly one report per window");
    }

    #[test]
    fn an_empty_floor_plan_routes_nowhere_rather_than_panicking() {
        // The map backend has a frame or two before the scene has loaded, and
        // crew ask for routes on their first update.
        let graph = NavGraph::build(&WalkableAreas::default(), NAV_RADIUS);
        assert!(graph.path(SPAWN_SPOT, ROOMS[0].center()).is_none());
    }

    #[test]
    fn readiness_is_published_only_after_a_nonempty_graph_is_built() {
        let mut app = App::new();
        app.init_resource::<WalkableAreas>()
            .init_resource::<NavGraph>()
            .add_systems(Update, rebuild_graph);

        app.update();
        assert!(
            !app.world().contains_resource::<MapReady>(),
            "an empty loading layout was advertised as ready",
        );

        let mut areas = WalkableAreas::default();
        areas.push(
            Bounds {
                min_x: -2.0,
                max_x: 2.0,
                min_z: -2.0,
                max_z: 2.0,
            },
            Some("Test Room".to_string()),
        );
        app.world_mut().insert_resource(areas);
        app.update();

        assert!(app.world().contains_resource::<MapReady>());
        assert_eq!(app.world().resource::<NavGraph>().nodes.len(), 1);
    }
}
