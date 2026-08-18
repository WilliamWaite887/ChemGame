//! Beakers, bottles and pills.
//!
//! A container is an entity, and who holds it lives in [`HeldBy`] on the
//! container itself rather than in a player-side inventory. That is what lets
//! a beaker sit on a bench, be carried by either chemist, or be locked in a
//! machine slot without any of those being a special case.

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{resolve_step, ResolveReport, Solution, Units};
use serde::{Deserialize, Serialize};

use crate::audio::{EmitWorldSfx, Sfx};
use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::interaction::{InteractRequested, Interactable};
use crate::lab::{self, Solid};
use crate::machines::chemist_entity;
use crate::net::is_authority;
use crate::player::{Chemist, LocalPlayer, PlayerCamera};
use crate::produce::Produce;
use crate::AppState;

/// Where a carried container sits in view: low and to the right, clear of the
/// crosshair so it never blocks what you are aiming at.
const HOLD_OFFSET: Vec3 = Vec3::new(0.26, -0.20, -0.5);

fn emit_world_sfx(sounds: &mut Option<ResMut<Messages<EmitWorldSfx>>>, sound: Sfx, position: Vec3) {
    if let Some(sounds) = sounds {
        sounds.write(EmitWorldSfx::new(sound, position));
    }
}

pub struct ContainerPlugin;

impl Plugin for ContainerPlugin {
    fn build(&self, app: &mut App) {
        app.add_client_message::<DropRequested>(Channel::Ordered)
            .add_message::<EmitWorldSfx>()
            .add_systems(
                OnEnter(AppState::Playing),
                (
                    // Both ends need the glass material to draw with; only the
                    // authority decides what glassware exists.
                    load_container_assets,
                    spawn_starting_glassware.run_if(is_authority),
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    (handle_pickup, handle_drop).run_if(is_authority),
                    (
                        request_drop.run_if(crate::settings::not_paused),
                        // Runs everywhere: a replicated beaker arrives as
                        // contents and a position, and each end builds the
                        // glass for it.
                        dress_containers,
                        carry_held_containers,
                        hide_stored_items,
                        update_liquid_visuals,
                    )
                        .chain(),
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
    /// dose at once, which is what makes it worth the trip to the Mixing Chamber.
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
    ///
    /// Steps by **zero seconds**, which is not the same as doing nothing:
    /// `resolve_step` runs every reaction with no `rate` to completion exactly
    /// as `resolve` always did, and leaves every *rated* one where it stands.
    /// That split is what keeps a slow batch honest. Pouring into a beaker
    /// must not advance the batch already in it by a frame's worth on the
    /// strength of having been touched — and, more importantly, an instant
    /// reaction has to stay instant *here*, because delivery grading and the
    /// panel both read the beaker in the same frame it was poured into.
    /// `machines::tick_reactions` is what advances the rated ones.
    pub fn mutate<R>(
        &mut self,
        db: &ChemDb,
        change: impl FnOnce(&mut Solution) -> R,
    ) -> (R, ResolveReport) {
        let result = change(&mut self.solution);
        let report = resolve_step(&mut self.solution, &db.reactions, 0.0);
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

/// Sitting in this machine's *second* container slot.
///
/// Only the [`crate::machines::MachineKind::MixingChamber`] has one — a
/// second, distinct component rather than an index on [`InSlot`], so every
/// other machine and every existing single-slot call site stays exactly as
/// it was: nothing about them changes shape to make room for a slot they
/// will never have.
#[derive(Component, Serialize, Deserialize)]
pub struct InSlotB(#[entities] pub Entity);

/// Shut away in this locker.
///
/// The same shape as [`InSlot`], and for the same reason: storage is a
/// relation, not a list. A locker holding a `Vec<Entity>` would have to be kept
/// in step with every other way an item can leave the world, and would have to
/// put a list of entity ids on the wire to do it. This way the item carries the
/// fact, replication maps the single id it holds, and an item that is destroyed
/// takes its own membership with it.
///
/// Says nothing about *what* is stored — that is the whole point of a locker,
/// and why anything added to the game later needs no work here to be storable.
#[derive(Component, Serialize, Deserialize)]
pub struct Stored(#[entities] pub Entity);

/// How far above a surface an item's origin sits when it is set down.
///
/// Glassware is a cylinder centred on its origin, so half its height. Anything
/// else is a small prop, and a couple of centimetres keeps it clear of the floor
/// without every future item having to declare a size.
pub fn set_down_lift(container: Option<&Container>) -> f32 {
    container.map_or(0.06, |container| container.kind.dimensions().1 * 0.5)
}

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

/// Puts a container in the world and returns its entity.
///
/// Deliberately spawns no mesh. What a beaker *is* — its kind and its
/// contents — is shared lab state and replicates; how it is drawn is each
/// end's own business, and [`dress_containers`] handles that on both.
pub fn spawn_container(commands: &mut Commands, kind: ContainerKind, position: Vec3) -> Entity {
    commands
        .spawn((
            Container::new(kind),
            Transform::from_translation(position),
            Replicated,
            crate::until_we_leave_the_lab(),
        ))
        .id()
}

pub(crate) fn load_container_assets(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(ContainerAssets {
        glass_material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.82, 0.90, 0.95, 0.28),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.05,
            metallic: 0.0,
            ..default()
        }),
    });
}

/// Builds the glass for every container that has appeared, however it got here.
///
/// Keyed on `Added<Container>`, so one code path covers a beaker spawned
/// locally by the authority and one that arrived over the wire. Dimensions
/// come from the kind, which is replicated, so the two ends always agree.
pub(crate) fn dress_containers(
    mut commands: Commands,
    assets: Option<Res<ContainerAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    containers: Query<(Entity, &Container), Added<Container>>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, container) in &containers {
        let kind = container.kind;
        let (radius, height) = kind.dimensions();

        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cylinder::new(radius, height))),
            MeshMaterial3d(assets.glass_material.clone()),
            Interactable::new(kind.label()),
        ));

