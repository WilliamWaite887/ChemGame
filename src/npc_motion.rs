//! Shared NPC body spacing. Player prediction and collisions remain separate.
use crate::{
    crew::{CrewMember, CrewPhase, CrewRoute},
    lab::WalkableAreas,
    AppState,
};
use bevy::prelude::*;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

pub const BODY_RADIUS: f32 = crate::nav::NAV_RADIUS;
const CLEARANCE: f32 = BODY_RADIUS * 2.0 + 0.02;

#[derive(Resource, Default)]
pub struct NpcMotion {
    bodies: HashMap<Entity, Vec3>,
    departing: HashSet<Entity>,
    waiting: HashSet<Entity>,
    detours: HashMap<Entity, Detour>,
    now: f64,
}

struct Detour {
    goal: Vec3,
    path: VecDeque<Vec3>,
    retry_at: f64,
}

pub struct NpcMotionPlugin;
impl Plugin for NpcMotionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NpcMotion>().add_systems(
            Update,
            snapshot
                .after(crate::crew::start_crew_at_their_department)
                .before(crate::crew::walk_route)
                .before(crate::crew::run_errands)
                .before(crate::showdown::run_pursuers)
                .run_if(crate::net::is_authority)
                .run_if(in_state(AppState::Playing)),
        );
    }
}

pub(crate) fn snapshot(
    mut motion: ResMut<NpcMotion>,
    areas: Option<Res<WalkableAreas>>,
    time: Option<Res<Time>>,
    mut crew: Query<(Entity, &mut Transform, Option<&CrewRoute>, Ref<CrewMember>)>,
) {
    motion.bodies.clear();
    motion.now = time.as_ref().map_or(0.0, |t| t.elapsed_secs_f64());
    motion.departing.clear();
    // Existing bodies retain their positions. A newly created visitor must
    // find an unoccupied patch of the same floor before it becomes visible.
    for (entity, at, _, member) in &crew {
        if !member.is_added() {
            motion.bodies.insert(entity, at.translation);
        }
    }
    let mut fresh: Vec<_> = crew
        .iter()
        .filter(|(_, _, _, m)| m.is_added())
        .map(|(e, ..)| e)
        .collect();
    fresh.sort();
    for entity in fresh {
        let Ok((_, mut at, _, _)) = crew.get_mut(entity) else {
            continue;
        };
        let origin = at.translation;
        let valid = |candidate: Vec3| {
            !motion
                .bodies
                .values()
                .any(|other| sweeps_body(candidate, candidate, *other))
                && areas.as_ref().is_none_or(|a| {
                    let on_floor =
                        a.contain_on_surface(candidate, BODY_RADIUS, crate::crew::BODY_OFFSET);
                    flat(on_floor - candidate).length() < 0.02
                        && (on_floor.y - candidate.y).abs() < 0.5
                })
        };
        if !valid(origin) {
            'search: for ring in 1..=14 {
                for step in 0..(ring * 12) {
                    let angle = step as f32 / (ring * 12) as f32 * std::f32::consts::TAU;
                    let candidate =
                        origin + Vec3::new(angle.cos(), 0.0, angle.sin()) * (ring as f32 * 0.8);
                    if valid(candidate) {
                        at.translation = candidate;
                        break 'search;
                    }
                }
            }
        }
        motion.bodies.insert(entity, at.translation);
    }
    for (entity, _, route, _) in &crew {
        if route.is_some_and(|r| r.phase == CrewPhase::Leaving) {
            motion.departing.insert(entity);
        }
    }
    let live: HashSet<_> = motion.bodies.keys().copied().collect();
    motion.waiting.retain(|e| live.contains(e));
    motion.detours.retain(|e, _| live.contains(e));
}

fn flat(v: Vec3) -> Vec2 {
    Vec2::new(v.x, v.z)
}

/// A waypoint is only a routing aid. If its successor is visible across
/// walkable floor, a passer can continue beside a body occupying that aid.
pub(crate) fn walkable_segment(areas: &WalkableAreas, from: Vec3, to: Vec3, offset: f32) -> bool {
    let steps = (flat(to - from).length() / 0.12).ceil().max(1.0) as usize;
    (0..=steps).all(|i| {
        let point = from.lerp(to, i as f32 / steps as f32);
        let confined = areas.contain_on_surface(point, BODY_RADIUS, offset);
        flat(confined - point).length() < 0.02 && (confined.y - point.y).abs() < 0.5
    })
}

