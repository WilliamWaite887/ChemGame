//! The TrenchBroom-authored lab: loads `assets/maps/lab.map` instead of
//! building the shell from [`ROOMS`] and [`WALLS`].
//!
//! This is the default station path. `cargo run --no-default-features` keeps
//! the compact const-table lab available as a supported fallback.
//!
//! # What the map owns, and what it does not
//!
//! Owned by the map: the shell geometry, the benches, the ceiling lights and
//! their per-room tint, where each machine stands, and where a chemist starts.
//! Each brush also becomes a [`Solid`], so walls you draw in TrenchBroom stop
//! the player without anyone restating them in Rust.
//!
//! Walkable containment and navigation are map-authored too, through
//! `func_walkable` brush entities. Moving structure therefore means moving its
//! floor volumes with it; map validation keeps both orthogonal and connected.
//!
//! # Colliders
//!
//! `bevy_trenchbroom` merges an entity's brushes into one mesh *per texture*,
//! so mesh entities are useless as colliders — every wall in the map would
//! share a single box the size of the lab. The colliders here are built from
//! [`QuakeMapEntity::brushes`] instead, one [`Solid`] per brush, which is the
//! same axis-aligned model the hand-built lab already uses. A brush that is not
//! axis-aligned gets its bounding box, so angled geometry collides as if it
//! were square; that is a real limit of reusing `Solid` rather than a physics
//! engine, and it is fine for a station made of corridors.

use bevy::asset::RenderAssetUsages;
use bevy::gltf::GltfAssetLabel;
use bevy::math::DVec3;
use bevy::mesh::Indices;
use bevy::prelude::*;
use bevy::render::render_resource::PrimitiveTopology;
use bevy::world_serialization::{WorldInstanceReady, WorldInstanceSpawner};
use bevy_trenchbroom::brush::ConvexHull;
use bevy_trenchbroom::class::QuakeClassSpawnView;
use bevy_trenchbroom::config::MapFileFormat;
use bevy_trenchbroom::prelude::*;

use crate::AppState;

use crate::crew::Departments;

use super::{
    machine_kind_named, Bounds, CrisisSpots, DoorSpots, LabLight, MachineSpots, MapReady, Solid,
    WalkableAreas, TB_SCALE,
};

/// The only world asset allowed to replace the station's map-derived state.
#[derive(Component)]
pub struct LabMapRoot;

/// The bounding box of a brush, in Bevy space and metres.
///
/// `Brush` arrives already converted — `Brush::from_quake_map` runs every plane
/// through `TrenchBroomConfig::to_bevy_space_f64` before we ever see it — so
/// there is deliberately no conversion here.
fn brush_extents(brush: &bevy_trenchbroom::brush::Brush) -> Option<(Vec3, Vec3)> {
    let mut min = DVec3::MAX;
    let mut max = DVec3::MIN;
    let mut found = false;

    for (vertex, _) in brush.calculate_vertices() {
        min = min.min(vertex);
        max = max.max(vertex);
        found = true;
    }

    found.then(|| (min.as_vec3(), max.as_vec3()))
}

/// Textures that are scenery: floors, ceilings and the painted door thresholds.
/// A brush textured entirely with these gets no collider, matching `decor_box`
/// in the hand-built lab.
fn is_scenery(texture: &str) -> bool {
    texture.starts_with("floor_") || matches!(texture, "ceiling" | "stripe")
}

/// A brush whose underside clears this gets no collider.
///
/// `push_out` resolves in the XZ plane only, on the stated assumption that
/// every `Solid` is tall enough to block at eye height. Door headers break that
/// assumption badly: a header's footprint *is* the doorway, so giving one a
/// collider bricks up the door it sits above, at floor level, invisibly. The
/// hand-built lab never hit this because headers went through `decor_box` and
/// never became `Solid` at all — the map has no such distinction, so the height
/// check has to live here.
///
/// This also covers anything else hung overhead in TrenchBroom — pipework, a
/// sign, a mezzanine — which would otherwise be an invisible wall.
const HEAD_ROOM: f32 = crate::player::EYE_HEIGHT + 0.25;

