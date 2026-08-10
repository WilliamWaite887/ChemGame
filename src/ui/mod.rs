//! Machine control panels.
//!
//! Panels are pure presentation. Every button carries a [`PanelAction`] which
//! one system turns into a message; nothing here mutates a solution.
//!
//! The whole panel is rebuilt whenever the state it shows changes, rather than
//! patching individual nodes. At this size that is far simpler to keep correct,
//! and it only happens on user action.

use bevy::prelude::*;
use chem_sim::{ReagentId, Units};

use crate::chem_data::ChemDb;
use crate::containers::{Container, ContainerKind, InSlot};
use crate::crew::{CrewMember, CrewPhase, CrewRoute};
use crate::interaction::{leave_machine, InteractionMode};
use crate::knowledge::{Knowledge, RecipeDiscovered, HINT_COST};
use crate::machines::{
    slotted_container, AnalyzeRequested, Buffer, BufferDirection, BufferTransferRequested,
    DispenseAmount, DispenseRequested, EjectRequested, EmptyRequested, GrindRequested, Hopper,
    Machine, MachineKind, PackageRequested, TestBenchStock,
};
use crate::orders::{Order, Shift};
use crate::player::LocalPlayer;
use crate::produce::{ProduceCatalog, ProduceId};
use crate::radio::RadioLog;
use crate::shift::{
    can_afford, BeginShiftRequested, RequisitionKind, RequisitionRequested, ShiftClock, ShiftPhase,
    ShiftReport, SignOffRequested,
};
use crate::AppState;

/// How many orders the queue can show at once.
///
/// Must be at least the highest `max_active` the difficulty ramp can reach, not
/// the base value in `station.orders.ron` — a queue shorter than the ramp hides
/// the order that is about to expire, which is the one the player most needs.
/// `the_order_queue_has_a_slot_for_every_concurrent_order` holds the two together.
pub(crate) const ORDER_SLOTS: usize = 5;
/// Radio lines on screen. Matches the log's own capacity.
const RADIO_SLOTS: usize = 6;

const PANEL_BG: Color = Color::srgba(0.07, 0.08, 0.10, 0.97);
const SECTION_BG: Color = Color::srgba(0.12, 0.13, 0.16, 0.9);
const TEXT: Color = Color::srgb(0.88, 0.90, 0.94);
const TEXT_DIM: Color = Color::srgb(0.55, 0.59, 0.66);
const BUTTON_IDLE: Color = Color::srgb(0.17, 0.19, 0.23);
const BUTTON_HOVER: Color = Color::srgb(0.25, 0.29, 0.35);
const BUTTON_ACTIVE: Color = Color::srgb(0.20, 0.45, 0.62);

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(AppState::Playing),
            (spawn_order_queue, spawn_radio_feed),
        )
        .add_systems(
            Update,
            (
                handle_panel_clicks,
                button_feedback,
                sync_panel,
                update_phase_banner,
                update_order_queue,
                update_radio_feed,
                announce_discoveries,
                // Presentation only, so unlike every other phase system this
                // runs on both ends: a joined chemist needs to know the shift
                // just started every bit as much as the host does.
                announce_phase,
                show_toasts,
                expire_toasts,
            )
                .chain()
                .run_if(in_state(AppState::Playing)),
        )
        .add_message::<ShowToast>();
    }
}

/// Root of the currently open panel.
#[derive(Component)]
struct PanelRoot;

/// Marks the currently chosen option in a group of buttons.
#[derive(Component)]
struct Selected;

/// What a button does when clicked.
#[derive(Component, Clone)]
enum PanelAction {
    SetAmount(Units),
    Dispense(ReagentId),
    Eject,
    Empty,
    ToBuffer(ReagentId, Units),
    ToContainer(ReagentId, Units),
    Package(ContainerKind),
    Analyze,
    Grind { all: bool },
    BuyHint(chem_sim::ReactionId),
    BeginShift,
    SignOff,
    Requisition(RequisitionKind),
    Close,
}

/// Everything the open panel displays, flattened for comparison.
///
/// Rebuilding is driven by comparing this against last frame's rather than by
/// change-detection filters. Change detection here has to span the mode, the
/// machine, a container that can be swapped out from under the panel, and the
/// buffer — and a missed signal shows the player stale contents, which in a
/// chemistry game means dosing off numbers that are no longer true. The
/// comparison is a few dozen integers; correctness is worth far more.
#[derive(PartialEq)]
struct PanelSignature {
    mode: InteractionMode,
    container: Option<Entity>,
    contents: Vec<(ReagentId, Units)>,
    buffer: Vec<(ReagentId, Units)>,
    hopper: Vec<ProduceId>,
    /// The loaded container is practice stock. Tracked separately because the
    /// delivery window has to explain why it will not take it.
    test_stock: bool,
    amount: Option<Units>,
    known_recipes: usize,
    /// The shift board draws entirely from these, so without them its panel
    /// would freeze on whatever it happened to show first.
    ///
    /// The countdown is deliberately absent: it moves every frame, and this
    /// comparison decides whether to despawn and rebuild the whole panel. The
    /// clock belongs on the HUD banner, which writes into fixed slots.
    phase: Option<ShiftPhase>,
    resolved: u32,
    reputation: i32,
    research_points: u32,
}

impl Default for PanelSignature {
    fn default() -> Self {
        PanelSignature {
            // Deliberately not `Roaming`: the default must differ from any
            // real state, or the first frame with no panel open would compare
            // equal and skip the despawn of a panel left over from last frame.
            mode: InteractionMode::ReadingBook,
            container: None,
            contents: Vec::new(),
            buffer: Vec::new(),
            hopper: Vec::new(),
            test_stock: false,
            amount: None,
            known_recipes: usize::MAX,
            phase: None,
            resolved: u32::MAX,
            reputation: i32::MAX,
            research_points: u32::MAX,
        }
    }
}