/// Minimum separation over a swept step, not just at its endpoint.
fn sweeps_body(from: Vec3, to: Vec3, other: Vec3) -> bool {
    if (from.y - other.y).abs().min((to.y - other.y).abs()) > 1.2 {
        return false;
    }
    let segment = flat(to - from);
    let fraction = if segment.length_squared() > 0.000001 {
        (flat(other - from).dot(segment) / segment.length_squared()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (flat(from) + segment * fraction).distance_squared(flat(other)) < CLEARANCE * CLEARANCE
}

impl NpcMotion {
    #[cfg(test)]
    pub(crate) fn diagnostic(&self, entity: Entity, areas: &WalkableAreas) -> String {
        let from = self.bodies[&entity];
        let valid = walkable_segment(areas, from, from, crate::crew::BODY_OFFSET);
        let neighbours: Vec<_> = [Vec3::X, -Vec3::X, Vec3::Z, -Vec3::Z]
            .map(|v| {
                let to = from + v * 0.25;
                (
                    walkable_segment(areas, from, to, crate::crew::BODY_OFFSET),
                    self.bodies
                        .iter()
                        .filter(|(e, p)| **e != entity && sweeps_body(from, to, **p))
                        .map(|(_, p)| p.distance(from))
                        .collect::<Vec<_>>(),
                )
            })
            .into();
        self.detours.get(&entity).map_or_else(
            || "no detour".into(),
            |d| {
                format!(
                    "goal {:?}, path {}, floor {}, neighbours {:?}",
                    d.goal,
                    d.path.len(),
                    valid,
                    neighbours
                )
            },
        )
    }
    pub fn waiting(&self, entity: Entity) -> bool {
        self.waiting.contains(&entity)
    }

    /// Follow a short, collision-checked detour around a stationary group.
    /// Remembering the route prevents two equally good sidesteps from making
    /// an NPC oscillate at a doorway. The normal navigation goal is preserved.
    #[allow(clippy::too_many_arguments)]
    pub fn advance(
        &mut self,
        entity: Entity,
        from: Vec3,
        waypoint: Vec3,
        goal: Vec3,
        distance: f32,
        areas: Option<&WalkableAreas>,
        offset: f32,
    ) -> Vec3 {
        let mut detour = self.detours.remove(&entity);
        if detour
            .as_ref()
            .is_some_and(|d| flat(d.goal - goal).length() > 0.3)
        {
            detour = None;
        }
        if let Some(d) = detour.as_mut() {
            while d
                .path
                .front()
                .is_some_and(|p| flat(*p - from).length() < 0.12)
            {
                d.path.pop_front();
            }
        }
        let next = detour
            .as_ref()
            .and_then(|d| d.path.front())
            .copied()
            .unwrap_or(waypoint);
        let direction = flat(next - from).normalize_or_zero();
        let desired = from
            + Vec3::new(direction.x, 0.0, direction.y) * distance.min(flat(next - from).length());
        let result = self.step(entity, from, desired, areas, offset);
        let blocked = self.waiting(entity);
        if blocked && detour.as_ref().is_none_or(|d| self.now >= d.retry_at) {
            if let Some(areas) = areas {
                detour = Some(Detour {
                    goal,
                    path: self.find_detour(entity, result, goal, areas, offset),
                    retry_at: self.now + 0.75,
                });
            }
        }
        if let Some(d) = detour {
            if !d.path.is_empty() {
                self.waiting.insert(entity);
            }
            self.detours.insert(entity, d);
        }
        result
    }

    fn find_detour(
        &self,
        entity: Entity,
        from: Vec3,
        goal: Vec3,
        areas: &WalkableAreas,
        offset: f32,
    ) -> VecDeque<Vec3> {
        const GRID: f32 = 0.25;
        if (from.y - goal.y).abs() > 0.6 {
            return VecDeque::new();
        }
        let target = flat(goal - from) / GRID;
        let min_x = target.x.floor().min(0.0) as i32 - 10;
        let max_x = target.x.ceil().max(0.0) as i32 + 10;
        let min_z = target.y.floor().min(0.0) as i32 - 10;
        let max_z = target.y.ceil().max(0.0) as i32 + 10;
        let position = |(x, z): (i32, i32)| from + Vec3::new(x as f32 * GRID, 0.0, z as f32 * GRID);
        let clear = |a: Vec3, b: Vec3| {
            walkable_segment(areas, a, b, offset)
                && !self
                    .bodies
                    .iter()
                    .any(|(other, at)| *other != entity && sweeps_body(a, b, *at))
        };
        let heuristic = |key| (flat(position(key) - goal).length() * 100.0) as u32;
        let mut frontier = BinaryHeap::from([std::cmp::Reverse((
            heuristic((0, 0)),
            0_u32,
            (0_i32, 0_i32),
        ))]);
        let mut costs = HashMap::from([((0, 0), 0_u32)]);
        let mut came = HashMap::new();
        let mut examined = 0;
        while let Some(std::cmp::Reverse((_, cost, key))) = frontier.pop() {
            if costs.get(&key) != Some(&cost) {
                continue;
            }
            examined += 1;
            if examined > 5000 {
                break;
            }
            let at = position(key);
            if flat(at - goal).length() <= 0.4 && clear(at, goal) {
                let mut result = VecDeque::from([goal]);
                let mut current = key;
                while current != (0, 0) {
                    result.push_front(position(current));
                    current = came[&current];
                }
                return result;
            }
            // Stable neighbor order provides predictable yielding.
            for (dx, dz) in [
                (1, 0),
                (0, 1),
                (-1, 0),
                (0, -1),
                (1, 1),
                (-1, 1),
                (-1, -1),
                (1, -1),
            ] {
                let next = (key.0 + dx, key.1 + dz);
                if next.0 < min_x || next.0 > max_x || next.1 < min_z || next.1 > max_z {
                    continue;
                }
                let next_cost = cost + if dx != 0 && dz != 0 { 35 } else { 25 };
                if costs.get(&next).is_some_and(|c| *c <= next_cost) || !clear(at, position(next)) {
                    continue;
                }
                costs.insert(next, next_cost);
                came.insert(next, key);
                frontier.push(std::cmp::Reverse((
                    next_cost + heuristic(next),
                    next_cost,
                    next,
                )));
            }
        }
        VecDeque::new()
    }

    pub fn step(
        &mut self,
        entity: Entity,
        from: Vec3,
        desired: Vec3,
        areas: Option<&WalkableAreas>,
        body_offset: f32,
    ) -> Vec3 {
        self.bodies.insert(entity, from);
        self.waiting.remove(&entity);
        if !from.is_finite() || !desired.is_finite() {
            return from;
        }
        let travel = flat(desired - from);
        if travel.length_squared() < 0.000001 {
            return from;
        }
        let distance = travel.length();
        let direction = travel / distance;
        // Small steps preserve both floor containment and collision sweeps at low FPS.
        let steps = (distance / 0.12).ceil().max(1.0) as usize;
        let stride = distance / steps as f32;
        let mut position = from;
        let mut diverted = false;
        let mut crowd = false;
        for _ in 0..steps {
            let mut next = None;
            // Keep right when passing. Opposite headings choose opposite sides.
            for angle in [0.0_f32, -0.65, -1.15, 0.65, 1.15, -1.57, 1.57] {
                let d = Vec2::from_angle(angle).rotate(direction);
                let candidate = position + Vec3::new(d.x, 0.0, d.y) * stride;
                let candidate = areas.map_or(candidate, |a| {
                    a.contain_on_surface(candidate, BODY_RADIUS, body_offset)
                });
                if flat(candidate - position).length() < stride * 0.3 {
                    continue;
                }
                if flat(candidate - position).length() > stride * 1.05 {
                    continue;
                }
                let blocked = self.bodies.iter().any(|(&other, &at)| {
                    if other == entity {
                        return false;
                    }
                    // Existing bad spawn positions may only move toward greater separation.
                    let old = flat(position - at).length();
                    if old < CLEARANCE && (position.y - at.y).abs() < 1.2 {
                        return flat(candidate - at).length() <= old + 0.00001;
                    }
                    sweeps_body(position, candidate, at)
                });
                if blocked {
                    crowd = true;
                    continue;
                }
                // Give an outgoing body room to reach the doorway before entering.
                if !self.departing.contains(&entity)
                    && angle == 0.0
                    && self.bodies.iter().any(|(e, at)| {
                        self.departing.contains(e)
                            && flat(*at - position).length() < 1.2
                            && flat(candidate - *at).length() < flat(position - *at).length()
                    })
                {
                    crowd = true;
                    continue;
                }
                next = Some(candidate);
                diverted |= angle != 0.0;
                break;
            }
            match next {
                Some(at) => position = at,
                None => {
                    diverted = true;
                    break;
                }
            }
            self.bodies.insert(entity, position);
        }
        if diverted && crowd {
            self.waiting.insert(entity);
        }
        self.bodies.insert(entity, position);
        position
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sweep_detects_a_body_between_clear_endpoints() {
        assert!(sweeps_body(Vec3::ZERO, Vec3::X * 5.0, Vec3::X * 2.5));
        assert!(!sweeps_body(
            Vec3::ZERO,
            Vec3::X * 5.0,
            Vec3::new(2.5, 3.0, 0.0)
        ));
    }
    #[test]
    fn moving_characters_never_exchange_places_through_each_other() {
        let a = Entity::from_bits(1);
        let b = Entity::from_bits(2);
        let mut motion = NpcMotion::default();
        let mut left = Vec3::ZERO;
        let mut right = Vec3::X * 2.0;
        motion.bodies.insert(a, left);
        motion.bodies.insert(b, right);
        for _ in 0..30 {
            left = motion.step(a, left, left + Vec3::X * 0.2, None, 0.0);
            right = motion.step(b, right, right - Vec3::X * 0.2, None, 0.0);
            assert!(flat(right - left).length() >= CLEARANCE - 0.0001);
        }
        assert!(
            left.x > right.x,
            "passing must make progress, not just stop forever"
        );
    }

    #[test]
    fn outgoing_and_incoming_traffic_pass_at_low_frame_rates() {
        let mut areas = WalkableAreas::default();
        areas.push(
            crate::lab::Bounds {
                min_x: 0.0,
                max_x: 2.0,
                min_z: -5.0,
                max_z: 5.0,
            },
            None,
        );
        let mut motion = NpcMotion::default();
        let outgoing = Entity::from_bits(1);
        let incoming = Entity::from_bits(2);
        motion.departing.insert(outgoing);
        let mut a = Vec3::new(1.0, 0.93, -3.0);
        let mut b = Vec3::new(1.0, 0.93, 3.0);
        let end_a = b;
        let end_b = a;
        motion.bodies.insert(outgoing, a);
        motion.bodies.insert(incoming, b);
        for frame in 0..120 {
            motion.now = frame as f64 * 0.2;
            a = motion.advance(outgoing, a, end_a, end_a, 0.42, Some(&areas), 0.93);
            b = motion.advance(incoming, b, end_b, end_b, 0.42, Some(&areas), 0.93);
            assert!(a.distance(b) >= CLEARANCE - 0.001);
            for p in [a, b] {
                assert!((0.35..=1.65).contains(&p.x));
            }
        }
        assert!(
            a.z > 2.5 && b.z < -2.5,
            "opposing traffic deadlocked: {a:?} {b:?}"
        );
    }

    #[test]
    fn a_stationary_body_remains_an_obstacle_and_a_different_floor_is_free() {
        let a = Entity::from_bits(1);
        let body = Entity::from_bits(2);
        let mut motion = NpcMotion::default();
        let obstacle = Vec3::new(2.0, 0.93, 0.0);
        motion.bodies.insert(body, obstacle);
        let mut at = Vec3::new(0.0, 0.93, 0.0);
        for _ in 0..12 {
            at = motion.step(a, at, at + Vec3::X * 1.5, None, 0.93);
            assert!(at.distance(obstacle) >= CLEARANCE - 0.001);
            assert_eq!(motion.bodies[&body], obstacle);
        }
        let above = Vec3::new(0.0, 4.53, 0.0);
        assert!(
            motion
                .step(a, above, above + Vec3::X * 4.0, None, 0.93)
                .distance(above + Vec3::X * 4.0)
                < 0.0001
        );
    }
}
