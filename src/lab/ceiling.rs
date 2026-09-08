//! Shallow ceiling finishes derived from the existing roof, with no collision.
//!
//! Roof brushes remain the shell authority. Panel seams, support ribs, vents,
//! and housings for the map's existing lights are combined by material in
//! 12.8 m chunks, rather than spawning an entity for every panel or grille.

use std::collections::BTreeMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::mesh::Indices;
use bevy::prelude::*;
use bevy::render::render_resource::PrimitiveTopology;

use super::Bounds;
#[cfg(feature = "trenchbroom")]
use super::{LabLight, MapReady};
use crate::AppState;

const PANEL_PITCH: f32 = 1.6;
const RIB_PITCH: f32 = PANEL_PITCH * 3.0;
const CHUNK_SIZE: f32 = PANEL_PITCH * 8.0;
#[cfg(any(feature = "trenchbroom", test))]
const MAX_DROP: f32 = 0.12;
#[cfg(feature = "trenchbroom")]
const MIN_CLEARANCE: f32 = 2.75;
const EPSILON: f32 = 0.0001;

pub(super) struct CeilingPlugin;

impl Plugin for CeilingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Playing), load_materials);
        #[cfg(feature = "trenchbroom")]
        app.add_systems(
            Update,
            dress_map_ceiling
                .run_if(in_state(AppState::Playing))
                .run_if(resource_exists::<MapReady>),
        );
        #[cfg(not(feature = "trenchbroom"))]
        app.add_systems(
            OnEnter(AppState::Playing),
            dress_legacy_ceiling
                .after(super::spawn_shell)
                .after(load_materials)
                .run_if(crate::session::career_session),
        );
    }
}

#[derive(Resource)]
pub(super) struct CeilingMaterials(pub(super) [Handle<StandardMaterial>; 4]);

#[derive(Component)]
#[cfg(feature = "trenchbroom")]
struct CeilingDressed;

/// The visible roof is also the endpoint for installed equipment services.
/// Keeping this on the map root gives connections the same reload lifetime.
#[derive(Component)]
#[cfg(feature = "trenchbroom")]
pub(super) struct CeilingSurfaces(Vec<RoofPatch>);

#[cfg(feature = "trenchbroom")]
impl CeilingSurfaces {
    pub(super) fn height_at(&self, point: Vec3) -> Option<f32> {
        self.0
            .iter()
            .find(|patch| patch.bounds.holds(point))
            .map(|patch| patch.height)
    }
}

#[derive(Clone, Copy, Debug)]
struct RoofPatch {
    bounds: Bounds,
    height: f32,
}

fn load_materials(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let repeat = |path: &'static str| -> Handle<Image> {
        asset_server
            .load_builder()
            .with_settings(|settings: &mut ImageLoaderSettings| {
                let mut sampler = ImageSamplerDescriptor::nearest();
                sampler.address_mode_u = ImageAddressMode::Repeat;
                sampler.address_mode_v = ImageAddressMode::Repeat;
                settings.sampler = ImageSampler::Descriptor(sampler);
            })
            .load(path)
    };
    // These copies live in the runtime texture pack. The kit's authoring PNGs
    // are intentionally excluded from releases because GLBs embed them.
    let casing = materials.add(StandardMaterial {
        base_color_texture: Some(repeat("textures/ceiling_casing.png")),
        perceptual_roughness: 0.83,
        metallic: 0.12,
        ..default()
    });
    let metal = materials.add(StandardMaterial {
        base_color_texture: Some(repeat("textures/ceiling_metal.png")),
        perceptual_roughness: 0.70,
        metallic: 0.45,
        ..default()
    });
    let recess = materials.add(StandardMaterial {
        base_color: Color::srgb(0.055, 0.065, 0.078),
        perceptual_roughness: 0.92,
        ..default()
    });
    let diffuser = materials.add(StandardMaterial {
        base_color: Color::srgb(0.82, 0.89, 0.91),
        emissive: LinearRgba::new(2.2, 2.5, 2.6, 1.0),
        perceptual_roughness: 0.62,
        ..default()
    });
    commands.insert_resource(CeilingMaterials([casing, metal, recess, diffuser]));
}

