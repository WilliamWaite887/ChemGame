//! Lab machines and the actions they perform.
//!
//! Panels never touch state directly. They emit the messages below and the
//! systems here apply them, so co-op can replicate actions later without
//! rewriting any UI.

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::thermal::approach;
use chem_sim::{Kelvin, ReactionActivation, ReagentId, Solution, Units};
use serde::{Deserialize, Serialize};

use crate::audio::{EmitWorldSfx, Sfx};
use crate::chem_data::ChemDb;
use crate::containers::{
    free_inventory_slot, set_down_lift, spawn_container, Container, ContainerKind, HeldBy, InSlot,
    InSlotB, InventorySlot, SelectedInventorySlot, Stored,
};
use crate::interaction::{
    InteractRequested, InteractionMode, LeaveMachineRequested, MachineOpened,
};
use crate::knowledge::Knowledge;
use crate::lab::Solid;
use crate::net::is_authority;
use crate::player::Chemist;
use crate::produce::{Produce, ProduceCatalog, ProduceId};
use crate::AppState;

pub struct MachinePlugin;

/// Recipe methods the shared notebook must contain before the analyzer's
/// separation program is considered calibrated.
pub const HPLC_RECIPE_REQUIREMENT: usize = 24;

fn emit_world_sfx(sounds: &mut Option<ResMut<Messages<EmitWorldSfx>>>, sound: Sfx, position: Vec3) {
    if let Some(sounds) = sounds {
        sounds.write(EmitWorldSfx::new(sound, position));
    }
}

impl Plugin for MachinePlugin {
    fn build(&self, app: &mut App) {
        // Every action a chemist can take is a client message: the panel asks,
        // the server decides. `ReactionsFired` stays local because it is a
        // server-side consequence, not a request.
        app.add_mapped_client_message::<DispenseRequested>(Channel::Ordered)
            .add_mapped_client_message::<EjectRequested>(Channel::Ordered)
            .add_mapped_client_message::<EmptyRequested>(Channel::Ordered)
            .add_mapped_client_message::<TakeRequested>(Channel::Ordered)
            .add_mapped_client_message::<BufferTransferRequested>(Channel::Ordered)
            .add_mapped_client_message::<AgitateRequested>(Channel::Ordered)
            .add_mapped_client_message::<PackageRequested>(Channel::Ordered)
            .add_mapped_client_message::<AnalyzeRequested>(Channel::Ordered)
            .add_mapped_client_message::<PurifyRequested>(Channel::Ordered)
            .add_mapped_client_message::<GrindRequested>(Channel::Ordered)
            .add_mapped_client_message::<SetTargetTemperature>(Channel::Ordered)
            .add_mapped_client_message::<SetHeaterPower>(Channel::Ordered)
            .add_message::<ReactionsFired>()
            .add_message::<EmitWorldSfx>()
            .add_systems(
                Update,
                (
                    recover_from_emp,
                    handle_machine_interact,
                    handle_leave_machine,
                    handle_dispense,
                    handle_agitate,
                    handle_buffer_transfer,
                    handle_package,
                    handle_analyze,
                    handle_purify,
                    handle_grind,
                    handle_eject,
                    handle_take,
                    handle_empty,
                    handle_thermostat_controls,
                    apply_thermostats,
                    cool_to_ambient,
                    // Last: everything above is a change to a beaker, and this
                    // is what carries whatever that change *started* forward
                    // in time. Running it first would step a batch before the
                    // frame's pours had gone in.
                    tick_reactions,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing))
                    // Authority: server, listen server, or singleplayer.
                    .run_if(is_authority),
            );
    }
}

/// Which piece of equipment this is.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum MachineKind {
    /// The acquisition machine: turns unlocked reagents into a loaded beaker.
    ChemMaster5000,
    /// The refinement machine: pulls a single named reagent out of a mixture
    /// into an internal buffer, then packages from the buffer.
    MixingChamber,
    Grinder,
    Analyzer,
    DeliveryWindow,
    /// Every department's standing, what each values, and live requisitions.
    StandingBoard,
    /// Heats and cools a loaded container toward a dialled-in temperature.
    ///
    /// The only machine that does nothing on its own: it changes the conditions
    /// and lets the chemistry decide what that means.
    ReactionChamber,
    /// Shelf space. Holds anything a chemist can carry, and hands it back.
    ///
    /// A machine rather than a thing of its own so it inherits the whole
    /// existing apparatus: the claim that stops two chemists reaching into it
    /// at once, the panel, and the walk-up-and-press-E that every other machine
    /// already teaches. Nothing about it knows what a beaker is, which is the
    /// point — it stores whatever exists now and whatever gets added later.
    Locker,
}

impl MachineKind {
    /// Every equipment kind. Authored map spots may repeat a kind (the paired
    /// dispenser/mixer lanes do); this list is the code-owned catalogue used
    /// for geometry, fittings and legacy placement coverage.
    pub const ALL: [MachineKind; 8] = [
        MachineKind::ChemMaster5000,
        MachineKind::MixingChamber,
        MachineKind::Grinder,
        MachineKind::Analyzer,
        MachineKind::DeliveryWindow,
        MachineKind::StandingBoard,
        MachineKind::ReactionChamber,
        MachineKind::Locker,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MachineKind::ChemMaster5000 => "ChemMaster 5000",
            MachineKind::MixingChamber => "Mixing Chamber",
            MachineKind::Grinder => "Reagent Grinder",
            MachineKind::Analyzer => "Sample Analyzer",
            MachineKind::DeliveryWindow => "Delivery Window",
            MachineKind::StandingBoard => "Standing Board",
            MachineKind::ReactionChamber => "Reaction Chamber",
            MachineKind::Locker => "Storage Locker",
        }
    }
}

/// A reaction chamber's dial.
///
/// Replicated: both chemists have to see what the other has set it to, or the
/// second one to walk up cooks the batch by accident.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thermostat {
    pub target: Kelvin,
    pub powered: bool,
}

impl Default for Thermostat {
    fn default() -> Self {
        Thermostat {
            target: Kelvin::AMBIENT,
            powered: false,
        }
    }
}

/// The dial's ends.
///
/// 100K below `ice`'s 273K cutoff and 100K above `chlorine_trifluoride`'s
/// 500K overheat threshold — the lowest and highest temperatures anything in
/// the data actually gates on, with margin on both sides. Used to be six
/// fixed preset buttons rather than a continuous dial: the panel is worked
/// with the mouse cursor freed the same way the settings screen is (see
/// `interaction::panel_input`), so a drag control turned out to work fine —
/// the "crosshair" concern that ruled it out originally no longer applies.
pub const TEMPERATURE_MIN: f32 = 173.0;
pub const TEMPERATURE_MAX: f32 = 600.0;

/// Where the dial actually matters, for the tick marks drawn on it — not
/// buttons any more, just a hint of where the real recipe thresholds sit.
/// `ice`'s 273K, `phlogiston`/`methamphetamine`'s shared 374K, `cyanide`'s
/// 380K, `chlorine_trifluoride`'s 424K minimum, and its 500K overheat.
pub const TEMPERATURE_MARKS: [f32; 5] = [273.0, 374.0, 380.0, 424.0, 500.0];

/// Fraction of the remaining gap a powered chamber closes per second.
///
/// Deliberately unhurried. From room temperature this reaches phlogiston's
/// 374K in about 7 seconds dialled to 400K, 3.7s at 450K and 2.5s at 500K —
/// so the target is a real speed-against-control decision, and there is time to
/// watch the readout climb and change your mind. At 0.55 the whole thing was
/// over in under two seconds and the machine may as well have been a button.
const CHAMBER_RATE: f32 = 0.18;

/// The same, for a container sitting out in the room.
///
/// Far slower, so a hot beaker stays useful long enough to carry to the
/// Mixing Chamber — but not forever. This is the clock a chemist is racing.
const AMBIENT_RATE: f32 = 0.06;

/// Timed chemistry advances in deterministic tenths of a second.
///
/// [`Units`] stores hundredths. Feeding a rated reaction arbitrary render-frame
/// deltas would round `rate * dt` on every frame, making low-rate recipes run
/// faster at 144 Hz than at 30 Hz. Accumulating wall time and only handing the
/// resolver this fixed quantum makes authored rates independent of rendering.
const CHEMISTRY_QUANTUM_SECS: f32 = 0.1;
const CHEMISTRY_QUANTUM_EPSILON: f32 = 0.000_001;

/// A machine's shared state.
///
/// `in_use_by` exists from the start on purpose: two chemists reaching for the
/// dispenser is the normal case in co-op, not an edge case.
#[derive(Component, Debug, Serialize, Deserialize)]
pub struct Machine {
    pub kind: MachineKind,
    /// Mapped on replication so each client sees the occupying chemist as
    /// their own entity id rather than the server's.
    #[entities]
    pub in_use_by: Option<Entity>,
    /// Seconds until the machine's controls recover from an EMP.
    #[serde(default)]
    pub disabled_for: f32,
}

impl Machine {
    pub fn new(kind: MachineKind) -> Self {
        Machine {
            kind,
            in_use_by: None,
            disabled_for: 0.0,
        }
    }

    pub fn available_to(&self, player: Entity) -> bool {
        self.disabled_for <= 0.0 && self.in_use_by.is_none_or(|current| current == player)
    }
}

fn recover_from_emp(time: Res<Time>, mut machines: Query<&mut Machine>) {
    for mut machine in &mut machines {
        if machine.disabled_for <= 0.0 {
            continue;
        }
        machine.disabled_for = (machine.disabled_for - time.delta_secs()).max(0.0);
        machine.in_use_by = None;
    }
}

/// Where a container sits when loaded, as an offset from the machine's origin.
#[derive(Component)]
pub struct ContainerSlot {
    pub offset: Vec3,
}

/// The Mixing Chamber's second slot. Static geometry like [`ContainerSlot`],
/// so it is derived locally by `lab::dress_machines` rather than replicated —
/// see that component's own doc comment for why.
#[derive(Component)]
pub struct ContainerSlotB {
    pub offset: Vec3,
}

/// The unit vector out of a machine's working face — the side a chemist stands
/// on to use it.
///
/// Set by `lab::dress_machines` from the same `fit` that decides the casing, so
/// it cannot drift from where the machine actually points. Everything a machine
/// hands back is placed along it: with the direction hardcoded to `+Z`, that
/// only ever pointed into the room for the two machines on the hall's north
/// wall, and the grinder — which faces `-Z` — ejected its beaker through the
/// storeroom wall.
#[derive(Component, Clone, Copy)]
pub struct Facing(pub Vec3);

/// How many items a [`MachineKind::Locker`] holds.
///
/// A limit at all, rather than none, so the contents stay a list a player can
/// read at a glance instead of a scrolling inventory screen — which is the kind
/// of UI this game has deliberately avoided everywhere else.
pub const LOCKER_CAPACITY: usize = 12;

/// The Mixing Chamber's internal buffer, shared by both of its beaker slots.
#[derive(Component, Serialize, Deserialize)]
pub struct Buffer(pub Solution);

/// Produce waiting to be ground.
///
/// Separate from the machine's [`ContainerSlot`], which holds the beaker the
/// extract runs into: the grinder needs both loaded at once, and a single slot
/// would mean swapping the beaker out for every plant.
#[derive(Component, Default, Serialize, Deserialize)]
pub struct Hopper(pub Vec<ProduceId>);

/// How much a dispenser gives per press. Persists between visits.
#[derive(Component, Serialize, Deserialize)]
pub struct DispenseAmount(pub Units);

impl Default for DispenseAmount {
    fn default() -> Self {
        DispenseAmount(Units::whole(10))
    }
}

/// The analyzer's most recent separation result.
///
/// Replicated with the machine so both chemists see the same yield accounting,
/// including the reject beaker that the authority placed beside it.  Keeping
/// the report after the click also lets the result remain readable after the
/// source solution has already changed.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HplcReport {
    pub source: ReagentId,
    pub product: ReagentId,
    pub input_amount: Units,
    pub product_amount: Units,
    pub reject_amount: Units,
    pub input_purity: f32,
    pub product_purity: f32,
    pub recovered_inverse: bool,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct DispenseRequested {
    #[entities]
    pub machine: Entity,
    pub reagent: ReagentId,
}

/// Reactions that just took place in a container.
///
/// This is the hook recipe discovery hangs off: the resolver already reports
/// which reactions fired, so learning is a matter of noticing, not of
/// re-deriving anything.
///
/// It carries the side effects too. `ResolveReport` has always reported smoke
/// and explosions and nothing ever read them; routing them through the message
/// every caller of `Container::mutate` already sends means hazards need no
/// parallel plumbing of their own.
#[derive(Message)]
pub struct ReactionsFired {
    pub reactions: Vec<chem_sim::ReactionId>,
    /// Where it happened, so a blast lands in the right part of the room.
    pub container: Entity,
    pub effects: Vec<chem_sim::ReactionEffect>,
    /// Distinct reagents present the instant this fired, straight from
    /// `ResolveReport::distinct_reagents` — `knowledge::learn_from_experiments`
    /// is the only reader, and only for deciding whether an experiment was
    /// focused enough to actually teach something. `0` for the analyzer's own
    /// path (below), which never runs the resolver and has nothing to be
    /// "crowded": identifying a sample already in hand is a deliberate,
    /// single-purpose check, not a shotgun mix, so it is always credited.
    pub distinct_reagents: usize,
}

impl ReactionsFired {
    /// Builds a report for `container`, or `None` if nothing worth announcing
    /// happened. Keeps the "did anything happen?" test in one place now that
    /// there are two ways for the answer to be yes.
    pub(crate) fn from_report(container: Entity, report: &chem_sim::ResolveReport) -> Option<Self> {
        if !report.reacted() && report.effects.is_empty() {
            return None;
        }
        Some(ReactionsFired {
            reactions: report.fired_reactions(),
            container,
            effects: report.effects.clone(),
            distinct_reagents: report.distinct_reagents,
        })
    }
}

#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct EjectRequested {
    #[entities]
    pub machine: Entity,
    pub slot: MachineSlot,
}

#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct EmptyRequested {
    #[entities]
    pub machine: Entity,
    pub slot: MachineSlot,
}

/// Take one named item back out of a locker.
///
/// Names the item as well as the locker, unlike [`EjectRequested`], because a
/// locker holds many things and the panel row the player clicked is the only
/// thing that knows which one they meant. Both ids are mapped on the way over;
/// the server checks they belong together rather than acting on either alone.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct TakeRequested {
    #[entities]
    pub machine: Entity,
    #[entities]
    pub item: Entity,
}

/// Which way reagent moves between the loaded container and the buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BufferDirection {
    ToBuffer,
    ToContainer,
}

/// Which of a machine's container slots a request means.
///
/// Every machine but the [`MachineKind::MixingChamber`] has exactly one slot,
/// so `A` is the only value their panels ever send — this exists at all so the
/// Mixing Chamber's second beaker can be addressed without the request
/// silently acting on whichever container happens to occupy slot A.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum MachineSlot {
    #[default]
    A,
    B,
}

#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct BufferTransferRequested {
    #[entities]
    pub machine: Entity,
    pub reagent: ReagentId,
    pub amount: Units,
    pub direction: BufferDirection,
    pub slot: MachineSlot,
}

/// Which loaded beaker is poured completely into the other when agitation
/// begins. Recipe preparation sides are orientation-independent; the direction
/// only decides which physical beaker is left holding the batch.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum AgitateDirection {
    AToB,
    BToA,
}

impl AgitateDirection {
    pub fn destination(self) -> MachineSlot {
        match self {
            Self::AToB => MachineSlot::B,
            Self::BToA => MachineSlot::A,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AToB => "A -> B",
            Self::BToA => "B -> A",
        }
    }
}

/// Starts the Mixing Chamber's staged, timed combination. The authority
/// reconstructs both slot occupants and captures provenance before it moves a
/// drop; no client-supplied ingredient list is trusted.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct AgitateRequested {
    #[entities]
    pub machine: Entity,
    pub direction: AgitateDirection,
}

#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct PackageRequested {
    #[entities]
    pub machine: Entity,
    pub kind: ContainerKind,
}

/// Run the loaded sample through the analyzer and work out how it was made.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct AnalyzeRequested {
    #[entities]
    pub machine: Entity,
}

/// Separate and refine one named portion in the analyzer's HPLC mode.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct PurifyRequested {
    #[entities]
    pub machine: Entity,
    pub reagent: ReagentId,
}

/// Break produce down into the loaded beaker.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct GrindRequested {
    #[entities]
    pub machine: Entity,
    /// Work through the whole hopper rather than one item.
    pub all: bool,
}

/// Dial the chamber to a temperature.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct SetTargetTemperature {
    #[entities]
    pub machine: Entity,
    /// `Kelvin` is a newtype over `f32` and needs no special wire encoding.
    /// The `deserialize_any` problem that `Units` has is specific to "is `15`
    /// an integer or a float", which a float does not have.
    pub target: Kelvin,
}

/// Switch the chamber on or off.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct SetHeaterPower {
    #[entities]
    pub machine: Entity,
    pub on: bool,
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Finds the container loaded into `machine`, if any.
pub fn slotted_container(machine: Entity, slotted: &Query<(Entity, &InSlot)>) -> Option<Entity> {
    slotted
        .iter()
        .find(|(_, slot)| slot.0 == machine)
        .map(|(entity, _)| entity)
}

/// The same, for a [`MachineKind::MixingChamber`]'s second slot.
pub fn slotted_container_b(machine: Entity, slotted: &Query<(Entity, &InSlotB)>) -> Option<Entity> {
    slotted
        .iter()
        .find(|(_, slot)| slot.0 == machine)
        .map(|(entity, _)| entity)
}

