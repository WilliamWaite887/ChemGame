//! Beakers, bottles, pills and the chemist's four-slot hotbar.
//!
//! A container is an entity. [`InventorySlot`] records which chemist owns it,
//! while [`HeldBy`] marks only the currently selected hotbar item. Keeping both
//! relations on the item is what lets it move between a bench, either chemist,
//! a locker, and a machine without maintaining parallel entity lists.

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{resolve_step, ResolveReport, Solution, Units};
use serde::{Deserialize, Serialize};

use crate::audio::{EmitWorldSfx, Sfx};
use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::interaction::{InteractRequested, Interactable};
use crate::lab::{self, Solid};
use crate::machines::{chemist_entity, ReactionsFired};
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
            .add_client_message::<SelectInventorySlotRequested>(Channel::Ordered)
            .add_message::<EmitWorldSfx>()
            .add_systems(
                OnEnter(AppState::Playing),
                (
                    // Both ends need the glass material to draw with; only the
                    // authority decides what glassware exists.
                    load_container_assets,
                    spawn_starting_glassware
                        .in_set(crate::world_state::WorldLoadSet::SpawnDefaults)
                        .run_if(is_authority),
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    (
                        ensure_inventory_selection,
                        handle_select_inventory_slot,
                        handle_pickup,
                        handle_drop,
                        tick_armed_charges,
                    )
                        .chain()
                        .run_if(is_authority),
                    (
                        request_inventory_slot.run_if(crate::settings::not_paused),
                        request_drop.run_if(crate::settings::not_paused),
                        // Runs everywhere: a replicated beaker arrives as
                        // contents and a position, and each end builds the
                        // glass for it.
                        dress_containers,
                        carry_held_containers,
                        sync_inventory_visibility,
                        hide_stored_items,
                        update_liquid_visuals,
                    )
                        .chain(),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[derive(Component, Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ContainerKind {
    Beaker,
    LargeBeaker,
    Bottle,
    Pill,
    /// Draws from a container and injects. The only route that delivers a whole
    /// dose at once, which is what makes it worth the trip to the Mixing Chamber.
    Syringe,
    /// Sealed energetic payloads. Separate appended variants keep the fuse
    /// choice serializable without adding mutable configuration to every
    /// ordinary bottle and beaker.
    ChemicalCharge5,
    ChemicalCharge10,
    ChemicalCharge20,
    SprayBottle,
    Patch,
    /// A fresh single-use strip. Used colors are separate appended variants
    /// so the approximate reading replicates without another component.
    PhPaper,
    PhPaperStrongAcid,
    PhPaperAcid,
    PhPaperNeutral,
    PhPaperBase,
    PhPaperStrongBase,
    /// A refillable portable smoke projector. Appended so existing serialized
    /// container discriminants retain their meaning.
    SmokeProjector,
    /// A longer-range syringe. Draws and injects exactly like [`Self::Syringe`]
    /// — only its [`Self::reach`] differs — appended so it never renumbers the
    /// existing kinds.
    SyringeGun,
    /// A pressurized sprayer: same aimed-mist route as [`Self::SprayBottle`],
    /// but reaches further and — unlike a hand sprayer — hits every body
    /// caught in its cone, not just the one under the crosshair. See
    /// [`Self::is_cone_spray`].
    PressureSprayer,
    /// The cheap sibling of [`Self::PressureSprayer`]: same cone, weaker
    /// per-target dose (see [`Self::application_dose`]).
    WaterGun,
}

impl ContainerKind {
    /// Ordered wire schema. Container variants are append-only because serde's
    /// binary representation uses their discriminants in multiplayer.
    pub const NETWORK_SCHEMA: &'static str = "Beaker|LargeBeaker|Bottle|Pill|Syringe|ChemicalCharge5|ChemicalCharge10|ChemicalCharge20|SprayBottle|Patch|PhPaper|PhPaperStrongAcid|PhPaperAcid|PhPaperNeutral|PhPaperBase|PhPaperStrongBase|SmokeProjector|SyringeGun|PressureSprayer|WaterGun";

    /// Every variant, in no particular order. Unlike [`Self::NETWORK_SCHEMA`]
    /// this is not wire-sensitive — it only drives which label textures
    /// `load_container_assets` loads up front — so nothing stops it being
    /// reordered or extended freely.
    pub const ALL: [ContainerKind; 20] = [
        ContainerKind::Beaker,
        ContainerKind::LargeBeaker,
        ContainerKind::Bottle,
        ContainerKind::Pill,
        ContainerKind::Syringe,
        ContainerKind::ChemicalCharge5,
        ContainerKind::ChemicalCharge10,
        ContainerKind::ChemicalCharge20,
        ContainerKind::SprayBottle,
        ContainerKind::Patch,
        ContainerKind::PhPaper,
        ContainerKind::PhPaperStrongAcid,
        ContainerKind::PhPaperAcid,
        ContainerKind::PhPaperNeutral,
        ContainerKind::PhPaperBase,
        ContainerKind::PhPaperStrongBase,
        ContainerKind::SmokeProjector,
        ContainerKind::SyringeGun,
        ContainerKind::PressureSprayer,
        ContainerKind::WaterGun,
    ];

    pub fn capacity(self) -> Units {
        match self {
            ContainerKind::Beaker => Units::whole(50),
            ContainerKind::LargeBeaker => Units::whole(100),
            ContainerKind::Bottle => Units::whole(30),
            ContainerKind::Pill => Units::whole(20),
            // Isolates the balance question to reach alone — a Syringe Gun
            // draws and injects exactly like a Syringe otherwise.
            ContainerKind::Syringe | ContainerKind::SyringeGun => Units::whole(15),
            ContainerKind::ChemicalCharge5
            | ContainerKind::ChemicalCharge10
            | ContainerKind::ChemicalCharge20 => Units::whole(50),
            ContainerKind::SprayBottle => Units::whole(30),
            // More capacity is the point of paying for the upgrade — more
            // shots before a trip back to refill.
            ContainerKind::PressureSprayer => Units::whole(45),
            ContainerKind::WaterGun => Units::whole(45),
            ContainerKind::Patch => Units::whole(10),
            ContainerKind::PhPaper
            | ContainerKind::PhPaperStrongAcid
            | ContainerKind::PhPaperAcid
            | ContainerKind::PhPaperNeutral
            | ContainerKind::PhPaperBase
            | ContainerKind::PhPaperStrongBase => Units::ZERO,
            ContainerKind::SmokeProjector => Units::whole(30),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContainerKind::Beaker => "Beaker",
            ContainerKind::LargeBeaker => "Large Beaker",
            ContainerKind::Bottle => "Bottle",
            ContainerKind::Pill => "Pill",
            ContainerKind::Syringe => "Syringe",
            ContainerKind::ChemicalCharge5 => "Chemical Charge (5s)",
            ContainerKind::ChemicalCharge10 => "Chemical Charge (10s)",
            ContainerKind::ChemicalCharge20 => "Chemical Charge (20s)",
            ContainerKind::SprayBottle => "Spray Bottle",
            ContainerKind::Patch => "Chemical Patch",
            ContainerKind::PhPaper => "Unused pH Paper",
            ContainerKind::PhPaperStrongAcid => "pH Paper — strong acid (0–2)",
            ContainerKind::PhPaperAcid => "pH Paper — acidic (3–5)",
            ContainerKind::PhPaperNeutral => "pH Paper — near neutral (6–8)",
            ContainerKind::PhPaperBase => "pH Paper — basic (9–11)",
            ContainerKind::PhPaperStrongBase => "pH Paper — strong base (12–14)",
            ContainerKind::SmokeProjector => "Smoke Projector",
            ContainerKind::SyringeGun => "Syringe Gun",
            ContainerKind::PressureSprayer => "Pressure Sprayer",
            ContainerKind::WaterGun => "Water Gun",
        }
    }

    /// Path under `assets/` to this kind's label texture, generated by
    /// `tools/gen_item_textures.py`. Every variant wraps a `Cylinder`, whose
    /// UV unwrap runs `u` once around the barrel and `v` from bottom cap to
    /// top cap (see that script's module doc), so each texture is one square
    /// image painted to read as glass, paper or metal at that layout.
    pub fn texture_path(self) -> &'static str {
        match self {
            ContainerKind::Beaker => "textures/items/beaker.png",
            ContainerKind::LargeBeaker => "textures/items/large_beaker.png",
            ContainerKind::Bottle => "textures/items/bottle.png",
            ContainerKind::Pill => "textures/items/pill.png",
            ContainerKind::Syringe => "textures/items/syringe.png",
            ContainerKind::ChemicalCharge5 => "textures/items/chemical_charge_5.png",
            ContainerKind::ChemicalCharge10 => "textures/items/chemical_charge_10.png",
            ContainerKind::ChemicalCharge20 => "textures/items/chemical_charge_20.png",
            ContainerKind::SprayBottle => "textures/items/spray_bottle.png",
            ContainerKind::Patch => "textures/items/patch.png",
            ContainerKind::PhPaper => "textures/items/ph_paper.png",
            ContainerKind::PhPaperStrongAcid => "textures/items/ph_paper_strong_acid.png",
            ContainerKind::PhPaperAcid => "textures/items/ph_paper_acid.png",
            ContainerKind::PhPaperNeutral => "textures/items/ph_paper_neutral.png",
            ContainerKind::PhPaperBase => "textures/items/ph_paper_base.png",
            ContainerKind::PhPaperStrongBase => "textures/items/ph_paper_strong_base.png",
            ContainerKind::SmokeProjector => "textures/items/smoke_projector.png",
            ContainerKind::SyringeGun => "textures/items/syringe_gun.png",
            ContainerKind::PressureSprayer => "textures/items/pressure_sprayer.png",
            ContainerKind::WaterGun => "textures/items/water_gun.png",
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
            ContainerKind::ChemicalCharge5
            | ContainerKind::ChemicalCharge10
            | ContainerKind::ChemicalCharge20 => (0.065, 0.10),
            ContainerKind::SprayBottle => (0.035, 0.12),
            ContainerKind::Patch => (0.025, 0.008),
            ContainerKind::PhPaper
            | ContainerKind::PhPaperStrongAcid
            | ContainerKind::PhPaperAcid
            | ContainerKind::PhPaperNeutral
            | ContainerKind::PhPaperBase
            | ContainerKind::PhPaperStrongBase => (0.018, 0.004),
            ContainerKind::SmokeProjector => (0.045, 0.15),
            ContainerKind::SyringeGun => (0.02, 0.16),
            ContainerKind::PressureSprayer => (0.05, 0.20),
            ContainerKind::WaterGun => (0.045, 0.14),
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
            ContainerKind::Pill
                | ContainerKind::Bottle
                | ContainerKind::Syringe
                | ContainerKind::SyringeGun
                | ContainerKind::Patch
        )
    }

    /// How far this item reaches, in metres. Every existing kind keeps the
    /// ordinary hand-reach distance; only the confrontation items extend it.
    /// First-pass constants, not tuned against real play.
    pub fn reach(self) -> f32 {
        match self {
            ContainerKind::SyringeGun => 6.0,
            ContainerKind::PressureSprayer => 4.5,
            ContainerKind::WaterGun => 5.0,
            _ => crate::interaction::REACH,
        }
    }

    /// Whether this item hits every body within an aimed cone rather than a
    /// single crosshair target. First-pass half-angle, not tuned.
    pub fn is_cone_spray(self) -> bool {
        matches!(
            self,
            ContainerKind::PressureSprayer | ContainerKind::WaterGun
        )
    }

    /// Half-angle of the cone, in degrees. Zero for anything that is not a
    /// cone spray — callers should gate on [`Self::is_cone_spray`] rather
    /// than trust this alone.
    pub fn cone_half_angle_deg(self) -> f32 {
        match self {
            ContainerKind::PressureSprayer | ContainerKind::WaterGun => 20.0,
            _ => 0.0,
        }
    }

    /// How much this item transfers per application — per hand-pour, per
    /// splash, or (for a cone spray) per body caught in one press. Reuses
    /// `body`'s own `SPRAY_DOSE`/`HAND_TRANSFER` constants rather than a
    /// second, driftable copy of the numbers.
    pub fn application_dose(self) -> Units {
        match self {
            ContainerKind::SprayBottle | ContainerKind::PressureSprayer => crate::body::SPRAY_DOSE,
            ContainerKind::WaterGun => crate::body::WATER_GUN_DOSE,
            _ => crate::body::HAND_TRANSFER,
        }
    }

    pub fn charge_fuse(self) -> Option<f32> {
        match self {
            ContainerKind::ChemicalCharge5 => Some(5.0),
            ContainerKind::ChemicalCharge10 => Some(10.0),
            ContainerKind::ChemicalCharge20 => Some(20.0),
            _ => None,
        }
    }

    /// Approximate band shown by a one-use strip. Exact pH remains analyzer
    /// territory, so the portable tool is useful without replacing machinery.
    pub fn used_ph_paper(ph: f32) -> Self {
        match ph.clamp(0.0, 14.0) {
            value if value < 3.0 => ContainerKind::PhPaperStrongAcid,
            value if value < 6.0 => ContainerKind::PhPaperAcid,
            value if value < 9.0 => ContainerKind::PhPaperNeutral,
            value if value < 12.0 => ContainerKind::PhPaperBase,
            _ => ContainerKind::PhPaperStrongBase,
        }
    }
}

/// A container and what is in it.
#[derive(Component, Serialize, Deserialize)]
pub struct Container {
    pub kind: ContainerKind,
    pub solution: Solution,
}

/// A placed charge counting down on the authority. It remains pickable and
/// movable; only delivering, drinking and pouring are forbidden by its kind.
#[derive(Component, Clone, Debug, Serialize, Deserialize, MapEntities)]
pub struct ArmedCharge {
    pub remaining_secs: f32,
    #[entities]
    pub owner: Entity,
}

fn tick_armed_charges(
    time: Res<Time>,
    db: Option<Res<ChemDb>>,
    mut charges: Query<(Entity, &mut ArmedCharge, &mut Container)>,
    mut fired: MessageWriter<ReactionsFired>,
) {
    let Some(db) = db else {
        return;
    };
    for (entity, mut armed, mut container) in &mut charges {
        armed.remaining_secs -= time.delta_secs();
        if armed.remaining_secs > 0.0 {
            continue;
        }
        let (_, report) = container.mutate(&db, |solution| {
            solution.temperature = chem_sim::Kelvin(600.0)
        });
        if let Some(message) = ReactionsFired::from_report(entity, &report) {
            fired.write(message);
        }
        // A valid charge always reports an explosion and is despawned by the
        // hazard consumer. Leaving attribution attached until then ensures
        // the blast is credited even across deferred command boundaries.
        armed.remaining_secs = f32::INFINITY;
    }
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

/// Four inventory cells, numbered left to right like a compact Minecraft
/// hotbar. The relation lives on the item so despawning an item automatically
/// frees its cell without maintaining a second entity list.
pub const INVENTORY_SLOTS: u8 = 4;

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, MapEntities)]
pub struct InventorySlot {
    #[entities]
    pub owner: Entity,
    pub slot: u8,
}

/// The inventory cell currently represented by [`HeldBy`].
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedInventorySlot(pub u8);

/// Requests selection of one of the four number-key cells.
#[derive(Message, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SelectInventorySlotRequested {
    pub slot: u8,
}

/// Finds a free cell, preferring the selected one so picking something up with
/// an empty hand immediately puts it in view.
/// `reserved` is used by handlers that may accept several requests in one frame.
/// Deferred commands do not appear in a query until the handler returns, so
/// reservations prevent two valid requests from claiming the same free cell.
pub fn free_inventory_slot(
    owner: Entity,
    preferred: u8,
    inventory: &Query<&InventorySlot>,
    reserved: &[(Entity, u8)],
) -> Option<u8> {
    let mut occupied = [false; INVENTORY_SLOTS as usize];
    for entry in inventory.iter().filter(|entry| entry.owner == owner) {
        if let Some(cell) = occupied.get_mut(entry.slot as usize) {
            *cell = true;
        }
    }
    for (_, slot) in reserved.iter().filter(|(reserved, _)| *reserved == owner) {
        if let Some(cell) = occupied.get_mut(*slot as usize) {
            *cell = true;
        }
    }
    if preferred < INVENTORY_SLOTS && !occupied[preferred as usize] {
        return Some(preferred);
    }
    occupied
        .iter()
        .position(|occupied| !occupied)
        .map(|slot| slot as u8)
}

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

/// Sitting in this machine's third container slot.
///
/// Delivery windows use all three slots; other equipment never receives this
/// relation. Keeping the relation on the item preserves the same replication
/// and despawn behavior as [`InSlot`] and [`InSlotB`].
#[derive(Component, Serialize, Deserialize)]
pub struct InSlotC(#[entities] pub Entity);

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

/// Materials shared by all glassware of a given kind.
///
/// One material per [`ContainerKind`] rather than one shared glass material:
/// each kind has its own label texture from `tools/gen_item_textures.py`, so
/// a beaker and a chemical charge no longer look alike except for size.
#[derive(Resource)]
pub struct ContainerAssets {
    materials: std::collections::HashMap<ContainerKind, Handle<StandardMaterial>>,
}

impl ContainerAssets {
    fn material(&self, kind: ContainerKind) -> Handle<StandardMaterial> {
        self.materials.get(&kind).cloned().unwrap_or_else(|| {
            panic!("no material loaded for {kind:?} — is it missing from ContainerKind::ALL?")
        })
    }
}

/// Whether this kind's label texture is meant to be seen through — real
/// glassware — or an opaque tool/paper/casing. Only glassware keeps the
/// alpha-blended look; everything else was rendering as translucent blue
/// glass regardless of what it actually was, which read as a rendering bug
/// on a sealed chemical charge or a paper pH strip.
fn is_glassware(kind: ContainerKind) -> bool {
    matches!(
        kind,
        ContainerKind::Beaker
            | ContainerKind::LargeBeaker
            | ContainerKind::Bottle
            | ContainerKind::Syringe
            | ContainerKind::SyringeGun
            | ContainerKind::SprayBottle
            | ContainerKind::PressureSprayer
            | ContainerKind::WaterGun
    )
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
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let by_kind = ContainerKind::ALL
        .into_iter()
        .map(|kind| {
            let texture: Handle<Image> = asset_server.load(kind.texture_path());
            let material = if is_glassware(kind) {
                materials.add(StandardMaterial {
                    base_color_texture: Some(texture),
                    base_color: Color::srgba(1.0, 1.0, 1.0, 0.55),
                    alpha_mode: AlphaMode::Blend,
                    perceptual_roughness: 0.05,
                    metallic: 0.0,
                    ..default()
                })
            } else {
                materials.add(StandardMaterial {
                    base_color_texture: Some(texture),
                    perceptual_roughness: 0.55,
                    ..default()
                })
            };
            (kind, material)
        })
        .collect();

    commands.insert_resource(ContainerAssets { materials: by_kind });
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
            MeshMaterial3d(assets.material(kind)),
        ));
        // Most glassware uses its kind as the prompt. Debug fixtures and
        // future authored samples may arrive with a more useful replicated
        // label already, and presentation must not erase gameplay data just
        // because the local mesh was built a frame later.
        commands
            .entity(entity)
            .insert_if_new(Interactable::new(kind.label()));

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

fn spawn_starting_glassware(
    mut commands: Commands,
    restored: Option<Res<crate::world_state::PendingWorldState>>,
) {
    // A world snapshot owns the complete item population. Spawning the starter
    // rack as well would duplicate every original beaker on every reload.
    if restored.is_some_and(|restored| restored.loaded_from_disk()) {
        return;
    }
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
    for x in [-0.35f32, -0.20, -0.05] {
        spawn_container(
            &mut commands,
            ContainerKind::PhPaper,
            Vec3::new(x, bench_top, -1.2),
        );
    }
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
        With<crate::social::SocialParcel>,
        With<crate::analysis_reports::AnalysisReport>,
        With<crate::machines::Overclock>,
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
    inventory: Query<&InventorySlot>,
    selected: Query<&SelectedInventorySlot>,
    stored: Query<&Stored>,
    custody: Query<(), With<crate::security_case::CaseCustody>>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<(&Body, &Bloodstream)>,
) {
    let mut reserved = Vec::new();
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
        if !pickable.contains(request.target)
            || held.contains(request.target)
            || inventory.contains(request.target)
        {
            continue;
        }
        // Belt and braces. A stored item is hidden, so the crosshair cannot
        // land on one — but the target comes off the wire, and the server does
        // not take a client's word for what it can see.
        if stored.contains(request.target) || custody.contains(request.target) {
            continue;
        }
        let preferred = selected.get(player).map_or(0, |selected| selected.0);
        let Some(slot) = free_inventory_slot(player, preferred, &inventory, &reserved) else {
            continue;
        };
        reserved.push((player, slot));

        let mut item = commands.entity(request.target);
        item.remove::<InSlot>()
            .remove::<InSlotB>()
            .remove::<InSlotC>()
            .insert(InventorySlot {
                owner: player,
                slot,
            });
        if slot == preferred {
            item.insert(HeldBy(player));
        }
    }
}