#[cfg(feature = "trenchbroom")]
fn roof_patches(brushes: &[bevy_trenchbroom::brush::Brush]) -> Vec<RoofPatch> {
    use bevy_trenchbroom::brush::ConvexHull;

    brushes
        .iter()
        .filter(|brush| {
            !brush.surfaces.is_empty()
                && brush
                    .surfaces
                    .iter()
                    .all(|surface| surface.texture == "ceiling")
        })
        .filter_map(|brush| {
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for (point, _) in brush.calculate_vertices() {
                min = min.min(point.as_vec3());
                max = max.max(point.as_vec3());
            }
            // Only horizontal overhead slabs can receive this finish. This
            // deliberately leaves the lower maintenance tunnels untouched.
            (min.is_finite() && min.y - MAX_DROP >= MIN_CLEARANCE).then_some(RoofPatch {
                bounds: Bounds {
                    min_x: min.x,
                    max_x: max.x,
                    min_z: min.z,
                    max_z: max.z,
                },
                height: min.y,
            })
        })
        .collect()
}

#[cfg(feature = "trenchbroom")]
type UndressedRoofs = (With<super::tb::LabWorldspawn>, Without<CeilingDressed>);

#[cfg(feature = "trenchbroom")]
fn dress_map_ceiling(
    mut commands: Commands,
    material: Option<Res<CeilingMaterials>>,
    brushes: Res<Assets<bevy_trenchbroom::geometry::BrushesAsset>>,
    roots: Query<(Entity, &bevy_trenchbroom::geometry::Brushes), UndressedRoofs>,
    lights: Query<&Transform, With<LabLight>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    use bevy_trenchbroom::geometry::Brushes;

    let Some(material) = material else { return };
    for (root, source) in &roots {
        let source = match source {
            Brushes::Owned(source) => source,
            Brushes::Shared(handle) => {
                let Some(source) = brushes.get(handle) else {
                    continue;
                };
                source
            }
        };
        let patches = visible_patches(roof_patches(source));
        let light_positions: Vec<Vec3> = lights.iter().map(|at| at.translation).collect();
        let dressing = build_dressing(&patches, &light_positions);
        spawn_dressing(&mut commands, &mut meshes, &material, Some(root), dressing);
        commands
            .entity(root)
            .insert((CeilingDressed, CeilingSurfaces(patches)));
    }
}

#[cfg(not(feature = "trenchbroom"))]
fn dress_legacy_ceiling(
    mut commands: Commands,
    material: Res<CeilingMaterials>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let patches = visible_patches(
        super::ROOMS
            .iter()
            .map(|room| RoofPatch {
                bounds: Bounds {
                    min_x: room.min_x,
                    max_x: room.max_x,
                    min_z: room.min_z,
                    max_z: room.max_z,
                },
                height: super::ROOM_HEIGHT,
            })
            .collect(),
    );
    let lights: Vec<Vec3> = super::ROOMS.iter().flat_map(super::light_spots).collect();
    let dressing = build_dressing(&patches, &lights);
    spawn_dressing(&mut commands, &mut meshes, &material, None, dressing);
}

/// Subtract the already-covered area. The station's broad roof overlaps the
/// Chemistry slabs; only the lowest visible face should receive geometry.
fn subtract(bounds: Bounds, cover: Bounds) -> Vec<Bounds> {
    let ix0 = bounds.min_x.max(cover.min_x);
    let ix1 = bounds.max_x.min(cover.max_x);
    let iz0 = bounds.min_z.max(cover.min_z);
    let iz1 = bounds.max_z.min(cover.max_z);
    if ix0 >= ix1 - EPSILON || iz0 >= iz1 - EPSILON {
        return vec![bounds];
    }
    [
        Bounds {
            max_x: ix0,
            ..bounds
        },
        Bounds {
            min_x: ix1,
            ..bounds
        },
        Bounds {
            min_x: ix0,
            max_x: ix1,
            max_z: iz0,
            ..bounds
        },
        Bounds {
            min_x: ix0,
            max_x: ix1,
            min_z: iz1,
            ..bounds
        },
    ]
    .into_iter()
    .filter(|part| part.max_x - part.min_x > EPSILON && part.max_z - part.min_z > EPSILON)
    .collect()
}

fn visible_patches(mut source: Vec<RoofPatch>) -> Vec<RoofPatch> {
    source.sort_by(|left, right| left.height.total_cmp(&right.height));
    let mut visible: Vec<RoofPatch> = Vec::new();
    for patch in source {
        let mut remaining = vec![patch.bounds];
        for previous in &visible {
            remaining = remaining
                .into_iter()
                .flat_map(|part| subtract(part, previous.bounds))
                .collect();
        }
        visible.extend(
            remaining
                .into_iter()
                .map(|bounds| RoofPatch { bounds, ..patch }),
        );
    }
    visible
}