/// The optional fittings a panel draws from: not every machine has an amount
/// dial, a buffer or a hopper, and the panel body decides what to do with the
/// ones its machine happens to carry.
type MachineParts<'w, 's> = Query<
    'w,
    's,
    (
        &'static Machine,
        Option<&'static DispenseAmount>,
        Option<&'static Buffer>,
        Option<&'static Hopper>,
    ),
>;

#[allow(clippy::too_many_arguments)]
fn sync_panel(
    mut commands: Commands,
    db: Res<ChemDb>,
    existing: Query<Entity, With<PanelRoot>>,
    modes: Query<&InteractionMode, With<LocalPlayer>>,
    machines: MachineParts,
    slotted: Query<(Entity, &InSlot)>,
    containers: Query<(&Container, Has<TestBenchStock>)>,
    knowledge: Res<Knowledge>,
    catalog: Option<Res<ProduceCatalog>>,
    clock: Res<ShiftClock>,
    shift: Res<Shift>,
    mut previous: Local<PanelSignature>,
) {
    let mode = modes.iter().next().copied().unwrap_or_default();
    let open_machine = match mode {
        InteractionMode::UsingMachine(machine) => Some(machine),
        _ => None,
    };
    let loaded_entity = open_machine.and_then(|machine| slotted_container(machine, &slotted));
    let slot_contents = loaded_entity.and_then(|entity| containers.get(entity).ok());
    let loaded = slot_contents.map(|(container, _)| container);
    let test_stock = slot_contents.is_some_and(|(_, practice)| practice);
    let machine_parts = open_machine.and_then(|machine| machines.get(machine).ok());

    let signature = PanelSignature {
        mode,
        container: loaded_entity,
        contents: loaded
            .map(|container| container.solution.iter().collect())
            .unwrap_or_default(),
        buffer: machine_parts
            .and_then(|(_, _, buffer, _)| buffer)
            .map(|buffer| buffer.0.iter().collect())
            .unwrap_or_default(),
        hopper: machine_parts
            .and_then(|(_, _, _, hopper)| hopper)
            .map(|hopper| hopper.0.clone())
            .unwrap_or_default(),
        test_stock,
        amount: machine_parts
            .and_then(|(_, amount, _, _)| amount)
            .map(|a| a.0),
        known_recipes: knowledge.known_count(),
        phase: Some(clock.phase),
        resolved: clock.resolved,
        reputation: shift.reputation,
        research_points: knowledge.research_points,
    };

    if signature == *previous {
        return;
    }
    *previous = signature;

    for panel in &existing {
        commands.entity(panel).despawn();
    }

    if mode == InteractionMode::ReadingBook {
        spawn_reference_book(&mut commands, &db, &knowledge);
        return;
    }
    if open_machine.is_none() {
        return;
    }
    let Some((machine, amount, buffer, hopper)) = machine_parts else {
        return;
    };

    commands
        .spawn((
            // Full-screen flex wrapper so the panel stays centred at any
            // resolution without hardcoded offsets.
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            PanelRoot,
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: px(760),
                        max_height: percent(86),
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(18)),
                        row_gap: px(10),
                        border_radius: BorderRadius::all(px(8)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                ))
                .with_children(|panel| {
                    panel.spawn(heading(machine.kind.label()));

                    match machine.kind {
                        MachineKind::Dispenser | MachineKind::TestBench => {
                            dispenser_body(panel, &db, machine.kind, amount, loaded);
                        }
                        MachineKind::ChemMaster => {
                            chemmaster_body(panel, &db, buffer, loaded);
                        }
                        MachineKind::Analyzer => {
                            analyzer_body(panel, &db, &knowledge, loaded);
                        }
                        MachineKind::Grinder => {
                            grinder_body(panel, &db, catalog.as_deref(), hopper, loaded);
                        }
                        MachineKind::DeliveryWindow => {
                            delivery_window_body(panel, &db, loaded, test_stock);
                        }
                        MachineKind::ShiftBoard => {
                            shift_board_body(panel, &clock, &shift);
                        }
                    }

                    panel
                        .spawn(row())
                        .with_children(|row| {
                            row.spawn(button("Close  (Esc)", PanelAction::Close));
                        });
                });
        });
}

/// The board, which is a different thing in each of the three phases.
///
/// One panel rather than three screens because `InteractionMode` is per-player:
/// a modal debrief would have to be global, and would trap one chemist at a
/// summary while the other was still working.
fn shift_board_body(panel: &mut ChildSpawnerCommands, clock: &ShiftClock, shift: &Shift) {
    panel.spawn(label(
        format!("Shift {} · {}", clock.shift, clock.phase.label()),
        13.0,
        TEXT_DIM,
    ));

    match clock.phase {
        ShiftPhase::Prep => {
            panel.spawn(label(
                match &clock.forecast {
                    Some(pick) => pick.briefing.clone(),
                    None => "No briefing yet.".to_string(),
                },
                15.0,
                TEXT,
            ));
            panel.spawn(label(
                "Nobody is asking for anything until you start. Stock up, run the \
                 bench, work something out.",
                13.0,
                TEXT_DIM,
            ));
            panel.spawn(label(
                format!(
                    "This shift: {} orders, up to {} at the counter.",
                    clock.rules.quota, clock.rules.max_active
                ),
                13.0,
                TEXT_DIM,
            ));
            panel.spawn(row()).with_children(|row| {
                row.spawn(button("Begin shift", PanelAction::BeginShift));
            });
        }

        ShiftPhase::Service => {
            panel.spawn(label(
                format!(
                    "{} of {} orders resolved.",
                    clock.resolved, clock.rules.quota
                ),
                15.0,
                TEXT,
            ));
            if let Some(pick) = &clock.forecast {
                panel.spawn(label(pick.briefing.clone(), 13.0, TEXT_DIM));
            }
        }

        ShiftPhase::Debrief => {
            let report = ShiftReport::between(&clock.opening, &clock.closing);
            panel.spawn(label(
                format!(
                    "Delivered {}   ·   botched {}   ·   standing {:+}",
                    report.delivered, report.botched, report.reputation
                ),
                15.0,
                TEXT,
            ));
            panel.spawn(label(
                format!(
                    "Research earned {}   ·   recipes recorded {}",
                    report.research, report.recipes
                ),
                13.0,
                TEXT_DIM,
            ));

            panel.spawn(label(
                format!("Requisition — {} standing available", shift.reputation),
                13.0,
                TEXT_DIM,
            ));
            panel.spawn(wrap_row()).with_children(|row| {
                for kind in RequisitionKind::ALL {
                    // A produce crate is a flag, not a count: buying a second
                    // one would charge again for something already ordered.
                    let already = kind == RequisitionKind::ProduceCrate
                        && clock.requisition.produce_crate;
                    let available = can_afford(shift.reputation, kind) && !already;
                    let caption = if already {
                        format!("{} — ordered", kind.label())
                    } else {
                        format!("{} ({})", kind.label(), kind.cost())
                    };
                    let mut entity = row.spawn(button(caption, PanelAction::Requisition(kind)));
                    // Drawn dead rather than drawn live and silently doing
                    // nothing — a button that looks clickable but is refused
                    // reads as the game being broken.
                    if !available {
                        entity.insert(BackgroundColor(Color::srgb(0.11, 0.12, 0.14)));
                    }
                }
            });
            for kind in RequisitionKind::ALL {
                panel.spawn(label(
                    format!("  {} — {}", kind.label(), kind.blurb()),
                    12.0,
                    TEXT_DIM,
                ));
            }

            panel.spawn(row()).with_children(|row| {
                row.spawn(button("Sign off", PanelAction::SignOff));
            });
        }
    }
}