fn ensure_inventory_selection(
    mut commands: Commands,
    chemists: Query<Entity, (Added<Chemist>, Without<SelectedInventorySlot>)>,
) {
    for chemist in &chemists {
        commands
            .entity(chemist)
            .insert(SelectedInventorySlot::default());
    }
}

fn request_inventory_slot(
    keys: Res<ButtonInput<KeyCode>>,
    players: Query<&crate::interaction::InteractionMode, With<LocalPlayer>>,
    mut requests: MessageWriter<SelectInventorySlotRequested>,
) {
    if !players.iter().any(|mode| mode.is_roaming()) {
        return;
    }
    let selected = [
        (KeyCode::Digit1, 0),
        (KeyCode::Digit2, 1),
        (KeyCode::Digit3, 2),
        (KeyCode::Digit4, 3),
    ]
    .into_iter()
    .find_map(|(key, slot)| keys.just_pressed(key).then_some(slot));
    if let Some(slot) = selected {
        requests.write(SelectInventorySlotRequested { slot });
    }
}

fn handle_select_inventory_slot(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<SelectInventorySlotRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<(&Body, &Bloodstream)>,
    mut selected: Query<&mut SelectedInventorySlot>,
    inventory: Query<(Entity, &InventorySlot, Option<&HeldBy>)>,
) {
    for request in requests.read() {
        if request.message.slot >= INVENTORY_SLOTS {
            continue;
        }
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        if bodies
            .get(player)
            .is_ok_and(|(body, blood)| body.0.collapsed || blood.0.incapacitated())
        {
            continue;
        }
        let Ok(mut current) = selected.get_mut(player) else {
            continue;
        };
        if current.0 == request.message.slot {
            continue;
        }
        current.0 = request.message.slot;
        for (item, entry, held) in &inventory {
            if entry.owner != player {
                continue;
            }
            if entry.slot == current.0 {
                if held.is_none() {
                    commands.entity(item).insert(HeldBy(player));
                }
            } else if held.is_some() {
                commands.entity(item).remove::<HeldBy>();
            }
        }
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
                .remove::<InventorySlot>()
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

/// Inventory items outside the selected cell exist and replicate, but remain
/// out of the world raycast until selected. Four cells per chemist is tiny, so
/// this direct reconciliation is cheaper and safer than coordinating several
/// added/removed-component edge systems.
fn sync_inventory_visibility(
    mut commands: Commands,
    inventory: Query<(Entity, Has<HeldBy>, Option<&Visibility>), With<InventorySlot>>,
) {
    for (item, active, visibility) in &inventory {
        let wanted = if active {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if visibility.is_none_or(|current| *current != wanted) {
            commands.entity(item).insert(wanted);
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
    use bevy::ecs::system::SystemState;

    #[test]
    fn inventory_prefers_the_selected_cell_then_fills_to_four() {
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let first = world.spawn(InventorySlot { owner, slot: 2 }).id();
        let mut state: SystemState<Query<&InventorySlot>> = SystemState::new(&mut world);
        let inventory = state.get(&world).unwrap();
        assert_eq!(free_inventory_slot(owner, 0, &inventory, &[]), Some(0));
        assert_eq!(
            free_inventory_slot(owner, 0, &inventory, &[(owner, 0)]),
            Some(1),
            "two pickup requests in one frame must reserve different cells"
        );
        state.apply(&mut world);

        world
            .entity_mut(first)
            .insert(InventorySlot { owner, slot: 0 });
        world.spawn(InventorySlot { owner, slot: 1 });
        world.spawn(InventorySlot { owner, slot: 2 });
        let mut state: SystemState<Query<&InventorySlot>> = SystemState::new(&mut world);
        let inventory = state.get(&world).unwrap();
        assert_eq!(free_inventory_slot(owner, 0, &inventory, &[]), Some(3));
        state.apply(&mut world);

        world.spawn(InventorySlot { owner, slot: 3 });
        let mut state: SystemState<Query<&InventorySlot>> = SystemState::new(&mut world);
        let inventory = state.get(&world).unwrap();
        assert_eq!(free_inventory_slot(owner, 0, &inventory, &[]), None);
    }

    #[test]
    fn selecting_a_hotbar_cell_moves_the_single_active_hand() {
        let mut app = App::new();
        app.add_message::<FromClient<SelectInventorySlotRequested>>()
            .add_systems(Update, handle_select_inventory_slot);
        let owner = app
            .world_mut()
            .spawn((
                Chemist {
                    client: ClientId::Server,
                },
                SelectedInventorySlot(0),
            ))
            .id();
        let first = app
            .world_mut()
            .spawn((InventorySlot { owner, slot: 0 }, HeldBy(owner)))
            .id();
        let second = app.world_mut().spawn(InventorySlot { owner, slot: 1 }).id();
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: SelectInventorySlotRequested { slot: 1 },
        });

        app.update();

        assert!(app.world().get::<HeldBy>(first).is_none());
        assert_eq!(
            app.world().get::<HeldBy>(second).map(|held| held.0),
            Some(owner)
        );
        assert_eq!(
            app.world().get::<SelectedInventorySlot>(owner).unwrap().0,
            1
        );
    }

    #[test]
    fn a_beaker_that_arrives_over_the_wire_gets_its_glass() {
        // Replication carries the kind and the contents; the mesh is built
        // from the kind at each end. Before the split, glassware was given its
        // mesh at spawn time on the server only — so a joining chemist walked
        // into a lab whose beakers were all invisible, and picked them up by
        // aiming at nothing.
        let mut app = App::new();
        app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<Image>()
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
        app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<Image>()
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

    #[test]
    fn dressing_preserves_an_authored_interaction_label() {
        let mut app = App::new();
        app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .add_systems(Startup, load_container_assets)
            .add_systems(Update, dress_containers);

        let sample = app
            .world_mut()
            .spawn((
                Container::new(ContainerKind::Bottle),
                Interactable::new("Hyperzine sample — hastened"),
            ))
            .id();
        app.update();

        assert_eq!(
            app.world().get::<Interactable>(sample).unwrap().label,
            "Hyperzine sample — hastened"
        );
    }
}
