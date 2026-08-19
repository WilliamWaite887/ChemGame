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

/// A walkable rectangle and the ways out of it.
struct Node {
    bounds: Bounds,
    profile_bounds: Bounds,
    floor: FloorProfile,
    edges: Vec<Edge>,
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

    /// The region a point is in, or failing that the nearest one.
    ///
    /// The fallback matters: crew spawn outside the station and walk in, and a
    /// body nudged a few centimetres into a wall by a collision still has to be
    /// able to ask for a route home.
    fn locate(&self, point: Vec3) -> Option<usize> {
        if let Some((index, _)) = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.bounds.holds(point))
            .min_by(|(_, a), (_, b)| {
                (a.floor_at(point) - point.y)
                    .abs()
                    .total_cmp(&(b.floor_at(point) - point.y).abs())
            })
        {
            return Some(index);
        }

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

    /// Waypoints from `from` to `to`, ending on `to`.
    ///
    /// `None` when the two are in parts of the station with no route between
    /// them, or when the graph has not been built yet. Callers should wait or
    /// stop; walking straight to the destination can cross station walls.
    pub fn path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        let start = self.locate(from)?;
        let goal = self.locate(to)?;
        let body_offset = from.y - self.nodes[start].floor_at(from);
        let normalized_goal = Vec3::new(to.x, self.nodes[goal].floor_at(to) + body_offset, to.z);
        if start == goal {
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
        Some(waypoints)
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
