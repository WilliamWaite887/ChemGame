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

use crate::lab::{Bounds, WalkableAreas};

/// The body radius paths are planned for.
///
/// One graph serves everyone, so this is the *widest* thing that walks — the
/// chemist at 0.35, rather than the 0.28 crew capsule. Planning for the widest
/// means a route is never too tight for whoever takes it; the cost is that a
/// crew member gives doorframes a few more centimetres than they need.
pub const NAV_RADIUS: f32 = 0.35;

/// A walkable rectangle and the ways out of it.
struct Node {
    bounds: Bounds,
    edges: Vec<Edge>,
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
        let bounds: Vec<Bounds> = areas
            .regions()
            .iter()
            .map(|region| region.bounds.inset(radius))
            .filter(|bounds| bounds.is_standable())
            .collect();

        let mut nodes: Vec<Node> = bounds
            .iter()
            .map(|bounds| Node {
                bounds: *bounds,
                edges: Vec::new(),
            })
            .collect();

        for i in 0..bounds.len() {
            for j in (i + 1)..bounds.len() {
                let Some(shared) = bounds[i].intersection(&bounds[j]) else {
                    continue;
                };
                let portal = shared.center();
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
        if let Some(index) = self
            .nodes
            .iter()
            .position(|node| node.bounds.holds(point))
        {
            return Some(index);
        }

        self.nodes
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let a = a.bounds.nearest(point).distance_squared(point);
                let b = b.bounds.nearest(point).distance_squared(point);
                a.total_cmp(&b)
            })
            .map(|(index, _)| index)
    }

    /// Waypoints from `from` to `to`, ending on `to`.
    ///
    /// `None` when the two are in parts of the station with no route between
    /// them, or when the graph has not been built yet. Callers are expected to
    /// have a fallback rather than to strand whoever asked.
    pub fn path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        let start = self.locate(from)?;
        let goal = self.locate(to)?;
        if start == goal {
            return Some(vec![to]);
        }

        // Dijkstra over regions, measuring the walk portal-to-portal rather than
        // centre-to-centre. Centres would send a body into the middle of a room
        // it is only passing through the corner of.
        let mut cheapest = vec![f32::INFINITY; self.nodes.len()];
        let mut entered_at = vec![from; self.nodes.len()];
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

        let mut waypoints = vec![to];
        let mut current = goal;
        while let Some((previous, portal)) = came_from[current] {
            waypoints.push(portal);
            current = previous;
            if current == start {
                break;
            }
        }
        waypoints.reverse();
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
            rebuild_graph.run_if(resource_exists_and_changed::<WalkableAreas>),
        );
    }
}

/// Rebuilds the graph when the floor plan changes.
///
/// Runs on both ends. It is derived data, identical either side, and a client
/// that ever wants to reason about the station's shape should not have to ask
/// the server for something it can work out from the map it already loaded.
fn rebuild_graph(areas: Res<WalkableAreas>, mut graph: ResMut<NavGraph>) {
    *graph = NavGraph::build(&areas, NAV_RADIUS);
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
}