/// Whether a brush spanning `min_y..max_y` is something a body can walk into.
fn blocks_a_body(min_y: f32, max_y: f32) -> bool {
    // `max_y` is only here to reject brushes with no height at all, which a
    // degenerate volume in the map could otherwise turn into a phantom wall.
    max_y > min_y && min_y < HEAD_ROOM
}

/// The lab's worldspawn: the static shell, plus a collider per brush.
///
/// Replaces `bevy_trenchbroom`'s empty built-in via [`App::override_class`].
#[solid_class(
    classname("worldspawn"),
    hooks(SceneHooks::new().push(LabWorldspawn::spawn_brush_colliders)),
)]
#[derive(Debug, Clone)]
pub struct LabWorldspawn;

impl LabWorldspawn {
    fn spawn_brush_colliders(
        view: &mut QuakeClassSpawnView,
    ) -> bevy_trenchbroom::anyhow::Result<()> {
        for brush in &view.src_entity.brushes {
            if brush
                .surfaces
                .iter()
                .all(|surface| is_scenery(&surface.texture))
            {
                continue;
            }

            let Some((min, max)) = brush_extents(brush) else {
                continue;
            };
            if !blocks_a_body(min.y, max.y) {
                continue;
            }

            // `apply_move_input` reads solids through `Query<(&Transform, &Solid)>`
            // — the *local* transform, not the global one. That is fine for the
            // hand-built lab, where every solid is a root entity, and it stays
            // fine here only because worldspawn sits at the origin: brush
            // vertices are already world-space, so local and global agree.
            // Parent the map root somewhere other than the origin and every
            // collider silently stops matching what you can see. If this
            // prototype is kept, that query should move to `GlobalTransform`.
            let solid = view
                .world
                .spawn((
                    Name::new("brush collider"),
                    Transform::from_translation((min + max) * 0.5),
                    Solid {
                        half_extents: (max - min) * 0.5,
                    },
                ))
                .id();
            view.world.entity_mut(solid).insert(ChildOf(view.entity));
        }

        Ok(())
    }
}

/// A volume of standable floor, drawn over the floor in TrenchBroom.
///
/// This is what makes the map, rather than [`ROOMS`](super::ROOMS), the
/// authority on where a body may go. Collision brushes cannot answer that on
/// their own: they say where you may *not* be, and "not inside a wall" also
/// describes the vacuum outside the hull. Somebody has to draw the floor, and
/// it should be whoever is drawing the station.
///
/// Texture these `clip`. It is in `TrenchBroomConfig::auto_remove_textures` by
/// default, so no mesh is built for them and they are invisible in game while
/// staying selectable in the editor. The brush data still reaches us either
/// way — only the *mesh* is skipped.
#[solid_class(
    classname("func_walkable"),
    hooks(SceneHooks::new().push(Walkable::measure)),
)]
#[derive(Debug, Clone, Default)]
pub struct Walkable {
    /// The room this covers, as shown on the HUD. Leave empty for a doorway
    /// bridge or a stretch of corridor that belongs to no room.
    pub room: String,
    /// Stable identity for a semantic threshold. Ordinary room and corridor
    /// floor omit it; the exterior lab door uses `lab_entrance` so map
    /// validation can prove the station has one connected entrance. Its
    /// powered door does not remove this floor from route planning when shut.
    pub bridge_id: Option<String>,
}

/// The XZ footprints of a [`Walkable`] entity's brushes, measured at load.
///
/// Measured in the scene world and read back in the main world, because a scene
/// hook cannot reach a resource.
#[derive(Component, Reflect, Default)]
#[reflect(Component)]
pub struct WalkableFootprints {
    pub bounds: Vec<Bounds>,
}

impl Walkable {
    fn measure(view: &mut QuakeClassSpawnView) -> bevy_trenchbroom::anyhow::Result<()> {
        let bounds = view
            .src_entity
            .brushes
            .iter()
            .filter_map(brush_extents)
            .map(|(min, max)| Bounds {
                min_x: min.x,
                max_x: max.x,
                min_z: min.z,
                max_z: max.z,
            })
            .collect();

        view.world
            .entity_mut(view.entity)
            .insert(WalkableFootprints { bounds });

        Ok(())
    }
}

