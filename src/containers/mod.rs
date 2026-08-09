//! Beakers, bottles and pills.
//!
//! A container is an entity, and who holds it lives in [`HeldBy`] on the
//! container itself rather than in a player-side inventory. That is what lets
//! a beaker sit on a bench, be carried by either chemist, or be locked in a
//! machine slot without any of those being a special case.

use bevy::prelude::*;
use bevy_replicon::prelude::Replicated;
use chem_sim::{resolve, ResolveReport, Solution, Units};
use serde::{Deserialize, Serialize};

use crate::chem_data::ChemDb;
use crate::interaction::{InteractRequested, Interactable};
use crate::player::{LocalPlayer, PlayerCamera};
use crate::AppState;

/// Where a carried container sits in view: low and to the right, clear of the
/// crosshair so it never blocks what you are aiming at.
const HOLD_OFFSET: Vec3 = Vec3::new(0.26, -0.20, -0.5);

pub struct ContainerPlugin;

impl Plugin for ContainerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Playing), spawn_starting_glassware)
            .add_systems(
                Update,
                (handle_pickup, handle_drop, update_liquid_visuals)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ContainerKind {
    Beaker,
    LargeBeaker,
    Bottle,
    Pill,
}

impl ContainerKind {
    pub fn capacity(self) -> Units {
        match self {
            ContainerKind::Beaker => Units::whole(50),
            ContainerKind::LargeBeaker => Units::whole(100),
            ContainerKind::Bottle => Units::whole(30),
            ContainerKind::Pill => Units::whole(20),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContainerKind::Beaker => "Beaker",
            ContainerKind::LargeBeaker => "Large Beaker",
            ContainerKind::Bottle => "Bottle",
            ContainerKind::Pill => "Pill",
        }
    }

    /// Radius and height of the glassware, in metres.
    fn dimensions(self) -> (f32, f32) {
        match self {
            ContainerKind::Beaker => (0.055, 0.13),
            ContainerKind::LargeBeaker => (0.07, 0.17),
            ContainerKind::Bottle => (0.035, 0.10),
            ContainerKind::Pill => (0.022, 0.012),
        }
    }
}

/// A container and what is in it.
#[derive(Component, Serialize, Deserialize)]
pub struct Container {
    pub kind: ContainerKind,
    pub solution: Solution,
}

impl Container {
    pub fn new(kind: ContainerKind) -> Self {
        Container {
            kind,
            solution: Solution::new(kind.capacity()),
        }
    }

    /// Changes the contents and immediately reacts them.
    ///
    /// Every mutation goes through here so reactions can never be forgotten —
    /// a beaker that has had reagent added but not resolved would show the
    /// player ingredients that should already have become medicine.
    pub fn mutate<R>(
        &mut self,
        db: &ChemDb,
        change: impl FnOnce(&mut Solution) -> R,
    ) -> (R, ResolveReport) {
        let result = change(&mut self.solution);
        let report = resolve(&mut self.solution, &db.reactions);
        (result, report)
    }
}

/// Carried by this player. Source of truth for who holds what.
///
/// `#[entities]` is what lets this survive replication: the entity id means
/// nothing on the other end without being mapped to the client's own id for
/// the same player.
#[derive(Component, Serialize, Deserialize)]
pub struct HeldBy(#[entities] pub Entity);

/// Sitting in this machine's container slot.
#[derive(Component, Serialize, Deserialize)]
pub struct InSlot(#[entities] pub Entity);

/// The liquid mesh inside a container, kept in sync with its contents.
#[derive(Component)]
pub struct LiquidVisual {
    pub container: Entity,
}

/// Meshes shared by all glassware of a given kind.
#[derive(Resource)]
pub struct ContainerAssets {
    glass_material: Handle<StandardMaterial>,
}

/// Spawns a container in the world and returns its entity.
pub fn spawn_container(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    assets: &ContainerAssets,
    kind: ContainerKind,
    position: Vec3,
) -> Entity {
    let (radius, height) = kind.dimensions();

    let container = commands
        .spawn((
            Container::new(kind),
            Mesh3d(meshes.add(Cylinder::new(radius, height))),
            MeshMaterial3d(assets.glass_material.clone()),
            Transform::from_translation(position),
            Interactable::new(kind.label()),
            // Glassware and its contents are shared lab state; the mesh and
            // material are not, and each client builds those itself.
            Replicated,
        ))
        .id();

    // The liquid is a child cylinder, scaled down as the container empties.
    // It starts invisible because a fresh container is empty.
    let liquid_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.5, 0.5, 0.5),
        perceptual_roughness: 0.25,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(Cylinder::new(radius * 0.86, height * 0.92))),
        MeshMaterial3d(liquid_material),
        Transform::default(),
        Visibility::Hidden,
        LiquidVisual { container },
        ChildOf(container),
    ));

    container
}