#[derive(Default)]
pub(super) struct MeshBatch {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl MeshBatch {
    pub(super) fn box_at(&mut self, center: Vec3, size: Vec3, shade: f32) {
        let half = size * 0.5;
        for (normal, u, v, radius, hu, hv) in [
            (Vec3::NEG_Y, Vec3::X, Vec3::Z, half.y, half.x, half.z),
            (Vec3::Y, Vec3::X, Vec3::NEG_Z, half.y, half.x, half.z),
            (Vec3::X, Vec3::Y, Vec3::Z, half.x, half.y, half.z),
            (Vec3::NEG_X, Vec3::Y, Vec3::NEG_Z, half.x, half.y, half.z),
            (Vec3::Z, Vec3::X, Vec3::Y, half.z, half.x, half.y),
            (Vec3::NEG_Z, Vec3::X, Vec3::NEG_Y, half.z, half.x, half.y),
        ] {
            let base = self.positions.len() as u32;
            for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let point = center + normal * radius + u * hu * su + v * hv * sv;
                self.positions.push(point.to_array());
                self.normals.push(normal.to_array());
                // 128px kit textures, 64 texels/metre; projection remains
                // aligned in metres instead of stretching on clipped panels.
                self.uvs.push([point.dot(u) * 0.5, point.dot(v) * 0.5]);
                self.colors.push([shade, shade, shade, 1.0]);
            }
            self.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    pub(super) fn mesh(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

#[derive(Default)]
struct Dressing {
    batches: BTreeMap<(i32, i32, usize), MeshBatch>,
    panels: usize,
    vents: usize,
    lights: usize,
}

impl Dressing {
    fn part(&mut self, material: usize, center: Vec3, size: Vec3, shade: f32) {
        let min = center - size * 0.5;
        let max = center + size * 0.5;
        // Clip long ribs at chunk boundaries too. Grouping only by their
        // centers would leave a 100 m beam in a nominally 12.8 m culling batch.
        for x in (min.x / CHUNK_SIZE).floor() as i32..(max.x / CHUNK_SIZE).ceil() as i32 {
            for z in (min.z / CHUNK_SIZE).floor() as i32..(max.z / CHUNK_SIZE).ceil() as i32 {
                let x0 = min.x.max(x as f32 * CHUNK_SIZE);
                let x1 = max.x.min((x + 1) as f32 * CHUNK_SIZE);
                let z0 = min.z.max(z as f32 * CHUNK_SIZE);
                let z1 = max.z.min((z + 1) as f32 * CHUNK_SIZE);
                if x1 - x0 <= EPSILON || z1 - z0 <= EPSILON {
                    continue;
                }
                self.batches.entry((x, z, material)).or_default().box_at(
                    Vec3::new((x0 + x1) * 0.5, center.y, (z0 + z1) * 0.5),
                    Vec3::new(x1 - x0, size.y, z1 - z0),
                    shade,
                );
            }
        }
    }
}

fn build_dressing(patches: &[RoofPatch], lights: &[Vec3]) -> Dressing {
    let mut dressing = Dressing::default();
    for patch in patches {
        let b = patch.bounds;
        let y = patch.height;
        for ix in (b.min_x / PANEL_PITCH).floor() as i32..(b.max_x / PANEL_PITCH).ceil() as i32 {
            for iz in (b.min_z / PANEL_PITCH).floor() as i32..(b.max_z / PANEL_PITCH).ceil() as i32
            {
                let x0 = (ix as f32 * PANEL_PITCH).max(b.min_x) + 0.024;
                let x1 = ((ix + 1) as f32 * PANEL_PITCH).min(b.max_x) - 0.024;
                let z0 = (iz as f32 * PANEL_PITCH).max(b.min_z) + 0.024;
                let z1 = ((iz + 1) as f32 * PANEL_PITCH).min(b.max_z) - 0.024;
                if x1 - x0 < 0.14 || z1 - z0 < 0.14 {
                    continue;
                }
                let center = Vec3::new((x0 + x1) * 0.5, y - 0.025, (z0 + z1) * 0.5);
                let width = x1 - x0;
                let depth = z1 - z0;
                let shade = if (ix + iz).rem_euclid(5) == 0 {
                    0.89
                } else {
                    1.0
                };
                dressing.part(1, center, Vec3::new(width, 0.032, depth), 0.90);
                dressing.part(
                    0,
                    center - Vec3::Y * 0.022,
                    Vec3::new(width - 0.075, 0.024, depth - 0.075),
                    shade,
                );
                dressing.panels += 1;
                // A restrained ventilation cadence gives each long run an
                // identifiable service detail without filling every tile.
                if ix.rem_euclid(4) == 1 && iz.rem_euclid(4) == 2 && width > 0.85 && depth > 0.65 {
                    let at = center - Vec3::Y * 0.041;
                    dressing.part(2, at, Vec3::new(0.65, 0.022, 0.37), 1.0);
                    for rib in 0..6 {
                        dressing.part(
                            1,
                            at + Vec3::new(0.0, -0.018, -0.145 + rib as f32 * 0.058),
                            Vec3::new(0.59, 0.025, 0.027),
                            1.0,
                        );
                    }
                    dressing.vents += 1;
                }
            }
        }
        // Primary ribs are only 9 cm deep. Their lower face stays above 2.90 m
        // under the 3 m station roof and above all character walking volumes.
        for ix in (b.min_x / RIB_PITCH).ceil() as i32..(b.max_x / RIB_PITCH).ceil() as i32 {
            let x = ix as f32 * RIB_PITCH;
            let x0 = (x - 0.042).max(b.min_x);
            let x1 = (x + 0.042).min(b.max_x);
            dressing.part(
                1,
                Vec3::new((x0 + x1) * 0.5, y - 0.050, (b.min_z + b.max_z) * 0.5),
                Vec3::new(x1 - x0, 0.09, b.max_z - b.min_z),
                0.84,
            );
        }
        for iz in (b.min_z / RIB_PITCH).ceil() as i32..(b.max_z / RIB_PITCH).ceil() as i32 {
            let z = iz as f32 * RIB_PITCH;
            let z0 = (z - 0.042).max(b.min_z);
            let z1 = (z + 0.042).min(b.max_z);
            dressing.part(
                1,
                Vec3::new((b.min_x + b.max_x) * 0.5, y - 0.050, (z0 + z1) * 0.5),
                Vec3::new(b.max_x - b.min_x, 0.09, z1 - z0),
                0.84,
            );
        }
    }
    for light in lights {
        let Some(patch) = patches
            .iter()
            .find(|patch| patch.bounds.holds(*light) && (patch.height - light.y).abs() < 0.70)
        else {
            continue;
        };
        let b = patch.bounds;
        // Clip the fixture instead of projecting through an outer roof edge.
        let x0 = (light.x - 0.53).max(b.min_x + 0.02);
        let x1 = (light.x + 0.53).min(b.max_x - 0.02);
        let z0 = (light.z - 0.23).max(b.min_z + 0.02);
        let z1 = (light.z + 0.23).min(b.max_z - 0.02);
        if x1 - x0 < 0.25 || z1 - z0 < 0.18 {
            continue;
        }
        let at = Vec3::new((x0 + x1) * 0.5, patch.height - 0.067, (z0 + z1) * 0.5);
        dressing.part(1, at, Vec3::new(x1 - x0, 0.084, z1 - z0), 1.0);
        dressing.part(
            2,
            at - Vec3::Y * 0.038,
            Vec3::new(x1 - x0 - 0.085, 0.015, z1 - z0 - 0.085),
            1.0,
        );
        dressing.part(
            3,
            at - Vec3::Y * 0.045,
            Vec3::new(x1 - x0 - 0.135, 0.014, z1 - z0 - 0.135),
            1.0,
        );
        dressing.lights += 1;
    }
    dressing
}

fn spawn_dressing(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &CeilingMaterials,
    parent: Option<Entity>,
    dressing: Dressing,
) {
    info!(
        "ceiling finish: {} panels, {} vents, {} light housings, {} mesh batches",
        dressing.panels,
        dressing.vents,
        dressing.lights,
        dressing.batches.len()
    );
    for ((x, z, material), batch) in dressing.batches {
        let mut entity = commands.spawn((
            Name::new(format!("Ceiling finish {x},{z} material {material}")),
            Mesh3d(meshes.add(batch.mesh())),
            MeshMaterial3d(materials.0[material].clone()),
            Transform::default(),
            // Existing authored light positions/illumination stay unchanged.
            // Housings must not accidentally shadow a point nested in a lip.
            NotShadowCaster,
            crate::until_we_leave_the_lab(),
        ));
        if let Some(parent) = parent {
            entity.insert(ChildOf(parent));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(x0: f32, x1: f32, z0: f32, z1: f32, height: f32) -> RoofPatch {
        RoofPatch {
            bounds: Bounds {
                min_x: x0,
                max_x: x1,
                min_z: z0,
                max_z: z1,
            },
            height,
        }
    }

    #[test]
    fn overlapping_roofs_have_one_visible_finish_at_the_lower_height() {
        let patches = visible_patches(vec![
            patch(0.0, 4.0, 0.0, 4.0, 3.2),
            patch(2.0, 6.0, 0.0, 4.0, 3.0),
        ]);
        let area: f32 = patches
            .iter()
            .map(|patch| {
                (patch.bounds.max_x - patch.bounds.min_x)
                    * (patch.bounds.max_z - patch.bounds.min_z)
            })
            .sum();
        assert!((area - 24.0).abs() < EPSILON);
        let overlap = Vec3::new(3.0, 0.0, 2.0);
        let above: Vec<_> = patches
            .iter()
            .filter(|patch| patch.bounds.holds(overlap))
            .collect();
        assert_eq!(above.len(), 1);
        assert_eq!(above[0].height, 3.0);
    }

    #[test]
    fn ceiling_geometry_keeps_headroom_and_outward_faces() {
        let dressing = build_dressing(
            &[patch(-8.0, 8.0, -8.0, 8.0, 3.0)],
            &[Vec3::new(0.0, 2.85, 0.0)],
        );
        assert!(dressing.panels > 60 && dressing.vents > 0);
        assert_eq!(dressing.lights, 1);
        assert!(dressing.batches.len() <= 16);
        for batch in dressing.batches.values() {
            for position in &batch.positions {
                assert!(position[1] >= 3.0 - MAX_DROP - EPSILON && position[1] <= 3.0);
                assert!(position[0] >= -8.0 && position[0] <= 8.0);
                assert!(position[2] >= -8.0 && position[2] <= 8.0);
            }
            for triangle in batch.indices.chunks_exact(3) {
                let a = Vec3::from(batch.positions[triangle[0] as usize]);
                let b = Vec3::from(batch.positions[triangle[1] as usize]);
                let c = Vec3::from(batch.positions[triangle[2] as usize]);
                let normal = Vec3::from(batch.normals[triangle[0] as usize]);
                assert!((b - a).cross(c - a).dot(normal) > 0.0);
            }
        }
    }

    #[cfg(feature = "trenchbroom")]
    #[test]
    fn authored_roofs_cover_the_station_with_bounded_geometry() {
        use bevy::math::DVec3;
        use bevy_trenchbroom::brush::{Brush, BrushPlane, BrushSurface};

        let source = std::fs::read("assets/maps/lab.map").unwrap();
        let map = quake_map::parse(&mut source.as_slice()).unwrap();
        let brushes: Vec<Brush> = map
            .entities
            .iter()
            .flat_map(|entity| &entity.brushes)
            .filter(|brush| {
                brush
                    .iter()
                    .all(|face| face.texture.to_string_lossy() == "ceiling")
            })
            .map(|brush| Brush {
                surfaces: brush
                    .iter()
                    .map(|face| BrushSurface {
                        plane: BrushPlane::from_triangle(face.half_space.map(|point| {
                            DVec3::new(-point[1], point[2], -point[0])
                                / super::super::TB_SCALE as f64
                        })),
                        texture: "ceiling".into(),
                        uv: default(),
                    })
                    .collect(),
            })
            .collect();
        let roofs = visible_patches(roof_patches(&brushes));
        for spot in [
            Vec3::new(-99.0, 0.0, 2.0),
            Vec3::new(-30.0, 0.0, 3.0),
            Vec3::new(-107.0, 0.0, 27.0),
            Vec3::new(-50.0, 0.0, 12.5),
            Vec3::new(-30.0, 0.0, 47.5),
            Vec3::new(-44.0, 0.0, 48.0),
        ] {
            assert!(
                roofs.iter().any(|roof| roof.bounds.holds(spot)),
                "missing roof over {spot:?}"
            );
        }
        let dressing = build_dressing(&roofs, &[]);
        assert!((2_000..4_500).contains(&dressing.panels));
        assert!(dressing.batches.len() < 320);
        let vertices: usize = dressing
            .batches
            .values()
            .map(|batch| batch.positions.len())
            .sum();
        assert!(
            vertices < 300_000,
            "ceiling became too expensive: {vertices} vertices"
        );
        for ((chunk_x, chunk_z, _), batch) in &dressing.batches {
            for position in &batch.positions {
                assert!(position[1] >= MIN_CLEARANCE && position[1] <= super::super::ROOM_HEIGHT);
                assert!(position[0] >= *chunk_x as f32 * CHUNK_SIZE - EPSILON);
                assert!(position[0] <= (*chunk_x + 1) as f32 * CHUNK_SIZE + EPSILON);
                assert!(position[2] >= *chunk_z as f32 * CHUNK_SIZE - EPSILON);
                assert!(position[2] <= (*chunk_z + 1) as f32 * CHUNK_SIZE + EPSILON);
            }
        }
    }
}