/// Folds the map's walkable volumes into [`WalkableAreas`].
///
/// Runs in the main world once the scene has been instantiated, which is why
/// the footprints were measured onto a component rather than written straight
/// to the resource.
/// Tells [`Departments`] where each department lives.
///
/// The `department` property is matched against a crew member's `role`, so a
/// wing labelled something no one on the roster works in simply never gets
/// visited — which `lab::tb_map` has a test for, since it is silent otherwise.
#[allow(clippy::too_many_arguments)]
fn collect_loaded_map(
    ready: On<WorldInstanceReady>,
    mut commands: Commands,
    roots: Query<(), With<LabMapRoot>>,
    spawner: Res<WorldInstanceSpawner>,
    walkables: Query<(&Walkable, &WalkableFootprints)>,
    machine_spots: Query<(&MachineSpot, &Transform)>,
    department_spots: Query<(&DepartmentSpot, &Transform)>,
    crisis_spots: Query<(&CrisisSpot, &Transform)>,
    door_spots: Query<(&DoorSpot, &Transform)>,
    point_lights: Query<&PointLight>,
) {
    // WorldInstanceReady is global. Other scenes must never replace station
    // resources just because they contain similarly-shaped components.
    if roots.get(ready.entity).is_err() {
        return;
    }

    let mut areas = WalkableAreas::default();
    let mut machines = MachineSpots::default();
    let mut departments = Departments::default();
    let mut crises = CrisisSpots::default();
    let mut doors = DoorSpots::default();

    for entity in spawner.iter_instance_entities(ready.instance_id) {
        if let Ok((walkable, footprints)) = walkables.get(entity) {
            let room = (!walkable.room.trim().is_empty()).then(|| walkable.room.trim().to_string());
            let bridge_id = walkable
                .bridge_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string);
            for bounds in &footprints.bounds {
                areas.push_with_bridge(*bounds, room.clone(), bridge_id.clone());
            }
        }

        if let Ok((spot, transform)) = machine_spots.get(entity) {
            let id = spot.id.trim();
            let kind_name = spot.kind.trim();
            if id.is_empty() {
                warn!("ignoring machine_spot with an empty id");
            } else if machines.get(id).is_some() {
                warn!("duplicate machine_spot id '{id}'; keeping the first");
            } else if let Some(kind) = machine_kind_named(kind_name) {
                let lane = if spot.lane.trim().is_empty() {
                    None
                } else {
                    crate::lab::DeliveryLane::parse(&spot.lane)
                };
                if !spot.lane.trim().is_empty() && lane.is_none() {
                    warn!(
                        "machine_spot '{id}' has unknown delivery lane '{}'",
                        spot.lane
                    );
                }
                machines.insert_with_lane(id, kind, *transform, lane);
            } else {
                warn!("ignoring machine_spot '{id}' with unknown kind '{kind_name}'");
            }
        }

        if let Ok((spot, transform)) = department_spots.get(entity) {
            let department = spot.department.trim();
            if !department.is_empty() {
                departments.set(department.to_string(), transform.translation);
            }
        }

        if let Ok((spot, transform)) = crisis_spots.get(entity) {
            let id = spot.id.trim();
            if id.is_empty() {
                warn!("ignoring crisis_spot with an empty id");
            } else if crises.get(id).is_some() {
                warn!("duplicate crisis_spot id '{id}'; keeping the first");
            } else {
                crises.insert(id, *transform);
            }
        }

        if let Ok((spot, transform)) = door_spots.get(entity) {
            let id = spot.id.trim();
            let bridge_id = spot.bridge_id.trim();
            if id.is_empty() || bridge_id.is_empty() {
                warn!("ignoring door_spot with an empty id or bridge_id");
            } else if doors.get(id).is_some() {
                warn!("duplicate door_spot id '{id}'; keeping the first");
            } else {
                doors.insert(id, bridge_id, *transform);
            }
        }

        if let Ok(light) = point_lights.get(entity) {
            commands.entity(entity).insert(LabLight {
                base_color: light.color,
            });
        }
    }

    // Hot reload fires another ready event while the old registry is still
    // live. Drop the gate in the same command batch as the replacements; nav
    // will restore it only after rebuilding from these areas.
    commands.remove_resource::<MapReady>();
    commands.insert_resource(areas);
    commands.insert_resource(machines);
    commands.insert_resource(departments);
    commands.insert_resource(crises);
    commands.insert_resource(doors);
}