fn dispenser_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    kind: MachineKind,
    amount: Option<&DispenseAmount>,
    loaded: Option<&Container>,
) {
    let selected = amount.map(|a| a.0).unwrap_or(Units::whole(10));

    if kind == MachineKind::TestBench {
        panel.spawn(label(
            "Free reagents. Anything made here cannot be delivered for credit.",
            13.0,
            TEXT_DIM,
        ));
    }

    panel.spawn(label("Dispense amount", 13.0, TEXT_DIM));
    panel.spawn(row()).with_children(|row| {
        for step in [1, 5, 10, 25, 50] {
            let units = Units::whole(step);
            let mut entity = row.spawn(button(format!("{step}u"), PanelAction::SetAmount(units)));
            if units == selected {
                entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
            }
        }
    });

    panel.spawn(label("Reagents", 13.0, TEXT_DIM));
    panel.spawn(wrap_row()).with_children(|row| {
        for reagent in db.reagents.dispensable() {
            row.spawn(button(
                reagent.name.clone(),
                PanelAction::Dispense(reagent.id),
            ));
        }
    });

    container_readout(panel, db, loaded, true);
}

fn chemmaster_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    buffer: Option<&Buffer>,
    loaded: Option<&Container>,
) {
    panel.spawn(label("Loaded container", 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let Some(container) = loaded else {
                section.spawn(label(
                    "No container loaded. Carry a beaker over and press E.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            };
            if container.solution.is_empty() {
                section.spawn(label("Empty.", 14.0, TEXT_DIM));
                return;
            }
            for (reagent, quantity) in container.solution.iter() {
                section.spawn(row()).with_children(|row| {
                    row.spawn(reagent_name(db, reagent, quantity));
                    for step in [5, 10] {
                        let units = Units::whole(step);
                        row.spawn(button(
                            format!("▸{step}"),
                            PanelAction::ToBuffer(reagent, units),
                        ));
                    }
                    row.spawn(button(
                        "▸All",
                        PanelAction::ToBuffer(reagent, quantity),
                    ));
                });
            }
        });

    panel.spawn(label("Buffer", 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let Some(buffer) = buffer else {
                return;
            };
            if buffer.0.is_empty() {
                section.spawn(label("Empty.", 14.0, TEXT_DIM));
                return;
            }
            for (reagent, quantity) in buffer.0.iter() {
                section.spawn(row()).with_children(|row| {
                    row.spawn(reagent_name(db, reagent, quantity));
                    row.spawn(button(
                        "◂All",
                        PanelAction::ToContainer(reagent, quantity),
                    ));
                });
            }
        });

    panel.spawn(label("Package from buffer", 13.0, TEXT_DIM));
    panel.spawn(row()).with_children(|row| {
        row.spawn(button(
            format!("Pill  ({})", ContainerKind::Pill.capacity()),
            PanelAction::Package(ContainerKind::Pill),
        ));
        row.spawn(button(
            format!("Bottle  ({})", ContainerKind::Bottle.capacity()),
            PanelAction::Package(ContainerKind::Bottle),
        ));
    });

    panel.spawn(row()).with_children(|row| {
        row.spawn(button("Eject container", PanelAction::Eject));
    });
}

fn analyzer_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    loaded: Option<&Container>,
) {
    panel.spawn(label(
        "Breaks a sample down and works out how it was put together.",
        13.0,
        TEXT_DIM,
    ));

    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let Some(container) = loaded else {
                section.spawn(label(
                    "No sample loaded. Carry a container over and press E.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            };
            if container.solution.is_empty() {
                section.spawn(label("Sample is empty.", 14.0, TEXT_DIM));
                return;
            }

            // Percentages rather than raw units: composition is what the
            // machine measures, and it is what identifies a mixture.
            let total = container.solution.total_volume().as_f32().max(0.01);
            for (reagent, quantity) in container.solution.iter() {
                let share = quantity.as_f32() / total * 100.0;
                section.spawn(label(
                    format!(
                        "{:<16} {:>8}   {:>5.1}%",
                        db.reagents.get(reagent).name,
                        quantity.to_string(),
                        share
                    ),
                    14.0,
                    TEXT,
                ));
            }

            let unknown: Vec<&str> = db
                .reactions
                .iter()
                .filter(|reaction| !knowledge.is_known(reaction.id))
                .filter(|reaction| {
                    reaction
                        .product_ids()
                        .any(|id| container.solution.volume_of(id).is_positive())
                })
                .map(|reaction| reaction.key.as_str())
                .collect();

            section.spawn(row()).with_children(|row| {
                row.spawn(button("Identify method", PanelAction::Analyze));
                row.spawn(button("Eject", PanelAction::Eject));
            });

            section.spawn(label(
                if unknown.is_empty() {
                    "Nothing here you have not already written up.".to_string()
                } else {
                    format!("{} unrecorded method(s) present.", unknown.len())
                },
                13.0,
                if unknown.is_empty() {
                    TEXT_DIM
                } else {
                    Color::srgb(0.70, 0.85, 0.60)
                },
            ));
        });
}

fn grinder_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    catalog: Option<&ProduceCatalog>,
    hopper: Option<&Hopper>,
    loaded: Option<&Container>,
) {
    panel.spawn(label(
        "Extracts produce straight into the beaker. Fast, and never clean — \
         what comes out still has to go through the ChemMaster.",
        13.0,
        TEXT_DIM,
    ));

    panel.spawn(label("Hopper", 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let (Some(catalog), Some(hopper)) = (catalog, hopper) else {
                return;
            };
            if hopper.0.is_empty() {
                section.spawn(label(
                    "Empty. Carry produce over from the counter and press E.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            }

            // Grouped by kind: five separate "Poppy" rows tell the player
            // nothing a count does not.
            for kind in catalog.iter() {
                let count = hopper.0.iter().filter(|id| **id == kind.id).count();
                if count == 0 {
                    continue;
                }
                let yields = kind
                    .yields
                    .iter()
                    .map(|(id, amount)| format!("{amount} {}", db.reagents.get(*id).name))
                    .collect::<Vec<_>>()
                    .join(" + ");
                let [r, g, b] = kind.color;
                section.spawn(label(
                    format!("{count} × {:<20} → {yields}", kind.name),
                    14.0,
                    Color::srgb(0.45 + r * 0.55, 0.45 + g * 0.55, 0.45 + b * 0.55),
                ));
            }
        });

    panel.spawn(row()).with_children(|row| {
        row.spawn(button("Grind one", PanelAction::Grind { all: false }));
        row.spawn(button("Grind all", PanelAction::Grind { all: true }));
    });

    container_readout(panel, db, loaded, true);
}

/// The counter tray.
///
/// There are no buttons to hand anything over: the window matches on its own
/// the moment somebody at the counter wants what is in it. So the panel's job
/// is to say what will happen and, when nothing does, why not.
fn delivery_window_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    loaded: Option<&Container>,
    test_stock: bool,
) {
    panel.spawn(label(
        "Anything left here goes to the next crew member at the counter who \
         asked for something in it. No need to wait around for them.",
        13.0,
        TEXT_DIM,
    ));

    container_readout(panel, db, loaded, false);

    let Some(container) = loaded else {
        return;
    };

    // Worst news first, same order the grading runs in.
    let (message, color) = if test_stock {
        (
            "Test bench stock. Nobody will take this — rinse it out and make \
             the real thing."
                .to_string(),
            Color::srgb(0.95, 0.55, 0.45),
        )
    } else if container.solution.is_empty() {
        (
            "Empty. Put a finished batch in and it will go out on its own."
                .to_string(),
            TEXT_DIM,
        )
    } else {
        (
            format!(
                "Waiting for someone who needs {}.",
                container
                    .solution
                    .iter()
                    .map(|(reagent, _)| db.reagents.get(reagent).name.clone())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            Color::srgb(0.70, 0.85, 0.60),
        )
    };
    panel.spawn(label(message, 13.0, color));
}

/// Shared contents readout for whatever is sitting in the machine's slot.
fn container_readout(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    loaded: Option<&Container>,
    show_empty_button: bool,
) {
    panel.spawn(label("Loaded container", 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let Some(container) = loaded else {
                section.spawn(label(
                    "No container loaded. Carry a beaker over and press E.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            };

            section.spawn(label(
                format!(
                    "{}   {} / {}",
                    container.kind.label(),
                    container.solution.total_volume(),
                    container.kind.capacity()
                ),
                15.0,
                TEXT,
            ));

            if container.solution.is_empty() {
                section.spawn(label("Empty.", 14.0, TEXT_DIM));
            } else {
                for (reagent, quantity) in container.solution.iter() {
                    section.spawn(reagent_name(db, reagent, quantity));
                }
            }

            section.spawn(row()).with_children(|row| {
                row.spawn(button("Eject", PanelAction::Eject));
                if show_empty_button {
                    row.spawn(button("Empty", PanelAction::Empty));
                }
            });
        });
}

// ---------------------------------------------------------------------------
// Reference book
// ---------------------------------------------------------------------------

/// The chemist's notes. Known recipes show the full method; locked ones show
/// only what a chemist would plausibly remember — what it treats, how many
/// ingredients, and whatever they have worked out so far.
fn spawn_reference_book(commands: &mut Commands, db: &ChemDb, knowledge: &Knowledge) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            PanelRoot,
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: px(820),
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(20)),
                        row_gap: px(8),
                        border_radius: BorderRadius::all(px(8)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                ))
                .with_children(|book| {
                    book.spawn(row()).with_children(|header| {
                        header.spawn(heading("Reference Book"));
                    });
                    book.spawn(label(
                        format!(
                            "{} of {} recipes recorded   ·   {} research   ·   B or Esc to close",
                            knowledge.known_count(),
                            db.reactions.len(),
                            knowledge.research_points
                        ),
                        13.0,
                        TEXT_DIM,
                    ));

                    for reaction in db.reactions.iter() {
                        let known = knowledge.is_known(reaction.id);
                        // Name the entry after what it makes, not the reaction
                        // id — that is how the crew ask for it.
                        let product = reaction
                            .products
                            .first()
                            .map(|(id, _)| db.reagents.get(*id));
                        let title = product
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| reaction.key.clone());

                        book.spawn((section(), BackgroundColor(SECTION_BG)))
                            .with_children(|entry| {
                                entry.spawn(label(
                                    if known {
                                        title.clone()
                                    } else {
                                        format!("{title}   —   not yet worked out")
                                    },
                                    16.0,
                                    if known { TEXT } else { TEXT_DIM },
                                ));

                                if let Some(treats) = product.and_then(|p| p.treats.as_ref()) {
                                    entry.spawn(label(treats.clone(), 13.0, TEXT_DIM));
                                }

                                if known {
                                    entry.spawn(label(recipe_line(db, reaction), 14.0, TEXT));
                                    if let Some(overdose) =
                                        product.and_then(|p| p.overdose)
                                    {
                                        entry.spawn(label(
                                            format!("Overdoses above {overdose} in a single dose."),
                                            13.0,
                                            Color::srgb(0.90, 0.62, 0.45),
                                        ));
                                    }
                                } else {
                                    entry.spawn(label(
                                        format!("{} ingredients.", reaction.reactants.len()),
                                        13.0,
                                        TEXT_DIM,
                                    ));
                                    for hint in knowledge.visible_hints(db, reaction.id) {
                                        entry.spawn(label(
                                            format!("· {hint}"),
                                            13.0,
                                            Color::srgb(0.70, 0.78, 0.62),
                                        ));
                                    }
                                    // Only offer the purchase when it can
                                    // actually go through; a button that
                                    // silently does nothing is worse than none.
                                    if knowledge.hint_available(db, reaction.id) {
                                        let affordable =
                                            knowledge.research_points >= HINT_COST;
                                        entry.spawn(row()).with_children(|row| {
                                            if affordable {
                                                row.spawn(button(
                                                    format!("Study further  ({HINT_COST} research)"),
                                                    PanelAction::BuyHint(reaction.id),
                                                ));
                                            } else {
                                                row.spawn(label(
                                                    format!(
                                                        "Needs {HINT_COST} research to study further."
                                                    ),
                                                    12.0,
                                                    TEXT_DIM,
                                                ));
                                            }
                                        });
                                    }
                                }
                            });
                    }
                });
        });
}

/// Renders a recipe as `1 Oxygen + 1 Carbon + 1 Sugar  →  3 Inaprovaline`.
fn recipe_line(db: &ChemDb, reaction: &chem_sim::Reaction) -> String {
    let part = |pairs: &[(ReagentId, Units)]| {
        pairs
            .iter()
            .map(|(id, amount)| format!("{} {}", amount, db.reagents.get(*id).name))
            .collect::<Vec<_>>()
            .join(" + ")
    };

    let mut line = format!("{}  →  {}", part(&reaction.reactants), part(&reaction.products));
    if !reaction.catalysts.is_empty() {
        // Catalysts read as ingredients unless they are called out, and a
        // player who consumes their only plasma has learned the wrong lesson.
        line.push_str(&format!(
            "     (catalyst: {}, not consumed)",
            part(&reaction.catalysts)
        ));
    }
    line
}

// ---------------------------------------------------------------------------
// Order queue
// ---------------------------------------------------------------------------

#[derive(Component)]
struct OrderSlot(usize);

#[derive(Component)]
struct ShiftReadout;

/// What the most urgent requester actually said.
#[derive(Component)]
struct PleaLine;

/// Which phase the lab is in, and what ends it.
#[derive(Component)]
struct PhaseBanner;

// The disjointness filters are noisy inline; naming them keeps the queue
// system's signature readable.
type ShiftText<'w, 's> = Single<
    'w,
    's,
    &'static mut Text,
    (
        With<ShiftReadout>,
        Without<OrderSlot>,
        Without<PleaLine>,
        Without<PhaseBanner>,
    ),
>;
type PleaText<'w, 's> = Single<
    'w,
    's,
    &'static mut Text,
    (
        With<PleaLine>,
        Without<OrderSlot>,
        Without<ShiftReadout>,
        Without<PhaseBanner>,
    ),
>;
type BannerText<'w, 's> = Single<
    'w,
    's,
    &'static mut Text,
    (
        With<PhaseBanner>,
        Without<OrderSlot>,
        Without<ShiftReadout>,
        Without<PleaLine>,
    ),
>;

/// Fixed slots, filled in each frame.
///
/// The queue shows a live countdown, so rebuilding it on change would mean
/// rebuilding every frame. Writing into pre-spawned rows keeps it to a couple
/// of string comparisons instead.
fn spawn_order_queue(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: px(16),
                right: px(16),
                width: px(310),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(6),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.07, 0.09, 0.82)),
        ))
        .with_children(|queue| {
            queue.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(Color::srgb(0.95, 0.88, 0.45)),
                PhaseBanner,
            ));
            queue.spawn(label("ORDERS", 13.0, TEXT_DIM));
            for index in 0..ORDER_SLOTS {
                queue.spawn((
                    Text::new(""),
                    TextFont::from_font_size(14.0),
                    TextColor(TEXT),
                    OrderSlot(index),
                ));
            }
            queue.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(Color::srgb(0.72, 0.78, 0.70)),
                PleaLine,
            ));
            queue.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(TEXT_DIM),
                ShiftReadout,
            ));
        });
}

