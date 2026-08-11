//! Beakers, bottles and pills.
//!
//! A container is an entity, and who holds it lives in [`HeldBy`] on the
//! container itself rather than in a player-side inventory. That is what lets
//! a beaker sit on a bench, be carried by either chemist, or be locked in a
//! machine slot without any of those being a special case.

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{resolve, ResolveReport, Solution, Units};
use serde::{Deserialize, Serialize};

use crate::body::Body;
use crate::chem_data::ChemDb;
use crate::interaction::{InteractRequested, Interactable};
use crate::machines::chemist_entity;
use crate::player::{Chemist, LocalPlayer, PlayerCamera};
use crate::produce::Produce;
use crate::AppState;

/// Where a carried container sits in view: low and to the right, clear of the
/// crosshair so it never blocks what you are aiming at.
const HOLD_OFFSET: Vec3 = Vec3::new(0.26, -0.20, -0.5);

pub struct ContainerPlugin;

impl Plugin for ContainerPlugin {
    fn build(&self, app: &mut App) {
        app.add_client_message::<DropRequested>(Channel::Ordered)
            .add_systems(
                OnEnter(AppState::Playing),
                spawn_starting_glassware.run_if(in_state(ClientState::Disconnected)),
            )
            .add_systems(
                Update,
                (
                    (handle_pickup, handle_drop).run_if(in_state(ClientState::Disconnected)),
                    (request_drop, carry_held_containers, update_liquid_visuals),
                )
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
    /// Draws from a container and injects. The only route that delivers a whole
    /// dose at once, which is what makes it worth the trip to the ChemMaster.
    Syringe,
}

impl ContainerKind {
    pub fn capacity(self) -> Units {
        match self {
            ContainerKind::Beaker => Units::whole(50),
            ContainerKind::LargeBeaker => Units::whole(100),
            ContainerKind::Bottle => Units::whole(30),
            ContainerKind::Pill => Units::whole(20),
            ContainerKind::Syringe => Units::whole(15),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContainerKind::Beaker => "Beaker",
            ContainerKind::LargeBeaker => "Large Beaker",
            ContainerKind::Bottle => "Bottle",
            ContainerKind::Pill => "Pill",
            ContainerKind::Syringe => "Syringe",
        }
    }

    /// Radius and height of the glassware, in metres.
    pub fn dimensions(self) -> (f32, f32) {
        match self {
            ContainerKind::Beaker => (0.055, 0.13),
            ContainerKind::LargeBeaker => (0.07, 0.17),
            ContainerKind::Bottle => (0.035, 0.10),
            ContainerKind::Pill => (0.022, 0.012),
            ContainerKind::Syringe => (0.012, 0.09),
        }
    }

    /// Whether this is swallowed or injected in one go rather than measured out.
    ///
    /// What separates a dose from bulk supply. A beaker of something gets
    /// portioned later; a pill, a bottle or a syringe is taken as it comes,
    /// which is why only these can be graded as an overdose.
    pub fn is_single_dose(self) -> bool {
        matches!(
            self,
            ContainerKind::Pill | ContainerKind::Bottle | ContainerKind::Syringe
        )
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

    // One syringe to start with. Cargo's restock deliberately ignores syringes
    // — they come out of the ChemMaster — so this is the only one that exists
    // until the player makes another.
    spawn_container(
        &mut commands,
        &mut meshes,
        &mut materials,
        &assets,
        ContainerKind::Syringe,
        Vec3::new(-0.7, bench_top, -1.2),
    );

    commands.insert_resource(assets);
}

/// A chemist wants to put down whatever they are carrying.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct DropRequested;

/// Everything a chemist can pick up off a bench.
type Pickable<'w, 's> = Query<'w, 's, (), Or<(With<Container>, With<Produce>)>>;

/// Server-side pickup. Only `HeldBy` changes here; how a carried beaker looks
/// is the holder's own business, handled in [`carry_held_containers`].
///
/// Produce is pickable too. It is not a container — it holds no solution and
/// must never be gradeable at the delivery window — but carrying is defined by
/// `HeldBy` alone, so everything downstream of this treats the two alike.
fn handle_pickup(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    pickable: Pickable,
    held: Query<&HeldBy>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&Body>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        // Someone on the floor cannot reach the bench.
        if bodies.get(player).is_ok_and(|body| body.0.collapsed) {
            continue;
        }
        if !pickable.contains(request.target) || held.contains(request.target) {
            continue;
        }
        // One hand, one beaker. Anything else needs an inventory, which this
        // game does not want.
        if held.iter().any(|holder| holder.0 == player) {
            continue;
        }

        commands
            .entity(request.target)
            .remove::<InSlot>()
            .insert(HeldBy(player));
    }
}

fn request_drop(keys: Res<ButtonInput<KeyCode>>, mut requests: MessageWriter<DropRequested>) {
    if keys.just_pressed(KeyCode::KeyQ) {
        requests.write(DropRequested);
    }
}

fn handle_drop(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<DropRequested>>,
    held: Query<(Entity, &HeldBy)>,
    chemists: Query<(Entity, &Chemist)>,
    transforms: Query<&Transform>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok(transform) = transforms.get(player) else {
            continue;
        };
        for (container, holder) in &held {
            if holder.0 != player {
                continue;
            }
            let ahead = transform.translation + transform.forward() * 0.6;
            commands
                .entity(container)
                .remove::<HeldBy>()
                .insert(Transform::from_translation(Vec3::new(
                    ahead.x, 0.08, ahead.z,
                )));
        }
    }
}

/// Client-side carry visual.
///
/// The beaker you are holding rides on your camera so it tracks your view with
/// no round trip. Someone else's beaker just sits at whatever position the
/// server replicated, which is all anyone needs to see.
fn carry_held_containers(
    mut commands: Commands,
    newly_held: Query<(Entity, &HeldBy), Added<HeldBy>>,
    mut dropped: RemovedComponents<HeldBy>,
    local: Query<Entity, With<LocalPlayer>>,
    cameras: Query<(Entity, &PlayerCamera)>,
) {
    let Ok(me) = local.single() else {
        return;
    };
    let Some((camera, _)) = cameras.iter().find(|(_, camera)| camera.chemist == me) else {
        return;
    };

    for (container, holder) in &newly_held {
        if holder.0 != me {
            continue;
        }
        commands
            .entity(container)
            .insert((ChildOf(camera), Transform::from_translation(HOLD_OFFSET)));
    }

    for container in dropped.read() {
        if let Ok(mut entity) = commands.get_entity(container) {
            entity.remove::<ChildOf>();
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