#[point_class(
    classname("door_spot"),
    base(Transform),
    color(220 48 64),
    size(-36 -10 0, 36 10 92),
)]
#[derive(Debug, Clone, Default)]
pub struct DoorSpot {
    pub id: String,
    pub bridge_id: String,
}

/// A semantic location at which authored gameplay may manifest something.
#[point_class(
    classname("crisis_spot"),
    base(Transform),
    color(255 64 128),
    size(-16 -16 0, 16 16 48),
)]
#[derive(Debug, Clone, Default)]
pub struct CrisisSpot {
    /// Stable semantic key consumed by gameplay data, not display text.
    pub id: String,
}

/// A fixed world-space station plaque.
#[point_class(
    classname("room_sign"),
    base(Transform),
    color(255 255 96),
    size(-56 -8 -16, 56 8 16),
)]
#[derive(Debug, Clone, Default)]
pub struct RoomSign {
    pub text: String,
}

const SIGN_WIDTH: f32 = 3.0;
const SIGN_HEIGHT: f32 = 0.72;
const SIGN_DEPTH: f32 = 0.08;

#[derive(Resource)]
struct RoomSignAssets {
    plaque_mesh: Handle<Mesh>,
    plaque_material: Handle<StandardMaterial>,
    lettering_material: Handle<StandardMaterial>,
}

#[derive(Component)]
struct RoomSignDressed;

fn load_room_sign_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(RoomSignAssets {
        plaque_mesh: meshes.add(Cuboid::new(SIGN_WIDTH, SIGN_HEIGHT, SIGN_DEPTH)),
        plaque_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.025, 0.035, 0.045),
            perceptual_roughness: 0.82,
            metallic: 0.18,
            ..default()
        }),
        lettering_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.92, 1.0, 0.62),
            emissive: LinearRgba::new(1.6, 1.9, 0.65, 1.0),
            unlit: true,
            double_sided: true,
            cull_mode: None,
            ..default()
        }),
    });
}

/// Turns a map marker into a real 3D plaque. Bevy's `Text2d` is rendered by a
/// 2D camera, so the lettering is a tiny pixel-font mesh: it remains correctly
/// occluded and perspective-scaled under the game's 3D camera.
fn dress_room_signs(
    mut commands: Commands,
    assets: Option<Res<RoomSignAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    signs: Query<(Entity, &RoomSign), Without<RoomSignDressed>>,
) {
    let Some(assets) = assets else {
        return;
    };
    for (entity, sign) in &signs {
        let lettering = (!sign.text.trim().is_empty())
            .then(|| meshes.add(room_sign_text_mesh(sign.text.trim())));
        commands
            .entity(entity)
            .insert(RoomSignDressed)
            .with_children(|parent| {
                parent.spawn((
                    Name::new("room sign plaque"),
                    Mesh3d(assets.plaque_mesh.clone()),
                    MeshMaterial3d(assets.plaque_material.clone()),
                    Transform::default(),
                ));
                if let Some(lettering) = lettering {
                    parent.spawn((
                        Name::new(format!("room sign text: {}", sign.text.trim())),
                        Mesh3d(lettering),
                        MeshMaterial3d(assets.lettering_material.clone()),
                        // By convention an authored room_sign's local +Z is its
                        // public face (the side the corridor is on).
                        Transform::from_xyz(0.0, 0.0, SIGN_DEPTH * 0.51),
                    ));
                }
            });
    }
}

