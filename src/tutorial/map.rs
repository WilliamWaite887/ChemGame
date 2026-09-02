//! The fallback reads the same authored training map; it does not invent a second layout.
use bevy::prelude::*;
pub(crate) fn property(entity: &quake_map::Entity, key: &str) -> Option<String> {
    entity
        .edict
        .iter()
        .find(|(k, _)| k.to_string_lossy() == key)
        .map(|(_, v)| v.to_string_lossy().to_string())
}
pub(crate) fn parse() -> Vec<quake_map::Entity> {
    quake_map::parse(&mut include_bytes!("../../assets/maps/tutorial.map").as_slice())
        .expect("valid training map")
        .entities
}
pub(crate) fn origin(entity: &quake_map::Entity) -> Vec3 {
    let s = property(entity, "origin").unwrap_or_default();
    let v: Vec<f32> = s
        .split_whitespace()
        .map(|n| n.parse().expect("map coordinate"))
        .collect();
    assert_eq!(v.len(), 3);
    Vec3::new(-v[1], v[2], -v[0]) / crate::lab::TB_SCALE
}
pub(crate) fn bounds(brush: &[quake_map::Surface]) -> (Vec3, Vec3) {
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    for surface in brush {
        for p in surface.half_space {
            let v = Vec3::new(-p[1] as f32, p[2] as f32, -p[0] as f32) / crate::lab::TB_SCALE;
            low = low.min(v);
            high = high.max(v);
        }
    }
    (low, high)
}
pub(crate) fn areas(entities: &[quake_map::Entity]) -> crate::lab::WalkableAreas {
    let mut areas = crate::lab::WalkableAreas::default();
    for e in entities {
        if property(e, "classname").as_deref() == Some("func_walkable") {
            for brush in &e.brushes {
                let (low, high) = bounds(brush);
                areas.push(
                    crate::lab::Bounds {
                        min_x: low.x,
                        max_x: high.x,
                        min_z: low.z,
                        max_z: high.z,
                    },
                    property(e, "room"),
                );
            }
        }
    }
    areas
}
#[cfg(not(feature = "trenchbroom"))]
pub(super) fn spawn(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let entities = parse();
    let mut spots = super::TrainingSpots::default();
    let mut machines = crate::lab::MachineSpots::default();
    let mut paths = crate::order_intake::queue::QueuePaths::default();
    let wall = materials.add(Color::srgb(0.28, 0.35, 0.37));
    for e in &entities {
        match property(e, "classname").as_deref() {
            Some("worldspawn") => {
                for b in &e.brushes {
                    let (lo, hi) = bounds(b);
                    commands.spawn((
                        Mesh3d(meshes.add(Cuboid::from_size(hi - lo))),
                        MeshMaterial3d(wall.clone()),
                        Transform::from_translation((lo + hi) / 2.0),
                        crate::lab::Solid {
                            half_extents: (hi - lo) / 2.0,
                        },
                        crate::until_we_leave_the_lab(),
                    ));
                }
            }
            Some("training_spot") => {
                spots.0.insert(
                    property(e, "id").unwrap(),
                    Transform::from_translation(origin(e)),
                );
            }
            Some("machine_spot") => {
                let kind_name = property(e, "kind").unwrap();
                let kind = *crate::machines::MachineKind::ALL
                    .iter()
                    .find(|k| format!("{k:?}") == kind_name)
                    .unwrap();
                let yaw = property(e, "angles")
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .parse::<f32>()
                    .unwrap();
                machines.insert_with_lane(
                    property(e, "id").unwrap(),
                    kind,
                    Transform::from_translation(origin(e))
                        .with_rotation(Quat::from_rotation_y(yaw.to_radians())),
                    (kind == crate::machines::MachineKind::DeliveryWindow)
                        .then_some(crate::lab::DeliveryLane::Public),
                );
            }
            Some("queue_point") => {
                paths.public.push(origin(e));
            }
            Some("light_point") => {
                commands.spawn((
                    PointLight {
                        intensity: 260000.0,
                        range: 12.0,
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    Transform::from_translation(origin(e)),
                    crate::until_we_leave_the_lab(),
                ));
            }
            _ => {}
        }
    }
    commands.insert_resource(areas(&entities));
    commands.insert_resource(spots);
    commands.insert_resource(machines);
    commands.insert_resource(paths);
    commands.insert_resource(GlobalAmbientLight {
        color: Color::WHITE,
        brightness: 220.0,
        ..default()
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn training_map_has_all_markers_and_connected_workstations() {
        let map = parse();
        let areas = areas(&map);
        let nav = crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS);
        let required = [
            "spawn",
            "supplies",
            "instructor",
            "customer",
            "customer_exit",
            "patient",
            "sample",
            "experiment",
            "cleanup",
        ];
        let spots: std::collections::HashMap<_, _> = map
            .iter()
            .filter(|e| property(e, "classname").as_deref() == Some("training_spot"))
            .map(|e| (property(e, "id").unwrap(), origin(e)))
            .collect();
        assert_eq!(spots.len(), required.len());
        for id in required {
            assert!(spots.contains_key(id));
            assert!(
                nav.path(spots["spawn"], spots[id]).is_some(),
                "unreachable {id}"
            );
        }
        for e in map
            .iter()
            .filter(|e| property(e, "classname").as_deref() == Some("machine_spot"))
        {
            assert!(nav.path(spots["spawn"], origin(e)).is_some());
        }
    }
}
