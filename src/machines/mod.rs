//! Lab machines and the actions they perform.
//!
//! Panels never touch state directly. They emit the messages below and the
//! systems here apply them, so co-op can replicate actions later without
//! rewriting any UI.

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::thermal::approach;
use chem_sim::{Kelvin, ReagentId, Solution, Units};
use serde::{Deserialize, Serialize};

use crate::chem_data::ChemDb;
use crate::containers::{
    set_down_lift, spawn_container, Container, ContainerKind, HeldBy, InSlot, Stored,
};
use crate::interaction::{
    InteractRequested, InteractionMode, LeaveMachineRequested, MachineOpened,
};
use crate::knowledge::Knowledge;
use crate::net::is_authority;
use crate::player::Chemist;
use crate::produce::{Produce, ProduceCatalog, ProduceId};
use crate::AppState;

pub struct MachinePlugin;

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
            .add_mapped_client_message::<PackageRequested>(Channel::Ordered)
            .add_mapped_client_message::<AnalyzeRequested>(Channel::Ordered)
            .add_mapped_client_message::<GrindRequested>(Channel::Ordered)
            .add_mapped_client_message::<SetTargetTemperature>(Channel::Ordered)
            .add_mapped_client_message::<SetHeaterPower>(Channel::Ordered)
            .add_message::<ReactionsFired>()
            .add_systems(
                Update,
                (
                    handle_machine_interact,
                    handle_leave_machine,
                    handle_dispense,
                    handle_buffer_transfer,
                    handle_package,
                    handle_analyze,
                    handle_grind,
                    handle_eject,
                    handle_take,
                    handle_empty,
                    handle_thermostat_controls,
                    apply_thermostats,
                    cool_to_ambient,
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
    Dispenser,
    ChemMaster,
    Grinder,
    Analyzer,
    /// Unlimited base reagents, but its output cannot be delivered for credit.
    /// Experimenting costs time, not materials.
    TestBench,
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
    /// Every kind, which is also every machine: the lab holds exactly one of
    /// each. Both the spawner and the client's fit-out iterate this, so a new
    /// machine cannot be added to one and forgotten in the other.
    pub const ALL: [MachineKind; 9] = [
        MachineKind::Dispenser,
        MachineKind::ChemMaster,
        MachineKind::Grinder,
        MachineKind::Analyzer,
        MachineKind::TestBench,
        MachineKind::DeliveryWindow,
        MachineKind::StandingBoard,
        MachineKind::ReactionChamber,
        MachineKind::Locker,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MachineKind::Dispenser => "Chemical Dispenser",
            MachineKind::ChemMaster => "ChemMaster 4000",
            MachineKind::Grinder => "Reagent Grinder",
            MachineKind::Analyzer => "Sample Analyzer",
            MachineKind::TestBench => "Test Bench",
            MachineKind::DeliveryWindow => "Delivery Window",
            MachineKind::StandingBoard => "Standing Board",
            MachineKind::ReactionChamber => "Reaction Chamber",
            MachineKind::Locker => "Storage Locker",
        }
    }

    pub fn screen_color(self) -> Color {
        match self {
            MachineKind::Dispenser => Color::srgb(0.30, 0.75, 0.95),
            MachineKind::ChemMaster => Color::srgb(0.45, 0.90, 0.55),
            MachineKind::Grinder => Color::srgb(0.95, 0.65, 0.25),
            MachineKind::Analyzer => Color::srgb(0.85, 0.45, 0.95),
            MachineKind::TestBench => Color::srgb(0.95, 0.85, 0.35),
            MachineKind::DeliveryWindow => Color::srgb(0.95, 0.35, 0.35),
            MachineKind::StandingBoard => Color::srgb(0.95, 0.88, 0.45),
            MachineKind::ReactionChamber => Color::srgb(0.98, 0.45, 0.18),
            MachineKind::Locker => Color::srgb(0.40, 0.85, 0.80),
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

/// The temperatures the panel offers.
///
/// Fixed buttons rather than a slider: this is a first-person game and a
/// draggable control under a crosshair is miserable. The spread covers freezing,
/// room temperature, and the bands the hot recipes sit in.
pub const TEMPERATURE_PRESETS: [f32; 6] = [273.0, 293.0, 350.0, 400.0, 450.0, 500.0];

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
/// ChemMaster — but not forever. This is the clock a chemist is racing.
const AMBIENT_RATE: f32 = 0.06;

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
}

impl Machine {
    pub fn new(kind: MachineKind) -> Self {
        Machine {
            kind,
            in_use_by: None,
        }
    }

    pub fn available_to(&self, player: Entity) -> bool {
        self.in_use_by.is_none_or(|current| current == player)
    }
}

/// Where a container sits when loaded, as an offset from the machine's origin.
#[derive(Component)]
pub struct ContainerSlot {
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

/// The ChemMaster's internal buffer.
#[derive(Component, Serialize, Deserialize)]
pub struct Buffer(pub Solution);

/// Set on a ChemMaster whose buffer currently holds test-bench stock.
///
/// [`TestBenchStock`] marks *glassware*, which was enough right up until the
/// buffer got involved: practice stock could be pushed into the buffer, and
/// then either drawn back into a clean beaker or packaged into a pill, and both
/// of those are containers that never touched the test bench and so carried no
/// mark. That laundered practice stock into deliverable product and bypassed
/// the whole "experimenting costs time, not materials" economy. The taint has
/// to live on the buffer for the same reason it lives on the container rather
/// than the reagent: it must survive being reacted and repackaged.
///
/// Cleared when the buffer runs dry, matching how rinsing a beaker clears
/// `TestBenchStock` — nothing is contaminated by association once it is empty.
#[derive(Component)]
pub struct BufferPracticeStock;

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

/// Marks a container that has held test-bench stock.
///
/// The bench is for working things out, not for filling orders — without this
/// it would simply be a second dispenser, and experimenting would carry no
/// cost at all.
#[derive(Component)]
pub struct TestBenchStock;

impl Default for DispenseAmount {
    fn default() -> Self {
        DispenseAmount(Units::whole(10))
    }
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
    fn from_report(container: Entity, report: &chem_sim::ResolveReport) -> Option<Self> {
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
}

#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct EmptyRequested {
    #[entities]
    pub machine: Entity,
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

#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct BufferTransferRequested {
    #[entities]
    pub machine: Entity,
    pub reagent: ReagentId,
    pub amount: Units,
    pub direction: BufferDirection,
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
    mut machines: Query<(&mut Machine, Option<&ContainerSlot>, &Transform)>,
    mut hoppers: Query<&mut Hopper>,
    mut modes: Query<&mut InteractionMode>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<(Entity, &HeldBy)>,
    produce: Query<&Produce>,
    slotted: Query<(Entity, &InSlot)>,
    stored: Query<(Entity, &Stored)>,
    bodies: Query<&crate::body::Body>,
    mut opened: MessageWriter<ToClients<MachineOpened>>,
) {
    for request in requests.read() {
        // The sender's identity comes from the connection, never from the
        // message. A client that could name its own player entity could act as
        // the other chemist.
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        // A collapsed chemist cannot work a machine. Checked before anything
        // is claimed, so going down never leaves a machine locked.
        if bodies.get(player).is_ok_and(|body| body.0.collapsed) {
            continue;
        }
        let Ok((mut machine, slot, transform)) = machines.get_mut(request.target) else {
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
        match (loading, slot) {
            (Some(container), Some(slot))
                if slotted_container(request.target, &slotted).is_none() =>
            {
                commands
                    .entity(container)
                    .remove::<HeldBy>()
                    .remove::<ChildOf>()
                    .insert(InSlot(request.target))
                    .insert(Transform::from_translation(
                        transform.translation + slot.offset,
                    ));
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
) {
    for request in targets.read() {
        if let Ok(mut thermostat) = thermostats.get_mut(request.machine) {
            thermostat.target = request.target;
        }
    }
    for request in power.read() {
        if let Ok(mut thermostat) = thermostats.get_mut(request.machine) {
            thermostat.powered = request.on;
        }
    }
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
    chambers: Query<(Entity, &Thermostat)>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
) {
    let dt = time.delta_secs();
    for (machine, thermostat) in &chambers {
        if !thermostat.powered {
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

        let (_, report) = container.mutate(&db, |solution| solution.temperature = next);
        if let Some(message) = ReactionsFired::from_report(target, &report) {
            fired.write(message);
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

        let (_, report) = container.mutate(&db, |solution| solution.temperature = next);
        if let Some(message) = ReactionsFired::from_report(entity, &report) {
            fired.write(message);
        }
    }
}

/// Resolves a connection to the chemist it drives.
pub fn chemist_entity(chemists: &Query<(Entity, &Chemist)>, client: ClientId) -> Option<Entity> {
    chemists
        .iter()
        .find(|(_, chemist)| chemist.client == client)
        .map(|(entity, _)| entity)
}

#[allow(clippy::too_many_arguments)]
fn handle_dispense(
    mut commands: Commands,
    db: Res<ChemDb>,
    knowledge: Res<Knowledge>,
    mut requests: MessageReader<FromClient<DispenseRequested>>,
    mut fired: MessageWriter<ReactionsFired>,
    machines: Query<&DispenseAmount>,
    kinds: Query<&Machine>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
) {
    for request in requests.read() {
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
        let (_, report) = container.mutate(&db, |solution| solution.add(request.reagent, amount.0));
        if let Some(message) = ReactionsFired::from_report(target, &report) {
            fired.write(message);
        }

        // Anything drawn from the test bench is practice stock. Marking the
        // container rather than the reagent is what makes it survive being
        // reacted, packaged and carried around.
        if kinds.get(request.machine).map(|m| m.kind) == Ok(MachineKind::TestBench) {
            commands.entity(target).insert(TestBenchStock);
        }
    }
}

fn handle_buffer_transfer(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<BufferTransferRequested>>,
    mut fired: MessageWriter<ReactionsFired>,
    mut buffers: Query<(&mut Buffer, Has<BufferPracticeStock>)>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<(&mut Container, Has<TestBenchStock>)>,
) {
    for request in requests.read() {
        let Ok((mut buffer, buffer_practice)) = buffers.get_mut(request.machine) else {
            continue;
        };
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok((mut container, container_practice)) = containers.get_mut(target) else {
            continue;
        };

        match request.direction {
            BufferDirection::ToBuffer => {
                // Pulling a single named reagent out of a mixture is the whole
                // point of the ChemMaster: it is how a contaminated batch gets
                // cleaned up before it goes in a pill.
                let moved = container.solution.remove(request.reagent, request.amount);
                let overflow = buffer.0.add(request.reagent, moved);
                if overflow.is_positive() {
                    let _ = container.solution.add(request.reagent, overflow);
                }
                // Practice stock taints the buffer it lands in — see
                // `BufferPracticeStock`.
                if container_practice && (moved - overflow).is_positive() {
                    commands.entity(request.machine).insert(BufferPracticeStock);
                }
            }
            BufferDirection::ToContainer => {
                let moved = buffer.0.remove(request.reagent, request.amount);
                let (overflow, report) =
                    container.mutate(&db, |solution| solution.add(request.reagent, moved));
                if overflow.is_positive() {
                    let _ = buffer.0.add(request.reagent, overflow);
                }
                if let Some(message) = ReactionsFired::from_report(target, &report) {
                    fired.write(message);
                }
                // ...and back out again into whatever glassware receives it,
                // which is the half of the round trip that used to launder it.
                if buffer_practice && (moved - overflow).is_positive() {
                    commands.entity(target).insert(TestBenchStock);
                }
            }
        }

        if buffer.0.total_volume().is_zero() {
            commands
                .entity(request.machine)
                .remove::<BufferPracticeStock>();
        }
    }
}

fn handle_package(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<PackageRequested>>,
    mut machines: Query<(&mut Buffer, &Transform, Has<BufferPracticeStock>)>,
) {
    for request in requests.read() {
        let Ok((mut buffer, transform, practice)) = machines.get_mut(request.machine) else {
            continue;
        };
        if !buffer.0.total_volume().is_positive() {
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

        // A pill pressed from practice stock is still practice stock. Without
        // this the packaging step minted brand-new, unmarked glassware and was
        // the shortest route around the test bench's whole restriction.
        if practice {
            commands.entity(package).insert(TestBenchStock);
        }
        if buffer.0.total_volume().is_zero() {
            commands
                .entity(request.machine)
                .remove::<BufferPracticeStock>();
        }

        // The container was only just queued for spawn, so its `Container`
        // component is not readable yet; fill it in on the command queue.
        let contents = portion;
        commands.queue(move |world: &mut World| {
            if let Some(mut container) = world.get_mut::<Container>(package) {
                for (reagent, amount) in contents.iter() {
                    let _ = container.solution.add(reagent, amount);
                }
            }
        });
    }
}

/// Floor level, just clear of a machine's working face.
///
/// Derived from the machine's own box — its transform carries the fit's size as
/// a scale — so it lands in front of whichever machine this is, at whatever
/// height that machine stands, rather than at one offset that happened to suit
/// the hall's north wall.
fn front_of(machine: &Transform, facing: Option<&Facing>, lift: f32) -> Vec3 {
    // `Vec3::Z` only for a machine that has not been dressed yet, which in
    // practice means a test that spawned one by hand.
    let facing = facing.map_or(Vec3::Z, |facing| facing.0);
    let depth = machine.scale.dot(facing.abs());
    let floor = machine.translation.y - machine.scale.y * 0.5;
    let spot = machine.translation + facing * (depth * 0.5 + 0.35);
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
    hands_full: bool,
    machine: &Transform,
    facing: Option<&Facing>,
    lift: f32,
) {
    let mut item = commands.entity(item);
    if hands_full {
        item.insert(Transform::from_translation(front_of(machine, facing, lift)));
    } else {
        item.insert(HeldBy(player));
    }
}

/// Ejecting hands the container back rather than guessing a spot for it.
///
/// It used to place it a metre up and half a metre along world `+Z`, which
/// points into the room for the dispenser and the ChemMaster and nowhere useful
/// for anything else. The grinder faces `-Z`, so its beaker was ejected
/// *through* the storeroom's south wall and 1.85 m up: gone, silently, with a
/// full batch inside it. Straight into the chemist's hand has no direction to
/// get wrong, and is what they were going to do next anyway.
fn handle_eject(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<EjectRequested>>,
    machines: Query<(&Machine, &Transform, Option<&Facing>)>,
    chemists: Query<(Entity, &Chemist)>,
    slotted: Query<(Entity, &InSlot)>,
    containers: Query<&Container>,
    held: Query<&HeldBy>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Some(container) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok((machine, transform, facing)) = machines.get(request.machine) else {
            continue;
        };
        // Only the chemist working the machine. This never mattered while the
        // beaker was placed on the floor — anyone could have walked over and
        // picked it up — but it does now that ejecting puts it in a *hand*,
        // and the hand is the asking client's.
        if !machine.available_to(player) {
            continue;
        }

        commands.entity(container).remove::<InSlot>();
        give_back(
            &mut commands,
            container,
            player,
            held.iter().any(|holder| holder.0 == player),
            transform,
            facing,
            set_down_lift(containers.get(container).ok()),
        );
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
    machines: Query<(&Machine, &Transform, Option<&Facing>)>,
    chemists: Query<(Entity, &Chemist)>,
    stored: Query<&Stored>,
    containers: Query<&Container>,
    held: Query<&HeldBy>,
    bodies: Query<&crate::body::Body>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        if bodies.get(player).is_ok_and(|body| body.0.collapsed) {
            continue;
        }
        // The item has to be in the locker the message names. Checked rather
        // than trusted: the pair comes off the wire, and without this a client
        // could name any entity in the world and have it handed over.
        let Ok(shut_in) = stored.get(request.item) else {
            continue;
        };
        if shut_in.0 != request.machine {
            continue;
        }
        let Ok((machine, transform, facing)) = machines.get(request.machine) else {
            continue;
        };
        // Whoever has the locker open. Same reasoning as ejecting: this hands
        // an item to the asking client, so it must be the client the server
        // granted the locker to.
        if !machine.available_to(player) {
            continue;
        }

        commands.entity(request.item).remove::<Stored>();
        give_back(
            &mut commands,
            request.item,
            player,
            held.iter().any(|holder| holder.0 == player),
            transform,
            facing,
            set_down_lift(containers.get(request.item).ok()),
        );
    }
}

fn handle_empty(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<EmptyRequested>>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
) {
    for request in requests.read() {
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        if let Ok(mut container) = containers.get_mut(target) {
            container.solution.clear();
            // Rinsing it out clears the practice-stock mark too, so a beaker
            // is not contaminated by association for the rest of the shift.
            commands.entity(target).remove::<TestBenchStock>();
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
    mut fired: MessageWriter<ReactionsFired>,
    slotted: Query<(Entity, &InSlot)>,
    containers: Query<&Container>,
) {
    for request in requests.read() {
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
    mut fired: MessageWriter<ReactionsFired>,
    mut hoppers: Query<&mut Hopper>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
) {
    let Some(catalog) = catalog else {
        return;
    };

    for request in requests.read() {
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
            let (_, report) = container.mutate(&db, |solution| {
                for (reagent, amount) in &kind.yields {
                    let _ = solution.add(*reagent, *amount);
                }
            });
            reactions.extend(report.fired_reactions());
            effects.extend(report.effects);
            distinct_reagents = distinct_reagents.max(report.distinct_reagents);
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

        // These tests exercise machine behaviour, not the dispenser tier
        // economy — start with the dispenser fully upgraded so a locked
        // reagent is never the reason a dispense silently does nothing.
        let mut knowledge = Knowledge::new(&data);
        knowledge.award_research(1000);
        while knowledge.upgrade_dispenser(&data) {}

        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .insert_resource(knowledge)
            .insert_resource(catalog)
            .add_message::<FromClient<DispenseRequested>>()
            .add_message::<ReactionsFired>()
            .add_message::<FromClient<BufferTransferRequested>>()
            .add_message::<FromClient<InteractRequested>>()
            .add_message::<FromClient<LeaveMachineRequested>>()
            .add_message::<FromClient<GrindRequested>>()
            .add_message::<FromClient<SetTargetTemperature>>()
            .add_message::<FromClient<SetHeaterPower>>()
            .add_message::<FromClient<PackageRequested>>()
            .add_message::<FromClient<EjectRequested>>()
            .add_message::<FromClient<TakeRequested>>()
            // What the server tells a client when it grants them a machine.
            // Headless there is nobody to tell, but the writer still has to
            // exist for the system to run.
            .add_message::<ToClients<MachineOpened>>()
            .init_resource::<Time>()
            .add_systems(
                Update,
                (
                    handle_machine_interact,
                    handle_leave_machine,
                    handle_dispense,
                    handle_buffer_transfer,
                    handle_package,
                    handle_grind,
                    handle_eject,
                    handle_take,
                    handle_thermostat_controls,
                    apply_thermostats,
                    cool_to_ambient,
                )
                    .chain(),
            );
        app
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
    fn dispensing_a_locked_reagent_is_refused() {
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

        let hydrogen = reagent(&app, "hydrogen"); // locked by default
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
            "a locked reagent must never be dispensed, even by a direct request"
        );
    }

    #[test]
    fn causing_a_reaction_reports_which_recipe_fired() {
        // Discovery is built on this: the resolver already names what fired,
        // so learning is a matter of noticing rather than re-deriving.
        let mut app = test_app();
        let dispenser = app.world_mut().spawn(DispenseAmount(Units::whole(15))).id();
        app.world_mut().spawn((
            Container::new(ContainerKind::LargeBeaker),
            InSlot(dispenser),
        ));

        // Oxygen, sugar, then a double helping of carbon: inaprovaline forms
        // first and the leftover carbon carries it on to bicaridine.
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

        let fired = app.world().resource::<Messages<ReactionsFired>>();
        let mut cursor = fired.get_cursor();
        let reported: Vec<chem_sim::ReactionId> = cursor
            .read(fired)
            .flat_map(|event| event.reactions.clone())
            .collect();

        let db = app.world().resource::<ChemDb>();
        let names: Vec<&str> = reported
            .iter()
            .map(|id| db.reactions.get(*id).key.as_str())
            .collect();
        assert!(
            names.contains(&"inaprovaline") && names.contains(&"bicaridine"),
            "both steps of the chain should be reported, got {names:?}"
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

        // What the ChemMaster does: pull the one reagent out into clean glass.
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
    fn grinding_into_a_loaded_beaker_can_set_off_a_reaction() {
        // Grinding is extraction, not chemistry — but what it extracts lands in
        // a beaker that may already hold something. Dylovene meeting radium is
        // hyronalin, and that has to count as a discovery like any other.
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

        let fired = app.world().resource::<Messages<ReactionsFired>>();
        let mut cursor = fired.get_cursor();
        let db = app.world().resource::<ChemDb>();
        let names: Vec<&str> = cursor
            .read(fired)
            .flat_map(|event| event.reactions.iter())
            .map(|id| db.reactions.get(*id).key.as_str())
            .collect();
        assert!(
            names.contains(&"hyronalin"),
            "the grind must go through Container::mutate so reactions resolve, got {names:?}"
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
                Machine::new(MachineKind::Dispenser),
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

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EjectRequested { machine },
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
    fn ejecting_with_full_hands_sets_the_beaker_down_in_front_of_the_machine() {
        // The fallback, and the one that has to respect which way the machine
        // points. A grinder standing against the storeroom's south wall faces
        // `-Z`, so the beaker belongs at a *smaller* z than the casing — the
        // direction the old constant got backwards.
        let mut app = test_app();
        let (client, chemist) = chemist(&mut app);
        let machine = grinder(&mut app, &[], Some(ContainerKind::Beaker));
        app.world_mut().entity_mut(machine).insert((
            Facing(Vec3::NEG_Z),
            Transform::from_translation(Vec3::new(-4.0, 0.85, 5.55))
                .with_scale(Vec3::new(1.5, 1.7, 0.8)),
        ));
        // Already carrying something else, so there is nowhere to hand it.
        app.world_mut()
            .spawn((Container::new(ContainerKind::Bottle), HeldBy(chemist)));

        let beaker = {
            let mut query = app.world_mut().query_filtered::<Entity, With<InSlot>>();
            query.single(app.world()).expect("a beaker is loaded")
        };

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EjectRequested { machine },
        });
        app.update();

        assert!(app.world().get::<HeldBy>(beaker).is_none());
        let resting = app.world().get::<Transform>(beaker).unwrap().translation;
        assert!(
            resting.z < 5.55 - 0.4,
            "it must land clear of the face the chemist stands at, got {resting:?}"
        );
        let (_, height) = ContainerKind::Beaker.dimensions();
        assert!(
            (resting.y - height * 0.5).abs() < 0.001,
            "and standing on the floor rather than sunk into it or floating \
             above it, got {resting:?}"
        );
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
        app.world_mut().get_mut::<Machine>(machine).unwrap().in_use_by = Some(other);

        let beaker = {
            let mut query = app.world_mut().query_filtered::<Entity, With<InSlot>>();
            query.single(app.world()).expect("a beaker is loaded")
        };

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EjectRequested { machine },
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
            .spawn((Machine::new(MachineKind::Locker), Transform::default()))
            .id()
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
            .spawn((Machine::new(MachineKind::Dispenser), Transform::default()))
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
            .spawn((Machine::new(MachineKind::Dispenser), Transform::default()))
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

    // -----------------------------------------------------------------------
    // Test-bench stock cannot be laundered through the buffer
    // -----------------------------------------------------------------------

    /// A ChemMaster with a beaker of practice stock already loaded, and the
    /// reagent that beaker holds.
    fn chemmaster_with_practice_stock(app: &mut App) -> (Entity, Entity, ReagentId) {
        let machine = app
            .world_mut()
            .spawn((
                Machine::new(MachineKind::ChemMaster),
                Buffer(Solution::unbounded()),
                Transform::default(),
            ))
            .id();
        let sugar = reagent(app, "sugar");
        let mut container = Container::new(ContainerKind::LargeBeaker);
        let _ = container.solution.add(sugar, Units::whole(60));
        let beaker = app
            .world_mut()
            .spawn((container, InSlot(machine), TestBenchStock))
            .id();
        (machine, beaker, sugar)
    }

    fn transfer(app: &mut App, machine: Entity, reagent: ReagentId, direction: BufferDirection) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: BufferTransferRequested {
                machine,
                reagent,
                amount: Units::whole(60),
                direction,
            },
        });
        app.update();
    }

    #[test]
    fn practice_stock_taints_the_buffer_it_is_pushed_into() {
        let mut app = test_app();
        let (machine, _beaker, sugar) = chemmaster_with_practice_stock(&mut app);

        transfer(&mut app, machine, sugar, BufferDirection::ToBuffer);

        assert!(
            app.world().get::<BufferPracticeStock>(machine).is_some(),
            "the buffer has to carry the mark, or the container is the only \
             thing that does and packaging mints a clean one"
        );
    }

    #[test]
    fn practice_stock_survives_a_round_trip_through_the_buffer() {
        // The laundering route that used to work: push practice stock into the
        // buffer, then draw it back out into a *different*, unmarked beaker.
        let mut app = test_app();
        let (machine, beaker, sugar) = chemmaster_with_practice_stock(&mut app);
        transfer(&mut app, machine, sugar, BufferDirection::ToBuffer);

        // Swap the marked beaker out for a clean one, exactly as a player
        // would by ejecting and loading another.
        app.world_mut().entity_mut(beaker).despawn();
        let clean = app
            .world_mut()
            .spawn((Container::new(ContainerKind::LargeBeaker), InSlot(machine)))
            .id();

        transfer(&mut app, machine, sugar, BufferDirection::ToContainer);

        assert!(
            app.world().get::<TestBenchStock>(clean).is_some(),
            "a clean beaker filled from a tainted buffer is still practice stock"
        );
    }

    #[test]
    fn a_pill_pressed_from_practice_stock_is_still_practice_stock() {
        // The shorter laundering route: practice stock into the buffer, then
        // straight into a pill. `handle_package` spawns brand-new glassware
        // that never touched the test bench, so it carried no mark at all.
        let mut app = test_app();
        let (machine, _beaker, sugar) = chemmaster_with_practice_stock(&mut app);
        transfer(&mut app, machine, sugar, BufferDirection::ToBuffer);

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: PackageRequested {
                machine,
                kind: ContainerKind::Pill,
            },
        });
        app.update();
        // The container is filled on the command queue, one frame behind.
        app.update();

        let pill = app
            .world_mut()
            .query_filtered::<Entity, (With<Container>, Without<InSlot>)>()
            .iter(app.world())
            .next()
            .expect("packaging should have spawned a pill");
        assert!(
            app.world().get::<TestBenchStock>(pill).is_some(),
            "packaging must not launder practice stock into deliverable product"
        );
    }

    #[test]
    fn draining_the_buffer_clears_the_practice_mark() {
        // Same rule rinsing a beaker already follows: nothing stays
        // contaminated by association once it is empty.
        let mut app = test_app();
        let (machine, beaker, sugar) = chemmaster_with_practice_stock(&mut app);
        transfer(&mut app, machine, sugar, BufferDirection::ToBuffer);
        assert!(app.world().get::<BufferPracticeStock>(machine).is_some());

        // Everything back out again leaves the buffer dry.
        let _ = beaker;
        transfer(&mut app, machine, sugar, BufferDirection::ToContainer);

        assert!(
            app.world().get::<BufferPracticeStock>(machine).is_none(),
            "an empty buffer is clean again"
        );
    }
}