fn room_sign_text_mesh(text: &str) -> Mesh {
    let chars: Vec<char> = text.chars().flat_map(char::to_uppercase).collect();
    let columns = chars
        .iter()
        .map(|character| if character.is_whitespace() { 4 } else { 6 })
        .sum::<usize>()
        .saturating_sub(1)
        .max(1);
    let cell = ((SIGN_WIDTH - 0.26) / columns as f32).min((SIGN_HEIGHT - 0.20) / 7.0);
    let pixel = cell * 0.78;
    let mut cursor = -(columns as f32 * cell) * 0.5;

    let mut positions = Vec::<[f32; 3]>::new();
    let mut normals = Vec::<[f32; 3]>::new();
    let mut indices = Vec::<u32>::new();

    for character in chars {
        if character.is_whitespace() {
            cursor += cell * 4.0;
            continue;
        }
        for (row, bits) in glyph_rows(character).into_iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                let center_x = cursor + (column as f32 + 0.5) * cell;
                let center_y = (3.0 - row as f32) * cell;
                let x0 = center_x - pixel * 0.5;
                let x1 = center_x + pixel * 0.5;
                let y0 = center_y - pixel * 0.5;
                let y1 = center_y + pixel * 0.5;
                let base = positions.len() as u32;
                positions.extend([[x0, y0, 0.0], [x0, y1, 0.0], [x1, y1, 0.0], [x1, y0, 0.0]]);
                normals.extend([[0.0, 0.0, 1.0]; 4]);
                indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
            }
        }
        cursor += cell * 6.0;
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_indices(Indices::U32(indices))
}

#[rustfmt::skip]
fn glyph_rows(character: char) -> [u8; 7] {
    match character {
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110],
        '6' => [0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
        '&' => [0b01100, 0b10010, 0b10100, 0b01000, 0b10101, 0b10010, 0b01101],
        '/' => [0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b00000, 0b00000],
        '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100],
        _ =>   [0b01110, 0b10001, 0b00010, 0b00100, 0b00100, 0b00000, 0b00100],
    }
}

/// Where a machine stands.
///
/// A marker, not the machine: machine *state* is still authority-owned and
/// replicated, this only moves *placement* out of `fit()` and into the map.
/// `kind` is a string rather than a dropdown because that would mean deriving
/// `FgdType` on [`MachineKind`](crate::machines::MachineKind) and tying the core
/// enum to this crate; worth doing if the prototype is kept.

#[point_class(
    classname("machine_spot"),
    base(Transform),
    color(0 255 255),
    size(-24 -24 0, 24 24 68),
)]
#[derive(Debug, Clone, Default)]
pub struct MachineSpot {
    /// Stable identity within the map. Kinds may repeat; ids may not.
    pub id: String,
    /// Must match a [`MachineKind`](crate::machines::MachineKind) variant name,
    /// e.g. `ChemMaster5000`, `MixingChamber`, `ReactionChamber`.
    pub kind: String,
    /// Optional for ordinary machines; `public` or `medical` for a delivery window.
    pub lane: String,
}

/// A department's home ground: where its crew belong when they are not
/// somewhere else.
///
/// `department` must match a `role` in `assets/data/station.crew.ron` —
/// Medical, Security, Engineering, Cargo or Service — which is what lets a crew
/// member's existing role decide where they live without a second roster.
#[point_class(
    classname("department_spot"),
    base(Transform),
    color(255 200 0),
    size(-20 -20 0, 20 20 72),
)]
#[derive(Debug, Clone, Default)]
pub struct DepartmentSpot {
    pub department: String,
}

/// Render-only department scenery, placed independently from crew home spots.
///
/// Keeping this separate from [`DepartmentSpot`] matters: the latter is a
/// navigation destination, while this marker is the floor-centred origin of a
/// multi-metre glTF dressing set. Moving one must never silently move the
/// other. The imported worlds intentionally receive no [`Solid`] components;
/// the map remains the sole owner of walkability and collision.
#[point_class(
    classname("department_dressing"),
    base(Transform),
    color(96 220 255),
    size(-28 -28 0, 28 28 80),
)]
#[derive(Debug, Clone, Default)]
pub struct DepartmentDressing {
    pub department: String,
}

