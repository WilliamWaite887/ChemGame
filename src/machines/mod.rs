//! Lab machines and the actions they perform.
//!
//! Panels never touch state directly. They emit the messages below and the
//! systems here apply them, so co-op can replicate actions later without
//! rewriting any UI.

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{ReagentId, Solution, Units};
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
        }
    }

}

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
#[derive(Message)]
pub struct ReactionsFired {
    pub reactions: Vec<chem_sim::ReactionId>,
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
) {
    for request in requests.read() {
        // The sender's identity comes from the connection, never from the
        // message. A client that could name its own player entity could act as
        // the other chemist.
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
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
        if report.reacted() {
            fired.write(ReactionsFired {
                reactions: report.fired_reactions(),
            });
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
                if report.reacted() {
                    fired.write(ReactionsFired {
                        reactions: report.fired_reactions(),
                    });
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
            fired.write(ReactionsFired {
                reactions: identified,
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
        }

        if !reactions.is_empty() {
            fired.write(ReactionsFired { reactions });
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
            .add_systems(
                Update,
                (
                    handle_machine_interact,
                    handle_dispense,
                    handle_buffer_transfer,
                    handle_grind,
                ),
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
}