/// The one line that tells the player what the lab is doing and what ends it.
///
/// Pure so the wording of every phase can be checked at once — this is the only
/// place the untimed first prep explains itself, and a player who cannot tell
/// why nobody is arriving will read the feature as the game being broken.
fn phase_banner_line(clock: &ShiftClock) -> String {
    let shift = clock.shift;
    match clock.phase {
        ShiftPhase::Prep => match clock.remaining {
            Some(left) => format!("PREP · SHIFT {shift} — service in {}", mmss(left)),
            None => format!("PREP · SHIFT {shift} — begin at the shift board when you're ready"),
        },
        ShiftPhase::Service => format!(
            "SERVICE · SHIFT {shift} — {} of {} resolved",
            clock.resolved, clock.rules.quota
        ),
        ShiftPhase::Debrief => match clock.remaining {
            Some(left) => format!(
                "DEBRIEF · SHIFT {shift} — sign off at the board · {}",
                mmss(left)
            ),
            None => format!("DEBRIEF · SHIFT {shift} — sign off at the board"),
        },
    }
}

fn mmss(seconds: f32) -> String {
    let whole = seconds.max(0.0) as u32;
    format!("{}:{:02}", whole / 60, whole % 60)
}

fn update_phase_banner(clock: Res<ShiftClock>, banner: BannerText) {
    let line = phase_banner_line(&clock);
    let mut banner = banner.into_inner();
    if banner.0 != line {
        banner.0 = line;
    }
}

