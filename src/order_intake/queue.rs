//! Map-authored greeting positions and a separate, ordered collection line.
use super::{AcceptedOrder, PendingOrder};
use crate::{
    crew::{AtCounter, CrewMember, CrewRoute},
    lab::{DeliveryLane, DeliveryStations, MapReady},
    nav::NavGraph,
    orders::Order,
    AppState,
};
use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct QueuePaths {
    pub public: Vec<Vec3>,
    pub medical: Vec<Vec3>,
    pub clearances: Vec<(Vec3, f32)>,
}

#[derive(Resource, Default)]
pub(crate) struct PreparedQueues {
    lanes: [Vec<Vec3>; 2],
}

#[derive(Component, Clone, Copy, Debug)]
pub struct QueuePosition {
    pub target: Vec3,
    pub reached: bool,
    pub pickup: bool,
}

pub struct OrderQueuePlugin;
impl Plugin for OrderQueuePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<QueuePaths>()
            .init_resource::<PreparedQueues>()
            .add_systems(
                Update,
                assign_positions
                    .after(super::accept_orders)
                    .before(crate::crew::walk_route)
                    .run_if(crate::net::is_authority)
                    .run_if(in_state(AppState::Playing))
                    .run_if(resource_exists::<MapReady>),
            );
        #[cfg(feature = "trenchbroom")]
        app.add_systems(
            Update,
            collect_paths
                .before(assign_positions)
                .run_if(resource_exists::<MapReady>),
        );
    }
}

#[cfg(feature = "trenchbroom")]
fn collect_paths(
    mut paths: ResMut<QueuePaths>,
    points: Query<(&crate::lab::tb::QueuePoint, &Transform)>,
) {
    if points.is_empty() {
        return;
    }
    let clearances: Vec<_> = points
        .iter()
        .filter(|(p, _)| p.clearance > 0.0)
        .map(|(p, t)| (t.translation, p.clearance))
        .collect();
    if paths.clearances != clearances {
        paths.clearances = clearances;
    }
    let mut ordered: Vec<_> = points.iter().filter(|(p, _)| p.lane == "public").collect();
    ordered.sort_by_key(|(p, _)| p.sequence);
    let next: Vec<_> = ordered.iter().map(|(_, t)| t.translation).collect();
    if paths.public != next {
        paths.public = next;
    }
    let mut ordered: Vec<_> = points.iter().filter(|(p, _)| p.lane == "medical").collect();
    ordered.sort_by_key(|(p, _)| p.sequence);
    let next: Vec<_> = ordered.iter().map(|(_, t)| t.translation).collect();
    if paths.medical != next {
        paths.medical = next;
    }
}

/// Uses distance along the bent line, never an x-offset that can land in a wall.
pub fn point_along(points: &[Vec3], mut distance: f32) -> Option<Vec3> {
    for pair in points.windows(2) {
        let length = pair[0].distance(pair[1]);
        if distance <= length {
            return Some(pair[0].lerp(pair[1], distance / length.max(0.0001)));
        }
        distance -= length;
    }
    if distance <= 0.001 {
        points.last().copied()
    } else {
        None
    }
}

pub(crate) fn navigable_line(anchors: &[Vec3], nav: &NavGraph) -> Vec<Vec3> {
    let mut line = Vec::new();
    for pair in anchors.windows(2) {
        let from = nav.standable_goal(pair[0]);
        let to = nav.standable_goal(pair[1]);
        if line.is_empty() {
            line.push(from);
        }
        if nav.path(from, to).is_none() {
            return Vec::new();
        }
        line.push(to);
    }
    line
}

/// One metre along the authored route, with additional clearance at sharp
/// corners and beside the greeting positions. Never clamp excess customers
/// onto the last position.
pub(crate) fn standing_points(
    line: &[Vec3],
    greetings: &[Vec3],
    clearances: &[(Vec3, f32)],
) -> Vec<Vec3> {
    let mut result: Vec<Vec3> = Vec::new();
    let mut distance = 0.0;
    while let Some(point) = point_along(line, distance) {
        let clear = result
            .iter()
            .chain(greetings)
            .all(|p| p.distance(point) >= 0.85)
            && clearances
                .iter()
                .all(|(at, radius)| at.distance(point) >= *radius);
        if clear {
            result.push(point);
            distance += 1.0;
        } else {
            distance += 0.1;
        }
    }
    result
}