        // The liquid is a child cylinder, scaled down as the container empties.
        // It starts invisible because a fresh container is empty; the first
        // run of `update_liquid_visuals` corrects a full one.
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
            LiquidVisual { container: entity },
            ChildOf(entity),
        ));
    }
}

fn spawn_starting_glassware(mut commands: Commands) {
    // Glassware waiting on the benches at the start of a shift.
    let bench_top = 0.9 + 0.065;
    for (index, x) in [-1.6f32, -1.15, -0.7].into_iter().enumerate() {
        let kind = if index == 2 {
            ContainerKind::LargeBeaker
        } else {
            ContainerKind::Beaker
        };
        spawn_container(&mut commands, kind, Vec3::new(x, bench_top, 0.9));
    }
    for x in [-1.6f32, -1.15] {
        spawn_container(
            &mut commands,
            ContainerKind::LargeBeaker,
            Vec3::new(x, bench_top, -1.2),
        );
    }

    // One syringe to start with. Cargo's restock deliberately ignores syringes
    // — they come out of the Mixing Chamber — so this is the only one that
    // exists until the player makes another.
    spawn_container(
        &mut commands,
        ContainerKind::Syringe,
        Vec3::new(-0.7, bench_top, -1.2),
    );
}

/// A chemist wants to put down whatever they are carrying.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct DropRequested;

/// Everything a chemist can pick up off a bench.
///
/// `rogue_security::Deterrent` joins this for the same reason `Produce`
/// already does: it is not a container either, but carrying is defined by
/// `HeldBy` alone, and `carry_held_containers` already attaches *any* newly
/// held entity to the camera generically, so nothing else here needs to
/// change for it to be pickable.
type Pickable<'w, 's> = Query<
    'w,
    's,
    (),
    Or<(
        With<Container>,
        With<Produce>,
        With<crate::rogue_security::Deterrent>,
    )>,
