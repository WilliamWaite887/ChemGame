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

use super::{Solid, TB_SCALE};

/// Textures that are scenery: floors, ceilings and the painted door thresholds.
/// A brush textured entirely with these gets no collider, matching `decor_box`
/// in the hand-built lab.
fn is_scenery(texture: &str) -> bool {
    texture.starts_with("floor_") || matches!(texture, "ceiling" | "stripe")
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
    fn spawn_brush_colliders(view: &mut QuakeClassSpawnView) -> bevy_trenchbroom::anyhow::Result<()> {
        for brush in &view.src_entity.brushes {
            if brush.surfaces.iter().all(|surface| is_scenery(&surface.texture)) {
                continue;
            }

            // `Brush` is already in Bevy space and metres — `Brush::from_quake_map`
            // runs every plane through `TrenchBroomConfig::to_bevy_space_f64`
            // before we ever see it, so no conversion belongs here.
            let mut min = DVec3::MAX;
            let mut max = DVec3::MIN;
            let mut found = false;
            for (vertex, _) in brush.calculate_vertices() {
                min = min.min(vertex);
                max = max.max(vertex);
                found = true;
            }
            if !found {
                continue;
            }

            let min = min.as_vec3();
            let max = max.as_vec3();

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
    /// e.g. `Dispenser`, `ChemMaster`, `ReactionChamber`.
    pub kind: String,
}

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
            // Not a Quake class — a plain component the worldspawn hook writes
            // into the scene world. The scene spawner panics on any component
            // it cannot find in the registry, so it has to be here too.
            .register_type::<Solid>();

        app.add_systems(OnEnter(AppState::Playing), spawn_lab_map);
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
    use crate::machines::MachineKind;

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
    fn the_checked_in_map_still_places_every_machine() {
        // Guards the drift the map/table split creates: delete a machine spot
        // in TrenchBroom and nothing else notices until the lab is missing a
        // ChemMaster. Positions are deliberately not checked — moving one is
        // the entire point of authoring in an editor.
        let map = std::fs::read_to_string("assets/maps/lab.map")
            .expect("assets/maps/lab.map, exported by lab::tb_export");

        for kind in MachineKind::ALL {
            assert!(
                map.contains(&format!("\"kind\" \"{kind:?}\"")),
                "{kind:?} has no machine_spot in lab.map",
            );
        }
        assert!(
            map.contains("\"classname\" \"chemist_start\""),
            "lab.map has nowhere for a chemist to start",
        );
    }
}