#[derive(Resource)]
struct DepartmentDressingAssets {
    chemistry: Handle<WorldAsset>,
    medical: Handle<WorldAsset>,
    engineering: Handle<WorldAsset>,
    cargo: Handle<WorldAsset>,
    security: Handle<WorldAsset>,
    service: Handle<WorldAsset>,
}

impl DepartmentDressingAssets {
    fn scene(&self, department: &str) -> Option<&Handle<WorldAsset>> {
        match department.trim() {
            "Chemistry" => Some(&self.chemistry),
            "Medical" => Some(&self.medical),
            "Engineering" => Some(&self.engineering),
            "Cargo" => Some(&self.cargo),
            "Security" => Some(&self.security),
            "Service" => Some(&self.service),
            _ => None,
        }
    }
}

#[derive(Component)]
struct DepartmentDressed;

fn load_department_dressing_assets(mut commands: Commands, assets: Res<AssetServer>) {
    let scene = |path: &'static str| assets.load(GltfAssetLabel::Scene(0).from_asset(path));
    commands.insert_resource(DepartmentDressingAssets {
        chemistry: scene("3dassets/station_starter_kit/glb/department_chemistry_dressing.glb"),
        medical: scene("3dassets/station_starter_kit/glb/department_medical_dressing.glb"),
        engineering: scene("3dassets/station_starter_kit/glb/department_engineering_dressing.glb"),
        cargo: scene("3dassets/station_starter_kit/glb/department_cargo_dressing.glb"),
        security: scene("3dassets/station_starter_kit/glb/department_security_dressing.glb"),
        service: scene("3dassets/station_starter_kit/glb/department_service_dressing.glb"),
    });
}

/// Attaches each shell-free department world to its authored map marker.
///
/// `Without<DepartmentDressed>` deliberately retries if this system ever runs
/// before the handles resource exists. It also makes map hot reload simple:
/// replacement marker entities arrive undressed and receive the new instance.
fn dress_departments(
    mut commands: Commands,
    assets: Option<Res<DepartmentDressingAssets>>,
    markers: Query<(Entity, &DepartmentDressing), Without<DepartmentDressed>>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, marker) in &markers {
        let department = marker.department.trim();
        let Some(scene) = assets.scene(department) else {
            warn!("department_dressing has unknown department '{department}'");
            commands.entity(entity).insert(DepartmentDressed);
            continue;
        };

        // WorldAssetRoot is visual hierarchy only. In particular, do not add a
        // Solid here: navigation is built from map floor volumes and cannot
        // route around arbitrary imported furniture.
        commands
            .entity(entity)
            .insert((WorldAssetRoot(scene.clone()), DepartmentDressed));
    }
}

/// Render-only modular scenery whose exact placement belongs to the map.
///
/// The `kind` string is deliberately a small authored API rather than an asset
/// path: maps cannot request arbitrary files, and renaming an export does not
/// silently invalidate every marker. Like department dressing, these scenes
/// never receive [`Solid`] or replicated gameplay state.
#[point_class(
    classname("decoration_spot"),
    base(Transform),
    color(255 128 48),
    size(-20 -20 0, 20 20 72),
)]
#[derive(Debug, Clone, Default)]
pub struct DecorationSpot {
    pub kind: String,
}

#[derive(Resource)]
struct DecorationAssets {
    supply_shelf: Handle<WorldAsset>,
    analysis_panel: Handle<WorldAsset>,
    emergency_station: Handle<WorldAsset>,
    service_board: Handle<WorldAsset>,
}

impl DecorationAssets {
    fn scene(&self, kind: &str) -> Option<&Handle<WorldAsset>> {
        match kind.trim() {
            "chem.supply_shelf" => Some(&self.supply_shelf),
            "chem.analysis_panel" => Some(&self.analysis_panel),
            "chem.emergency_station" => Some(&self.emergency_station),
            "chem.service_board" => Some(&self.service_board),
            _ => None,
        }
    }
}

#[derive(Component)]
struct DecorationDressed;