>;

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
    stored: Query<&Stored>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<(&Body, &Bloodstream)>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        // Someone on the floor cannot reach the bench.
        if bodies
            .get(player)
            .is_ok_and(|(body, blood)| body.0.collapsed || blood.0.incapacitated())
        {
            continue;
        }
        if !pickable.contains(request.target) || held.contains(request.target) {
            continue;
        }
        // Belt and braces. A stored item is hidden, so the crosshair cannot
        // land on one — but the target comes off the wire, and the server does
        // not take a client's word for what it can see.
        if stored.contains(request.target) {
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

fn request_drop(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<crate::settings::Settings>,
    mut requests: MessageWriter<DropRequested>,
) {
    if keys.just_pressed(settings.bindings.drop) {
        requests.write(DropRequested);
    }
}

/// Puts down whatever the chemist is carrying, on top of whatever is under it.
///
/// The height used to be a hardcoded `0.08` — floor level, whatever the chemist
/// was standing at. Setting a beaker down at a bench dropped it *through* the
/// bench and left it sitting inside the box, out of sight and out of reach of
/// the crosshair. [`lab::resting_place`] answers the question the constant was
/// standing in for: which surface is actually under the spot.
fn handle_drop(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<DropRequested>>,
    held: Query<(Entity, &HeldBy, Option<&Container>)>,
    chemists: Query<(Entity, &Chemist)>,
    transforms: Query<&Transform>,
    solids: Query<(&Transform, &Solid)>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    let mut surfaces = None;

    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok(transform) = transforms.get(player) else {
            continue;
        };
        // Gathered once, and only if somebody actually dropped something: this
        // is every wall, bench and machine in the suite, and a drop is a
        // keypress rather than a per-frame event.
        let surfaces = surfaces.get_or_insert_with(|| {
            solids
                .iter()
                .map(|(transform, solid)| (transform.translation, solid.half_extents))
                .collect::<Vec<_>>()
        });

        for (container, holder, contents) in &held {
            if holder.0 != player {
                continue;
            }
            // Far enough forward to land on the bench you are facing rather
            // than between your feet, and near enough to still be inside the
            // reach the crosshair picks it back up at.
            let ahead = transform.translation + transform.forward() * 0.65;
            let resting = lab::resting_place(ahead, transform.translation, surfaces);
            let position = resting + Vec3::Y * set_down_lift(contents);
            commands
                .entity(container)
                .remove::<HeldBy>()
                .insert(Transform::from_translation(position));
            if contents.is_some_and(|contents| {
                matches!(
                    contents.kind,
                    ContainerKind::Beaker | ContainerKind::LargeBeaker | ContainerKind::Bottle
                )
            }) {
                emit_world_sfx(&mut sounds, Sfx::GlassClunk, position);
            }
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

/// Takes a stored item out of sight, and puts it back when it comes out.
///
/// Runs everywhere, keyed on the replicated [`Stored`] arriving or leaving, for
/// the reason every other presentation system here does: the authority decides
/// what is in the locker and both ends draw the consequence. Hiding is all the
/// hiding needed — the interaction raycast uses the default
/// `RayCastVisibility::VisibleInView`, so an invisible beaker is also one the
/// crosshair cannot reach through the locker door.
///
/// The liquid is a child of the glass, so it inherits this and no second pass
/// is needed for it.
fn hide_stored_items(
    mut commands: Commands,
    newly_stored: Query<Entity, Added<Stored>>,
    mut taken_out: RemovedComponents<Stored>,
) {
    for item in &newly_stored {
        commands.entity(item).insert(Visibility::Hidden);
    }

    // `commands.get_entity` rather than a plain `entity`: an item can leave the
    // locker by being despawned, and the removal fires either way.
    for item in taken_out.read() {
        if let Ok(mut entity) = commands.get_entity(item) {
            entity.insert(Visibility::Inherited);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_beaker_that_arrives_over_the_wire_gets_its_glass() {
        // Replication carries the kind and the contents; the mesh is built
        // from the kind at each end. Before the split, glassware was given its
        // mesh at spawn time on the server only — so a joining chemist walked
        // into a lab whose beakers were all invisible, and picked them up by
        // aiming at nothing.
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_systems(Startup, load_container_assets)
            .add_systems(Update, dress_containers);

        let arrived = app
            .world_mut()
            .spawn(Container::new(ContainerKind::LargeBeaker))
            .id();

        app.update();

        assert!(
            app.world().get::<Mesh3d>(arrived).is_some(),
            "an undressed beaker is an invisible one"
        );
        assert!(
            app.world().get::<Interactable>(arrived).is_some(),
            "and an unpickable one"
        );

        let mut liquids = app.world_mut().query::<&LiquidVisual>();
        let owners: Vec<Entity> = liquids
            .iter(app.world())
            .map(|liquid| liquid.container)
            .collect();
        assert_eq!(
            owners,
            vec![arrived],
            "the liquid child is what shows the other chemist what is in the beaker"
        );
    }

    #[test]
    fn dressing_happens_once_per_container() {
        // `Added` fires on arrival, not on every contents change — a beaker
        // being filled at the dispenser changes `Container` constantly, and
        // re-dressing would pile up a new liquid child each time.
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_systems(Startup, load_container_assets)
            .add_systems(Update, dress_containers);

        let beaker = app
            .world_mut()
            .spawn(Container::new(ContainerKind::Beaker))
            .id();
        app.update();

        // Something changes the contents, as the dispenser would.
        let _ = app
            .world_mut()
            .get_mut::<Container>(beaker)
            .expect("beaker exists")
            .solution
            .add(chem_sim::ReagentId(0), Units::whole(10));
        app.update();
        app.update();

        let mut liquids = app.world_mut().query::<&LiquidVisual>();
        assert_eq!(liquids.iter(app.world()).count(), 1);
    }
}