fn update_order_queue(
    db: Res<ChemDb>,
    shift: Res<Shift>,
    orders: Query<(&CrewMember, &Order, &CrewRoute)>,
    mut slots: Query<(&OrderSlot, &mut Text, &mut TextColor), Without<ShiftReadout>>,
    readout: ShiftText,
    plea_line: PleaText,
) {
    // Most urgent first, so the one about to expire is always at the top.
    let mut pending: Vec<(&CrewMember, &Order, &CrewRoute)> = orders.iter().collect();
    pending.sort_by(|a, b| {
        a.1.timer
            .remaining_secs()
            .total_cmp(&b.1.timer.remaining_secs())
    });

    for (slot, mut text, mut color) in &mut slots {
        let line = pending.get(slot.0).map(|(member, order, route)| {
            let reagent = &db.reagents.get(order.reagent).name;
            match route.phase {
                CrewPhase::Arriving => {
                    format!("{}\n  {} {}  ·  on the way", member.name, order.amount, reagent)
                }
                _ => {
                    let remaining = order.timer.remaining_secs().max(0.0) as u32;
                    format!(
                        "{}\n  {} {}  ·  {}:{:02}",
                        member.name,
                        order.amount,
                        reagent,
                        remaining / 60,
                        remaining % 60
                    )
                }
            }
        });

        let urgent = pending
            .get(slot.0)
            .map(|(_, order, _)| order.timer.remaining_secs() < 30.0)
            .unwrap_or(false);
        let wanted = if urgent {
            Color::srgb(0.95, 0.55, 0.45)
        } else {
            TEXT
        };
        if color.0 != wanted {
            color.0 = wanted;
        }

        let line = line.unwrap_or_default();
        if text.0 != line {
            text.0 = line;
        }
    }

    // Only the most urgent request gets its words shown; three at once is a
    // wall of text nobody reads mid-shift.
    let plea = pending
        .first()
        .map(|(_, order, _)| format!("\u{201c}{}\u{201d}", order.plea))
        .unwrap_or_default();
    let mut plea_line = plea_line.into_inner();
    if plea_line.0 != plea {
        plea_line.0 = plea;
    }

    let summary = format!(
        "delivered {}   botched {}   reputation {:+}",
        shift.succeeded, shift.botched, shift.reputation
    );
    let mut readout = readout.into_inner();
    if readout.0 != summary {
        readout.0 = summary;
    }
}