#[allow(clippy::type_complexity)]
fn assign_positions(
    mut commands: Commands,
    paths: Res<QueuePaths>,
    stations: Res<DeliveryStations>,
    nav: Res<NavGraph>,
    mut prepared: ResMut<PreparedQueues>,
    mut people: Query<
        (
            Entity,
            &Transform,
            &mut CrewRoute,
            Option<&PendingOrder>,
            Option<&AcceptedOrder>,
            Option<&mut QueuePosition>,
        ),
        With<CrewMember>,
    >,
    stale: Query<Entity, (With<QueuePosition>, Without<PendingOrder>, Without<Order>)>,
) {
    for entity in &stale {
        commands.entity(entity).remove::<QueuePosition>();
    }
    for (entity, _, route, _, _, place) in &mut people {
        if place.is_some() && route.phase == crate::crew::CrewPhase::Leaving {
            commands.entity(entity).remove::<QueuePosition>();
        }
    }
    for (lane_index, lane) in [DeliveryLane::Public, DeliveryLane::Medical]
        .into_iter()
        .enumerate()
    {
        let station = stations.station(lane);
        let authored = if lane == DeliveryLane::Public {
            &paths.public
        } else {
            &paths.medical
        };
        // The compact no-map fallback still has a real line, oriented to its window.
        let fallback = [
            station.queue_position(2.0),
            station.queue_position(2.0) - station.transform.rotation * Vec3::Z * 50.0,
        ];
        if paths.is_changed()
            || stations.is_changed()
            || nav.is_changed()
            || prepared.lanes[lane_index].is_empty()
        {
            let line = navigable_line(
                if authored.is_empty() {
                    &fallback
                } else {
                    authored
                },
                &nav,
            );
            let greetings = [
                nav.standable_goal(station.queue_position(0.0)),
                nav.standable_goal(station.queue_position(1.0)),
            ];
            prepared.lanes[lane_index] = standing_points(&line, &greetings, &paths.clearances);
        }
        let mut waiting: Vec<_> = people
            .iter()
            .filter(|(_, _, r, p, _, _)| r.delivery_lane == lane && p.is_some())
            .map(|(e, _, _, p, _, _)| (e, p.unwrap().context.id))
            .collect();
        let mut accepted: Vec<_> = people
            .iter()
            .filter(|(_, _, r, _, a, _)| r.delivery_lane == lane && a.is_some())
            .map(|(e, _, _, _, a, _)| (e, a.unwrap().sequence))
            .collect();
        accepted.retain(|(e, _)| {
            people
                .get(*e)
                .is_ok_and(|(_, _, r, _, _, _)| r.phase != crate::crew::CrewPhase::Leaving)
        });
        waiting.sort_by_key(|(_, id)| *id);
        accepted.sort_by_key(|(_, id)| *id);
        let mut greeting_places = std::collections::HashMap::new();
        for (entity, _) in &waiting {
            if let Ok((_, _, _, _, _, Some(place))) = people.get(*entity) {
                if !place.pickup {
                    greeting_places.insert(*entity, place.target);
                }
            }
        }
        for (entity, _) in &waiting {
            if greeting_places.contains_key(entity) {
                continue;
            }
            if let Some(target) = (0..2)
                .map(|slot| nav.standable_goal(station.queue_position(slot as f32)))
                .find(|target| {
                    greeting_places
                        .values()
                        .all(|taken| taken.distance(*target) > 0.5)
                })
            {
                greeting_places.insert(*entity, target);
            }
        }
        for (pickup, list) in [(false, waiting), (true, accepted)] {
            for (index, (entity, _)) in list.iter().enumerate() {
                let raw = if pickup {
                    prepared.lanes[lane_index].get(index).copied()
                } else {
                    greeting_places.get(entity).copied()
                };
                let Some(raw) = raw else {
                    continue;
                };
                let target = nav.standable_goal(raw);
                let Ok((_, at, mut route, _, _, existing)) = people.get_mut(*entity) else {
                    continue;
                };
                if route.phase == crate::crew::CrewPhase::Leaving {
                    continue;
                }
                let reached = crate::nav::flat_distance(at.translation, target) < 0.20;
                let changed = existing
                    .as_ref()
                    .is_none_or(|p| p.target.distance_squared(target) > 0.01 || p.pickup != pickup);
                if changed {
                    route.queue_to(target, lane);
                    commands
                        .entity(*entity)
                        .remove::<AtCounter>()
                        .insert(QueuePosition {
                            target,
                            reached,
                            pickup,
                        });
                } else if let Some(mut existing) = existing {
                    existing.reached = reached;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_follows_a_corner_instead_of_stacking_at_the_last_slot() {
        let path = [Vec3::ZERO, Vec3::X * 2.0, Vec3::new(2.0, 0.0, 20.0)];
        assert_eq!(point_along(&path, 5.0), Some(Vec3::new(2.0, 0.0, 3.0)));
        let slots: Vec<_> = (0..20)
            .map(|i| point_along(&path, i as f32).unwrap())
            .collect();
        for (i, a) in slots.iter().enumerate() {
            for b in slots.iter().skip(i + 1) {
                assert!(a.distance(*b) >= 0.99);
            }
        }
        assert!(point_along(&path, 24.0).is_none());
    }

    #[test]
    fn long_pickup_line_forms_through_the_door_and_middle_customer_can_leave() {
        exercise_long_line(DeliveryLane::Public);
    }

    #[test]
    fn long_medical_line_forms_and_middle_customer_can_leave() {
        exercise_long_line(DeliveryLane::Medical);
    }

    fn exercise_long_line(lane: DeliveryLane) {
        let areas = crate::lab::tb_map::authored_walkable_areas();
        let nav = NavGraph::build(&areas, crate::nav::NAV_RADIUS);
        let paths = crate::lab::tb_map::authored_queue_paths();
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<DeliveryStations>()
            .init_resource::<PreparedQueues>()
            .init_resource::<crate::npc_motion::NpcMotion>()
            .insert_resource(nav)
            .insert_resource(areas)
            .insert_resource(paths)
            .add_systems(
                Update,
                (
                    crate::npc_motion::snapshot,
                    assign_positions,
                    crate::crew::walk_route,
                )
                    .chain(),
            );
        let mut customers = Vec::new();
        for i in 0..14 {
            customers.push(
                app.world_mut()
                    .spawn((
                        CrewMember {
                            name: format!("Customer {i}"),
                            role: if lane == DeliveryLane::Public {
                                "Service"
                            } else {
                                "Medical"
                            }
                            .into(),
                        },
                        Transform::from_xyz(
                            (if lane == DeliveryLane::Public {
                                11.0
                            } else {
                                -16.0
                            }) - i as f32 * 1.1,
                            crate::crew::BODY_OFFSET,
                            12.0,
                        ),
                        CrewRoute::arrival_for(lane, 0.0),
                        super::super::tests::sample_order(),
                        AcceptedOrder { sequence: i },
                    ))
                    .id(),
            );
        }
        let advance = |app: &mut App, frames: usize| {
            for _ in 0..frames {
                app.world_mut()
                    .resource_mut::<Time>()
                    .advance_by(std::time::Duration::from_millis(100));
                app.update();
                let positions: Vec<_> = app
                    .world_mut()
                    .query::<&Transform>()
                    .iter(app.world())
                    .map(|t| t.translation)
                    .collect();
                for (i, a) in positions.iter().enumerate() {
                    for b in positions.iter().skip(i + 1) {
                        assert!(
                            a.distance(*b) >= 0.70,
                            "NPC bodies overlapped: {a:?} and {b:?}"
                        );
                    }
                }
            }
        };
        advance(&mut app, 1400);
        let unreached: Vec<_> = customers
            .iter()
            .filter_map(|e| {
                let p = app.world().get::<QueuePosition>(*e)?;
                (!p.reached).then_some((
                    *e,
                    app.world().get::<Transform>(*e).unwrap().translation,
                    p.target,
                    app.world()
                        .resource::<crate::npc_motion::NpcMotion>()
                        .diagnostic(*e, app.world().resource::<crate::lab::WalkableAreas>()),
                ))
            })
            .collect();
        assert!(
            unreached.is_empty(),
            "customers never reached their queue places: {unreached:?}"
        );
        assert!(customers
            .iter()
            .any(|e| app.world().get::<Transform>(*e).unwrap().translation.z > 10.0));
        let departing = customers.remove(5);
        app.world_mut()
            .entity_mut(departing)
            .remove::<(Order, AcceptedOrder)>();
        let mut route = app.world_mut().get_mut::<CrewRoute>(departing).unwrap();
        *route = CrewRoute::to(Vec3::new(
            if lane == DeliveryLane::Public {
                -15.0
            } else {
                -40.0
            },
            0.0,
            12.0,
        ));
        route.phase = crate::crew::CrewPhase::Leaving;
        advance(&mut app, 1000);
        assert!(
            app.world().get_entity(departing).is_err(),
            "middle customer was trapped behind the line"
        );
        assert!(customers
            .iter()
            .all(|e| app.world().get::<QueuePosition>(*e).unwrap().reached));
    }
}