fn load_decoration_assets(mut commands: Commands, assets: Res<AssetServer>) {
    let scene = |path: &'static str| assets.load(GltfAssetLabel::Scene(0).from_asset(path));
    commands.insert_resource(DecorationAssets {
        supply_shelf: scene("3dassets/station_starter_kit/glb/decor_chem_supply_shelf.glb"),
        analysis_panel: scene("3dassets/station_starter_kit/glb/decor_chem_analysis_panel.glb"),
        emergency_station: scene(
            "3dassets/station_starter_kit/glb/decor_chem_emergency_station.glb",
        ),
        service_board: scene("3dassets/station_starter_kit/glb/decor_chem_service_board.glb"),
    });
}

fn dress_decorations(
    mut commands: Commands,
    assets: Option<Res<DecorationAssets>>,
    markers: Query<(Entity, &DecorationSpot), Without<DecorationDressed>>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, marker) in &markers {
        let kind = marker.kind.trim();
        let Some(scene) = assets.scene(kind) else {
            warn!("decoration_spot has unknown kind '{kind}'");
            commands.entity(entity).insert(DecorationDressed);
            continue;
        };
        commands
            .entity(entity)
            .insert((WorldAssetRoot(scene.clone()), DecorationDressed));
    }
}

/// The way off the station. Eventually the end of a run; for now, a place on
/// the far side of the map that everything can be routed to.
#[point_class(
    classname("escape_pod"),
    base(Transform),
    color(0 200 255),
    size(-32 -32 0, 32 32 80),
)]
#[derive(Debug, Clone, Default)]
pub struct EscapePod;

/// Where a chemist starts their shift. The map's answer to `SPAWN_SPOT`.
#[point_class(
    classname("chemist_start"),
    base(Transform),
    color(0 255 0),
    size(-16 -16 0, 16 16 72),
)]
#[derive(Debug, Clone, Default)]
pub struct ChemistStart;

pub struct LabTrenchBroomPlugin;