// ---------------------------------------------------------------------------
// Discovery toast
// ---------------------------------------------------------------------------

#[derive(Component)]
struct Toast(Timer);

/// Something worth interrupting for.
///
/// A message rather than a direct spawn so that [`show_toasts`] is the only
/// thing that ever puts one on screen. Despawning through `Commands` is
/// deferred, so two spawners in the same frame — a recipe discovered as the
/// shift ends, or a reaction cascade discovering two at once — would each see a
/// stale "no toasts exist" and stack their cards at the same position.
#[derive(Message)]
struct ShowToast {
    kicker: &'static str,
    title: String,
    subtitle: String,
    background: Color,
}

/// Puts up at most one card per frame.
///
/// One at a time on purpose: two moments worth interrupting for that land
/// together are still one interruption, and stacked cards would cover the room.
/// The newest wins, because it is the one describing what just happened.
fn show_toasts(
    mut commands: Commands,
    mut requests: MessageReader<ShowToast>,
    existing: Query<Entity, With<Toast>>,
) {
    let Some(request) = requests.read().last() else {
        return;
    };
    for toast in &existing {
        commands.entity(toast).despawn();
    }
    spawn_toast(
        &mut commands,
        request.kicker,
        request.title.clone(),
        request.subtitle.clone(),
        request.background,
    );
}

fn spawn_toast(
    commands: &mut Commands,
    kicker: &'static str,
    title: String,
    subtitle: String,
    background: Color,
) {
    let kicker_text = kicker.to_string();
    let kicker_color = Color::srgb(0.95, 0.88, 0.45);

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: percent(22),
                width: percent(100),
                justify_content: JustifyContent::Center,
                ..default()
            },
            Toast(Timer::from_seconds(4.5, TimerMode::Once)),
        ))
        .with_children(|toast| {
            toast
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        padding: UiRect::axes(px(22), px(12)),
                        row_gap: px(3),
                        border_radius: BorderRadius::all(px(6)),
                        ..default()
                    },
                    BackgroundColor(background),
                ))
                .with_children(|card| {
                    card.spawn(label(kicker_text, 12.0, kicker_color));
                    card.spawn(label(title, 20.0, TEXT));
                    card.spawn(label(subtitle, 12.0, TEXT_DIM));
                });
        });
}

/// A brief banner when a recipe is worked out.
///
/// The radio carries it too, but the radio is a slow feed you might be looking
/// away from — and discovering a recipe is the one moment that deserves to
/// interrupt.
fn announce_discoveries(
    mut discovered: MessageReader<RecipeDiscovered>,
    mut toasts: MessageWriter<ShowToast>,
) {
    for event in discovered.read() {
        toasts.write(ShowToast {
            kicker: "RECIPE RECORDED",
            title: event.name.clone(),
            subtitle: "Added to your reference book (B)".to_string(),
            background: Color::srgba(0.10, 0.20, 0.13, 0.94),
        });
    }
}

