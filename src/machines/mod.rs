//! Lab machines and the actions they perform.
//!
//! Panels never touch state directly. They emit the messages below and the
//! systems here apply them, so co-op can replicate actions later without
//! rewriting any UI.

use bevy::prelude::*;
use chem_sim::{ReagentId, Solution, Units};

use crate::chem_data::ChemDb;
use crate::containers::{
    spawn_container, Container, ContainerAssets, ContainerKind, HeldBy, InSlot,
};
use crate::interaction::{InteractRequested, InteractionMode};
use crate::AppState;

pub struct MachinePlugin;

impl Plugin for MachinePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<DispenseRequested>()
            .add_message::<EjectRequested>()
            .add_message::<EmptyRequested>()
            .add_message::<BufferTransferRequested>()
            .add_message::<PackageRequested>()
            .add_systems(
                Update,
                (
                    handle_machine_interact,
                    handle_dispense,
                    handle_buffer_transfer,
                    handle_package,
                    handle_eject,
                    handle_empty,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Which piece of equipment this is.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
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
#[derive(Component, Debug)]
pub struct Machine {
    pub kind: MachineKind,
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
#[derive(Component)]
pub struct Buffer(pub Solution);

/// How much a dispenser gives per press. Persists between visits.
#[derive(Component)]
pub struct DispenseAmount(pub Units);

impl Default for DispenseAmount {
    fn default() -> Self {
        DispenseAmount(Units::whole(10))
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Message)]
pub struct DispenseRequested {
    pub machine: Entity,
    pub reagent: ReagentId,
}

#[derive(Message)]
pub struct EjectRequested {
    pub machine: Entity,
}

#[derive(Message)]
pub struct EmptyRequested {
    pub machine: Entity,
}

/// Which way reagent moves between the loaded container and the buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BufferDirection {
    ToBuffer,
    ToContainer,
}

#[derive(Message)]
pub struct BufferTransferRequested {
    pub machine: Entity,
    pub reagent: ReagentId,
    pub amount: Units,
    pub direction: BufferDirection,
}

#[derive(Message)]
pub struct PackageRequested {
    pub machine: Entity,
    pub kind: ContainerKind,
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
fn handle_machine_interact(
    mut commands: Commands,
    mut requests: MessageReader<InteractRequested>,
    mut machines: Query<(&mut Machine, Option<&ContainerSlot>, &Transform)>,
    mut modes: Query<&mut InteractionMode>,
    held: Query<(Entity, &HeldBy)>,
    slotted: Query<(Entity, &InSlot)>,
) {
    for request in requests.read() {
        let Ok((mut machine, slot, transform)) = machines.get_mut(request.target) else {
            continue;
        };

        let carrying = held
            .iter()
            .find(|(_, holder)| holder.0 == request.player)
            .map(|(entity, _)| entity);

        match (carrying, slot) {
            (Some(container), Some(slot)) if slotted_container(request.target, &slotted).is_none() => {
                commands
                    .entity(container)
                    .remove::<HeldBy>()
                    .remove::<ChildOf>()
                    .insert(InSlot(request.target))
                    .insert(Transform::from_translation(transform.translation + slot.offset));
            }
            _ => {
                if !machine.available_to(request.player) {
                    continue;
                }
                machine.in_use_by = Some(request.player);
                if let Ok(mut mode) = modes.get_mut(request.player) {
                    *mode = InteractionMode::UsingMachine(request.target);
                }
            }
        }
    }
}

fn handle_dispense(
    db: Res<ChemDb>,
    mut requests: MessageReader<DispenseRequested>,
    machines: Query<&DispenseAmount>,
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
        container.mutate(&db, |solution| solution.add(request.reagent, amount.0));
    }
}

fn handle_buffer_transfer(
    db: Res<ChemDb>,
    mut requests: MessageReader<BufferTransferRequested>,
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
                let (overflow, _) = container.mutate(&db, |solution| {
                    solution.add(request.reagent, moved)
                });
                if overflow.is_positive() {
                    let _ = buffer.0.add(request.reagent, overflow);
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
    mut requests: MessageReader<PackageRequested>,
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
    mut requests: MessageReader<EjectRequested>,
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
    mut requests: MessageReader<EmptyRequested>,
    slotted: Query<(Entity, &InSlot)>,
    mut containers: Query<&mut Container>,
) {
    for request in requests.read() {
        let Some(target) = slotted_container(request.machine, &slotted) else {
            continue;
        };
        if let Ok(mut container) = containers.get_mut(target) {
            container.solution.clear();
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

        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .add_message::<DispenseRequested>()
            .add_message::<BufferTransferRequested>()
            .add_message::<InteractRequested>()
            .add_systems(
                Update,
                (
                    handle_machine_interact,
                    handle_dispense,
                    handle_buffer_transfer,
                ),
            );
        app
    }

    fn reagent(app: &App, key: &str) -> ReagentId {
        app.world().resource::<ChemDb>().reagent(key)
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
            app.world_mut().write_message(DispenseRequested {
                machine: dispenser,
                reagent,
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
            app.world_mut().write_message(DispenseRequested {
                machine: chemmaster,
                reagent,
            });
            app.update();
        }

        let oxygen = reagent(&app, "oxygen");
        let sugar = reagent(&app, "sugar");
        app.world_mut().write_message(BufferTransferRequested {
            machine: chemmaster,
            reagent: oxygen,
            amount: Units::whole(20),
            direction: BufferDirection::ToBuffer,
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
    fn a_machine_can_only_be_claimed_by_one_chemist() {
        // The co-op invariant, checkable long before co-op exists.
        let mut app = test_app();
        let machine = app
            .world_mut()
            .spawn((Machine::new(MachineKind::Dispenser), Transform::default()))
            .id();
        let first = app.world_mut().spawn(InteractionMode::default()).id();
        let second = app.world_mut().spawn(InteractionMode::default()).id();

        app.world_mut().write_message(InteractRequested {
            player: first,
            target: machine,
        });
        app.update();

        app.world_mut().write_message(InteractRequested {
            player: second,
            target: machine,
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