/// Using a machine with a beaker in hand loads it; using one empty-handed
/// opens the panel. That matches how SS13 plays and avoids a separate
/// "insert" control.
///
/// Produce follows the same rule but only at the grinder. Loading keys off
/// *what* is in hand rather than merely that something is, because a plant
/// dropped into the dispenser's beaker slot would sit there doing nothing with
/// no way to tell the player why.
#[allow(clippy::too_many_arguments)]
fn handle_machine_interact(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    mut machines: Query<(
        &mut Machine,
        Option<&ContainerSlot>,
        Option<&ContainerSlotB>,
        &Transform,
    )>,
    mut hoppers: Query<&mut Hopper>,
    mut modes: Query<&mut InteractionMode>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<(Entity, &HeldBy)>,
    produce: Query<&Produce>,
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    stored: Query<(Entity, &Stored)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    mut opened: MessageWriter<ToClients<MachineOpened>>,
) {
    for request in requests.read() {
        // The sender's identity comes from the connection, never from the
        // message. A client that could name its own player entity could act as
        // the other chemist.
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        // A collapsed or chemically incapacitated chemist cannot work a
        // machine. Checked before anything is claimed or loaded so forged
        // input cannot operate the lab while its owner is unconscious.
        if bodies.get(player).is_ok_and(|body| body.0.collapsed)
            || bloodstreams
                .get(player)
                .is_ok_and(|blood| blood.0.incapacitated())
        {
            continue;
        }
        let Ok((mut machine, slot, slot_b, transform)) = machines.get_mut(request.target) else {
            continue;
        };

        let carrying = held
            .iter()
            .find(|(_, holder)| holder.0 == player)
            .map(|(entity, _)| entity);

        // A locker takes whatever is in hand and asks nothing about it. That
        // is deliberately unlike the two branches below, which both key off
        // *what* the player is carrying: storage is the one thing in the lab
        // that should keep working for items that do not exist yet.
        if machine.kind == MachineKind::Locker {
            if let Some(item) = carrying {
                let room = stored
                    .iter()
                    .filter(|(_, shut_in)| shut_in.0 == request.target)
                    .count()
                    < LOCKER_CAPACITY;
                if room {
                    commands
                        .entity(item)
                        .remove::<HeldBy>()
                        .remove::<InventorySlot>()
                        .remove::<ChildOf>()
                        .insert(Stored(request.target))
                        // Parked inside the casing. It is hidden while stored,
                        // so this only matters for the frame it comes back out
                        // on and for anything that reads a position regardless.
                        .insert(Transform::from_translation(transform.translation));
                    continue;
                }
                // Full. Falls through to open the panel, which says so — a
                // locker that silently refused would read as a dead keypress.
            }
        }

        // Produce into the hopper. The item is consumed on load rather than
        // parked in the machine, so the hopper is a list of kinds and nothing
        // has to track entities the player can no longer reach.
        if let Some(item) = carrying {
            if let Ok(kind) = produce.get(item) {
                if let Ok(mut hopper) = hoppers.get_mut(request.target) {
                    hopper.0.push(kind.0);
                    commands.entity(item).despawn();
                    continue;
                }
            }
        }

        let loading = carrying.filter(|item| !produce.contains(*item));

        // Slot A first; slot B only exists on the Mixing Chamber, and only
        // comes into play once A is already taken — that is what lets it hold
        // two beakers at once instead of forcing an eject to swap one in.
        let free_slot = match (slot, slot_b) {
            (Some(slot), _) if slotted_container(request.target, &slotted).is_none() => {
                Some((slot.offset, false))
            }
            (_, Some(slot_b)) if slotted_container_b(request.target, &slotted_b).is_none() => {
                Some((slot_b.offset, true))
            }
            _ => None,
        };

        match (loading, free_slot) {
            (Some(container), Some((offset, in_b))) => {
                let mut item = commands.entity(container);
                item.remove::<HeldBy>()
                    .remove::<InventorySlot>()
                    .remove::<ChildOf>()
                    .insert(Transform::from_translation(transform.translation + offset));
                if in_b {
                    item.insert(InSlotB(request.target));
                } else {
                    item.insert(InSlot(request.target));
                }
            }
            _ => {
                if !machine.available_to(player) {
                    continue;
                }
                machine.in_use_by = Some(player);
                if let Ok(mut mode) = modes.get_mut(player) {
                    *mode = InteractionMode::UsingMachine(request.target);
                }
                // The claim is granted here but the panel lives on the asking
                // client, and `InteractionMode` is deliberately local. Telling
                // them is what actually opens it.
                opened.write(ToClients {
                    targets: SendTargets::Single(request.client_id),
                    message: MachineOpened {
                        machine: request.target,
                    },
                });
            }
        }
    }
}

/// Releases a chemist's claim when they close their panel.
///
/// The server's own copy of `InteractionMode` is cleared alongside the
/// machine: leaving them to disagree would mean a chemist the server still
/// believes is at the dispenser, unable to open anything else.
fn handle_leave_machine(
    mut requests: MessageReader<FromClient<LeaveMachineRequested>>,
    mut machines: Query<&mut Machine>,
    mut modes: Query<&mut InteractionMode>,
    chemists: Query<(Entity, &Chemist)>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok(mut mode) = modes.get_mut(player) else {
            continue;
        };
        // Not a bare `UsingMachine` match: on a listening host this component
        // *is* the local player's, so it can be `ReadingBook` over a machine
        // the claim is still theirs.
        if let Some(machine) = mode.claimed_machine() {
            if let Ok(mut machine) = machines.get_mut(machine) {
                if machine.in_use_by == Some(player) {
                    machine.in_use_by = None;
                }
            }
        }
        *mode = InteractionMode::Roaming;
    }
}