/// Says out loud what just changed about the lab.
///
/// Watches the clock from `Update` rather than hanging off `OnEnter`, because
/// what it has to say is filled in *by* the phase-entry systems — the shift's
/// quota, the report, the briefing. Read on the transition it would show the
/// previous shift's numbers, or none at all on the first prep of a session,
/// where the forecast is not drawn until the data files land.
fn announce_phase(
    clock: Res<ShiftClock>,
    mut announced: Local<Option<(ShiftPhase, u32)>>,
    mut toasts: MessageWriter<ShowToast>,
) {
    // Prep on the first frame is not yet open: it has no briefing to give.
    if clock.pending_open {
        return;
    }
    let current = (clock.phase, clock.shift);
    if *announced == Some(current) {
        return;
    }
    *announced = Some(current);

    let (kicker, subtitle, background) = match clock.phase {
        ShiftPhase::Prep => (
            "PREP",
            match &clock.forecast {
                Some(pick) => pick.briefing.clone(),
                None => "Nobody is asking for anything yet.".to_string(),
            },
            Color::srgba(0.10, 0.14, 0.22, 0.94),
        ),
        ShiftPhase::Service => (
            "SHIFT STARTED",
            format!("{} orders to work through", clock.rules.quota),
            Color::srgba(0.18, 0.15, 0.09, 0.94),
        ),
        ShiftPhase::Debrief => {
            let report = ShiftReport::between(&clock.opening, &clock.closing);
            (
                "SHIFT OVER",
                format!(
                    "{} delivered, {} botched, {:+} standing",
                    report.delivered, report.botched, report.reputation
                ),
                Color::srgba(0.10, 0.20, 0.13, 0.94),
            )
        }
    };

    toasts.write(ShowToast {
        kicker,
        title: format!("Shift {}", clock.shift),
        subtitle,
        background,
    });
}

fn expire_toasts(mut commands: Commands, time: Res<Time>, mut toasts: Query<(Entity, &mut Toast)>) {
    for (entity, mut toast) in &mut toasts {
        if toast.0.tick(time.delta()).just_finished() {
            commands.entity(entity).despawn();
        }
    }
}

// ---------------------------------------------------------------------------
// Radio feed
// ---------------------------------------------------------------------------

#[derive(Component)]
struct RadioSlot(usize);

fn spawn_radio_feed(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: px(16),
                left: px(16),
                width: px(520),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(4),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.05, 0.06, 0.08, 0.78)),
        ))
        .with_children(|feed| {
            feed.spawn(label("STATION RADIO", 12.0, TEXT_DIM));
            for index in 0..RADIO_SLOTS {
                feed.spawn((
                    Text::new(""),
                    TextFont::from_font_size(13.0),
                    TextColor(TEXT_DIM),
                    RadioSlot(index),
                ));
            }
        });
}

fn update_radio_feed(
    log: Res<RadioLog>,
    mut slots: Query<(&RadioSlot, &mut Text, &mut TextColor)>,
) {
    if !log.is_changed() {
        return;
    }
    for (slot, mut text, mut color) in &mut slots {
        let entry = log.entries.get(slot.0);
        let line = entry
            .map(|entry| format!("[{}] {}", entry.channel, entry.text))
            .unwrap_or_default();

        // Older lines fade, so the newest report is the one that catches the
        // eye without needing an alert.
        let age = log.entries.len().saturating_sub(slot.0 + 1);
        let fade = 1.0 - (age as f32 * 0.13).min(0.55);
        let base = match entry {
            Some(entry) if entry.good => Color::srgb(0.62, 0.86, 0.62),
            Some(_) => Color::srgb(0.86, 0.88, 0.92),
            None => TEXT_DIM,
        };
        let wanted = base.with_alpha(fade);
        if color.0 != wanted {
            color.0 = wanted;
        }
        if text.0 != line {
            text.0 = line;
        }
    }
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

fn heading(text: impl Into<String>) -> impl Bundle {
    (
        Text::new(text.into()),
        TextFont::from_font_size(22.0),
        TextColor(TEXT),
    )
}

fn label(text: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(text.into()),
        TextFont::from_font_size(size),
        TextColor(color),
    )
}

fn reagent_name(db: &ChemDb, reagent: ReagentId, quantity: Units) -> impl Bundle {
    let definition = db.reagents.get(reagent);
    let [r, g, b] = definition.color;
    (
        Text::new(format!("{:<16} {:>8}", definition.name, quantity.to_string())),
        TextFont::from_font_size(14.0),
        // Tinting toward the reagent's own colour makes a mixed beaker
        // scannable at a glance instead of a wall of identical text.
        TextColor(Color::srgb(
            0.45 + r * 0.55,
            0.45 + g * 0.55,
            0.45 + b * 0.55,
        )),
        Node {
            min_width: px(230),
            ..default()
        },
    )
}

fn row() -> impl Bundle {
    Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: px(4),
        ..default()
    }
}

fn wrap_row() -> impl Bundle {
    Node {
        flex_direction: FlexDirection::Row,
        flex_wrap: FlexWrap::Wrap,
        align_items: AlignItems::Center,
        ..default()
    }
}

fn section() -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(px(10)),
        row_gap: px(3),
        border_radius: BorderRadius::all(px(5)),
        ..default()
    }
}

