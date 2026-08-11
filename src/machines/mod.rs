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
    spawn_container, Container, ContainerAssets, ContainerKind, HeldBy, InSlot,
};
use crate::interaction::{InteractRequested, InteractionMode};
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
                    handle_dispense,
                    handle_buffer_transfer,
                    handle_package,
                    handle_analyze,
                    handle_grind,
                    handle_eject,
                    handle_empty,
                    handle_thermostat_controls,
                    apply_thermostats,
                    cool_to_ambient,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing))
                    // Authority: server, listen server, or singleplayer.
                    .run_if(in_state(ClientState::Disconnected)),
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
    /// Where the shift is started, signed off, and requisitioned against.
    ShiftBoard,
    /// Heats and cools a loaded container toward a dialled-in temperature.
    ///
    /// The only machine that does nothing on its own: it changes the conditions
    /// and lets the chemistry decide what that means.
    ReactionChamber,
}

impl MachineKind {
    pub fn label(self) -> &'static str {
        match self {
            MachineKind::Dispenser => "Chemical Dispenser",
            MachineKind::ChemMaster => "ChemMaster 4000",
            MachineKind::Grinder => "Reagent Grinder",
            MachineKind::Analyzer => "Sample Analyzer",
            MachineKind::TestBench => "Test Bench",
            MachineKind::DeliveryWindow => "Delivery Window",
            MachineKind::ShiftBoard => "Shift Board",
            MachineKind::ReactionChamber => "Reaction Chamber",
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
            MachineKind::ShiftBoard => Color::srgb(0.95, 0.88, 0.45),
            MachineKind::ReactionChamber => Color::srgb(0.98, 0.45, 0.18),
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

/// The ChemMaster's internal buffer.
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
pub fn slotted_container(
    machine: Entity,
    slotted: &Query<(Entity, &InSlot)>,
) -> Option<Entity> {
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
    bodies: Query<&crate::body::Body>,
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
            (Some(container), Some(slot)) if slotted_container(request.target, &slotted).is_none() => {
                commands
                    .entity(container)
                    .remove::<HeldBy>()
                    .remove::<ChildOf>()
                    .insert(InSlot(request.target))
                    .insert(Transform::from_translation(transform.translation + slot.offset));
            }
            _ => {
                if !machine.available_to(player) {
                    continue;
                }
                machine.in_use_by = Some(player);
                if let Ok(mut mode) = modes.get_mut(player) {
                    *mode = InteractionMode::UsingMachine(request.target);
                }
            }
        }
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
    mut requests: MessageReader<FromClient<DispenseRequested>>,
    mut fired: MessageWriter<ReactionsFired>,
    machines: Query<&DispenseAmount>,
    kinds: Query<&Machine>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
) {
    for request in requests.read() {
        let Ok(amount) = machines.get(request.machine) else {
            continue;
        };
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok(mut container) = containers.get_mut(target) else {
            continue;
        };
        let (_, report) =
            container.mutate(&db, |solution| solution.add(request.reagent, amount.0));
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
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<BufferTransferRequested>>,
    mut fired: MessageWriter<ReactionsFired>,
    mut buffers: Query<&mut Buffer>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
) {
    for request in requests.read() {
        let Ok(mut buffer) = buffers.get_mut(request.machine) else {
            continue;
        };
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok(mut container) = containers.get_mut(target) else {
            continue;
        };

        match request.direction {
            BufferDirection::ToBuffer => {
                // Pulling a single named reagent out of a mixture is the whole
                // point of the ChemMaster: it is how a contaminated batch gets
                // cleaned up before it goes in a pill.
                let moved = container
                    .solution
                    .remove(request.reagent, request.amount);
                let overflow = buffer.0.add(request.reagent, moved);
                if overflow.is_positive() {
                    let _ = container.solution.add(request.reagent, overflow);
                }
            }
            BufferDirection::ToContainer => {
                let moved = buffer.0.remove(request.reagent, request.amount);
                let (overflow, report) = container.mutate(&db, |solution| {
                    solution.add(request.reagent, moved)
                });
                if overflow.is_positive() {
                    let _ = buffer.0.add(request.reagent, overflow);
                }
                if let Some(message) = ReactionsFired::from_report(target, &report) {
                        fired.write(message);
        }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_package(
    mut commands: Commands,
    assets: Option<Res<ContainerAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut requests: MessageReader<FromClient<PackageRequested>>,
    mut machines: Query<(&mut Buffer, &Transform)>,
) {
    let Some(assets) = assets else {
        return;
    };

    for request in requests.read() {
        let Ok((mut buffer, transform)) = machines.get_mut(request.machine) else {
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
        let package = spawn_container(
            &mut commands,
            &mut meshes,
            &mut materials,
            &assets,
            request.kind,
            drop_at,
        );

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

fn handle_eject(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<EjectRequested>>,
    machines: Query<&Transform, With<Machine>>,
    slotted: Query<(Entity, &InSlot)>,
) {
    for request in requests.read() {
        let Some(container) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        let Ok(transform) = machines.get(request.machine) else {
            continue;
        };
        commands
            .entity(container)
            .remove::<InSlot>()
            .insert(Transform::from_translation(
                transform.translation + Vec3::new(0.0, 1.0, 0.55),
            ));
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
            // one. Nothing here can smoke or detonate.
            fired.write(ReactionsFired {
                reactions: identified,
                container: target,
                effects: Vec::new(),
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
        }

        if !reactions.is_empty() || !effects.is_empty() {
            fired.write(ReactionsFired {
                reactions,
                container: target,
                effects,
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

        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .insert_resource(catalog)
            .add_message::<FromClient<DispenseRequested>>()
            .add_message::<ReactionsFired>()
            .add_message::<FromClient<BufferTransferRequested>>()
            .add_message::<FromClient<InteractRequested>>()
            .add_message::<FromClient<GrindRequested>>()
            .add_message::<FromClient<SetTargetTemperature>>()
            .add_message::<FromClient<SetHeaterPower>>()
            .init_resource::<Time>()
            .add_systems(
                Update,
                (
                    handle_machine_interact,
                    handle_dispense,
                    handle_buffer_transfer,
                    handle_grind,
                    handle_thermostat_controls,
                    apply_thermostats,
                    cool_to_ambient,
                )
                    .chain(),
            );
        app
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
        let dispenser = app
            .world_mut()
            .spawn(DispenseAmount(Units::whole(15)))
            .id();
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
            app.world_mut().write_message(FromClient { client_id: ClientId::Server, message: DispenseRequested {
                machine: dispenser,
                reagent,
            }});
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
    fn causing_a_reaction_reports_which_recipe_fired() {
        // Discovery is built on this: the resolver already names what fired,
        // so learning is a matter of noticing rather than re-deriving.
        let mut app = test_app();
        let dispenser = app
            .world_mut()
            .spawn(DispenseAmount(Units::whole(15)))
            .id();
        app.world_mut().spawn((
            Container::new(ContainerKind::LargeBeaker),
            InSlot(dispenser),
        ));

        // Oxygen, sugar, then a double helping of carbon: inaprovaline forms
        // first and the leftover carbon carries it on to bicaridine.
        for key in ["oxygen", "sugar", "carbon", "carbon"] {
            let reagent = reagent(&app, key);
            app.world_mut().write_message(FromClient { client_id: ClientId::Server, message: DispenseRequested {
                machine: dispenser,
                reagent,
            }});
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
        use crate::orders::{grade, Outcome};

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
        assert_eq!(
            grade(
                dylovene,
                Units::whole(24),
                &dirty,
                ContainerKind::LargeBeaker,
                None
            ),
            Outcome::Impure,
            "plant fibre rides along, so a straight grind cannot be handed over"
        );

        // What the ChemMaster does: pull the one reagent out into clean glass.
        let mut clean = Solution::new(ContainerKind::Beaker.capacity());
        let _ = clean.add(dylovene, dirty.volume_of(dylovene));
        assert_eq!(
            grade(
                dylovene,
                Units::whole(24),
                &clean,
                ContainerKind::Beaker,
                None
            ),
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
        let item = app.world_mut().spawn((Produce(poppy), HeldBy(chemist))).id();

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
}
