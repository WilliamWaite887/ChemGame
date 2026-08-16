//! The TrenchBroom-authored lab: loads `assets/maps/lab.map` instead of
//! building the shell from [`ROOMS`] and [`WALLS`].
//!
//! Enable with `cargo run --features trenchbroom`. Off by default, so the
//! const-table lab stays the shipping path while the two are compared.
//!
//! # What the map owns, and what it does not
//!
//! Owned by the map: the shell geometry, the benches, the ceiling lights and
//! their per-room tint, where each machine stands, and where a chemist starts.
//! Each brush also becomes a [`Solid`], so walls you draw in TrenchBroom stop
//! the player without anyone restating them in Rust.
//!
//! **Not** owned by the map: [`super::contain`], the walkable-region backstop.
//! It still reads `ROOMS`/`WALLS`, because a pile of brushes says where you
//! cannot go, not where you may stand — the two are only the same thing in a
//! sealed room. That is the one real coupling left, and closing it means
//! authoring walkable volumes as their own brush entities (a `func_walkable`
//! solid class) and having `contain` read those. Until that is done, moving a
//! wall in TrenchBroom moves the geometry and its collider but not the
//! containment fallback, and the two can disagree.
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

use bevy::math::DVec3;
use bevy::prelude::*;
use bevy_trenchbroom::brush::ConvexHull;
use bevy_trenchbroom::class::QuakeClassSpawnView;
use bevy_trenchbroom::config::MapFileFormat;
use bevy_trenchbroom::prelude::*;

use crate::AppState;

use crate::crew::Departments;

use super::{Bounds, Solid, WalkableAreas, TB_SCALE};

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
fn collect_department_spots(
    spots: Query<(&DepartmentSpot, &Transform), Added<DepartmentSpot>>,
    mut departments: ResMut<Departments>,
) {
    for (spot, transform) in &spots {
        if spot.department.trim().is_empty() {
            continue;
        }
        departments.set(spot.department.clone(), transform.translation);
    }
}

fn collect_walkable_volumes(
    volumes: Query<(&Walkable, &WalkableFootprints), Added<WalkableFootprints>>,
    mut areas: ResMut<WalkableAreas>,
) {
    for (walkable, footprints) in &volumes {
        // An unnamed volume is a doorway bridge or a corridor: floor that
        // belongs to no room, which `room_at` must not name.
        let room = (!walkable.room.trim().is_empty()).then(|| walkable.room.clone());
        for bounds in &footprints.bounds {
            areas.push(*bounds, room.clone());
        }
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
    /// Must match a [`MachineKind`](crate::machines::MachineKind) variant name,
    /// e.g. `ChemMaster5000`, `MixingChamber`, `ReactionChamber`.
    pub kind: String,
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
            .register_type::<EscapePod>()
            // Not Quake classes — plain components the hooks write into the
            // scene world. The scene spawner panics on any component it cannot
            // find in the registry, so they have to be here too.
            .register_type::<Solid>()
            .register_type::<WalkableFootprints>();

        app.add_systems(OnEnter(AppState::Playing), spawn_lab_map)
            .add_systems(
                Update,
                (collect_walkable_volumes, collect_department_spots)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

fn spawn_lab_map(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn(WorldAssetRoot(assets.load("maps/lab.map#Scene")));

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
}