fn spawn_starting_glassware(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let assets = ContainerAssets {
        glass_material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.82, 0.90, 0.95, 0.28),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.05,
            metallic: 0.0,
            ..default()
        }),
    };

    // Glassware waiting on the benches at the start of a shift.
    let bench_top = 0.9 + 0.065;
    for (index, x) in [-1.6f32, -1.15, -0.7].into_iter().enumerate() {
        let kind = if index == 2 {
            ContainerKind::LargeBeaker
        } else {
            ContainerKind::Beaker
        };
        spawn_container(
            &mut commands,
            &mut meshes,
            &mut materials,
            &assets,
            kind,
            Vec3::new(x, bench_top, 0.9),
        );
    }
    for x in [-1.6f32, -1.15] {
        spawn_container(
            &mut commands,
            &mut meshes,
            &mut materials,
            &assets,
            ContainerKind::LargeBeaker,
            Vec3::new(x, bench_top, -1.2),
        );
    }

    commands.insert_resource(assets);
}

/// Picking a container up parents it to the camera, so it rides along with
/// the view without any per-frame follow logic.
fn handle_pickup(
    mut commands: Commands,
    mut requests: MessageReader<InteractRequested>,
    containers: Query<(), With<Container>>,
    held: Query<&HeldBy>,
    cameras: Query<(Entity, &ChildOf), With<PlayerCamera>>,
) {
    for request in requests.read() {
        if !containers.contains(request.target) || held.contains(request.target) {
            continue;
        }
        // One hand, one beaker. Anything else needs an inventory, which this
        // game does not want.
        if held.iter().any(|holder| holder.0 == request.player) {
            continue;
        }
        let Some((camera, _)) = cameras
            .iter()
            .find(|(_, child_of)| child_of.parent() == request.player)
        else {
            continue;
        };

        commands
            .entity(request.target)
            .remove::<InSlot>()
            .insert((
                HeldBy(request.player),
                ChildOf(camera),
                Transform::from_translation(HOLD_OFFSET),
            ));
    }
}

fn handle_drop(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    held: Query<(Entity, &HeldBy)>,
    players: Query<(Entity, &Transform), With<LocalPlayer>>,
) {
    if !keys.just_pressed(KeyCode::KeyQ) {
        return;
    }
    for (player, transform) in &players {
        for (container, holder) in &held {
            if holder.0 != player {
                continue;
            }
            let ahead = transform.translation + transform.forward() * 0.6;
            commands
                .entity(container)
                .remove::<HeldBy>()
                .remove::<ChildOf>()
                .insert(Transform::from_translation(Vec3::new(
                    ahead.x, 0.08, ahead.z,
                )));
        }
    }
}

/// Scales and tints the liquid mesh to match its container's contents.
fn update_liquid_visuals(
    db: Option<Res<ChemDb>>,
    containers: Query<&Container, Changed<Container>>,
    mut liquids: Query<(
        &LiquidVisual,
        &mut Transform,
        &mut Visibility,
        &MeshMaterial3d<StandardMaterial>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(db) = db else {
        return;
    };

    for (liquid, mut transform, mut visibility, material) in &mut liquids {
        let Ok(container) = containers.get(liquid.container) else {
            continue;
        };

        let volume = container.solution.total_volume();
        if !volume.is_positive() {
            *visibility = Visibility::Hidden;
            continue;
        }

        let fill = (volume.as_f32() / container.kind.capacity().as_f32()).clamp(0.02, 1.0);
        *visibility = Visibility::Inherited;
        transform.scale.y = fill;
        // Cylinders are centred on their origin, so shrinking alone would
        // leave the liquid floating in the middle of the glass.
        let (_, height) = container.kind.dimensions();
        transform.translation.y = -(1.0 - fill) * height * 0.46;

        if let Some(material) = materials.get_mut(&material.0).as_mut() {
            let [r, g, b] = container.solution.color(&db.reagents);
            material.base_color = Color::srgb(r, g, b);
        }
    }
}
