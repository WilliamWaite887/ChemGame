//! Crew who come to the lab to collect what they ordered.
//!
//! Movement is a fixed waypoint walk rather than pathfinding. The route from
//! the door to the counter is a straight, permanently clear corridor, so
//! anything cleverer would be machinery with nothing to solve.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::lab::{COUNTER_SPOT, DOOR_MAX_X, DOOR_MIN_X};
use crate::AppState;

/// Walking pace, metres per second.
const WALK_SPEED: f32 = 2.1;
/// Close enough to count as arrived.
const ARRIVE_EPSILON: f32 = 0.12;

pub struct CrewPlugin;

impl Plugin for CrewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Playing), load_crew_assets)
            .add_systems(
                Update,
                walk_route
                    .run_if(in_state(AppState::Playing))
                    // The walk is simulated once, on the server; clients
                    // receive the resulting Transform.
                    .run_if(in_state(bevy_replicon::prelude::ClientState::Disconnected)),
            );
    }
}

/// A crew member as written in `assets/data/station.crew.ron`.
#[derive(Clone, Debug, Deserialize)]
pub struct CrewDef {
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
}

#[derive(Component, Clone, Serialize, Deserialize)]
pub struct CrewMember {
    pub name: String,
    pub role: String,
}

/// Where a crew member is in their visit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CrewPhase {
    Arriving,
    Waiting,
    Leaving,
}

#[derive(Component)]
pub struct CrewRoute {
    waypoints: Vec<Vec3>,
    index: usize,
    pub phase: CrewPhase,
}

impl CrewRoute {
    /// The walk in: through the door, then across to the counter.
    pub fn arrival(lane: f32) -> Self {
        CrewRoute {
            waypoints: vec![
                Vec3::new(door_x(), 0.0, COUNTER_SPOT.z),
                Vec3::new(COUNTER_SPOT.x + lane, 0.0, COUNTER_SPOT.z),
            ],
            index: 0,
            phase: CrewPhase::Arriving,
        }
    }

    /// Sends them back out the way they came.
    pub fn leave(&mut self) {
        self.waypoints = vec![
            Vec3::new(door_x(), 0.0, COUNTER_SPOT.z),
            Vec3::new(door_x(), 0.0, spawn_z()),
        ];
        self.index = 0;
        self.phase = CrewPhase::Leaving;
    }
}

fn door_x() -> f32 {
    (DOOR_MIN_X + DOOR_MAX_X) * 0.5
}

fn spawn_z() -> f32 {
    7.2
}

/// Shared meshes for crew bodies.
#[derive(Resource)]
pub struct CrewAssets {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
    skin: Handle<StandardMaterial>,
}

fn load_crew_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(CrewAssets {
        body: meshes.add(Capsule3d::new(0.28, 0.85)),
        head: meshes.add(Sphere::new(0.19)),
        skin: materials.add(StandardMaterial {
            base_color: Color::srgb(0.76, 0.62, 0.52),
            perceptual_roughness: 0.8,
            ..default()
        }),
    });
}

/// Spawns a crew member outside the door, walking in.
pub fn spawn_crew_member(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
    assets: &CrewAssets,
    def: &CrewDef,
    lane: f32,
) -> Entity {
    let [r, g, b] = def.color;
    let uniform = materials.add(StandardMaterial {
        base_color: Color::srgb(r, g, b),
        perceptual_roughness: 0.75,
        ..default()
    });

    let position = Vec3::new(door_x(), 0.93, spawn_z());
    let crew = commands
        .spawn((
            CrewMember {
                name: def.name.clone(),
                role: def.role.clone(),
            },
            CrewRoute::arrival(lane),
            Mesh3d(assets.body.clone()),
            MeshMaterial3d(uniform),
            Transform::from_translation(position),
            // The route stays server-side; clients see the resulting Transform.
            bevy_replicon::prelude::Replicated,
        ))
        .id();

    commands.spawn((
        Mesh3d(assets.head.clone()),
        MeshMaterial3d(assets.skin.clone()),
        Transform::from_xyz(0.0, 0.62, 0.0),
        ChildOf(crew),
    ));

    crew
}

/// Advances each crew member along their waypoints, and despawns them once
/// they are back outside.
fn walk_route(
    mut commands: Commands,
    time: Res<Time>,
    mut crew: Query<(Entity, &mut Transform, &mut CrewRoute)>,
) {
    for (entity, mut transform, mut route) in &mut crew {
        let Some(target) = route.waypoints.get(route.index).copied() else {
            // Route finished. Arriving crew wait; leaving crew are done.
            if route.phase == CrewPhase::Leaving {
                commands.entity(entity).despawn();
            } else if route.phase == CrewPhase::Arriving {
                route.phase = CrewPhase::Waiting;
            }
            continue;
        };

        let flat_target = Vec3::new(target.x, transform.translation.y, target.z);
        let to_target = flat_target - transform.translation;
        if to_target.length() <= ARRIVE_EPSILON {
            route.index += 1;
            continue;
        }

        let step = to_target.normalize() * WALK_SPEED * time.delta_secs();
        transform.translation += step;
        // Face the direction of travel so they read as people rather than
        // sliding props.
        transform.rotation = Quat::from_rotation_y(to_target.x.atan2(to_target.z));
    }
}