impl Plugin for LabTrenchBroomPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(TrenchBroomPlugins(
            TrenchBroomConfig::new("chemgame")
                .scale(TB_SCALE)
                // The exporter writes Valve220 without Quake 2 surface flags,
                // so the editor should be told to expect exactly that.
                .file_formats(vec![MapFileFormat::Valve]),
        ));

        // Must run after `TrenchBroomPlugins`: `override_class` walks the type
        // registry disabling classes that share a classname, so the built-in
        // worldspawn has to already be in it.
        app.override_class::<LabWorldspawn>()
            .register_type::<MachineSpot>()
            .register_type::<ChemistStart>()
            .register_type::<Walkable>()
            .register_type::<DepartmentSpot>()
            .register_type::<DepartmentDressing>()
            .register_type::<DecorationSpot>()
            .register_type::<EscapePod>()
            .register_type::<CrisisSpot>()
            .register_type::<DoorSpot>()
            .register_type::<RoomSign>()
            // Not Quake classes — plain components the hooks write into the
            // scene world. The scene spawner panics on any component it cannot
            // find in the registry, so they have to be here too.
            .register_type::<Solid>()
            .register_type::<WalkableFootprints>();

        app.add_observer(collect_loaded_map)
            .add_systems(
                OnEnter(AppState::Playing),
                (
                    reset_map_runtime,
                    load_room_sign_assets,
                    load_department_dressing_assets,
                    load_decoration_assets,
                    spawn_lab_map,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (dress_room_signs, dress_departments, dress_decorations)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

fn reset_map_runtime(mut commands: Commands) {
    commands.remove_resource::<MapReady>();
    commands.insert_resource(WalkableAreas::default());
    commands.insert_resource(CrisisSpots::default());
    commands.insert_resource(MachineSpots::default());
    commands.insert_resource(DoorSpots::default());
    commands.insert_resource(Departments::default());
}

fn spawn_lab_map(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn((
        LabMapRoot,
        WorldAssetRoot(assets.load("maps/lab.map#Scene")),
        crate::until_we_leave_the_lab(),
    ));

    // The map carries its own point lights, but ambient is a global rather than
    // an entity, so it stays here. A few warm points beat one bright source for
    // making primitive geometry read as a room.
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.62, 0.68, 0.80),
        brightness: 220.0,
        ..default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks on the map file itself live in `lab::tb_map`, which runs whether or
    // not this feature is on — the map is in the repo either way.

    #[test]
    fn a_door_header_never_walls_up_the_door_beneath_it() {
        // The bug: `push_out` is flat, so a collider on the header above a
        // doorway blocks the doorway itself, at floor level, with nothing to
        // see. Headers run from DOOR_HEIGHT (2.3) to ROOM_HEIGHT (3.2).
        assert!(
            !blocks_a_body(2.3, 3.2),
            "the header above a door must not collide",
        );

        // A full-height wall still must.
        assert!(blocks_a_body(0.0, 3.2), "a wall must stop the player");
        // So must a waist-height bench, and a kerb underfoot.
        assert!(blocks_a_body(0.0, 0.9), "a bench must stop the player");
        // A brush with no height at all is not a wall.
        assert!(!blocks_a_body(0.0, 0.0));
    }

    #[test]
    fn scenery_never_collides_and_structure_always_does() {
        for texture in ["floor_mixing_hall", "floor_lobby", "ceiling", "stripe"] {
            assert!(is_scenery(texture), "{texture} should be walk-through");
        }
        for texture in ["wall", "bench"] {
            assert!(!is_scenery(texture), "{texture} should stop the player");
        }
    }

    #[test]
    fn room_sign_lettering_is_real_world_space_geometry() {
        let mesh = room_sign_text_mesh("ATMOS / UTILITY");
        assert!(
            mesh.count_vertices() > 100,
            "a populated sign should contain visible glyph quads",
        );
        assert_eq!(mesh.primitive_topology(), PrimitiveTopology::TriangleList);
    }

    #[test]
    fn department_dressing_is_visual_only_and_keeps_the_map_transform() {
        let mut app = App::new();
        app.insert_resource(DepartmentDressingAssets {
            chemistry: default(),
            medical: default(),
            engineering: default(),
            cargo: default(),
            security: default(),
            service: default(),
        })
        .add_systems(Update, dress_departments);

        let authored = Transform::from_xyz(-21.0, 0.0, 20.2)
            .with_rotation(Quat::from_rotation_y(std::f32::consts::PI));
        let marker = app
            .world_mut()
            .spawn((
                DepartmentDressing {
                    department: "Medical".to_string(),
                },
                authored,
            ))
            .id();

        app.update();

        let world = app.world();
        assert!(world.get::<WorldAssetRoot>(marker).is_some());
        assert!(world.get::<DepartmentDressed>(marker).is_some());
        assert!(
            world.get::<Solid>(marker).is_none(),
            "department scenery must not become navigation-blind collision",
        );
        assert_eq!(
            world.get::<Transform>(marker),
            Some(&authored),
            "dressing must preserve the transform authored in TrenchBroom",
        );
    }

    #[test]
    fn modular_decoration_is_visual_only_and_keeps_the_map_transform() {
        let mut app = App::new();
        app.insert_resource(DecorationAssets {
            supply_shelf: default(),
            analysis_panel: default(),
            emergency_station: default(),
            service_board: default(),
        })
        .add_systems(Update, dress_decorations);

        let authored = Transform::from_xyz(-7.4, 0.0, 4.4)
            .with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
        let marker = app
            .world_mut()
            .spawn((
                DecorationSpot {
                    kind: "chem.supply_shelf".to_string(),
                },
                authored,
            ))
            .id();

        app.update();

        let world = app.world();
        assert!(world.get::<WorldAssetRoot>(marker).is_some());
        assert!(world.get::<DecorationDressed>(marker).is_some());
        assert!(
            world.get::<Solid>(marker).is_none(),
            "modular decoration must not become navigation-blind collision",
        );
        assert_eq!(
            world.get::<Transform>(marker),
            Some(&authored),
            "decoration must preserve the transform authored in TrenchBroom",
        );
    }
}
