//! Map-aware service connections make permanent fixtures part of the room.
//! The asset remains reusable; the installed copy meets its actual roof/floor.
//! These narrow visual fittings stay inside the existing fixture footprint.

use bevy::prelude::*;
use std::collections::BTreeMap;

use super::ceiling::{CeilingMaterials, CeilingSurfaces, MeshBatch};
use super::{tb::DecorationSpot, MapReady};
use crate::AppState;

pub(super) struct InstallationsPlugin;

impl Plugin for InstallationsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            connect_equipment
                .run_if(in_state(AppState::Playing))
                .run_if(resource_exists::<MapReady>),
        );
    }
}

#[derive(Component)]
struct Installed;

#[derive(Clone, Copy)]
struct Connection {
    /// Connection point on the authored model, in Y-up local metres.
    at: Vec3,
    width: f32,
    depth: f32,
    overhead: bool,
}

fn connection(x: f32, y: f32, z: f32, width: f32, depth: f32, overhead: bool) -> Connection {
    Connection {
        at: Vec3::new(x, y, z),
        width,
        depth,
        overhead,
    }
}

fn services(kind: &str) -> Vec<Connection> {
    match kind {
        "eng.generator_turbine" => {
            vec![connection(-0.90, 2.28, -0.56, 0.24, 0.24, true)]
        }
        "eng.smes_bank" => vec![connection(0.72, 2.07, -0.29, 0.08, 0.07, true)],
        "eng.pump_assembly" => vec![connection(-0.73, 1.17, -0.075, 0.065, 0.105, false)],
        "sec.dispatch_console" => vec![connection(0.0, 0.58, -0.37, 0.16, 0.12, false)],
        "med.patient_bay" => vec![connection(0.20, 1.35, -1.04, 0.09, 0.055, true)],
        "eng.cable_junction" => [-0.47, 0.47]
            .into_iter()
            .flat_map(|x| {
                [
                    connection(x, 1.79, 0.074, 0.065, 0.06, true),
                    connection(x, 0.45, 0.074, 0.065, 0.06, false),
                ]
            })
            .collect(),
        "sec.wall_camera" => vec![connection(0.0, 2.20, 0.035, 0.035, 0.03, true)],
        "hall.wall_light" => vec![connection(0.0, 2.44, 0.035, 0.04, 0.03, true)],
        "sec.radio_charger" => vec![connection(0.0, 1.02, 0.035, 0.04, 0.03, false)],
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy)]
struct Part {
    material: usize,
    center: Vec3,
    size: Vec3,
}

fn fittings(service: Connection, roof: f32) -> Vec<Part> {
    let (bottom, top) = if service.overhead {
        (service.at.y, roof - 0.012)
    } else {
        (0.006, service.at.y)
    };
    if top - bottom < 0.10 {
        return Vec::new();
    }
    let at = service.at;
    let mut parts = vec![Part {
        material: 1,
        center: Vec3::new(at.x, (bottom + top) * 0.5, at.z),
        size: Vec3::new(service.width, top - bottom, service.depth),
    }];
    // Compressed dark gaskets and metal collars at both ends read as a fitted
    // connection. Offset the bands along the shaft to avoid coplanar faces.
    for y in [bottom + 0.028, top - 0.028] {
        parts.push(Part {
            material: 2,
            center: Vec3::new(at.x, y, at.z),
            size: Vec3::new(service.width * 1.16, 0.048, service.depth * 1.16),
        });
        parts.push(Part {
            material: 0,
            center: Vec3::new(at.x, y, at.z),
            size: Vec3::new(service.width * 1.32, 0.026, service.depth * 1.32),
        });
    }
    parts
}

fn connect_equipment(
    mut commands: Commands,
    materials: Option<Res<CeilingMaterials>>,
    roofs: Query<&CeilingSurfaces>,
    markers: Query<(Entity, &DecorationSpot, &GlobalTransform), Without<Installed>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Some(materials) = materials else { return };
    if roofs.is_empty() {
        return;
    }
    for (entity, marker, transform) in &markers {
        let mut batches = BTreeMap::<usize, MeshBatch>::new();
        for service in services(marker.kind.trim()) {
            let socket = transform.transform_point(service.at);
            let Some(height) = roofs.iter().find_map(|roof| roof.height_at(socket)) else {
                continue;
            };
            // Authored decoration markers have identity scale and upright yaw.
            let local_roof = height - transform.translation().y;
            for part in fittings(service, local_roof) {
                batches
                    .entry(part.material)
                    .or_default()
                    .box_at(part.center, part.size, 1.0);
            }
        }
        if !batches.is_empty() {
            info!("installed station services for {}", marker.kind);
        }
        for (material, batch) in batches {
            commands.spawn((
                Name::new(format!("{} station connection", marker.kind)),
                Mesh3d(meshes.add(batch.mesh())),
                MeshMaterial3d(materials.0[material].clone()),
                Transform::default(),
                ChildOf(entity),
                crate::until_we_leave_the_lab(),
            ));
        }
        commands.entity(entity).insert(Installed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_services_meet_real_roof_or_floor_without_leaving_fixture_footprints() {
        for (kind, half_x, half_z) in [
            ("eng.generator_turbine", 1.30, 1.00),
            ("eng.smes_bank", 1.10, 0.50),
            ("eng.pump_assembly", 0.775, 0.45),
            ("sec.dispatch_console", 1.20, 0.60),
            ("med.patient_bay", 1.30, 1.08),
            ("eng.cable_junction", 0.60, 0.24),
            ("sec.wall_camera", 0.30, 0.60),
            ("hall.wall_light", 0.48, 0.18),
            ("sec.radio_charger", 0.60, 0.30),
        ] {
            for service in services(kind) {
                for roof in [3.0, 3.2] {
                    let parts = fittings(service, roof);
                    assert!(!parts.is_empty(), "missing {kind} connection");
                    for part in &parts {
                        let min = part.center - part.size * 0.5;
                        let max = part.center + part.size * 0.5;
                        assert!(min.y >= 0.0 && max.y <= roof);
                        assert!(min.x >= -half_x && max.x <= half_x, "{kind} width");
                        assert!(min.z >= -half_z && max.z <= half_z, "{kind} depth");
                        if kind.contains("wall_")
                            || kind == "eng.cable_junction"
                            || kind == "sec.radio_charger"
                        {
                            assert!(min.z >= 0.0, "{kind} crosses its wall");
                        }
                    }
                    let shaft = &parts[0];
                    if service.overhead {
                        assert!(
                            (shaft.center.y + shaft.size.y * 0.5 - roof + 0.012).abs() < 0.0001
                        );
                    } else {
                        assert!((shaft.center.y - shaft.size.y * 0.5 - 0.006).abs() < 0.0001);
                    }
                }
            }
        }
        assert!(fittings(connection(0.0, 3.1, 0.1, 0.1, 0.1, true), 3.0).is_empty());
    }
}