fn button(text: impl Into<String>, action: PanelAction) -> impl Bundle {
    (
        Button,
        Node {
            padding: UiRect::axes(px(11), px(6)),
            margin: UiRect::all(px(3)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: BorderRadius::all(px(4)),
            ..default()
        },
        BackgroundColor(BUTTON_IDLE),
        action,
        children![(
            Text::new(text.into()),
            TextFont::from_font_size(14.0),
            TextColor(TEXT),
        )],
    )
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

type ChangedButtons<'w, 's> = Query<
    'w,
    's,
    (&'static Interaction, &'static mut BackgroundColor, Has<Selected>),
    (Changed<Interaction>, With<Button>),
>;

fn button_feedback(mut buttons: ChangedButtons) {
    for (interaction, mut background, selected) in &mut buttons {
        background.0 = match interaction {
            Interaction::Pressed => BUTTON_ACTIVE,
            Interaction::Hovered => BUTTON_HOVER,
            // `Interaction` counts as changed on spawn, so without the
            // `Selected` check the highlighted amount button would be reset to
            // idle the frame the panel is built.
            Interaction::None if selected => BUTTON_ACTIVE,
            Interaction::None => BUTTON_IDLE,
        };
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_panel_clicks(
    buttons: Query<(&Interaction, &PanelAction), Changed<Interaction>>,
    mut modes: Query<(Entity, &mut InteractionMode), With<LocalPlayer>>,
    mut machines: Query<&mut Machine>,
    mut amounts: Query<&mut DispenseAmount>,
    mut dispense: MessageWriter<DispenseRequested>,
    mut eject: MessageWriter<EjectRequested>,
    mut empty: MessageWriter<EmptyRequested>,
    mut transfer: MessageWriter<BufferTransferRequested>,
    mut package: MessageWriter<PackageRequested>,
    mut analyze: MessageWriter<AnalyzeRequested>,
    mut grind: MessageWriter<GrindRequested>,
    mut begin_shift: MessageWriter<BeginShiftRequested>,
    mut sign_off: MessageWriter<SignOffRequested>,
    mut requisition: MessageWriter<RequisitionRequested>,
    mut knowledge: ResMut<Knowledge>,
    db: Res<ChemDb>,
) {
    let Some((player, mut mode)) = modes.iter_mut().next() else {
        return;
    };
    let open_machine = match *mode {
        InteractionMode::UsingMachine(machine) => Some(machine),
        _ => None,
    };

    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }

        // Actions available without a machine panel open: the book's own
        // controls, and closing whatever is up.
        match action {
            PanelAction::BuyHint(reaction) => {
                knowledge.buy_hint(&db, *reaction);
                continue;
            }
            PanelAction::Close => {
                leave_machine(player, &mut mode, &mut machines);
                return;
            }
            _ => {}
        }

        let Some(machine) = open_machine else {
            continue;
        };
        match action {
            PanelAction::SetAmount(units) => {
                if let Ok(mut amount) = amounts.get_mut(machine) {
                    amount.0 = *units;
                }
            }
            PanelAction::Dispense(reagent) => {
                dispense.write(DispenseRequested {
                    machine,
                    reagent: *reagent,
                });
            }
            PanelAction::Eject => {
                eject.write(EjectRequested { machine });
            }
            PanelAction::Empty => {
                empty.write(EmptyRequested { machine });
            }
            PanelAction::ToBuffer(reagent, amount) => {
                transfer.write(BufferTransferRequested {
                    machine,
                    reagent: *reagent,
                    amount: *amount,
                    direction: BufferDirection::ToBuffer,
                });
            }
            PanelAction::ToContainer(reagent, amount) => {
                transfer.write(BufferTransferRequested {
                    machine,
                    reagent: *reagent,
                    amount: *amount,
                    direction: BufferDirection::ToContainer,
                });
            }
            PanelAction::Package(kind) => {
                package.write(PackageRequested {
                    machine,
                    kind: *kind,
                });
            }
            PanelAction::Analyze => {
                analyze.write(AnalyzeRequested { machine });
            }
            PanelAction::Grind { all } => {
                grind.write(GrindRequested { machine, all: *all });
            }
            // The board's three. Requests, not writes: the server owns the
            // phase and the standing, and a client that moved either locally
            // would be corrected out from under the player a frame later.
            PanelAction::BeginShift => {
                begin_shift.write(BeginShiftRequested { board: machine });
            }
            PanelAction::SignOff => {
                sign_off.write(SignOffRequested { board: machine });
            }
            PanelAction::Requisition(kind) => {
                requisition.write(RequisitionRequested {
                    board: machine,
                    kind: *kind,
                });
            }
            // Handled above, before the machine guard.
            PanelAction::BuyHint(_) | PanelAction::Close => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(phase: ShiftPhase, remaining: Option<f32>) -> ShiftClock {
        ShiftClock {
            shift: 3,
            phase,
            remaining,
            ..default()
        }
    }

    #[test]
    fn an_untimed_prep_says_how_it_ends() {
        // The one case with nothing on screen counting down. Without this line
        // an empty room reads as the game having stopped working.
        let line = phase_banner_line(&clock(ShiftPhase::Prep, None));
        assert!(line.contains("PREP"), "{line}");
        assert!(line.contains("shift board"), "{line}");
        assert!(!line.contains(':'), "an untimed prep should show no clock: {line}");
    }

    #[test]
    fn a_timed_prep_shows_the_countdown() {
        let line = phase_banner_line(&clock(ShiftPhase::Prep, Some(107.0)));
        assert!(line.contains("1:47"), "{line}");
    }

    #[test]
    fn service_shows_progress_against_the_quota() {
        let mut clock = clock(ShiftPhase::Service, None);
        clock.resolved = 2;
        clock.rules.quota = 7;
        let line = phase_banner_line(&clock);
        assert!(line.contains("SERVICE"), "{line}");
        assert!(line.contains("2 of 7"), "{line}");
    }

    #[test]
    fn the_debrief_points_at_the_board() {
        let line = phase_banner_line(&clock(ShiftPhase::Debrief, Some(41.0)));
        assert!(line.contains("sign off"), "{line}");
        assert!(line.contains("0:41"), "{line}");
    }

    #[test]
    fn the_clock_reads_as_minutes_and_seconds() {
        assert_eq!(mmss(0.0), "0:00");
        assert_eq!(mmss(9.6), "0:09");
        assert_eq!(mmss(120.0), "2:00");
        // A countdown that has run past zero must not render as a negative.
        assert_eq!(mmss(-5.0), "0:00");
    }
}