fn handle_thermostat_controls(
    mut targets: MessageReader<FromClient<SetTargetTemperature>>,
    mut power: MessageReader<FromClient<SetHeaterPower>>,
    mut thermostats: Query<&mut Thermostat>,
    machines: Query<&Machine>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
) {
    for request in targets.read() {
        if authorized_machine_actor(
            request.client_id,
            machines.get(request.machine).ok(),
            &[MachineKind::ReactionChamber],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        if let Ok(mut thermostat) = thermostats.get_mut(request.machine) {
            // The panel's slider already clamps to this range, but a request
            // is trusted input from the network — the server has to hold the
            // same line rather than merely suggesting it.
            thermostat.target = Kelvin(request.target.0.clamp(TEMPERATURE_MIN, TEMPERATURE_MAX));
        }
    }
    for request in power.read() {
        if authorized_machine_actor(
            request.client_id,
            machines.get(request.machine).ok(),
            &[MachineKind::ReactionChamber],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        if let Ok(mut thermostat) = thermostats.get_mut(request.machine) {
            thermostat.powered = request.on;
        }
    }
}

/// Matches `ui::panel_temperature`'s own rounding, so replication wakes up
/// exactly as often as the panel readout can actually show — no coarser
/// (which would visibly lag the display) and no finer (which would just be
/// wire traffic for a digit nobody sees change).
fn temperature_bucket(kelvin: Kelvin) -> i32 {
    (kelvin.0 / 5.0).round() as i32
}

/// Drives a loaded container toward its chamber's target temperature.
///
/// Nothing here knows what a recipe is. Every change goes through
/// [`Container::mutate`], which resolves — and `Reaction::max_scale` has always
/// checked `min_temp`/`max_temp`. So crossing a threshold *is* the trigger, and
/// temperature-gating a recipe costs no gating code at all.
fn apply_thermostats(
    time: Res<Time>,
    db: Res<ChemDb>,
    mut fired: MessageWriter<ReactionsFired>,
    chambers: Query<(Entity, &Thermostat, &Machine)>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
) {
    let dt = time.delta_secs();
    for (machine, thermostat, state) in &chambers {
        if !thermostat.powered || state.disabled_for > 0.0 {
            continue;
        }
        let Some(target) = slotted_container(machine, &slotted) else {
            continue;
        };
        let Ok(mut container) = containers.get_mut(target) else {
            continue;
        };

        let current = container.solution.temperature;
        let next = approach(current, thermostat.target, CHAMBER_RATE, dt);
        // Settled. Going through `mutate` anyway would re-resolve and mark the
        // component changed every frame, which wakes the panel and replication
        // for nothing.
        if next == current {
            continue;
        }

        // `mutate` still runs at full precision every frame, so a reaction
        // threshold is crossed on the exact frame it happens — only whether
        // the *component* gets marked changed (and so re-replicated) is
        // throttled, down to the same 5K bucket the panel already rounds to.
        let bucket_before = temperature_bucket(current);
        let quiet = container.bypass_change_detection();
        let (_, report) = quiet.mutate(&db, |solution| solution.temperature = next);
        let reacted = report.reacted() || !report.effects.is_empty();
        if let Some(message) = ReactionsFired::from_report(target, &report) {
            fired.write(message);
        }
        if reacted || temperature_bucket(next) != bucket_before {
            container.set_changed();
        }
    }
}

/// Everything not in a powered chamber drifts back to room temperature.
///
/// Without this a beaker heated once stays hot for the rest of the shift, and
/// the chamber becomes a one-time switch rather than something you have to
/// work against.
fn cool_to_ambient(
    time: Res<Time>,
    db: Res<ChemDb>,
    mut fired: MessageWriter<ReactionsFired>,
    heating: Query<(Entity, &Thermostat)>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<(Entity, &mut Container)>,
) {
    let dt = time.delta_secs();
    // Whatever a powered chamber is actively holding is exempt.
    let held_hot: Vec<Entity> = heating
        .iter()
        .filter(|(_, thermostat)| thermostat.powered)
        .filter_map(|(machine, _)| slotted_container(machine, &slotted))
        .collect();

    for (entity, mut container) in &mut containers {
        if held_hot.contains(&entity) || container.solution.is_empty() {
            continue;
        }
        let current = container.solution.temperature;
        let next = approach(current, chem_sim::Kelvin::AMBIENT, AMBIENT_RATE, dt);
        if next == current {
            continue;
        }

        // Same reasoning as `apply_thermostats`: this runs for every
        // not-yet-ambient container in the lab, so an idle beaker slowly
        // cooling on a shelf would otherwise wake replication every frame
        // for a value the panel doesn't render any finer than 5K anyway.
        let bucket_before = temperature_bucket(current);
        let quiet = container.bypass_change_detection();
        let (_, report) = quiet.mutate(&db, |solution| solution.temperature = next);
        let reacted = report.reacted() || !report.effects.is_empty();
        if let Some(message) = ReactionsFired::from_report(entity, &report) {
            fired.write(message);
        }
        if reacted || temperature_bucket(next) != bucket_before {
            container.set_changed();
        }
    }
}

// ---------------------------------------------------------------------------
// Reactions that take time
// ---------------------------------------------------------------------------

/// An agitation run owned by one Mixing Chamber.
///
/// Unlike [`Reacting`], this is replicated. An agitated recipe cannot be
/// inferred from the combined solution alone: the [`ReactionActivation`] is
/// the proof that its sides were in separate beakers immediately before the
/// transfer. Keeping that proof on the machine also gives every client a
/// stable source for the progress and remaining-time readout.
#[derive(Component, Clone, Debug, Serialize, Deserialize, MapEntities)]
pub struct AgitationRun {
    #[entities]
    pub destination: Entity,
    pub direction: AgitateDirection,
    pub activation: ReactionActivation,
    pub elapsed_secs: f32,
    pub expected_secs: f32,
    /// Unprocessed wall time. Replicated with the run so authority migration
    /// or a joining observer sees one coherent clock state.
    pub chemistry_accumulator_secs: f32,
    /// Authority-side report aggregation. These fields cross the wire with the
    /// component because a joining client needs one coherent serialisable
    /// state, but only [`tick_reactions`] reads or writes them.
    reactions: Vec<chem_sim::ReactionId>,
    effects: Vec<chem_sim::ReactionEffect>,
    distinct_reagents: usize,
}

impl AgitationRun {
    pub fn progress(&self) -> f32 {
        if self.expected_secs <= f32::EPSILON {
            1.0
        } else {
            (self.elapsed_secs / self.expected_secs).clamp(0.0, 1.0)
        }
    }

    pub fn remaining_secs(&self) -> f32 {
        (self.expected_secs - self.elapsed_secs).max(0.0)
    }
}

/// A batch part-way through, and what it has done so far.
///
/// Only reactions that name a `rate` in `chem.reactions.ron` can produce one:
/// everything else still completes inside the `Container::mutate` that started
/// it, and never touches this at all.
///
/// It exists to answer two questions that a per-frame report cannot:
///
/// 1. **What was in the beaker when this started?** `distinct_reagents` is how
///    `knowledge::learn_from_experiments` tells a focused experiment from a
///    shotgun dump, and the resolver eats intermediates as it goes — so read
///    mid-run it flatters a dump that would have been rejected on frame one.
/// 2. **Has it already been announced?** A four-second reaction is sixty
///    frames, and sixty `ReactionsFired` means sixty smoke clouds and sixty
///    "too much going on in that beaker" lines.
///
/// Deliberately **not** replicated, and deliberately not what anything asks
/// "is this batch finished?". That question is answered by
/// `chem_sim::is_reacting` from the solution and the chemistry — both of which
/// a guest already has — so the panel and the delivery window get the same
/// answer on both ends without a component that changes every frame of every
/// batch going on the wire. This is purely the authority's bookkeeping.
#[derive(Component)]
pub struct Reacting {
    /// Distinct reagents present on the frame the run began.
    distinct_reagents: usize,
    /// Every reaction that has fired during this run, in order of first firing.
    reactions: Vec<chem_sim::ReactionId>,
    /// Side effects accumulated across the run, emitted once at the end.
    ///
    /// Held back rather than emitted per step because `Smoke` and `Explosion`
    /// do not scale with how much reacted — they are "this happened" flags —
    /// so a stepped reaction would vent a full cloud every frame it ran.
    effects: Vec<chem_sim::ReactionEffect>,
    /// Same fixed-step clock as [`AgitationRun`]. Ambient timed recipes use the
    /// identical path so adding one later cannot reintroduce frame-rate drift.
    chemistry_accumulator_secs: f32,
}

/// Advances every beaker in the lab by one frame of chemistry.
///
/// Does nothing at all for the instant recipes, which are already finished by
/// the time this sees them. What it is for is the rated ones: a batch that
/// takes real seconds is a batch you can stand and watch, pull out of the
/// chamber halfway, or be interrupted during — which is a verb this game did
/// not have when every recipe was one click.
///
/// The Mixing Chamber's buffer is deliberately **not** stepped. Reagents in the
/// buffer are held separated on purpose — that is the whole point of the
/// machine, and it has never run the resolver — so reacting them there would
/// break the one tool the player has for cleaning up a contaminated batch.
fn accumulate_reaction_report(
    reactions: &mut Vec<chem_sim::ReactionId>,
    effects: &mut Vec<chem_sim::ReactionEffect>,
    report: chem_sim::ResolveReport,
) -> bool {
    let changed = report.reacted() || !report.effects.is_empty();
    for reaction in report.fired_reactions() {
        if !reactions.contains(&reaction) {
            reactions.push(reaction);
        }
    }
    effects.extend(report.effects);
    changed
}

fn tick_reactions(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    mut fired: MessageWriter<ReactionsFired>,
    mut agitations: Query<(Entity, &mut AgitationRun)>,
    mut containers: Query<(Entity, &mut Container, Option<&mut Reacting>)>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    // Agitated destinations use the provenance-aware resolver and must not be
    // stepped again by the ambient pass below. The token is retained for the
    // whole run; recreating it from the combined liquid would make ordinary
    // pouring an unintended shortcut.
    let mut agitated_destinations = Vec::new();
    for (machine, mut run) in &mut agitations {
        let destination = run.destination;
        agitated_destinations.push(destination);
        let Ok((_, mut container, _)) = containers.get_mut(destination) else {
            commands.entity(machine).remove::<AgitationRun>();
            continue;
        };

        if container.solution.is_empty() {
            commands.entity(machine).remove::<AgitationRun>();
            continue;
        }

        // The panel only ever compares `AgitationRun` at 0.1s resolution (see
        // `PanelSignature`'s own `elapsed_secs * 10.0`), so the clock advance
        // below is authority-only bookkeeping — same reasoning as
        // `apply_thermostats` applies to `Container`'s temperature. Marking
        // the whole component changed every frame would otherwise wake
        // replication for the full 4-8s of every batch for no visible gain.
        let displayed_tenths_before = (run.elapsed_secs * 10.0).floor() as i32;
        let state = run.bypass_change_detection();
        state.elapsed_secs += dt;
        state.chemistry_accumulator_secs += dt;
        let mut finished = false;
        let mut changed = false;
        while state.chemistry_accumulator_secs + CHEMISTRY_QUANTUM_EPSILON >= CHEMISTRY_QUANTUM_SECS
        {
            state.chemistry_accumulator_secs =
                (state.chemistry_accumulator_secs - CHEMISTRY_QUANTUM_SECS).max(0.0);
            let solution = &mut container.bypass_change_detection().solution;
            let report = chem_sim::resolve_step_with_activation(
                solution,
                &db.reactions,
                CHEMISTRY_QUANTUM_SECS,
                &state.activation,
            );
            changed |= accumulate_reaction_report(&mut state.reactions, &mut state.effects, report);
            finished =
                !chem_sim::is_reacting_with_activation(solution, &db.reactions, &state.activation);
            if finished {
                break;
            }
        }
        if changed {
            container.set_changed();
        }
        // Wake replication only when something a client can actually see
        // moved: a reaction fired/finished, or the displayed tenths-of-a-
        // second advanced.
        if changed || finished {
            run.set_changed();
        } else {
            let displayed_tenths_after = (run.elapsed_secs * 10.0).floor() as i32;
            if displayed_tenths_after != displayed_tenths_before {
                run.set_changed();
            }
        }
        if finished {
            if !run.reactions.is_empty() || !run.effects.is_empty() {
                fired.write(ReactionsFired {
                    reactions: std::mem::take(&mut run.reactions),
                    container: destination,
                    effects: std::mem::take(&mut run.effects),
                    distinct_reagents: run.distinct_reagents,
                });
            }
            commands.entity(machine).remove::<AgitationRun>();
        }
    }

    for (entity, mut container, run) in &mut containers {
        if agitated_destinations.contains(&entity) {
            continue;
        }
        let Some(mut run) = run else {
            if container.solution.is_empty() {
                continue;
            }

            // Pick up any instant chemistry first without advancing a rated
            // recipe. A raw solution can reach this system in tests/tools;
            // normal gameplay mutations already perform the same zero-step.
            let solution = &mut container.bypass_change_detection().solution;
            let report = chem_sim::resolve_step(solution, &db.reactions, 0.0);
            let distinct_reagents = report.distinct_reagents;
            let mut reactions = Vec::new();
            let mut effects = Vec::new();
            let mut changed = accumulate_reaction_report(&mut reactions, &mut effects, report);
            let mut accumulator = dt;
            let mut still_reacting = chem_sim::is_reacting(solution, &db.reactions);

            while still_reacting
                && accumulator + CHEMISTRY_QUANTUM_EPSILON >= CHEMISTRY_QUANTUM_SECS
            {
                accumulator = (accumulator - CHEMISTRY_QUANTUM_SECS).max(0.0);
                let report =
                    chem_sim::resolve_step(solution, &db.reactions, CHEMISTRY_QUANTUM_SECS);
                changed |= accumulate_reaction_report(&mut reactions, &mut effects, report);
                still_reacting = chem_sim::is_reacting(solution, &db.reactions);
            }

            if changed {
                container.set_changed();
            }
            if still_reacting {
                commands.entity(entity).insert(Reacting {
                    distinct_reagents,
                    reactions,
                    effects,
                    chemistry_accumulator_secs: accumulator,
                });
            } else if !reactions.is_empty() || !effects.is_empty() {
                fired.write(ReactionsFired {
                    reactions,
                    container: entity,
                    effects,
                    distinct_reagents,
                });
            }
            continue;
        };

        // An empty beaker cannot react, but the interrupted run must still be
        // closed now rather than attaching its report to whatever is poured in
        // next.
        if container.solution.is_empty() {
            if !run.reactions.is_empty() || !run.effects.is_empty() {
                fired.write(ReactionsFired {
                    reactions: std::mem::take(&mut run.reactions),
                    container: entity,
                    effects: std::mem::take(&mut run.effects),
                    distinct_reagents: run.distinct_reagents,
                });
            }
            commands.entity(entity).remove::<Reacting>();
            continue;
        }

        run.chemistry_accumulator_secs += dt;
        let solution = &mut container.bypass_change_detection().solution;
        let mut still_reacting = chem_sim::is_reacting(solution, &db.reactions);
        let mut changed = false;
        while still_reacting
            && run.chemistry_accumulator_secs + CHEMISTRY_QUANTUM_EPSILON >= CHEMISTRY_QUANTUM_SECS
        {
            run.chemistry_accumulator_secs =
                (run.chemistry_accumulator_secs - CHEMISTRY_QUANTUM_SECS).max(0.0);
            let report = chem_sim::resolve_step(solution, &db.reactions, CHEMISTRY_QUANTUM_SECS);
            let state: &mut Reacting = &mut run;
            changed |= accumulate_reaction_report(&mut state.reactions, &mut state.effects, report);
            still_reacting = chem_sim::is_reacting(solution, &db.reactions);
        }
        if changed {
            container.set_changed();
        }
        if still_reacting {
            continue;
        }

        if !run.reactions.is_empty() || !run.effects.is_empty() {
            fired.write(ReactionsFired {
                reactions: std::mem::take(&mut run.reactions),
                container: entity,
                effects: std::mem::take(&mut run.effects),
                distinct_reagents: run.distinct_reagents,
            });
        }
        commands.entity(entity).remove::<Reacting>();
    }
}

/// Resolves a connection to the chemist it drives.
pub fn chemist_entity(chemists: &Query<(Entity, &Chemist)>, client: ClientId) -> Option<Entity> {
    chemists
        .iter()
        .find(|(_, chemist)| chemist.client == client)
        .map(|(entity, _)| entity)
}

/// Resolves and authorizes one machine control request.
///
/// The connection supplies identity; the message supplies only intent. Every
/// action handler calls this before mutation so delayed or forged requests
/// cannot operate a machine after its claim moved to another player, cannot
/// use the wrong machine kind, and cannot act while incapacitated.
fn authorized_machine_actor(
    client: ClientId,
    machine: Option<&Machine>,
    allowed: &[MachineKind],
    chemists: &Query<(Entity, &Chemist)>,
    bodies: &Query<&crate::body::Body>,
    bloodstreams: &Query<&crate::body::Bloodstream>,
) -> Option<Entity> {
    // Functional unit tests historically send trusted authority messages
    // without constructing the player/claim fixture. Release builds have no
    // bypass; peer-boundary tests use a real ClientId and exercise this path.
    #[cfg(test)]
    if client == ClientId::Server {
        return chemist_entity(chemists, client).or(Some(Entity::PLACEHOLDER));
    }

    let player = chemist_entity(chemists, client)?;
    let machine = machine?;
    if machine.disabled_for > 0.0
        || !allowed.contains(&machine.kind)
        || machine.in_use_by != Some(player)
    {
        return None;
    }
    if bodies.get(player).is_ok_and(|body| body.0.collapsed)
        || bloodstreams
            .get(player)
            .is_ok_and(|blood| blood.0.incapacitated())
    {
        return None;
    }
    Some(player)
}

fn handle_dispense(
    db: Res<ChemDb>,
    knowledge: Res<Knowledge>,
    mut requests: MessageReader<FromClient<DispenseRequested>>,
    mut fired: MessageWriter<ReactionsFired>,
    machines: Query<&DispenseAmount>,
    machine_states: Query<&Machine>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    transforms: Query<&Transform>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    for request in requests.read() {
        if authorized_machine_actor(
            request.client_id,
            machine_states.get(request.machine).ok(),
            &[MachineKind::ChemMaster5000],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        // The panel only ever offers unlocked reagents as live buttons, but
        // a request is trusted input from the network — refusing a locked
        // one here is what actually enforces the lock rather than merely
        // suggesting it.
        if !knowledge.is_reagent_unlocked(&db, request.reagent) {
            continue;
        }
        let Ok(amount) = machines.get(request.machine) else {
            continue;
        };
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok(mut container) = containers.get_mut(target) else {
            continue;
        };
        let reagent = db.reagents.get(request.reagent);
        let (overflow, report) = container.mutate(&db, |solution| match reagent.key.as_str() {
            "acidic_buffer" | "basic_buffer" if !solution.is_empty() => {
                let resulting = solution.total_volume() + amount.0;
                let movement = amount.0.as_f32() / resulting.as_f32() * 30.0;
                solution.shift_ph(if reagent.key == "acidic_buffer" {
                    -movement
                } else {
                    movement
                });
                Units::ZERO
            }
            "acidic_buffer" | "basic_buffer" => amount.0,
            _ => solution.add_profiled(request.reagent, amount.0, 1.0, reagent.ph),
        });
        if let Some(message) = ReactionsFired::from_report(target, &report) {
            fired.write(message);
        }
        if overflow != amount.0 {
            if let Ok(transform) = transforms.get(request.machine) {
                emit_world_sfx(&mut sounds, Sfx::DispensePour, transform.translation);
            }
        }
    }
}

fn expected_agitation_secs(
    solution: &Solution,
    activation: &ReactionActivation,
    db: &ChemDb,
) -> f32 {
    activation
        .reactions()
        .filter_map(|id| {
            let reaction = db.reactions.get(id);
            let scale = reaction.max_scale(solution)?;
            let rate = reaction.rate?;
            rate.is_positive().then(|| scale.as_f32() / rate.as_f32())
        })
        .fold(0.0_f32, f32::max)
        .max(0.01)
}

/// Captures the two-side provenance and pours the complete source beaker into
/// the destination. No liquid moves unless all validation succeeds, so a
/// wrong-side attempt or an undersized destination is harmless and retryable.
#[allow(clippy::too_many_arguments)]
fn handle_agitate(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<AgitateRequested>>,
    machines: Query<(&Machine, Option<&AgitationRun>)>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    mut containers: Query<(Entity, &mut Container, Has<Reacting>)>,
) {
    for request in requests.read() {
        let Ok((machine, active)) = machines.get(request.machine) else {
            continue;
        };
        if authorized_machine_actor(
            request.client_id,
            Some(machine),
            &[MachineKind::MixingChamber],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
            || active.is_some()
        {
            continue;
        }

        let Some(slot_a) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Some(slot_b) = slotted_container_b(request.machine, &slotted_b) else {
            continue;
        };
        if slot_a == slot_b {
            continue;
        }
        let (source, destination) = match request.direction {
            AgitateDirection::AToB => (slot_a, slot_b),
            AgitateDirection::BToA => (slot_b, slot_a),
        };
        let Ok(
            [(_, mut source_container, source_reacting), (_, mut destination_container, destination_reacting)],
        ) = containers.get_many_mut([source, destination])
        else {
            continue;
        };

        if !matches!(
            source_container.kind,
            ContainerKind::Beaker | ContainerKind::LargeBeaker
        ) || !matches!(
            destination_container.kind,
            ContainerKind::Beaker | ContainerKind::LargeBeaker
        ) {
            continue;
        }

        // A partly running ambient batch has its own accumulated report. Do
        // not splice a second process into it and lose attribution for either.
        if source_reacting
            || destination_reacting
            || chem_sim::is_reacting(&source_container.solution, &db.reactions)
            || chem_sim::is_reacting(&destination_container.solution, &db.reactions)
        {
            continue;
        }
        let source_volume = source_container.solution.total_volume();
        let destination_volume = destination_container.solution.total_volume();
        if !source_volume.is_positive()
            || destination_container.solution.available_volume() < source_volume
        {
            continue;
        }

        // This is the security boundary for staged chemistry: provenance is
        // inspected before combination and retained as an opaque activation.
        let activation = db
            .reactions
            .activate_agitation(&source_container.solution, &destination_container.solution);
        if activation.is_empty() {
            continue;
        }

        let combined_temperature = Kelvin(
            (source_container.solution.temperature.0 * source_volume.as_f32()
                + destination_container.solution.temperature.0 * destination_volume.as_f32())
                / (source_volume + destination_volume).as_f32(),
        );
        let moved = source_container
            .solution
            .transfer_to(&mut destination_container.solution, source_volume);
        if moved != source_volume {
            // Capacity was checked before mutation, so this is defensive. The
            // transfer API puts rejected liquid back in the source.
            continue;
        }
        destination_container.solution.temperature = combined_temperature;
        source_container.set_changed();
        destination_container.set_changed();

        let expected_secs =
            expected_agitation_secs(&destination_container.solution, &activation, &db);
        let distinct_reagents = destination_container.solution.len();
        commands.entity(request.machine).insert(AgitationRun {
            destination,
            direction: request.direction,
            activation,
            elapsed_secs: 0.0,
            expected_secs,
            chemistry_accumulator_secs: 0.0,
            reactions: Vec::new(),
            effects: Vec::new(),
            distinct_reagents,
        });
    }
}

fn handle_buffer_transfer(
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<BufferTransferRequested>>,
    mut fired: MessageWriter<ReactionsFired>,
    mut buffers: Query<&mut Buffer>,
    machine_states: Query<&Machine>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    transforms: Query<&Transform>,
    active: Query<(), With<AgitationRun>>,
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    mut containers: Query<&mut Container>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    for request in requests.read() {
        if authorized_machine_actor(
            request.client_id,
            machine_states.get(request.machine).ok(),
            &[MachineKind::MixingChamber],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        if active.contains(request.machine) {
            continue;
        }
        let Ok(mut buffer) = buffers.get_mut(request.machine) else {
            continue;
        };
        let target = match request.slot {
            MachineSlot::A => slotted_container(request.machine, &slotted),
            MachineSlot::B => slotted_container_b(request.machine, &slotted_b),
        };
        let Some(target) = target else {
            continue;
        };
        let Ok(mut container) = containers.get_mut(target) else {
            continue;
        };

        let moved_any = match request.direction {
            BufferDirection::ToBuffer => {
                // Pulling a single named reagent out of a mixture is the whole
                // point of the Mixing Chamber: it is how a contaminated batch
                // gets cleaned up before it goes in a pill.
                let purity = container.solution.purity_of(request.reagent);
                let ph = container.solution.reagent_ph(request.reagent);
                let moved = container.solution.remove(request.reagent, request.amount);
                let overflow = buffer.0.add_profiled(request.reagent, moved, purity, ph);
                if overflow.is_positive() {
                    let _ = container
                        .solution
                        .add_profiled(request.reagent, overflow, purity, ph);
                }
                moved != overflow
            }
            BufferDirection::ToContainer => {
                let purity = buffer.0.purity_of(request.reagent);
                let ph = buffer.0.reagent_ph(request.reagent);
                let moved = buffer.0.remove(request.reagent, request.amount);
                let (overflow, report) = container.mutate(&db, |solution| {
                    solution.add_profiled(request.reagent, moved, purity, ph)
                });
                if overflow.is_positive() {
                    let _ = buffer.0.add_profiled(request.reagent, overflow, purity, ph);
                }
                if let Some(message) = ReactionsFired::from_report(target, &report) {
                    fired.write(message);
                }
                moved != overflow
            }
        };
        if moved_any {
            if let Ok(transform) = transforms.get(request.machine) {
                emit_world_sfx(&mut sounds, Sfx::BufferTransfer, transform.translation);
            }
        }
    }
}

fn handle_package(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<PackageRequested>>,
    mut machines: Query<(&mut Buffer, &Transform)>,
    machine_states: Query<&Machine>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    for request in requests.read() {
        if authorized_machine_actor(
            request.client_id,
            machine_states.get(request.machine).ok(),
            &[MachineKind::MixingChamber],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        let Ok((mut buffer, transform)) = machines.get_mut(request.machine) else {
            continue;
        };
        if !matches!(
            request.kind,
            ContainerKind::Bottle
                | ContainerKind::Pill
                | ContainerKind::Syringe
                | ContainerKind::Patch
                | ContainerKind::SprayBottle
                | ContainerKind::SmokeProjector
                | ContainerKind::ChemicalCharge5
                | ContainerKind::ChemicalCharge10
                | ContainerKind::ChemicalCharge20
                | ContainerKind::PhPaper
        ) {
            // The kind crosses the network. Only authored package forms are
            // accepted; a forged request cannot mint beakers or used tools.
            continue;
        }
        if request.kind == ContainerKind::PhPaper {
            let drop_at = transform.translation + Vec3::new(0.0, 0.95, 0.45);
            spawn_container(&mut commands, ContainerKind::PhPaper, drop_at);
            emit_world_sfx(&mut sounds, Sfx::PackagePop, drop_at);
            continue;
        }
        if !buffer.0.total_volume().is_positive() {
            continue;
        }
        if request.kind.charge_fuse().is_some()
            && !buffer
                .0
                .iter()
                .any(|(reagent, _)| db.reagents.get(reagent).explosive.is_some())
        {
            continue;
        }

        // Packaging draws proportionally, so a pill made from a dirty buffer
        // carries the contamination through rather than magically purifying.
        let portion = buffer.0.split(request.kind.capacity());
        if !portion.total_volume().is_positive() {
            continue;
        }

        let drop_at = transform.translation + Vec3::new(0.0, 0.95, 0.45);
        let package = spawn_container(&mut commands, request.kind, drop_at);
        emit_world_sfx(
            &mut sounds,
            if matches!(request.kind, ContainerKind::Pill | ContainerKind::Syringe) {
                Sfx::PackagePop
            } else {
                Sfx::GlassClunk
            },
            drop_at,
        );

        // The container was only just queued for spawn, so its `Container`
        // component is not readable yet; fill it in on the command queue.
        let contents = portion;
        commands.queue(move |world: &mut World| {
            if let Some(mut container) = world.get_mut::<Container>(package) {
                let mut contents = contents;
                let amount = contents.total_volume();
                let _ = contents.transfer_to(&mut container.solution, amount);
            }
        });
    }
}

/// Floor level, just clear of a machine's working face.
///
/// Derived from the machine's code-owned collision box, so it lands in front of
/// whichever machine this is, at whatever height that machine stands, rather
/// than at one offset that happened to suit the hall's north wall. The transform
/// deliberately stays at identity scale now that the authored GLB is a child.
fn front_of(machine: &Transform, solid: &Solid, facing: Option<&Facing>, lift: f32) -> Vec3 {
    // `Vec3::Z` only for a machine that has not been dressed yet, which in
    // practice means a test that spawned one by hand.
    let facing = facing.map_or(Vec3::Z, |facing| facing.0);
    let front = solid.half_extents.dot(facing.abs());
    let floor = machine.translation.y - solid.half_extents.y;
    let spot = machine.translation + facing * (front + 0.35);
    Vec3::new(spot.x, floor + lift, spot.z)
}

/// Gives `item` to the chemist who asked for it, or sets it down in front of
/// the machine if their hands are already full.
///
/// Shared by ejecting and by taking something out of a locker, because both are
/// the same act: the machine is done with it, so it goes back to a place the
/// player can actually get at. The caller removes whatever marker was holding
/// it — [`InSlot`] or [`Stored`] — before calling.
fn give_back(
    commands: &mut Commands,
    item: Entity,
    player: Entity,
    inventory_cell: Option<(u8, u8)>,
    machine: &Transform,
    solid: &Solid,
    facing: Option<&Facing>,
    lift: f32,
) {
    let mut item = commands.entity(item);
    if let Some((slot, selected)) = inventory_cell {
        item.insert(InventorySlot {
            owner: player,
            slot,
        });
        if slot == selected {
            item.insert(HeldBy(player));
        }
    } else {
        item.insert(Transform::from_translation(front_of(
            machine, solid, facing, lift,
        )));
    }
}

/// Ejecting hands the container straight to the chemist rather than guessing
/// a spot for it.
///
/// It used to place it a metre up and half a metre along world `+Z`, which
/// points into the room for the ChemMaster 5000 and the Mixing Chamber and
/// nowhere useful for anything else. The grinder faces `-Z`, so its beaker was ejected
/// *through* the storeroom's south wall and 1.85 m up: gone, silently, with a
/// full batch inside it. Straight into the chemist's hand has no direction to
/// get wrong, and is what they were going to do next anyway.
///
/// Refused outright with empty hands the only way in — unlike
/// [`handle_take`], which still falls back to setting the item down. A locker
/// take always names the one item it means, so a full-handed take has one
/// natural place to land; an eject with a machine that has a second slot
/// (only the Mixing Chamber does) does not, and a fallback spot shared by
/// both slots would just let two ejects in a row land on top of each other.
fn handle_eject(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<EjectRequested>>,
    machines: Query<(&Machine, &Transform)>,
    active: Query<(), With<AgitationRun>>,
    chemists: Query<(Entity, &Chemist)>,
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    held: Query<&HeldBy>,
    inventory: Query<&InventorySlot>,
    selected: Query<&SelectedInventorySlot>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    let mut reserved = Vec::new();
    for request in requests.read() {
        if active.contains(request.machine) {
            continue;
        }
        let container = match request.slot {
            MachineSlot::A => slotted_container(request.machine, &slotted),
            MachineSlot::B => slotted_container_b(request.machine, &slotted_b),
        };
        let Some(container) = container else {
            continue;
        };
        let Ok((machine, transform)) = machines.get(request.machine) else {
            continue;
        };
        let Some(player) = authorized_machine_actor(
            request.client_id,
            Some(machine),
            &[
                MachineKind::ChemMaster5000,
                MachineKind::MixingChamber,
                MachineKind::Grinder,
                MachineKind::Analyzer,
                MachineKind::ReactionChamber,
            ],
            &chemists,
            &bodies,
            &bloodstreams,
        ) else {
            continue;
        };
        let selected = selected.get(player).map_or(0, |selected| selected.0);
        let legacy_full_hand = held.iter().any(|held| held.0 == player)
            && !inventory
                .iter()
                .any(|entry| entry.owner == player && entry.slot == selected);
        if legacy_full_hand {
            continue;
        }
        let Some(slot) = free_inventory_slot(player, selected, &inventory, &reserved) else {
            continue;
        };
        reserved.push((player, slot));

        match request.slot {
            MachineSlot::A => commands.entity(container).remove::<InSlot>(),
            MachineSlot::B => commands.entity(container).remove::<InSlotB>(),
        };
        let mut item = commands.entity(container);
        item.insert(InventorySlot {
            owner: player,
            slot,
        });
        if slot == selected {
            item.insert(HeldBy(player));
        }
        emit_world_sfx(&mut sounds, Sfx::Eject, transform.translation);
    }
}

/// Everything shut in `locker`, in a stable order.
///
/// Sorted, because query iteration follows archetype order and shuffles as
/// items go in and out — which would move a row out from under the button the
/// player was about to click.
pub fn stored_in(locker: Entity, stored: &Query<(Entity, &Stored)>) -> Vec<Entity> {
    let mut items: Vec<Entity> = stored
        .iter()
        .filter(|(_, shut_in)| shut_in.0 == locker)
        .map(|(item, _)| item)
        .collect();
    items.sort_unstable_by_key(|item| item.to_bits());
    items
}

/// Takes one item back out of a locker.
#[allow(clippy::too_many_arguments)]
fn handle_take(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<TakeRequested>>,
    machines: Query<(&Machine, &Transform, &Solid, Option<&Facing>)>,
    chemists: Query<(Entity, &Chemist)>,
    stored: Query<&Stored>,
    containers: Query<&Container>,
    held: Query<&HeldBy>,
    inventory: Query<&InventorySlot>,
    selected: Query<&SelectedInventorySlot>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
) {
    let mut reserved = Vec::new();
    for request in requests.read() {
        // The item has to be in the locker the message names. Checked rather
        // than trusted: the pair comes off the wire, and without this a client
        // could name any entity in the world and have it handed over.
        let Ok(shut_in) = stored.get(request.item) else {
            continue;
        };
        if shut_in.0 != request.machine {
            continue;
        }
        let Ok((machine, transform, solid, facing)) = machines.get(request.machine) else {
            continue;
        };
        let Some(player) = authorized_machine_actor(
            request.client_id,
            Some(machine),
            &[MachineKind::Locker],
            &chemists,
            &bodies,
            &bloodstreams,
        ) else {
            continue;
        };

        let selected = selected.get(player).map_or(0, |selected| selected.0);
        let legacy_full_hand = held.iter().any(|held| held.0 == player)
            && !inventory
                .iter()
                .any(|entry| entry.owner == player && entry.slot == selected);
        let cell = (!legacy_full_hand)
            .then(|| free_inventory_slot(player, selected, &inventory, &reserved))
            .flatten()
            .map(|slot| (slot, selected));
        if let Some((slot, _)) = cell {
            reserved.push((player, slot));
        }
        commands.entity(request.item).remove::<Stored>();
        give_back(
            &mut commands,
            request.item,
            player,
            cell,
            transform,
            solid,
            facing,
            set_down_lift(containers.get(request.item).ok()),
        );
    }
}

fn handle_empty(
    mut requests: MessageReader<FromClient<EmptyRequested>>,
    machines: Query<&Machine>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    active: Query<(), With<AgitationRun>>,
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    mut containers: Query<&mut Container>,
) {
    for request in requests.read() {
        if authorized_machine_actor(
            request.client_id,
            machines.get(request.machine).ok(),
            &[
                MachineKind::ChemMaster5000,
                MachineKind::MixingChamber,
                MachineKind::Grinder,
                MachineKind::Analyzer,
                MachineKind::ReactionChamber,
            ],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        if active.contains(request.machine) {
            continue;
        }
        let target = match request.slot {
            MachineSlot::A => slotted_container(request.machine, &slotted),
            MachineSlot::B => slotted_container_b(request.machine, &slotted_b),
        };
        let Some(target) = target else {
            continue;
        };
        if let Ok(mut container) = containers.get_mut(target) {
            container.solution.clear();
        }
    }
}

/// Works out the method behind whatever is in the analyzer.
///
/// This is the reverse-engineering route into a recipe: get hold of a sample
/// by any means — a lucky mix, a vial a crew member left behind — and the
/// machine tells you how it was put together. It is also the anti-softlock, so
/// an order for something unmakeable is never a dead end.
fn handle_analyze(
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<AnalyzeRequested>>,
    machines: Query<&Machine>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    mut fired: MessageWriter<ReactionsFired>,
    transforms: Query<&Transform>,
    slotted: Query<(Entity, &InSlot)>,
    containers: Query<&Container>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    for request in requests.read() {
        if authorized_machine_actor(
            request.client_id,
            machines.get(request.machine).ok(),
            &[MachineKind::Analyzer],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok(container) = containers.get(target) else {
            continue;
        };

        // Any reaction that produces something in the sample is a reaction the
        // analyzer can account for.
        let identified: Vec<chem_sim::ReactionId> = db
            .reactions
            .iter()
            .filter(|reaction| {
                reaction
                    .product_ids()
                    .any(|product| container.solution.volume_of(product).is_positive())
            })
            .map(|reaction| reaction.id)
            .collect();

        if !identified.is_empty() {
            if let Ok(transform) = transforms.get(request.machine) {
                emit_world_sfx(&mut sounds, Sfx::AnalyzerFinish, transform.translation);
            }
            // No effects: the analyzer identifies a sample, it does not react
            // one. Nothing here can smoke or detonate. `distinct_reagents: 0`
            // deliberately exempts this from the crowd-threshold check in
            // `learn_from_experiments` — see `ReactionsFired::distinct_reagents`.
            fired.write(ReactionsFired {
                reactions: identified,
                container: target,
                effects: Vec::new(),
                distinct_reagents: 0,
            });
        }
    }
}

/// HPLC-style separation and recovery.
///
/// Ordinary material is concentrated at a ten-percent yield cost and every
/// contaminant is collected in a reject beaker. Selecting an authored inverse
/// instead recovers sixty percent of it as the linked useful compound. This is
/// deliberately deterministic: expert chemistry should reward planning, not
/// a hidden dice roll.
fn handle_purify(
    mut commands: Commands,
    db: Res<ChemDb>,
    knowledge: Res<Knowledge>,
    mut requests: MessageReader<FromClient<PurifyRequested>>,
    machines: Query<&Machine>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    transforms: Query<&Transform>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    if knowledge.known_count() < HPLC_RECIPE_REQUIREMENT {
        return;
    }
    for request in requests.read() {
        if authorized_machine_actor(
            request.client_id,
            machines.get(request.machine).ok(),
            &[MachineKind::Analyzer],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok(mut container) = containers.get_mut(target) else {
            continue;
        };
        let amount = container.solution.volume_of(request.reagent);
        if !amount.is_positive() {
            continue;
        }

        let original_temperature = container.solution.temperature;
        let original_purity = container.solution.purity_of(request.reagent);
        let original_ph = container.solution.reagent_ph(request.reagent);
        let mut rejects = Solution::unbounded();
        rejects.temperature = original_temperature;

        for (reagent, quantity) in container.solution.clone().iter() {
            if reagent == request.reagent {
                continue;
            }
            let purity = container.solution.purity_of(reagent);
            let ph = container.solution.reagent_ph(reagent);
            let removed = container.solution.remove(reagent, quantity);
            let _ = rejects.add_profiled(reagent, removed, purity, ph);
        }

        let removed = container.solution.remove(request.reagent, amount);
        let inverse = db.reagents.get(request.reagent).recovers_to.as_deref();
        let (product, product_amount, recovered_inverse) = if let Some(recovered) =
            inverse.and_then(|key| db.reagents.id_of(key))
        {
            let recovered_amount = Units::from_raw(removed.raw() * 3 / 5);
            let rejected_amount = removed - recovered_amount;
            let recovered_def = db.reagents.get(recovered);
            let _ = container.solution.add_profiled(
                recovered,
                recovered_amount,
                original_purity.max(0.70),
                recovered_def.ph,
            );
            let _ = rejects.add_profiled(
                request.reagent,
                rejected_amount,
                original_purity,
                original_ph,
            );
            (recovered, recovered_amount, true)
        } else {
            let retained = Units::from_raw(removed.raw() * 9 / 10);
            let rejected = removed - retained;
            let refined_purity = (original_purity + (1.0 - original_purity) * 0.75).min(1.0);
            let _ = container.solution.add_profiled(
                request.reagent,
                retained,
                refined_purity,
                original_ph,
            );
            let _ = rejects.add_profiled(request.reagent, rejected, original_purity, original_ph);
            (request.reagent, retained, false)
        };
        container.solution.temperature = original_temperature;

        let report = HplcReport {
            source: request.reagent,
            product,
            input_amount: removed,
            product_amount,
            reject_amount: rejects.total_volume(),
            input_purity: original_purity,
            product_purity: container.solution.purity_of(product),
            recovered_inverse,
        };
        commands.entity(request.machine).insert(report);

        if !rejects.is_empty() {
            let drop_at = transforms
                .get(request.machine)
                .map(|transform| transform.translation + Vec3::new(0.45, 0.95, 0.35))
                .unwrap_or(Vec3::ZERO);
            let reject = spawn_container(&mut commands, ContainerKind::LargeBeaker, drop_at);
            commands.queue(move |world: &mut World| {
                if let Some(mut container) = world.get_mut::<Container>(reject) {
                    let mut rejects = rejects;
                    let amount = rejects.total_volume();
                    container.solution.temperature = rejects.temperature;
                    let _ = rejects.transfer_to(&mut container.solution, amount);
                }
            });
        }
        if let Ok(transform) = transforms.get(request.machine) {
            emit_world_sfx(&mut sounds, Sfx::AnalyzerFinish, transform.translation);
        }
    }
}

/// Breaks produce down into the loaded beaker.
///
/// Extraction, not chemistry: the yields are absolute quantities out of the
/// data file and the resolver never decides them. It does run *afterwards*
/// though, because the grind goes in through [`Container::mutate`] — so
/// grinding ambrosia into a beaker of radium makes hyronalin, and that counts
/// as a discovery like any other.
fn handle_grind(
    db: Res<ChemDb>,
    catalog: Option<Res<ProduceCatalog>>,
    mut requests: MessageReader<FromClient<GrindRequested>>,
    machines: Query<&Machine>,
    chemists: Query<(Entity, &Chemist)>,
    bodies: Query<&crate::body::Body>,
    bloodstreams: Query<&crate::body::Bloodstream>,
    mut fired: MessageWriter<ReactionsFired>,
    mut hoppers: Query<&mut Hopper>,
    transforms: Query<&Transform>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    let Some(catalog) = catalog else {
        return;
    };

    for request in requests.read() {
        if authorized_machine_actor(
            request.client_id,
            machines.get(request.machine).ok(),
            &[MachineKind::Grinder],
            &chemists,
            &bodies,
            &bloodstreams,
        )
        .is_none()
        {
            continue;
        }
        let Ok(mut hopper) = hoppers.get_mut(request.machine) else {
            continue;
        };
        // No beaker means nothing to grind into. The hopper keeps its contents
        // rather than the machine running dry and eating them.
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok(mut container) = containers.get_mut(target) else {
            continue;
        };

        let passes = if request.all { hopper.0.len() } else { 1 };
        let mut reactions = Vec::new();
        // Accumulated across passes alongside the reactions, for the same
        // reason: grinding the whole hopper is one action to the player, and
        // reporting it as several would spawn several smoke clouds.
        let mut effects = Vec::new();
        // The worst single pass, not a sum — how crowded the beaker ever got
        // during this action is what tells a focused grind from a dumping
        // ground, and each pass's own count already reflects everything
        // still sitting there from the passes before it.
        let mut distinct_reagents = 0;
        let mut ground = 0usize;
        for _ in 0..passes {
            let Some(&next) = hopper.0.first() else {
                break;
            };
            let kind = catalog.get(next);

            // Checked before the item is consumed: a full beaker must refuse
            // the plant rather than swallow it and drop the overflow, which is
            // the same contract `Solution::add` holds callers to.
            if container.solution.available_volume() < kind.total_yield() {
                break;
            }

            hopper.0.remove(0);
            ground += 1;
            let (_, report) = container.mutate(&db, |solution| {
                for (reagent, amount) in &kind.yields {
                    let definition = db.reagents.get(*reagent);
                    let _ = solution.add_profiled(*reagent, *amount, 1.0, definition.ph);
                }
            });
            reactions.extend(report.fired_reactions());
            effects.extend(report.effects);
            distinct_reagents = distinct_reagents.max(report.distinct_reagents);
        }

        if ground > 0 {
            if let Ok(transform) = transforms.get(request.machine) {
                emit_world_sfx(&mut sounds, Sfx::Grinder, transform.translation);
            }
        }

        if !reactions.is_empty() || !effects.is_empty() {
            fired.write(ReactionsFired {
                reactions,
                container: target,
                effects,
                distinct_reagents,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    //! Headless tests for the machine wiring: message in, state out, no
    //! window and no renderer. They cover the paths a player can only reach by
    //! clicking, which is exactly where manual testing is least reliable.

    use super::*;
    use crate::containers::ContainerKind;
    use chem_sim::ChemData;

    fn test_app() -> App {
        let data = ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should load");

        let catalog = ProduceCatalog::from_config(
            &ron::from_str(include_str!("../../assets/data/station.produce.ron"))
                .expect("produce data should load"),
            &data.reagents,
        );

        // All authored dispenser bases are available from the start; research
        // now advances recipe knowledge and machinery instead of stock access.
        let knowledge = Knowledge::new(&data);

        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .insert_resource(knowledge)
            .insert_resource(catalog)
            .add_message::<FromClient<DispenseRequested>>()
            .add_message::<EmitWorldSfx>()
            .add_message::<ReactionsFired>()
            .add_message::<FromClient<BufferTransferRequested>>()
            .add_message::<FromClient<AgitateRequested>>()
            .add_message::<FromClient<InteractRequested>>()
            .add_message::<FromClient<LeaveMachineRequested>>()
            .add_message::<FromClient<GrindRequested>>()
            .add_message::<FromClient<SetTargetTemperature>>()
            .add_message::<FromClient<SetHeaterPower>>()
            .add_message::<FromClient<PackageRequested>>()
            .add_message::<FromClient<PurifyRequested>>()
            .add_message::<FromClient<EjectRequested>>()
            .add_message::<FromClient<EmptyRequested>>()
            .add_message::<FromClient<TakeRequested>>()
            // What the server tells a client when it grants them a machine.
            // Headless there is nobody to tell, but the writer still has to
            // exist for the system to run.
            .add_message::<ToClients<MachineOpened>>()
            .init_resource::<Time>()
            .add_systems(
                Update,
                (
                    recover_from_emp,
                    handle_machine_interact,
                    handle_leave_machine,
                    handle_dispense,
                    handle_agitate,
                    handle_buffer_transfer,
                    handle_package,
                    handle_purify,
                    handle_grind,
                    handle_eject,
                    handle_take,
                    handle_empty,
                    handle_thermostat_controls,
                    apply_thermostats,
                    cool_to_ambient,
                    tick_reactions,
                )
                    .chain(),
            );
        app
    }

    #[test]
    fn emp_lockout_denies_claims_until_the_machine_recovers() {
        let mut app = test_app();
        let player = app.world_mut().spawn_empty().id();
        let machine = app
            .world_mut()
            .spawn(Machine {
                kind: MachineKind::MixingChamber,
                in_use_by: Some(player),
                disabled_for: 2.0,
            })
            .id();

        assert!(!app
            .world()
            .get::<Machine>(machine)
            .unwrap()
            .available_to(player));
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(1.0));
        app.update();
        let state = app.world().get::<Machine>(machine).unwrap();
        assert!(state.disabled_for > 0.0);
        assert_eq!(state.in_use_by, None, "an EMP clears stale ownership");

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(1.1));
        app.update();
        assert!(app
            .world()
            .get::<Machine>(machine)
            .unwrap()
            .available_to(player));
    }

    /// A chemist with a connection of their own, as the server sees one.
    fn chemist(app: &mut App) -> (ClientId, Entity) {
        let client = ClientId::Client(app.world_mut().spawn_empty().id());
        let chemist = app
            .world_mut()
            .spawn((InteractionMode::default(), Chemist { client }))
            .id();
        (client, chemist)
    }

    fn reagent(app: &App, key: &str) -> ReagentId {
        app.world().resource::<ChemDb>().reagent(key)
    }

    /// The produce kind whose name starts with `prefix`, e.g. "Poppy".
    fn produce(app: &App, prefix: &str) -> ProduceId {
        app.world()
            .resource::<ProduceCatalog>()
            .iter()
            .find(|kind| kind.name.starts_with(prefix))
            .unwrap_or_else(|| panic!("no produce kind named '{prefix}'"))
            .id
    }

    /// A grinder with `hopper` loaded and, optionally, a beaker in the slot.
    fn grinder(app: &mut App, hopper: &[ProduceId], beaker: Option<ContainerKind>) -> Entity {
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::Grinder),
                Hopper(hopper.to_vec()),
                Transform::default(),
            ))
            .id();
        if let Some(kind) = beaker {
            app.world_mut()
                .spawn((Container::new(kind), InSlot(machine)));
        }
        machine
    }

    fn grind(app: &mut App, machine: Entity, all: bool) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: GrindRequested { machine, all },
        });
        app.update();
    }

    fn hopper_of(app: &App, machine: Entity) -> &[ProduceId] {
        &app.world().get::<Hopper>(machine).unwrap().0
    }

    fn request_package(app: &mut App, machine: Entity, kind: ContainerKind) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: PackageRequested { machine, kind },
        });
        app.update();
    }

    fn request_purification(app: &mut App, machine: Entity, reagent: ReagentId) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: PurifyRequested { machine, reagent },
        });
        app.update();
    }

    #[test]
    fn chemical_charges_require_an_energetic_payload_and_preserve_quality() {
        let mut app = test_app();
        let water = reagent(&app, "water");
        let gunpowder = reagent(&app, "gunpowder");
        let mut buffer = Solution::unbounded();
        let _ = buffer.add_profiled(water, Units::whole(50), 0.42, 6.2);
        let machine = app
            .world_mut()
            .spawn((Buffer(buffer), Transform::default()))
            .id();

        request_package(&mut app, machine, ContainerKind::ChemicalCharge5);
        assert_eq!(
            app.world_mut()
                .query::<&Container>()
                .iter(app.world())
                .count(),
            0,
            "ordinary liquid cannot be disguised as a charge"
        );

        let mut explosive = Solution::unbounded();
        let _ = explosive.add_profiled(gunpowder, Units::whole(50), 0.82, 7.4);
        app.world_mut()
            .entity_mut(machine)
            .insert(Buffer(explosive));
        request_package(&mut app, machine, ContainerKind::ChemicalCharge5);

        let mut containers = app.world_mut().query::<&Container>();
        let charge = containers
            .single(app.world())
            .expect("one charge should be packaged");
        assert_eq!(charge.kind, ContainerKind::ChemicalCharge5);
        assert_eq!(charge.solution.total_volume(), Units::whole(50));
        assert!((charge.solution.purity_of(gunpowder) - 0.82).abs() < 0.001);
        assert!((charge.solution.ph() - 7.4).abs() < 0.001);
    }

    #[test]
    fn mixing_chamber_dispenses_ph_paper_without_consuming_chemicals() {
        let mut app = test_app();
        let machine = app
            .world_mut()
            .spawn((Buffer(Solution::unbounded()), Transform::default()))
            .id();

        request_package(&mut app, machine, ContainerKind::PhPaper);

        let mut containers = app.world_mut().query::<&Container>();
        let paper = containers
            .single(app.world())
            .expect("one strip should be dispensed");
        assert_eq!(paper.kind, ContainerKind::PhPaper);
        assert!(paper.solution.is_empty());
    }

    #[test]
    fn smoke_projector_packages_a_profiled_payload_and_forged_forms_are_rejected() {
        let mut app = test_app();
        let water = reagent(&app, "water");
        let mut buffer = Solution::unbounded();
        let _ = buffer.add_profiled(water, Units::whole(40), 0.73, 6.4);
        let machine = app
            .world_mut()
            .spawn((Buffer(buffer), Transform::default()))
            .id();

        request_package(&mut app, machine, ContainerKind::PhPaperStrongBase);
        assert_eq!(
            app.world_mut()
                .query::<&Container>()
                .iter(app.world())
                .count(),
            0,
            "clients cannot mint already-used tools"
        );

        request_package(&mut app, machine, ContainerKind::SmokeProjector);
        let mut containers = app.world_mut().query::<&Container>();
        let projector = containers.single(app.world()).unwrap();
        assert_eq!(projector.kind, ContainerKind::SmokeProjector);
        assert_eq!(projector.solution.total_volume(), Units::whole(30));
        assert!((projector.solution.purity_of(water) - 0.73).abs() < 0.001);
        assert!((projector.solution.ph() - 6.4).abs() < 0.001);
    }

    #[test]
    fn hplc_separates_contaminants_and_recovers_an_authored_inverse() {
        let mut app = test_app();
        let inverse = reagent(&app, "libitoil");
        let medicine = reagent(&app, "libital");
        let water = reagent(&app, "water");
        let chemistry = app.world().resource::<ChemDb>().0.clone();
        app.world_mut()
            .resource_mut::<Knowledge>()
            .unlock_all(&chemistry);
        let machine = app.world_mut().spawn(Transform::default()).id();
        let mut sample = Container::new(ContainerKind::LargeBeaker);
        let _ = sample
            .solution
            .add_profiled(inverse, Units::whole(10), 0.30, 8.2);
        let _ = sample
            .solution
            .add_profiled(water, Units::whole(10), 1.0, 7.0);
        let source = app.world_mut().spawn((sample, InSlot(machine))).id();

        request_purification(&mut app, machine, inverse);

        let refined = app.world().get::<Container>(source).unwrap();
        assert_eq!(refined.solution.volume_of(medicine), Units::whole(6));
        assert_eq!(refined.solution.volume_of(water), Units::ZERO);
        assert!((refined.solution.purity_of(medicine) - 0.70).abs() < 0.001);

        let mut containers = app.world_mut().query::<(Entity, &Container)>();
        let reject = containers
            .iter(app.world())
            .find(|(entity, _)| *entity != source)
            .map(|(_, container)| container)
            .expect("the rejected fraction should be collected");
        assert_eq!(reject.solution.volume_of(inverse), Units::whole(4));
        assert_eq!(reject.solution.volume_of(water), Units::whole(10));

        let report = app
            .world()
            .get::<HplcReport>(machine)
            .expect("the analyzer should retain its yield report");
        assert_eq!(report.source, inverse);
        assert_eq!(report.product, medicine);
        assert_eq!(report.input_amount, Units::whole(10));
        assert_eq!(report.product_amount, Units::whole(6));
        assert_eq!(report.reject_amount, Units::whole(14));
        assert!(report.recovered_inverse);
        assert!((report.input_purity - 0.30).abs() < 0.001);
        assert!((report.product_purity - 0.70).abs() < 0.001);
    }

    #[test]
    fn hplc_purifies_a_clean_inverse_pair_member_without_converting_it() {
        let mut app = test_app();
        let medicine = reagent(&app, "libital");
        let harmful_inverse = reagent(&app, "libitoil");
        let chemistry = app.world().resource::<ChemDb>().0.clone();
        app.world_mut()
            .resource_mut::<Knowledge>()
            .unlock_all(&chemistry);
        let machine = app.world_mut().spawn(Transform::default()).id();
        let mut sample = Container::new(ContainerKind::LargeBeaker);
        let _ = sample
            .solution
            .add_profiled(medicine, Units::whole(10), 0.80, 8.2);
        let source = app.world_mut().spawn((sample, InSlot(machine))).id();

        request_purification(&mut app, machine, medicine);

        let refined = app.world().get::<Container>(source).unwrap();
        assert_eq!(refined.solution.volume_of(medicine), Units::whole(9));
        assert_eq!(refined.solution.volume_of(harmful_inverse), Units::ZERO);
        let report = app.world().get::<HplcReport>(machine).unwrap();
        assert_eq!(report.product, medicine);
        assert!(!report.recovered_inverse);
    }

    #[test]
    fn a_peer_can_only_purify_its_claimed_analyzer() {
        let mut app = test_app();
        let inverse = reagent(&app, "libitoil");
        let medicine = reagent(&app, "libital");
        let chemistry = app.world().resource::<ChemDb>().0.clone();
        app.world_mut()
            .resource_mut::<Knowledge>()
            .unlock_all(&chemistry);
        let (owner_client, owner) = chemist(&mut app);
        let (intruder_client, _) = chemist(&mut app);
        let mut state = Machine::new(MachineKind::Analyzer);
        state.in_use_by = Some(owner);
        let machine = app.world_mut().spawn((state, Transform::default())).id();
        let mut sample = Container::new(ContainerKind::LargeBeaker);
        let _ = sample
            .solution
            .add_profiled(inverse, Units::whole(10), 0.30, 8.2);
        let source = app.world_mut().spawn((sample, InSlot(machine))).id();

        app.world_mut().write_message(FromClient {
            client_id: intruder_client,
            message: PurifyRequested {
                machine,
                reagent: inverse,
            },
        });
        app.update();
        assert_eq!(
            app.world()
                .get::<Container>(source)
                .unwrap()
                .solution
                .volume_of(inverse),
            Units::whole(10),
            "another peer must not operate the claimed analyzer"
        );

        app.world_mut().get_mut::<Machine>(machine).unwrap().kind = MachineKind::MixingChamber;
        app.world_mut().write_message(FromClient {
            client_id: owner_client,
            message: PurifyRequested {
                machine,
                reagent: inverse,
            },
        });
        app.update();
        assert_eq!(
            app.world()
                .get::<Container>(source)
                .unwrap()
                .solution
                .volume_of(inverse),
            Units::whole(10),
            "the right owner cannot forge an analyzer action on another machine kind"
        );

        app.world_mut().get_mut::<Machine>(machine).unwrap().kind = MachineKind::Analyzer;
        app.world_mut().write_message(FromClient {
            client_id: owner_client,
            message: PurifyRequested {
                machine,
                reagent: inverse,
            },
        });
        app.update();
        assert_eq!(
            app.world()
                .get::<Container>(source)
                .unwrap()
                .solution
                .volume_of(medicine),
            Units::whole(6)
        );
    }

    #[test]
    fn hplc_stays_locked_until_the_notebook_has_expert_coverage() {
        let mut app = test_app();
        let water = reagent(&app, "water");
        let machine = app.world_mut().spawn(Transform::default()).id();
        let mut sample = Container::new(ContainerKind::Beaker);
        let _ = sample
            .solution
            .add_profiled(water, Units::whole(10), 0.40, 7.0);
        let source = app.world_mut().spawn((sample, InSlot(machine))).id();

        request_purification(&mut app, machine, water);

        let unchanged = app.world().get::<Container>(source).unwrap();
        assert_eq!(unchanged.solution.volume_of(water), Units::whole(10));
        assert!((unchanged.solution.purity_of(water) - 0.40).abs() < 0.001);
        assert_eq!(
            app.world_mut()
                .query::<&Container>()
                .iter(app.world())
                .count(),
            1,
            "a forged early request must not create a reject beaker"
        );
        assert!(
            app.world().get::<HplcReport>(machine).is_none(),
            "a rejected request must not forge a successful result"
        );
    }

    fn slot_contents(app: &mut App, machine: Entity) -> Solution {
        let mut query = app.world_mut().query::<(&Container, &InSlot)>();
        query
            .iter(app.world())
            .find(|(_, slot)| slot.0 == machine)
            .map(|(container, _)| container.solution.clone())
            .expect("a container should be loaded")
    }

    #[test]
    fn dispensing_the_right_ratio_produces_medicine() {
        let mut app = test_app();
        let dispenser = app.world_mut().spawn(DispenseAmount(Units::whole(15))).id();
        let beaker = app
            .world_mut()
            .spawn((
                Container::new(ContainerKind::LargeBeaker),
                InSlot(dispenser),
            ))
            .id();

        // 15u each of oxygen, carbon and sugar — the inaprovaline recipe.
        for key in ["oxygen", "carbon", "sugar"] {
            let reagent = reagent(&app, key);
            app.world_mut().write_message(FromClient {
                client_id: ClientId::Server,
                message: DispenseRequested {
                    machine: dispenser,
                    reagent,
                },
            });
            app.update();
        }

        let inaprovaline = reagent(&app, "inaprovaline");
        let container = app.world().get::<Container>(beaker).unwrap();
        assert_eq!(
            container.solution.volume_of(inaprovaline),
            Units::whole(45),
            "reactions must run as part of dispensing, not only on demand"
        );
        assert_eq!(container.solution.len(), 1, "reagents should be consumed");
    }

    #[test]
    fn dispensing_sounds_once_only_after_an_accepted_transfer() {
        let mut app = test_app();
        let origin = Vec3::new(4.0, 0.5, -2.0);
        let dispenser = app
            .world_mut()
            .spawn((
                DispenseAmount(Units::whole(15)),
                Transform::from_translation(origin),
            ))
            .id();
        app.world_mut().spawn((
            Container::new(ContainerKind::LargeBeaker),
            InSlot(dispenser),
        ));
        let oxygen = reagent(&app, "oxygen");

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: DispenseRequested {
                machine: dispenser,
                reagent: oxygen,
            },
        });
        app.update();
        let accepted: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<EmitWorldSfx>>()
            .drain()
            .collect();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0], EmitWorldSfx::new(Sfx::DispensePour, origin));

        let invalid_machine = app.world_mut().spawn_empty().id();
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: DispenseRequested {
                machine: invalid_machine,
                reagent: oxygen,
            },
        });
        app.update();
        assert!(
            app.world_mut()
                .resource_mut::<Messages<EmitWorldSfx>>()
                .drain()
                .next()
                .is_none(),
            "a rejected request must not predict a physical sound"
        );
    }

    #[test]
    fn dispensing_a_crafted_reagent_is_refused() {
        // `test_app()` starts with everything unlocked, which is right for
        // every other test here — this one is specifically about what a
        // *locked* reagent does, so it overwrites that with a fresh
        // `Knowledge`, closer to what a real career actually starts with.
        let mut app = test_app();
        let data = app.world().resource::<ChemDb>().0.clone();
        app.insert_resource(Knowledge::new(&data));

        let dispenser = app.world_mut().spawn(DispenseAmount(Units::whole(15))).id();
        let beaker = app
            .world_mut()
            .spawn((
                Container::new(ContainerKind::LargeBeaker),
                InSlot(dispenser),
            ))
            .id();

        let hydrogen = reagent(&app, "bicaridine");
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: DispenseRequested {
                machine: dispenser,
                reagent: hydrogen,
            },
        });
        app.update();

        let container = app.world().get::<Container>(beaker).unwrap();
        assert!(
            container.solution.is_empty(),
            "a synthesized reagent must never be dispensed, even by a forged request"
        );
    }

    #[test]
    fn direct_dispenser_combination_cannot_start_an_agitated_recipe() {
        let mut app = test_app();
        let dispenser = app.world_mut().spawn(DispenseAmount(Units::whole(15))).id();
        app.world_mut().spawn((
            Container::new(ContainerKind::LargeBeaker),
            InSlot(dispenser),
        ));

        // Oxygen, sugar, then a double helping of carbon: inaprovaline forms,
        // but the leftover carbon shares its beaker and has no provenance.
        for key in ["oxygen", "sugar", "carbon", "carbon"] {
            let reagent = reagent(&app, key);
            app.world_mut().write_message(FromClient {
                client_id: ClientId::Server,
                message: DispenseRequested {
                    machine: dispenser,
                    reagent,
                },
            });
            app.update();
        }
        let names = reactions_over(&mut app, 8.0);
        assert!(
            names.iter().any(|key| key == "inaprovaline"),
            "the tutorial recipe should still resolve, got {names:?}"
        );
        assert!(
            !names.iter().any(|key| key == "bicaridine"),
            "ordinary dispensing must not forge staged provenance, got {names:?}"
        );
    }

    // -- reactions that take time ------------------------------------------

    #[test]
    fn a_batch_that_takes_time_is_reported_once_not_once_per_frame() {
        // Bicaridine is rated, so it runs across dozens of frames. One report
        // per frame would mean a recipe with a `Smoke` effect venting a full
        // cloud sixty times, and `knowledge::learn_from_experiments` grading
        // one experiment sixty times over.
        let mut app = test_app();
        let (client, machine, _, _) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);
        let bicaridine = reaction_id(&app, "bicaridine");

        let reports = fired_over(&mut app, 8.0);
        let mentions = reports
            .iter()
            .filter(|report| report.reactions.contains(&bicaridine))
            .count();
        assert_eq!(mentions, 1, "one batch, one report");
    }

    #[test]
    fn the_crowd_count_is_taken_when_a_batch_starts_not_when_it_ends() {
        // `distinct_reagents` is how discovery tells a focused experiment from
        // a shotgun dump. The resolver eats intermediates as it goes, so read
        // at the *end* of a slow run it flatters a beaker that started as a
        // pile of everything — the exact dump the check exists to refuse.
        let mut app = test_app();
        let (client, machine, _, _) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);
        let bicaridine = reaction_id(&app, "bicaridine");

        let reports = fired_over(&mut app, 8.0);
        let report = reports
            .iter()
            .find(|report| report.reactions.contains(&bicaridine))
            .expect("the batch should have been reported");
        assert_eq!(
            report.distinct_reagents, 2,
            "two reagents went in; by the end there is only the product left"
        );
    }

    #[test]
    fn a_running_batch_says_so_and_stops_saying_so() {
        // What the panel's "Still reacting…" line reads, and the difference
        // between "not finished yet" and "this is all you are getting".
        let mut app = test_app();
        let (client, machine, _, beaker) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);

        run_for(&mut app, 0.5);
        assert!(
            app.world().get::<AgitationRun>(machine).is_some(),
            "a five-second agitation should not finish in half a second"
        );

        run_for(&mut app, 8.0);
        assert!(app.world().get::<AgitationRun>(machine).is_none());
        let container = app.world().get::<Container>(beaker).unwrap();
        let product = container
            .solution
            .volume_of(app.world().resource::<ChemDb>().reagent("bicaridine"));
        assert_eq!(product, Units::whole(10));
    }

    #[test]
    fn an_untouched_beaker_is_never_marked_for_replication() {
        // A `Container` is replicated, so touching one puts it on the wire.
        // Stepping every beaker in the lab every frame must cost nothing until
        // something in one of them actually moves.
        let mut app = test_app();
        let beaker = loose_beaker(&mut app, &[("carbon", 10)]);

        run_for(&mut app, 1.0);
        let tick = app.world().read_change_tick();
        run_for(&mut app, 1.0);

        let container = app
            .world()
            .entity(beaker)
            .get_ref::<Container>()
            .expect("the beaker is still there");
        assert!(
            !container
                .last_changed()
                .is_newer_than(tick, app.world().read_change_tick()),
            "carbon on its own reacts with nothing and must not be re-sent"
        );
    }

    #[test]
    fn a_beaker_emptied_mid_batch_does_not_carry_the_run_into_the_next_one() {
        // Otherwise the next thing poured in inherits the last batch's reagent
        // count and its half-finished list of what fired.
        let mut app = test_app();
        let (client, machine, _, beaker) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);
        run_for(&mut app, 0.5);
        assert!(app.world().get::<AgitationRun>(machine).is_some());

        app.world_mut()
            .get_mut::<Container>(beaker)
            .unwrap()
            .solution
            .clear();
        run_for(&mut app, 0.5);

        assert!(
            app.world().get::<AgitationRun>(machine).is_none(),
            "an interrupted run has to be closed out, not left attached"
        );
    }

    #[test]
    fn buffer_transfer_isolates_a_single_reagent() {
        let mut app = test_app();
        let chemmaster = app
            .world_mut()
            .spawn((
                DispenseAmount(Units::whole(20)),
                Buffer(Solution::new(Units::whole(300))),
            ))
            .id();
        let beaker = app
            .world_mut()
            .spawn((
                Container::new(ContainerKind::LargeBeaker),
                InSlot(chemmaster),
            ))
            .id();

        for key in ["oxygen", "sugar"] {
            let reagent = reagent(&app, key);
            app.world_mut().write_message(FromClient {
                client_id: ClientId::Server,
                message: DispenseRequested {
                    machine: chemmaster,
                    reagent,
                },
            });
            app.update();
        }

        let oxygen = reagent(&app, "oxygen");
        let sugar = reagent(&app, "sugar");
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: BufferTransferRequested {
                machine: chemmaster,
                reagent: oxygen,
                amount: Units::whole(20),
                direction: BufferDirection::ToBuffer,
                slot: MachineSlot::A,
            },
        });
        app.update();

        // Pulling one named reagent out of a mixture is how a contaminated
        // batch gets cleaned up before it goes into a pill.
        let buffer = app.world().get::<Buffer>(chemmaster).unwrap();
        assert_eq!(buffer.0.volume_of(oxygen), Units::whole(20));
        assert_eq!(buffer.0.volume_of(sugar), Units::ZERO);

        let container = app.world().get::<Container>(beaker).unwrap();
        assert_eq!(container.solution.volume_of(oxygen), Units::ZERO);
        assert_eq!(container.solution.volume_of(sugar), Units::whole(20));
    }

    // -----------------------------------------------------------------------
    // The Mixing Chamber's second slot
    // -----------------------------------------------------------------------

    #[test]
    fn agitation_rejects_wrong_sides_and_a_missing_second_beaker() {
        let mut app = test_app();
        let (client, machine, beaker_a, beaker_b) =
            staged_mixer(&mut app, &[("water", 5)], &[("carbon", 5)]);
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);
        assert!(app.world().get::<AgitationRun>(machine).is_none());
        assert_eq!(
            app.world()
                .get::<Container>(beaker_a)
                .unwrap()
                .solution
                .total_volume(),
            Units::whole(5),
            "a rejected request must not drain its source"
        );
        assert_eq!(
            app.world()
                .get::<Container>(beaker_b)
                .unwrap()
                .solution
                .total_volume(),
            Units::whole(5)
        );

        let mut app = test_app();
        let (client, machine, beaker_a, beaker_b) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        app.world_mut().entity_mut(beaker_b).remove::<InSlotB>();
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);
        assert!(app.world().get::<AgitationRun>(machine).is_none());
        assert_eq!(
            app.world()
                .get::<Container>(beaker_a)
                .unwrap()
                .solution
                .total_volume(),
            Units::whole(5),
            "one loaded beaker is not a staged mix"
        );

        let mut app = test_app();
        let (client, machine, packaged_source, _) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        app.world_mut()
            .get_mut::<Container>(packaged_source)
            .unwrap()
            .kind = ContainerKind::Bottle;
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);
        assert!(
            app.world().get::<AgitationRun>(machine).is_none(),
            "staged preparation requires laboratory beakers, not packaged doses"
        );
    }

    #[test]
    fn agitation_works_in_both_directions_and_finishes_on_its_recipe_clock() {
        for direction in [AgitateDirection::AToB, AgitateDirection::BToA] {
            let mut app = test_app();
            let (client, machine, beaker_a, beaker_b) =
                staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
            let (source, destination) = match direction {
                AgitateDirection::AToB => (beaker_a, beaker_b),
                AgitateDirection::BToA => (beaker_b, beaker_a),
            };

            request_agitation(&mut app, client, machine, direction);
            let run = app
                .world()
                .get::<AgitationRun>(machine)
                .expect("valid sides should start a run");
            assert_eq!(run.destination, destination);
            assert!((run.expected_secs - 5.0).abs() < 0.01);
            assert!(
                app.world()
                    .get::<Container>(source)
                    .unwrap()
                    .solution
                    .is_empty(),
                "the complete source beaker is transferred"
            );

            run_for(&mut app, 4.0);
            assert!(
                app.world().get::<AgitationRun>(machine).is_some(),
                "the visible run must remain active before its tuned duration"
            );
            run_for(&mut app, 1.2);
            assert!(app.world().get::<AgitationRun>(machine).is_none());
            let product = reagent(&app, "bicaridine");
            assert_eq!(
                app.world()
                    .get::<Container>(destination)
                    .unwrap()
                    .solution
                    .volume_of(product),
                Units::whole(10)
            );
        }
    }

    #[test]
    fn agitation_duration_is_frame_rate_independent_for_authored_order_batches() {
        struct Batch<'a> {
            label: &'a str,
            product: &'a str,
            side_a: Vec<(&'a str, Units)>,
            side_b: Vec<(&'a str, Units)>,
        }

        let whole = Units::whole;
        let batches = [
            Batch {
                label: "bicaridine minimum",
                product: "bicaridine",
                side_a: vec![("inaprovaline", whole(4))],
                side_b: vec![("carbon", whole(4))],
            },
            Batch {
                label: "bicaridine maximum",
                product: "bicaridine",
                side_a: vec![("inaprovaline", whole(7))],
                side_b: vec![("carbon", whole(7))],
            },
            Batch {
                label: "dermaline minimum",
                product: "dermaline",
                side_a: vec![("kelotane", whole(2))],
                side_b: vec![("oxygen", whole(2)), ("phosphorus", whole(2))],
            },
            Batch {
                label: "dermaline maximum",
                product: "dermaline",
                side_a: vec![("kelotane", whole(3))],
                side_b: vec![("oxygen", whole(3)), ("phosphorus", whole(3))],
            },
            Batch {
                label: "mannitol minimum",
                product: "mannitol",
                // 6.68 of each input yields 10.02u, the first hundredth-unit
                // batch at or above the authored 10u order minimum.
                side_a: vec![
                    ("hydrogen", Units::from_raw(668)),
                    ("water", Units::from_raw(668)),
                ],
                side_b: vec![("sugar", Units::from_raw(668))],
            },
            Batch {
                label: "mannitol maximum",
                product: "mannitol",
                side_a: vec![("hydrogen", whole(10)), ("water", whole(10))],
                side_b: vec![("sugar", whole(10))],
            },
        ];

        for batch in batches {
            let mut measured = Vec::new();
            for hz in [30.0_f32, 60.0, 144.0] {
                let mut app = test_app();
                let (client, machine, _, destination) =
                    staged_mixer_units(&mut app, &batch.side_a, &batch.side_b);
                request_agitation(&mut app, client, machine, AgitateDirection::AToB);
                let expected = app
                    .world()
                    .get::<AgitationRun>(machine)
                    .expect("the authored sides should activate")
                    .expected_secs;
                assert!(
                    (4.0..=8.0).contains(&expected),
                    "{} has an authored clock outside 4–8s: {expected}",
                    batch.label
                );

                let frame = 1.0 / hz;
                let mut elapsed = 0.0;
                while app.world().get::<AgitationRun>(machine).is_some() {
                    app.world_mut()
                        .resource_mut::<Time>()
                        .advance_by(std::time::Duration::from_secs_f32(frame));
                    app.update();
                    elapsed += frame;
                    assert!(elapsed < 8.2, "{} never finished at {hz}Hz", batch.label);
                }
                assert!(
                    elapsed + 0.000_1 >= expected,
                    "{} finished early at {hz}Hz: {elapsed:.4}s vs {expected:.4}s",
                    batch.label
                );
                assert!(
                    elapsed <= expected + CHEMISTRY_QUANTUM_SECS + frame,
                    "{} finished late at {hz}Hz: {elapsed:.4}s vs {expected:.4}s",
                    batch.label
                );
                assert!(
                    app.world()
                        .get::<Container>(destination)
                        .unwrap()
                        .solution
                        .volume_of(reagent(&app, batch.product))
                        .is_positive(),
                    "{} did not produce its medicine at {hz}Hz",
                    batch.label
                );
                measured.push(elapsed);
            }

            let fastest = measured.iter().copied().fold(f32::INFINITY, f32::min);
            let slowest = measured.iter().copied().fold(0.0_f32, f32::max);
            assert!(
                slowest - fastest <= 1.0 / 30.0 + 0.002,
                "{} drifted with frame rate: {measured:?}",
                batch.label
            );
        }
    }

    #[test]
    fn ambient_timed_reactions_use_the_same_frame_rate_independent_clock() {
        let mut measured = Vec::new();
        for hz in [30.0_f32, 60.0, 144.0] {
            let data = ChemData::from_ron(
                r#"[
                    (id: "feed", name: "Feed", color: (0.4, 0.4, 0.4), dispensable: true),
                    (id: "product", name: "Product", color: (0.6, 0.6, 0.6)),
                ]"#,
                r#"[
                    (
                        id: "slow_ambient",
                        reactants: [("feed", 1)],
                        products: [("product", 1)],
                        rate: Some(1),
                    ),
                ]"#,
            )
            .expect("the focused ambient fixture should load");
            let feed = data.reagent("feed");
            let product = data.reagent("product");
            let mut container = Container::new(ContainerKind::LargeBeaker);
            assert!(container.solution.add(feed, Units::whole(5)).is_zero());

            let mut app = App::new();
            app.insert_resource(ChemDb(data))
                .init_resource::<Time>()
                .add_message::<ReactionsFired>()
                .add_systems(Update, tick_reactions);
            let beaker = app.world_mut().spawn(container).id();
            let frame = 1.0 / hz;
            let mut elapsed = 0.0;
            loop {
                app.world_mut()
                    .resource_mut::<Time>()
                    .advance_by(std::time::Duration::from_secs_f32(frame));
                app.update();
                elapsed += frame;
                let contents = &app.world().get::<Container>(beaker).unwrap().solution;
                if contents.volume_of(product) == Units::whole(5)
                    && app.world().get::<Reacting>(beaker).is_none()
                {
                    break;
                }
                assert!(elapsed < 5.2, "ambient batch never finished at {hz}Hz");
            }
            assert!(elapsed + 0.000_1 >= 5.0);
            assert!(elapsed <= 5.0 + CHEMISTRY_QUANTUM_SECS + frame);
            measured.push(elapsed);
        }

        let fastest = measured.iter().copied().fold(f32::INFINITY, f32::min);
        let slowest = measured.iter().copied().fold(0.0_f32, f32::max);
        assert!(
            slowest - fastest <= 1.0 / 30.0 + 0.002,
            "ambient timing drifted with frame rate: {measured:?}"
        );
    }

    #[test]
    fn agitation_requires_room_for_the_entire_source() {
        let mut app = test_app();
        let (client, machine, beaker_a, beaker_b) =
            staged_mixer(&mut app, &[("inaprovaline", 60)], &[("carbon", 50)]);
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);

        assert!(app.world().get::<AgitationRun>(machine).is_none());
        assert_eq!(
            app.world()
                .get::<Container>(beaker_a)
                .unwrap()
                .solution
                .total_volume(),
            Units::whole(60)
        );
        assert_eq!(
            app.world()
                .get::<Container>(beaker_b)
                .unwrap()
                .solution
                .total_volume(),
            Units::whole(50),
            "an undersized destination must be left untouched"
        );
    }

    #[test]
    fn agitation_blends_the_two_prepared_temperatures() {
        let mut app = test_app();
        let (client, machine, beaker_a, beaker_b) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        app.world_mut()
            .get_mut::<Container>(beaker_a)
            .unwrap()
            .solution
            .temperature = Kelvin(400.0);
        app.world_mut()
            .get_mut::<Container>(beaker_b)
            .unwrap()
            .solution
            .temperature = Kelvin(300.0);

        request_agitation(&mut app, client, machine, AgitateDirection::AToB);
        assert_eq!(
            app.world()
                .get::<Container>(beaker_b)
                .unwrap()
                .solution
                .temperature,
            Kelvin(350.0),
            "equal volumes should retain their volume-weighted temperature"
        );
    }

    #[test]
    fn an_active_agitation_locks_separation_ejection_and_emptying() {
        let mut app = test_app();
        let (client, machine, _, destination) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        request_agitation(&mut app, client, machine, AgitateDirection::AToB);
        let carbon = reagent(&app, "carbon");

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: BufferTransferRequested {
                machine,
                reagent: carbon,
                amount: Units::whole(5),
                direction: BufferDirection::ToBuffer,
                slot: MachineSlot::B,
            },
        });
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EmptyRequested {
                machine,
                slot: MachineSlot::B,
            },
        });
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EjectRequested {
                machine,
                slot: MachineSlot::B,
            },
        });
        app.update();

        assert!(app.world().get::<InSlotB>(destination).is_some());
        assert!(app
            .world()
            .get::<Container>(destination)
            .unwrap()
            .solution
            .total_volume()
            .is_positive());
        assert!(app.world().get::<Buffer>(machine).unwrap().0.is_empty());
    }

    #[test]
    fn two_mixing_chambers_can_agitate_concurrently() {
        let mut app = test_app();
        let (client_a, machine_a, _, destination_a) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);
        let (client_b, machine_b, _, destination_b) =
            staged_mixer(&mut app, &[("inaprovaline", 5)], &[("carbon", 5)]);

        request_agitation(&mut app, client_a, machine_a, AgitateDirection::AToB);
        request_agitation(&mut app, client_b, machine_b, AgitateDirection::AToB);
        assert!(app.world().get::<AgitationRun>(machine_a).is_some());
        assert!(app.world().get::<AgitationRun>(machine_b).is_some());

        run_for(&mut app, 5.2);
        let product = reagent(&app, "bicaridine");
        for (machine, destination) in [(machine_a, destination_a), (machine_b, destination_b)] {
            assert!(app.world().get::<AgitationRun>(machine).is_none());
            assert_eq!(
                app.world()
                    .get::<Container>(destination)
                    .unwrap()
                    .solution
                    .volume_of(product),
                Units::whole(10)
            );
        }
    }

    #[test]
    fn loading_two_beakers_fills_a_then_b_then_refuses() {
        // The whole point of the second slot: two beakers can sit in the
        // Mixing Chamber at once, without ejecting one to swap the other in.
        let mut app = test_app();
        let (client, chemist) = chemist(&mut app);
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::MixingChamber),
                ContainerSlot { offset: Vec3::ZERO },
                ContainerSlotB { offset: Vec3::ZERO },
                Transform::default(),
            ))
            .id();

        let first = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), HeldBy(chemist)))
            .id();
        press_e(&mut app, client, machine);
        assert_eq!(
            app.world().get::<InSlot>(first).map(|slot| slot.0),
            Some(machine),
            "the first beaker loads into slot A"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(chemist).unwrap(),
            InteractionMode::Roaming,
            "loading a beaker should not also open the panel"
        );

        let second = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), HeldBy(chemist)))
            .id();
        press_e(&mut app, client, machine);
        assert_eq!(
            app.world().get::<InSlotB>(second).map(|slot| slot.0),
            Some(machine),
            "the second beaker loads into slot B rather than being refused"
        );

        // A third beaker with both slots full just opens the panel, same as
        // walking up to any other full machine.
        let third = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), HeldBy(chemist)))
            .id();
        press_e(&mut app, client, machine);
        assert!(
            app.world().get::<InSlot>(third).is_none()
                && app.world().get::<InSlotB>(third).is_none(),
            "a third beaker has nowhere to go"
        );
        assert_eq!(
            app.world().get::<HeldBy>(third).map(|held| held.0),
            Some(chemist),
            "so it stays in hand"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(chemist).unwrap(),
            InteractionMode::UsingMachine(machine),
            "and the panel opens instead"
        );
    }

    #[test]
    fn buffer_transfer_can_target_the_second_slot() {
        let mut app = test_app();
        let sugar = reagent(&app, "sugar");
        let machine = app
            .world_mut()
            .spawn(Buffer(Solution::new(Units::whole(300))))
            .id();
        let mut container = Container::new(ContainerKind::LargeBeaker);
        let _ = container.solution.add(sugar, Units::whole(20));
        let beaker_b = app.world_mut().spawn((container, InSlotB(machine))).id();

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: BufferTransferRequested {
                machine,
                reagent: sugar,
                amount: Units::whole(20),
                direction: BufferDirection::ToBuffer,
                slot: MachineSlot::B,
            },
        });
        app.update();

        let buffer = app.world().get::<Buffer>(machine).unwrap();
        assert_eq!(
            buffer.0.volume_of(sugar),
            Units::whole(20),
            "slot B feeds the same shared buffer slot A does"
        );
        let container = app.world().get::<Container>(beaker_b).unwrap();
        assert_eq!(container.solution.volume_of(sugar), Units::ZERO);
    }

    #[test]
    fn ejecting_one_mixing_chamber_slot_leaves_the_other_alone() {
        let mut app = test_app();
        let (client, chemist) = chemist(&mut app);
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::MixingChamber),
                Transform::default(),
            ))
            .id();
        let beaker_a = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), InSlot(machine)))
            .id();
        let beaker_b = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), InSlotB(machine)))
            .id();

        press_e(&mut app, client, machine);

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EjectRequested {
                machine,
                slot: MachineSlot::B,
            },
        });
        app.update();

        assert!(
            app.world().get::<InSlotB>(beaker_b).is_none(),
            "slot B is freed"
        );
        assert_eq!(
            app.world().get::<HeldBy>(beaker_b).map(|held| held.0),
            Some(chemist)
        );
        assert!(
            app.world().get::<InSlot>(beaker_a).is_some(),
            "slot A's beaker is untouched by ejecting slot B"
        );
    }

    #[test]
    fn a_reagent_moves_from_slot_a_to_slot_b_through_the_shared_buffer() {
        // The mixing model the second slot exists for: pull a reagent out of
        // one beaker and push it into the other, without ejecting either one.
        let mut app = test_app();
        let sugar = reagent(&app, "sugar");
        let machine = app
            .world_mut()
            .spawn(Buffer(Solution::new(Units::whole(300))))
            .id();
        let mut source = Container::new(ContainerKind::LargeBeaker);
        let _ = source.solution.add(sugar, Units::whole(30));
        app.world_mut().spawn((source, InSlot(machine)));
        app.world_mut()
            .spawn((Container::new(ContainerKind::LargeBeaker), InSlotB(machine)));

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: BufferTransferRequested {
                machine,
                reagent: sugar,
                amount: Units::whole(30),
                direction: BufferDirection::ToBuffer,
                slot: MachineSlot::A,
            },
        });
        app.update();

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: BufferTransferRequested {
                machine,
                reagent: sugar,
                amount: Units::whole(30),
                direction: BufferDirection::ToContainer,
                slot: MachineSlot::B,
            },
        });
        app.update();

        let buffer = app.world().get::<Buffer>(machine).unwrap();
        assert_eq!(buffer.0.volume_of(sugar), Units::ZERO);

        let mut query = app.world_mut().query::<(&Container, &InSlotB)>();
        let (container_b, _) = query
            .iter(app.world())
            .find(|(_, slot)| slot.0 == machine)
            .expect("slot B should still be loaded");
        assert_eq!(
            container_b.solution.volume_of(sugar),
            Units::whole(30),
            "it should have crossed from A to B without ever being ejected"
        );
    }

    #[test]
    fn grinding_produce_yields_its_extract_and_a_contaminant() {
        let mut app = test_app();
        let ambrosia = produce(&app, "Ambrosia");
        let grinder = grinder(&mut app, &[ambrosia], Some(ContainerKind::LargeBeaker));

        grind(&mut app, grinder, false);

        let dylovene = reagent(&app, "dylovene");
        let fibre = reagent(&app, "plant_fibre");
        let contents = slot_contents(&mut app, grinder);
        assert_eq!(contents.volume_of(dylovene), Units::whole(12));
        assert_eq!(contents.volume_of(fibre), Units::whole(8));
        assert!(hopper_of(&app, grinder).is_empty(), "the plant is consumed");
    }

    #[test]
    fn ground_produce_is_impure_until_the_chemmaster_has_had_it() {
        // The whole reason the grinder is worth having *and* worth cleaning up
        // after. Fast to a useful chemical, never deliverable as it comes out.
        use crate::orders::{grade, Outcome, Wanted};

        let mut app = test_app();
        let ambrosia = produce(&app, "Ambrosia");
        let grinder = grinder(
            &mut app,
            &[ambrosia, ambrosia],
            Some(ContainerKind::LargeBeaker),
        );
        grind(&mut app, grinder, true);

        let dylovene = reagent(&app, "dylovene");
        let dirty = slot_contents(&mut app, grinder);
        let db = app.world().resource::<ChemDb>();
        assert_eq!(
            grade(
                Wanted::Exact(dylovene),
                Units::whole(24),
                &dirty,
                ContainerKind::LargeBeaker,
                db,
            )
            .0,
            Outcome::Impure,
            "plant fibre rides along, so a straight grind cannot be handed over"
        );

        // What the Mixing Chamber does: pull the one reagent out into clean glass.
        let mut clean = Solution::new(ContainerKind::Beaker.capacity());
        let _ = clean.add(dylovene, dirty.volume_of(dylovene));
        assert_eq!(
            grade(
                Wanted::Exact(dylovene),
                Units::whole(24),
                &clean,
                ContainerKind::Beaker,
                db,
            )
            .0,
            Outcome::Success
        );
    }

    #[test]
    fn grinding_into_a_full_beaker_keeps_the_produce() {
        // Refusing costs the player a click. Grinding anyway and dropping the
        // overflow costs them a plant they cannot get back, and `Solution::add`
        // holds every caller to that same rule.
        let mut app = test_app();
        let poppy = produce(&app, "Poppy");
        let grinder = grinder(&mut app, &[poppy], Some(ContainerKind::Pill));

        // A 20u pill cannot take a poppy's 20u of yield *and* what is already
        // in it, so top it up first.
        let water = reagent(&app, "water");
        {
            let mut query = app.world_mut().query::<&mut Container>();
            let mut container = query.single_mut(app.world_mut()).unwrap();
            let _ = container.solution.add(water, Units::whole(15));
        }

        grind(&mut app, grinder, true);

        assert_eq!(
            hopper_of(&app, grinder),
            &[poppy],
            "no room means the plant stays in the hopper"
        );
        let bicaridine = reagent(&app, "bicaridine");
        assert_eq!(
            slot_contents(&mut app, grinder).volume_of(bicaridine),
            Units::ZERO
        );
    }

    #[test]
    fn grinding_with_no_beaker_loaded_keeps_the_produce() {
        let mut app = test_app();
        let aloe = produce(&app, "Aloe");
        let grinder = grinder(&mut app, &[aloe], None);

        grind(&mut app, grinder, true);

        assert_eq!(
            hopper_of(&app, grinder),
            &[aloe],
            "nothing to grind into means nothing is ground"
        );
    }

    #[test]
    fn grinding_into_a_loaded_beaker_does_not_bypass_agitation() {
        // Dylovene extracted into a beaker already holding radium has every
        // ingredient for hyronalin, but both recipe sides now share one vessel.
        let mut app = test_app();
        let ambrosia = produce(&app, "Ambrosia");
        let grinder = grinder(&mut app, &[ambrosia], Some(ContainerKind::LargeBeaker));

        let radium = reagent(&app, "radium");
        {
            let mut query = app.world_mut().query::<&mut Container>();
            let mut container = query.single_mut(app.world_mut()).unwrap();
            let _ = container.solution.add(radium, Units::whole(12));
        }

        grind(&mut app, grinder, false);
        let names = reactions_over(&mut app, 8.0);
        assert!(
            !names.iter().any(|key| key == "hyronalin"),
            "a grinder must not counterfeit a staged mix, got {names:?}"
        );
    }

    #[test]
    fn produce_cannot_be_loaded_into_a_machine_that_is_not_the_grinder() {
        // Without the guard, holding a plant and pressing E on the dispenser
        // parks it in the beaker slot, where it does nothing and blocks the
        // slot with no way to tell the player why.
        let mut app = test_app();
        let poppy = produce(&app, "Poppy");
        let dispenser = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::ChemMaster5000),
                ContainerSlot { offset: Vec3::ZERO },
                Transform::default(),
            ))
            .id();

        let client = ClientId::Client(app.world_mut().spawn_empty().id());
        let chemist = app
            .world_mut()
            .spawn((InteractionMode::default(), Chemist { client }))
            .id();
        let item = app
            .world_mut()
            .spawn((Produce(poppy), HeldBy(chemist)))
            .id();

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: InteractRequested { target: dispenser },
        });
        app.update();

        assert!(
            app.world().get::<InSlot>(item).is_none(),
            "produce must not end up in a beaker slot"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(chemist).unwrap(),
            InteractionMode::UsingMachine(dispenser),
            "it should just open the panel instead"
        );
    }

    #[test]
    fn using_the_grinder_with_produce_in_hand_loads_the_hopper() {
        let mut app = test_app();
        let aloe = produce(&app, "Aloe");
        let machine = grinder(&mut app, &[], None);
        app.world_mut()
            .entity_mut(machine)
            .insert(ContainerSlot { offset: Vec3::ZERO });

        let client = ClientId::Client(app.world_mut().spawn_empty().id());
        let chemist = app
            .world_mut()
            .spawn((InteractionMode::default(), Chemist { client }))
            .id();
        let item = app.world_mut().spawn((Produce(aloe), HeldBy(chemist))).id();

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: InteractRequested { target: machine },
        });
        app.update();

        assert_eq!(hopper_of(&app, machine), &[aloe]);
        assert!(
            app.world().get_entity(item).is_err(),
            "the item is consumed on loading, so nothing tracks an entity the \
             player can no longer reach"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(chemist).unwrap(),
            InteractionMode::Roaming,
            "loading the hopper should not also open the panel"
        );
    }

    #[test]
    fn ejecting_puts_the_beaker_in_the_chemists_hand() {
        // The bug: eject placed the container at a fixed `machine + (0, 1, 0.55)`,
        // which is only "in front and on top" for the two machines that happen
        // to face world `+Z`. The grinder faces `-Z` against the storeroom's
        // south wall, so its beaker was posted through the wall 1.85 m up and
        // was simply gone — full batch and all, with nothing said.
        let mut app = test_app();
        let (client, chemist) = chemist(&mut app);
        let machine = grinder(&mut app, &[], Some(ContainerKind::Beaker));
        app.world_mut()
            .entity_mut(machine)
            .insert(Facing(Vec3::NEG_Z));

        let beaker = {
            let mut query = app.world_mut().query_filtered::<Entity, With<InSlot>>();
            query.single(app.world()).expect("a beaker is loaded")
        };

        press_e(&mut app, client, machine);

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EjectRequested {
                machine,
                slot: MachineSlot::A,
            },
        });
        app.update();

        assert!(
            app.world().get::<InSlot>(beaker).is_none(),
            "the slot has to be freed either way"
        );
        assert_eq!(
            app.world().get::<HeldBy>(beaker).map(|held| held.0),
            Some(chemist),
            "an ejected beaker goes to the one place that has no direction to \
             get wrong: the hand of whoever pressed the button"
        );
    }

    #[test]
    fn ejecting_with_full_hands_is_refused() {
        // Used to set the beaker down in front of the machine instead. That
        // fallback needed a spot per *machine*, and the Mixing Chamber's
        // second slot has no natural one to reuse — two ejects in a row with
        // full hands would have landed both beakers in exactly the same
        // place. Refusing outright has no such edge case and matches what a
        // chemist would expect: hands are visibly full.
        let mut app = test_app();
        let (client, chemist) = chemist(&mut app);
        let machine = grinder(&mut app, &[], Some(ContainerKind::Beaker));
        app.world_mut()
            .entity_mut(machine)
            .insert(Facing(Vec3::NEG_Z));
        // Already carrying something else, so there is nowhere to hand it.
        app.world_mut()
            .spawn((Container::new(ContainerKind::Bottle), HeldBy(chemist)));

        let beaker = {
            let mut query = app.world_mut().query_filtered::<Entity, With<InSlot>>();
            query.single(app.world()).expect("a beaker is loaded")
        };

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EjectRequested {
                machine,
                slot: MachineSlot::A,
            },
        });
        app.update();

        assert!(
            app.world().get::<InSlot>(beaker).is_some(),
            "the beaker stays put rather than landing anywhere unexpected"
        );
        assert!(app.world().get::<HeldBy>(beaker).is_none());
    }

    #[test]
    fn ejecting_from_a_machine_someone_else_is_working_is_refused() {
        // Ejecting used to put the beaker on the floor, where anybody could
        // have picked it up anyway. It puts it in a *hand* now, and the hand
        // belongs to whoever sent the message — so without this a client could
        // pull the other chemist's batch out of the grinder from across the
        // lab, in the middle of their grind.
        let mut app = test_app();
        let (client, _) = chemist(&mut app);
        let (_, other) = chemist(&mut app);
        let machine = grinder(&mut app, &[], Some(ContainerKind::Beaker));
        app.world_mut()
            .entity_mut(machine)
            .insert(Facing(Vec3::NEG_Z));
        app.world_mut()
            .get_mut::<Machine>(machine)
            .unwrap()
            .in_use_by = Some(other);

        let beaker = {
            let mut query = app.world_mut().query_filtered::<Entity, With<InSlot>>();
            query.single(app.world()).expect("a beaker is loaded")
        };

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EjectRequested {
                machine,
                slot: MachineSlot::A,
            },
        });
        app.update();

        assert!(
            app.world().get::<InSlot>(beaker).is_some(),
            "it stays where the chemist who claimed the machine put it"
        );
        assert!(app.world().get::<HeldBy>(beaker).is_none());
    }

    /// A locker, and a chemist standing at it holding `item`.
    fn locker(app: &mut App) -> Entity {
        app.world_mut()
            .spawn((
                Machine::new(MachineKind::Locker),
                Transform::default(),
                Solid {
                    half_extents: Vec3::splat(0.5),
                },
            ))
            .id()
    }

    #[test]
    fn front_of_uses_collision_geometry_with_an_identity_scale_root() {
        let machine = Transform::from_translation(Vec3::new(4.0, 0.575, 4.6));
        let solid = Solid {
            half_extents: Vec3::new(1.7, 0.575, 0.35),
        };

        let spot = front_of(&machine, &solid, Some(&Facing(Vec3::NEG_Z)), 0.08);

        assert_eq!(machine.scale, Vec3::ONE);
        assert!(
            spot.distance(Vec3::new(4.0, 0.08, 3.9)) < 0.000_01,
            "set-down spot {spot} did not use the delivery counter's collider"
        );
    }

    fn press_e(app: &mut App, client: ClientId, target: Entity) {
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: InteractRequested { target },
        });
        app.update();
    }

    #[test]
    fn a_locker_takes_whatever_is_in_hand_and_gives_it_back() {
        let mut app = test_app();
        let (client, chemist) = chemist(&mut app);
        let locker = locker(&mut app);
        let beaker = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), HeldBy(chemist)))
            .id();

        press_e(&mut app, client, locker);

        assert_eq!(
            app.world().get::<Stored>(beaker).map(|stored| stored.0),
            Some(locker),
        );
        assert!(
            app.world().get::<HeldBy>(beaker).is_none(),
            "it is on the shelf now, not in a hand"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(chemist).unwrap(),
            InteractionMode::Roaming,
            "putting something away should not also open the panel"
        );

        // Taking from storage is a panel action, so claim the locker just as
        // a real player must after putting the first item away.
        press_e(&mut app, client, locker);

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: TakeRequested {
                machine: locker,
                item: beaker,
            },
        });
        app.update();

        assert!(app.world().get::<Stored>(beaker).is_none());
        assert_eq!(
            app.world().get::<HeldBy>(beaker).map(|held| held.0),
            Some(chemist),
        );
    }

    #[test]
    fn a_locker_stores_things_that_are_not_containers() {
        // The whole reason it is a locker and not a beaker rack. Produce is the
        // one non-container that already exists; anything added later takes
        // this path without the locker learning about it.
        let mut app = test_app();
        let (client, chemist) = chemist(&mut app);
        let locker = locker(&mut app);
        let aloe = produce(&app, "Aloe");
        let item = app.world_mut().spawn((Produce(aloe), HeldBy(chemist))).id();

        press_e(&mut app, client, locker);

        assert_eq!(
            app.world().get::<Stored>(item).map(|stored| stored.0),
            Some(locker),
            "a locker with no hopper must not eat the plant the way the \
             grinder does"
        );
        assert!(app.world().get_entity(item).is_ok());
    }

    #[test]
    fn a_full_locker_opens_its_panel_instead_of_refusing_in_silence() {
        let mut app = test_app();
        let (client, chemist) = chemist(&mut app);
        let locker = locker(&mut app);
        for _ in 0..LOCKER_CAPACITY {
            app.world_mut()
                .spawn((Container::new(ContainerKind::Beaker), Stored(locker)));
        }
        let beaker = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), HeldBy(chemist)))
            .id();

        press_e(&mut app, client, locker);

        assert!(
            app.world().get::<Stored>(beaker).is_none(),
            "the thirteenth item must not go in"
        );
        assert_eq!(
            app.world().get::<HeldBy>(beaker).map(|held| held.0),
            Some(chemist),
            "and must stay in hand rather than being dropped on the floor"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(chemist).unwrap(),
            InteractionMode::UsingMachine(locker),
            "the panel is what tells the player the locker is full"
        );
    }

    #[test]
    fn a_locker_only_hands_over_what_is_actually_in_it() {
        // Both ids come off the wire. Without the check a client could name any
        // entity in the world and have the server hand it over — including one
        // the other chemist is holding.
        let mut app = test_app();
        let (client, _) = chemist(&mut app);
        let (_, other) = chemist(&mut app);
        let locker = locker(&mut app);
        let elsewhere = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), HeldBy(other)))
            .id();

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: TakeRequested {
                machine: locker,
                item: elsewhere,
            },
        });
        app.update();

        assert_eq!(
            app.world().get::<HeldBy>(elsewhere).map(|held| held.0),
            Some(other),
            "it was never in the locker, so the locker cannot give it away"
        );
    }

    #[test]
    fn leaving_releases_a_machine_still_held_under_the_reference_book() {
        // Opening the book at a machine keeps the claim on purpose — a chemist
        // checking a recipe mid-batch has not walked away from the dispenser —
        // so every release path has to recognise that shape too. A bare
        // `UsingMachine` check here would leave the machine in use for the
        // rest of the shift the moment somebody closed out from the book.
        let mut app = test_app();
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::ChemMaster5000),
                Transform::default(),
            ))
            .id();
        let client = ClientId::Client(app.world_mut().spawn_empty().id());
        let chemist = app
            .world_mut()
            .spawn((InteractionMode::default(), Chemist { client }))
            .id();

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: InteractRequested { target: machine },
        });
        app.update();
        assert_eq!(
            app.world().get::<Machine>(machine).unwrap().in_use_by,
            Some(chemist),
            "the claim has to exist before this test means anything"
        );

        // What pressing B at an open panel leaves behind.
        app.world_mut()
            .entity_mut(chemist)
            .insert(InteractionMode::ReadingBook(Some(machine)));
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: LeaveMachineRequested,
        });
        app.update();

        assert_eq!(
            app.world().get::<Machine>(machine).unwrap().in_use_by,
            None,
            "the dispenser must not stay locked against the other chemist"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(chemist).unwrap(),
            InteractionMode::Roaming
        );
    }

    #[test]
    fn a_machine_can_only_be_claimed_by_one_chemist() {
        // The co-op invariant, checkable long before co-op exists.
        let mut app = test_app();
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::ChemMaster5000),
                Transform::default(),
            ))
            .id();
        // Two real connections, each driving their own chemist.
        let first_client = ClientId::Client(app.world_mut().spawn_empty().id());
        let second_client = ClientId::Client(app.world_mut().spawn_empty().id());
        let first = app
            .world_mut()
            .spawn((
                InteractionMode::default(),
                Chemist {
                    client: first_client,
                },
            ))
            .id();
        let second = app
            .world_mut()
            .spawn((
                InteractionMode::default(),
                Chemist {
                    client: second_client,
                },
            ))
            .id();

        app.world_mut().write_message(FromClient {
            client_id: first_client,
            message: InteractRequested { target: machine },
        });
        app.update();

        app.world_mut().write_message(FromClient {
            client_id: second_client,
            message: InteractRequested { target: machine },
        });
        app.update();

        assert_eq!(
            app.world().get::<Machine>(machine).unwrap().in_use_by,
            Some(first),
            "the first chemist keeps the machine"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(second).unwrap(),
            InteractionMode::Roaming,
            "the second chemist must be turned away, not silently take over"
        );
    }

    #[test]
    fn four_clients_claim_both_core_lanes_without_stealing_each_others_panels() {
        let mut app = test_app();
        let machines: Vec<Entity> = [
            MachineKind::ChemMaster5000,
            MachineKind::ChemMaster5000,
            MachineKind::MixingChamber,
            MachineKind::MixingChamber,
        ]
        .into_iter()
        .map(|kind| {
            app.world_mut()
                .spawn((Machine::new(kind), Transform::default()))
                .id()
        })
        .collect();
        let chemists: Vec<(ClientId, Entity)> = (0..4).map(|_| chemist(&mut app)).collect();

        for ((client, _), machine) in chemists.iter().zip(machines.iter()) {
            press_e(&mut app, *client, *machine);
        }
        for ((_, player), machine) in chemists.iter().zip(machines.iter()) {
            assert_eq!(
                app.world().get::<Machine>(*machine).unwrap().in_use_by,
                Some(*player),
                "every physical lane machine should have its own claim"
            );
        }

        // Each operator attempts to take the next operator's station. Every
        // request is refused independently and their original panel remains.
        for index in 0..4 {
            let requester = chemists[index];
            let occupied = machines[(index + 1) % machines.len()];
            press_e(&mut app, requester.0, occupied);
        }
        for ((_, player), machine) in chemists.iter().zip(machines.iter()) {
            assert_eq!(
                app.world().get::<Machine>(*machine).unwrap().in_use_by,
                Some(*player)
            );
            assert_eq!(
                *app.world().get::<InteractionMode>(*player).unwrap(),
                InteractionMode::UsingMachine(*machine),
                "a refused claim must not close or redirect the operator's own panel"
            );
        }
    }

    #[test]
    fn another_player_can_load_a_free_mixer_slot_without_stealing_the_panel() {
        let mut app = test_app();
        let (owner_client, owner) = chemist(&mut app);
        let (loader_client, loader) = chemist(&mut app);
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::MixingChamber),
                ContainerSlot { offset: Vec3::ZERO },
                ContainerSlotB { offset: Vec3::X },
                Transform::default(),
            ))
            .id();
        app.world_mut()
            .spawn((Container::new(ContainerKind::Beaker), InSlot(machine)));

        press_e(&mut app, owner_client, machine);
        assert_eq!(
            app.world().get::<Machine>(machine).unwrap().in_use_by,
            Some(owner)
        );

        let second_beaker = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), HeldBy(loader)))
            .id();
        press_e(&mut app, loader_client, machine);

        assert_eq!(
            app.world().get::<InSlotB>(second_beaker).map(|slot| slot.0),
            Some(machine),
            "the free physical slot remains collaborative while the panel is claimed"
        );
        assert_eq!(
            app.world().get::<Machine>(machine).unwrap().in_use_by,
            Some(owner),
            "loading glassware must not change panel ownership"
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(owner).unwrap(),
            InteractionMode::UsingMachine(machine)
        );
        assert_eq!(
            *app.world().get::<InteractionMode>(loader).unwrap(),
            InteractionMode::Roaming,
            "loading is a physical action, not a second panel claim"
        );
    }

    #[test]
    fn chemically_incapacitated_clients_cannot_operate_or_take_from_machines() {
        let mut app = test_app();
        let (client, player) = chemist(&mut app);
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::Locker),
                Transform::default(),
                Solid {
                    half_extents: Vec3::splat(0.5),
                },
            ))
            .id();
        let stored = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Beaker), Stored(machine)))
            .id();
        let mut blood = crate::body::Bloodstream::default();
        blood.0.add_status(chem_sim::StatusKind::Sedated, 20.0, 2.0);
        app.world_mut()
            .entity_mut(player)
            .insert((crate::body::Body::default(), blood));

        press_e(&mut app, client, machine);
        assert_eq!(
            app.world().get::<Machine>(machine).unwrap().in_use_by,
            None,
            "sedation must be enforced by the authority, not only by local input"
        );

        // Forge the claim too: `handle_take` must independently enforce the
        // condition rather than relying on the panel-open path.
        app.world_mut()
            .get_mut::<Machine>(machine)
            .unwrap()
            .in_use_by = Some(player);
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: TakeRequested {
                machine,
                item: stored,
            },
        });
        app.update();
        assert_eq!(
            app.world().get::<Stored>(stored).map(|stored| stored.0),
            Some(machine),
            "an incapacitated forged request cannot remove stored equipment"
        );
    }

    // -----------------------------------------------------------------------
    // Reaction chamber
    // -----------------------------------------------------------------------

    /// A chamber with a beaker in the slot, dialled to `target`.
    fn chamber(app: &mut App, target: f32, powered: bool, contents: &[(&str, i32)]) -> Entity {
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::ReactionChamber),
                Thermostat {
                    target: Kelvin(target),
                    powered,
                },
                Transform::default(),
            ))
            .id();

        let ids: Vec<(ReagentId, i32)> = contents
            .iter()
            .map(|(key, amount)| (reagent(app, key), *amount))
            .collect();
        let mut container = Container::new(ContainerKind::Beaker);
        for (id, amount) in ids {
            let overflow = container.solution.add(id, Units::whole(amount));
            assert!(overflow.is_zero(), "the test beaker overflowed");
        }
        app.world_mut().spawn((container, InSlot(machine)));
        machine
    }

    #[test]
    fn a_target_request_is_clamped_to_the_dial_range() {
        // The slider already clamps client-side, but a request is trusted
        // input from the network — the server has to hold the same line
        // rather than merely suggesting it, same as the locked-reagent check
        // on dispensing.
        let mut app = test_app();
        let machine = chamber(&mut app, 293.0, false, &[]);

        for (requested, expected) in [(50.0, TEMPERATURE_MIN), (9000.0, TEMPERATURE_MAX)] {
            app.world_mut().write_message(FromClient {
                client_id: ClientId::Server,
                message: SetTargetTemperature {
                    machine,
                    target: Kelvin(requested),
                },
            });
            app.update();

            assert_eq!(
                app.world().get::<Thermostat>(machine).unwrap().target,
                Kelvin(expected),
                "a target of {requested}K should clamp to {expected}K"
            );
        }
    }

    /// Runs `seconds` of game time in one-tenth-second frames.
    fn run_for(app: &mut App, seconds: f32) {
        let frames = (seconds / 0.1).round() as u32;
        for _ in 0..frames {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        }
    }

    /// Takes every `ReactionsFired` written so far.
    ///
    /// Drains rather than reads, so it can be called every frame without
    /// double-counting — `Messages` keeps two frames' worth, so a plain cursor
    /// read per frame would see each report twice.
    fn drain_fired(app: &mut App) -> Vec<ReactionsFired> {
        app.world_mut()
            .resource_mut::<Messages<ReactionsFired>>()
            .drain()
            .collect()
    }

    /// The same, as recipe names.
    fn drain_reported(app: &mut App) -> Vec<String> {
        let fired = drain_fired(app);
        let db = app.world().resource::<ChemDb>();
        fired
            .iter()
            .flat_map(|event| event.reactions.iter())
            .map(|id| db.reactions.get(*id).key.clone())
            .collect()
    }

    /// A loose beaker holding whole units of each named reagent, with nothing
    /// reacted yet — the state `tick_reactions` is meant to pick up.
    fn loose_beaker(app: &mut App, contents: &[(&str, i32)]) -> Entity {
        let mut container = Container::new(ContainerKind::LargeBeaker);
        for (key, amount) in contents {
            let reagent = reagent(app, key);
            let overflow = container.solution.add(reagent, Units::whole(*amount));
            assert!(overflow.is_zero(), "the test beaker overflowed");
        }
        app.world_mut().spawn(container).id()
    }

    /// A claimed Mixing Chamber with independently prepared beakers in A and
    /// B. Ingredients are inserted without calling the resolver so the helper
    /// models the exact state immediately before the operator presses the
    /// agitation control.
    fn staged_mixer(
        app: &mut App,
        side_a: &[(&str, i32)],
        side_b: &[(&str, i32)],
    ) -> (ClientId, Entity, Entity, Entity) {
        let side_a = side_a
            .iter()
            .map(|(key, amount)| (*key, Units::whole(*amount)))
            .collect::<Vec<_>>();
        let side_b = side_b
            .iter()
            .map(|(key, amount)| (*key, Units::whole(*amount)))
            .collect::<Vec<_>>();
        staged_mixer_units(app, &side_a, &side_b)
    }

    fn staged_mixer_units(
        app: &mut App,
        side_a: &[(&str, Units)],
        side_b: &[(&str, Units)],
    ) -> (ClientId, Entity, Entity, Entity) {
        let (client, player) = chemist(app);
        let mut machine_state = Machine::new(MachineKind::MixingChamber);
        machine_state.in_use_by = Some(player);
        let machine = app
            .world_mut()
            .spawn((
                machine_state,
                Buffer(Solution::new(Units::whole(300))),
                Transform::default(),
            ))
            .id();

        let make_beaker = |contents: &[(&str, Units)]| {
            let mut container = Container::new(ContainerKind::LargeBeaker);
            for (key, amount) in contents {
                let id = reagent(app, key);
                assert!(
                    container.solution.add(id, *amount).is_zero(),
                    "test beaker overflowed"
                );
            }
            container
        };
        let a = make_beaker(side_a);
        let b = make_beaker(side_b);
        let beaker_a = app.world_mut().spawn((a, InSlot(machine))).id();
        let beaker_b = app.world_mut().spawn((b, InSlotB(machine))).id();
        (client, machine, beaker_a, beaker_b)
    }

    fn request_agitation(
        app: &mut App,
        client: ClientId,
        machine: Entity,
        direction: AgitateDirection,
    ) {
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: AgitateRequested { machine, direction },
        });
        app.update();
    }

    fn reaction_id(app: &App, key: &str) -> chem_sim::ReactionId {
        app.world()
            .resource::<ChemDb>()
            .reactions
            .find(key)
            .expect("the recipe should exist")
            .id
    }

    /// Every report written across `seconds` of game time, plus anything
    /// already pending when the run started.
    fn fired_over(app: &mut App, seconds: f32) -> Vec<ReactionsFired> {
        let mut reports = drain_fired(app);
        let frames = (seconds / 0.1).round() as u32;
        for _ in 0..frames {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            reports.extend(drain_fired(app));
        }
        reports
    }

    /// Every recipe reported across `seconds` of game time, plus anything
    /// already pending when the run started.
    ///
    /// Collected as it goes rather than read at the end, because a rated
    /// reaction reports when its batch *finishes* — seconds after the pour
    /// that started it, and long after the instant half of the same chain has
    /// been reported and aged out of the message buffer.
    fn reactions_over(app: &mut App, seconds: f32) -> Vec<String> {
        let mut names = drain_reported(app);
        let frames = (seconds / 0.1).round() as u32;
        for _ in 0..frames {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            names.extend(drain_reported(app));
        }
        names
    }

    fn slot_temperature(app: &mut App, machine: Entity) -> Kelvin {
        slot_contents(app, machine).temperature
    }

    /// Puts the loaded beaker at `kelvin` without waiting for the chamber.
    fn preheat_slot(app: &mut App, machine: Entity, kelvin: f32) {
        let mut query = app.world_mut().query::<(&mut Container, &InSlot)>();
        let (mut container, _) = query
            .iter_mut(app.world_mut())
            .find(|(_, slot)| slot.0 == machine)
            .expect("a container should be loaded");
        container.solution.temperature = Kelvin(kelvin);
    }

    #[test]
    fn a_powered_chamber_heats_its_beaker_and_settles_on_the_target() {
        let mut app = test_app();
        let machine = chamber(&mut app, 400.0, true, &[("water", 20)]);

        run_for(&mut app, 2.0);
        let partway = slot_temperature(&mut app, machine);
        assert!(
            partway.0 > 293.15 && partway.0 < 400.0,
            "it should be on its way, not there yet: {partway}"
        );

        // Closing the last fraction of a kelvin is the slow part of an
        // exponential approach — a full settle takes around forty seconds at
        // `CHAMBER_RATE`, which is deliberate: the chamber is meant to be
        // watched, not waited on once.
        run_for(&mut app, 60.0);
        assert_eq!(
            slot_temperature(&mut app, machine),
            Kelvin(400.0),
            "and it should stop exactly on the dial rather than creep at it"
        );
    }

    #[test]
    fn an_unpowered_chamber_heats_nothing() {
        let mut app = test_app();
        let machine = chamber(&mut app, 500.0, false, &[("water", 20)]);

        run_for(&mut app, 5.0);

        assert_eq!(
            slot_temperature(&mut app, machine),
            Kelvin::AMBIENT,
            "the dial is set but the switch is off"
        );
    }

    #[test]
    fn an_empty_chamber_is_a_no_op() {
        let mut app = test_app();
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::ReactionChamber),
                Thermostat {
                    target: Kelvin(500.0),
                    powered: true,
                },
                Transform::default(),
            ))
            .id();

        // Nothing loaded. The assertion is that this runs at all: an empty slot
        // is the normal state of the machine between batches, and a chamber
        // that panicked on it would take the whole shift with it.
        run_for(&mut app, 5.0);

        let mut slots = app.world_mut().query::<&InSlot>();
        assert_eq!(
            slots.iter(app.world()).count(),
            0,
            "and heating an empty chamber must not conjure a container"
        );
        assert!(app.world().get::<Thermostat>(machine).unwrap().powered);
    }

    #[test]
    fn a_beaker_out_of_the_chamber_cools_back_to_the_room() {
        let mut app = test_app();
        let machine = chamber(&mut app, 450.0, true, &[("water", 20)]);
        run_for(&mut app, 60.0);
        assert_eq!(slot_temperature(&mut app, machine), Kelvin(450.0));

        // Switch off: the beaker is now just a hot beaker on a bench.
        app.world_mut()
            .get_mut::<Thermostat>(machine)
            .unwrap()
            .powered = false;
        run_for(&mut app, 10.0);

        let cooling = slot_temperature(&mut app, machine);
        assert!(
            cooling.0 < 450.0,
            "it should be losing heat to the room: {cooling}"
        );
        assert!(
            cooling.0 > Kelvin::AMBIENT.0,
            "but slowly enough to still be worth carrying somewhere: {cooling}"
        );
    }

    #[test]
    fn crossing_a_recipes_minimum_temperature_is_what_fires_it() {
        // The point of the whole machine: no gating code anywhere in the game
        // layer. `Container::mutate` resolves on every change, and
        // `Reaction::max_scale` has always checked `min_temp`.
        let data = ChemData::from_ron(
            r#"[
                (id: "cold", name: "Cold", color: (0.2, 0.4, 0.9), dispensable: true),
                (id: "hot",  name: "Hot",  color: (0.9, 0.4, 0.2)),
            ]"#,
            r#"[
                (id: "bake", reactants: [("cold", 1)], products: [("hot", 1)],
                 min_temp: Some((380.0)), hints: ["Needs heat."]),
            ]"#,
        )
        .expect("fixture chemistry should load");

        let mut app = test_app();
        app.insert_resource(ChemDb(data));

        let machine = chamber(&mut app, 400.0, true, &[("cold", 20)]);
        let hot = reagent(&app, "hot");

        run_for(&mut app, 0.5);
        assert_eq!(
            slot_contents(&mut app, machine).volume_of(hot),
            Units::ZERO,
            "still too cold"
        );

        run_for(&mut app, 20.0);
        assert_eq!(
            slot_contents(&mut app, machine).volume_of(hot),
            Units::whole(20),
            "and it fires the moment the chamber carries it over the line"
        );
    }

    /// Reagents added to a beaker that is *already* too hot.
    ///
    /// This is the only way a chamber can overheat something, and the reason is
    /// worth writing down: the resolver is instant, so a reaction fires the
    /// moment the rising temperature crosses its `min_temp` — hundreds of
    /// degrees before the chamber reaches its overheat threshold. Heating a
    /// loaded beaker slowly can therefore never spoil it. What spoils a batch
    /// is putting the reagents into a chamber somebody already left running,
    /// or a reaction exothermic enough to cook itself (covered in
    /// `crates/chem_sim/tests/reactions.rs`).
    #[test]
    fn reagents_dropped_into_an_already_hot_chamber_waste_the_batch() {
        let data = ChemData::from_ron(
            r#"[
                (id: "cold", name: "Cold", color: (0.2, 0.4, 0.9), dispensable: true),
                (id: "hot",  name: "Hot",  color: (0.9, 0.4, 0.2)),
            ]"#,
            r#"[
                (id: "bake", reactants: [("cold", 1)], products: [("hot", 1)],
                 min_temp: Some((380.0)), overheat_temp: Some((420.0)),
                 overheat: ReducedYield(over: 60.0), hints: ["Needs heat."]),
            ]"#,
        )
        .expect("fixture chemistry should load");

        let mut app = test_app();
        app.insert_resource(ChemDb(data));

        let machine = chamber(&mut app, 450.0, true, &[("cold", 20)]);
        // Already at 440K when the reagents are in it: hot enough to run the
        // recipe, and 20K past the point where it starts going wrong. Nothing
        // has resolved yet, because nothing has changed the solution.
        preheat_slot(&mut app, machine, 440.0);

        // One frame of the chamber nudging the temperature is enough to trigger
        // the resolve, and it resolves hot.
        run_for(&mut app, 0.2);

        let made = slot_contents(&mut app, machine).volume_of(reagent(&app, "hot"));
        assert!(
            made.is_positive() && made < Units::whole(20),
            "past the threshold the yield should fall short, got {made} from 20u"
        );
        assert_eq!(
            slot_contents(&mut app, machine).volume_of(reagent(&app, "cold")),
            Units::ZERO,
            "and the reactants go in full regardless — that is what overheating costs"
        );
    }
}
