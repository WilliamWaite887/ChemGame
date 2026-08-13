//! Machine control panels.
//!
//! Panels are pure presentation. Every button carries a [`PanelAction`] which
//! one system turns into a message; nothing here mutates a solution.
//!
//! The whole panel is rebuilt whenever the state it shows changes, rather than
//! patching individual nodes. At this size that is far simpler to keep correct,
//! and it only happens on user action.

use std::collections::HashSet;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use chem_sim::{Category, DamageKind, Kelvin, ReactionId, ReagentId, Units};

use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::containers::{Container, ContainerKind, InSlot};
use crate::crew::{CrewMember, CrewPhase, CrewRoute};
use crate::interaction::{leave_machine, InteractionMode, LeaveMachineRequested};
use crate::knowledge::{
    product_name, reaction_categories, BuyHintRequested, Knowledge, RecipeDiscovered,
    UpgradeDispenserRequested, HINT_COST,
};
use crate::machines::{
    slotted_container, AnalyzeRequested, Buffer, BufferDirection, BufferTransferRequested,
    DispenseAmount, DispenseRequested, EjectRequested, EmptyRequested, GrindRequested, Hopper,
    Machine, MachineKind, PackageRequested, SetHeaterPower, SetTargetTemperature, TestBenchStock,
    Thermostat, TEMPERATURE_PRESETS,
};
use crate::orders::{reference_category, Department, Order, Shift};
use crate::player::LocalPlayer;
use crate::produce::{ProduceCatalog, ProduceId};
use crate::radio::RadioLog;
use crate::shift::{can_afford, RequisitionKind, RequisitionRequested, ToggleAcceptingOrders};
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

// Shared with the main menu, so the first screen the player sees and every
// panel afterwards are visibly the same game.
pub(crate) const PANEL_BG: Color = Color::srgba(0.07, 0.08, 0.10, 0.97);
pub(crate) const SECTION_BG: Color = Color::srgba(0.12, 0.13, 0.16, 0.9);
pub(crate) const TEXT: Color = Color::srgb(0.88, 0.90, 0.94);
pub(crate) const TEXT_DIM: Color = Color::srgb(0.55, 0.59, 0.66);
/// A failed connection attempt, shown on the mode screen after a bounce back
/// from `AppState::Connecting` — the one place in the menu that needs to say
/// something went wrong out loud rather than staying silent.
pub(crate) const ERROR_TEXT: Color = Color::srgb(0.85, 0.35, 0.35);
pub(crate) const BUTTON_IDLE: Color = Color::srgb(0.17, 0.19, 0.23);
const BUTTON_HOVER: Color = Color::srgb(0.25, 0.29, 0.35);
const BUTTON_ACTIVE: Color = Color::srgb(0.20, 0.45, 0.62);

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(AppState::Playing),
            (
                spawn_order_queue,
                spawn_radio_feed,
                spawn_vitals_panel,
                spawn_room_label,
            ),
        )
        .add_systems(
            Update,
            (
                handle_panel_clicks,
                button_feedback,
                sync_panel,
                update_phase_banner,
                update_order_queue,
                update_vitals_panel,
                update_room_label,
                update_radio_feed,
                scroll_reference_book,
                announce_discoveries,
                announce_accepting_toggle,
                show_toasts,
                expire_toasts,
            )
                .chain()
                .run_if(in_state(AppState::Playing)),
        )
        .init_resource::<BookView>()
        .add_message::<ShowToast>();
    }
}

/// Root of the currently open panel.
#[derive(Component)]
struct PanelRoot;

/// Marks the currently chosen option in a group of buttons.
///
/// `pub(crate)` only because it is part of `button_feedback`'s query, which the
/// menu runs too; nothing outside this module adds it.
#[derive(Component)]
pub(crate) struct Selected;

/// What a button does when clicked.
#[derive(Component, Clone)]
enum PanelAction {
    SetAmount(Units),
    Dispense(ReagentId),
    UpgradeDispenser,
    Eject,
    Empty,
    ToBuffer(ReagentId, Units),
    ToContainer(ReagentId, Units),
    Package(ContainerKind),
    Analyze,
    Grind { all: bool },
    SetTarget(Kelvin),
    TogglePower,
    BuyHint(ReactionId),
    ShowCategory(Option<Category>),
    OpenRecipe(ReactionId),
    CloseRecipe,
    ToggleAcceptingOrders,
    Requisition(RequisitionKind),
    Close,
}

/// Which heading the reference book is open at. `None` is the "All" tab.
///
/// Local presentation state: it is neither replicated nor saved, because which
/// page a chemist happens to have open is nobody else's business and is not
/// worth a line in `save.ron`.
#[derive(Resource, Default)]
struct BookView {
    category: Option<Category>,
    /// The recipe whose tree screen is open, if any. `None` is the list.
    open_recipe: Option<ReactionId>,
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
    /// The book's open heading. Here rather than tracked separately because
    /// switching tab is exactly the same kind of change as any other: it
    /// alters what the panel shows, so it rebuilds the panel.
    book_category: Option<Category>,
    /// The recipe whose tree screen is open, if any — same rationale as
    /// `book_category`: opening or closing it changes what the panel shows.
    book_recipe: Option<ReactionId>,
    /// The standing board draws entirely from these, so without them its
    /// panel would freeze on whatever it happened to show first.
    department_standing: Vec<(Department, i32)>,
    accepting_orders: bool,
    research_points: u32,
    /// Chamber state. The sample's temperature is **rounded to 5K** for exactly
    /// the reason the countdown above is absent: while a chamber runs it moves
    /// every frame, and comparing the raw value would rebuild the panel every
    /// frame with it. Five kelvin is fine enough to watch a batch climb and
    /// coarse enough to cost one rebuild every second or so.
    temperature: Option<i32>,
    target: Option<i32>,
    powered: bool,
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
            book_category: None,
            book_recipe: None,
            department_standing: Vec::new(),
            // Neither `true` nor `false` alone is guaranteed to differ from
            // the first real frame's value, so the vector above carries the
            // "never compared yet" signal on its own — this field just needs
            // *a* starting value.
            accepting_orders: false,
            research_points: u32::MAX,
            temperature: Some(i32::MAX),
            target: Some(i32::MAX),
            powered: true,
        }
    }
}

/// Rounds a temperature to the granularity [`PanelSignature`] compares at.
fn panel_temperature(kelvin: Kelvin) -> i32 {
    (kelvin.0 / 5.0).round() as i32
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
        Option<&'static Thermostat>,
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
    book: Res<BookView>,
    catalog: Option<Res<ProduceCatalog>>,
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
            .and_then(|(_, _, buffer, _, _)| buffer)
            .map(|buffer| buffer.0.iter().collect())
            .unwrap_or_default(),
        hopper: machine_parts
            .and_then(|(_, _, _, hopper, _)| hopper)
            .map(|hopper| hopper.0.clone())
            .unwrap_or_default(),
        test_stock,
        amount: machine_parts
            .and_then(|(_, amount, _, _, _)| amount)
            .map(|a| a.0),
        known_recipes: knowledge.known_count(),
        book_category: book.category,
        book_recipe: book.open_recipe,
        department_standing: Department::ALL
            .into_iter()
            .map(|dept| (dept, shift.standing(dept)))
            .collect(),
        accepting_orders: shift.accepting_orders,
        research_points: knowledge.research_points,
        // Only tracked while a chamber panel is open, so no other machine pays
        // for the extra comparison.
        temperature: machine_parts
            .filter(|(machine, ..)| machine.kind == MachineKind::ReactionChamber)
            .and(loaded)
            .map(|container| panel_temperature(container.solution.temperature)),
        target: machine_parts
            .and_then(|(_, _, _, _, thermostat)| thermostat)
            .map(|thermostat| panel_temperature(thermostat.target)),
        powered: machine_parts
            .and_then(|(_, _, _, _, thermostat)| thermostat)
            .is_some_and(|thermostat| thermostat.powered),
    };

    if signature == *previous {
        return;
    }
    *previous = signature;

    for panel in &existing {
        commands.entity(panel).despawn();
    }

    if mode == InteractionMode::ReadingBook {
        spawn_reference_book(&mut commands, &db, &knowledge, book.category, book.open_recipe);
        return;
    }
    if open_machine.is_none() {
        return;
    }
    let Some((machine, amount, buffer, hopper, thermostat)) = machine_parts else {
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
                            dispenser_body(panel, &db, &knowledge, machine.kind, amount, loaded);
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
                        MachineKind::StandingBoard => {
                            standing_board_body(panel, &shift);
                        }
                        MachineKind::ReactionChamber => {
                            heater_body(panel, &db, thermostat, loaded);
                        }
                    }

                    panel.spawn(row()).with_children(|row| {
                        row.spawn(button("Close  (Esc)", PanelAction::Close));
                    });
                });
        });
}

/// Every department's standing, what each values, and live requisitions.
///
/// One panel rather than a modal, exactly like the old shift board: this is
/// per-player `InteractionMode`, and a modal would trap one chemist on a
/// summary screen while the other was still working the counter.
fn standing_board_body(panel: &mut ChildSpawnerCommands, shift: &Shift) {
    panel.spawn(label(
        if shift.accepting_orders {
            "Taking requests."
        } else {
            "Not accepting requests right now — flip the sign back when you're ready."
        },
        13.0,
        TEXT_DIM,
    ));
    panel.spawn(row()).with_children(|row| {
        let caption = if shift.accepting_orders {
            "Stop taking requests"
        } else {
            "Start taking requests"
        };
        row.spawn(button(caption, PanelAction::ToggleAcceptingOrders));
    });

    for department in Department::ALL {
        panel.spawn(label(
            format!(
                "{}  ·  {:+} standing",
                department.label(),
                shift.standing(department)
            ),
            15.0,
            TEXT,
        ));
        panel.spawn(label(department.blurb(), 12.0, TEXT_DIM));

        let kinds: Vec<RequisitionKind> = RequisitionKind::ALL
            .into_iter()
            .filter(|kind| kind.department() == department)
            .collect();
        if kinds.is_empty() {
            continue;
        }
        panel.spawn(wrap_row()).with_children(|row| {
            for &kind in &kinds {
                let available = can_afford(shift.standing(department), kind);
                let caption = format!("{} ({})", kind.label(), kind.cost());
                let mut entity = row.spawn(button(caption, PanelAction::Requisition(kind)));
                // Drawn dead rather than drawn live and silently doing
                // nothing — a button that looks clickable but is refused
                // reads as the game being broken.
                if !available {
                    entity.insert(BackgroundColor(Color::srgb(0.11, 0.12, 0.14)));
                }
            }
        });
        for kind in kinds {
            panel.spawn(label(
                format!("  {} — {}", kind.label(), kind.blurb()),
                12.0,
                TEXT_DIM,
            ));
        }
    }
}

fn dispenser_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
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

    // One upgrade purchase unlocks a whole tier at once, replacing the old
    // per-reagent unlock buttons below. Same "drawn dead rather than drawn
    // live and silently doing nothing" rule `standing_board_body` already
    // uses for an unaffordable requisition — clicking it either way sends
    // the same request, and `Knowledge::upgrade_dispenser` re-checks
    // affordability server-side, so a dead button is just inert, never wrong.
    if let Some(cost) = knowledge.next_upgrade_cost() {
        let affordable = knowledge.research_points >= cost;
        let caption = format!(
            "Upgrade dispenser to tier {}  ({cost} research)",
            knowledge.dispenser_tier() + 1
        );
        let mut entity = panel.spawn(button(caption, PanelAction::UpgradeDispenser));
        if !affordable {
            entity.insert(BackgroundColor(Color::srgb(0.11, 0.12, 0.14)));
        }
    }

    // Grouped by tier rather than the old flat, unsorted list — locked and
    // unlocked reagents no longer interleave, and each tier reads as one
    // step of the dispenser's own progression rather than 17 separate
    // purchases.
    let mut reagents: Vec<&chem_sim::Reagent> = db.reagents.dispensable().collect();
    reagents.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.name.cmp(&b.name)));

    let mut index = 0;
    while index < reagents.len() {
        let tier = reagents[index].tier;
        let end = reagents[index..].partition_point(|r| r.tier == tier) + index;
        let unlocked = knowledge.dispenser_tier() >= tier;

        panel.spawn(label(
            if tier == 0 {
                "Starting".to_string()
            } else {
                format!("Tier {tier}")
            },
            12.0,
            TEXT_DIM,
        ));
        // Only the very next tier previews its reagents by name — a chemist
        // can see what the *next* upgrade buys, but anything further out
        // stays a mystery until they've climbed that far, so the tier list
        // reads as a roadmap rather than a spoiler.
        let revealed = tier <= knowledge.dispenser_tier() + 1;
        panel.spawn(wrap_row()).with_children(|row| {
            for reagent in &reagents[index..end] {
                if unlocked {
                    row.spawn(button(reagent.name.clone(), PanelAction::Dispense(reagent.id)));
                } else {
                    // Not yet unlocked at this dispenser tier: shown so a
                    // chemist can see what's coming, but inert — upgrading
                    // is the only way to reach it, not a per-reagent buy.
                    let caption = if revealed { reagent.name.clone() } else { "???".to_string() };
                    let mut entity = row.spawn(button(caption, PanelAction::Dispense(reagent.id)));
                    entity.insert(BackgroundColor(Color::srgb(0.11, 0.12, 0.14)));
                }
            }
        });
        index = end;
    }

    container_readout(panel, db, loaded, true);
}

/// The reaction chamber: a dial, a switch, and a thermometer.
///
/// Deliberately spare. The machine does nothing on its own — everything
/// interesting happens because the chemistry noticed the temperature changed —
/// so the panel's whole job is telling you where you are and where you are
/// headed.
fn heater_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    thermostat: Option<&Thermostat>,
    loaded: Option<&Container>,
) {
    let thermostat = thermostat.copied().unwrap_or_default();

    panel.spawn(label(
        "Heats and cools whatever is loaded. Some reactions will not start until \
         it is hot enough; some come apart if it gets hotter still.",
        13.0,
        TEXT_DIM,
    ));

    let current = loaded.map(|container| container.solution.temperature);
    panel.spawn(label(
        match current {
            Some(temperature) => format!(
                "Sample  {temperature}          Target  {}          {}",
                thermostat.target,
                if thermostat.powered { "RUNNING" } else { "OFF" }
            ),
            None => "No container loaded. Carry a beaker over and press E.".to_string(),
        },
        15.0,
        // Warm as it climbs, so a chamber running away is visible without
        // reading the number.
        match current {
            Some(t) if t.0 >= 420.0 => Color::srgb(0.98, 0.45, 0.30),
            Some(t) if t.0 >= 350.0 => Color::srgb(0.95, 0.78, 0.40),
            _ => TEXT,
        },
    ));

    panel.spawn(label("Target temperature", 13.0, TEXT_DIM));
    panel.spawn(row()).with_children(|row| {
        for kelvin in TEMPERATURE_PRESETS {
            let target = Kelvin(kelvin);
            let mut entity = row.spawn(button(
                format!("{kelvin:.0}K"),
                PanelAction::SetTarget(target),
            ));
            if (thermostat.target.0 - kelvin).abs() < 0.5 {
                entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
            }
        }
    });

    panel.spawn(row()).with_children(|row| {
        let mut entity = row.spawn(button(
            if thermostat.powered {
                "Stop"
            } else {
                "Start heating"
            },
            PanelAction::TogglePower,
        ));
        if thermostat.powered {
            entity.insert(BackgroundColor(BUTTON_ACTIVE));
        }
        row.spawn(button("Eject container", PanelAction::Eject));
    });

    container_readout(panel, db, loaded, false);
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
                    row.spawn(button("▸All", PanelAction::ToBuffer(reagent, quantity)));
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
                    row.spawn(button("◂All", PanelAction::ToContainer(reagent, quantity)));
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
        // The only source of syringes in the lab. Cargo does not stock them,
        // so a chemist who wants one makes it.
        row.spawn(button(
            format!("Syringe  ({})", ContainerKind::Syringe.capacity()),
            PanelAction::Package(ContainerKind::Syringe),
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
            "Empty. Put a finished batch in and it will go out on its own.".to_string(),
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
///
/// Laid out as a sidebar of headings beside a scrolling pane. The single
/// column this replaced was written when there were nine recipes; there are
/// thirty-five now, and a chemist hunting for a burn treatment should not have
/// to scroll past the explosives to find it.
fn spawn_reference_book(
    commands: &mut Commands,
    db: &ChemDb,
    knowledge: &Knowledge,
    selected: Option<Category>,
    open_recipe: Option<ReactionId>,
) {
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
                        width: px(1040),
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
                            "{} of {} recipes recorded   ·   {} research   ·   \
                             scroll to read   ·   B or Esc to close",
                            knowledge.known_count(),
                            db.reactions.len(),
                            knowledge.research_points
                        ),
                        13.0,
                        TEXT_DIM,
                    ));

                    book.spawn(Node {
                        column_gap: px(16),
                        align_items: AlignItems::Start,
                        ..default()
                    })
                    .with_children(|columns| match open_recipe {
                        Some(id) => spawn_recipe_tree(columns, db, knowledge, db.reactions.get(id)),
                        None => {
                            book_sidebar(columns, db, knowledge, selected);
                            book_entries(columns, db, knowledge, selected);
                        }
                    });
                });
        });
}

/// The column of headings, each with how much of it the chemist has recorded.
fn book_sidebar(
    columns: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    selected: Option<Category>,
) {
    columns
        .spawn(Node {
            width: px(240),
            flex_direction: FlexDirection::Column,
            row_gap: px(4),
            flex_shrink: 0.0,
            ..default()
        })
        .with_children(|sidebar| {
            // "All" first, and it is what a fresh book opens on.
            let tabs = std::iter::once(None).chain(Category::ALL.map(Some));
            for tab in tabs {
                let (known, total) = category_counts(db, knowledge, tab);
                let name = match tab {
                    Some(category) => category.label(),
                    None => "All recipes",
                };
                let mut entity = sidebar.spawn(button(
                    format!("{name}   {known}/{total}"),
                    PanelAction::ShowCategory(tab),
                ));
                // Same marker the dispense-amount row uses, so `button_feedback`
                // colours the open tab with no extra code.
                if tab == selected {
                    entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
                }
            }
        });
}

/// The scrolling pane of entries for whichever heading is open.
fn book_entries(
    columns: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    selected: Option<Category>,
) {
    let heading_text = selected.map(|category| (category.label(), category.blurb()));
    let visible = recipes_in(db, knowledge, selected);

    columns
        .spawn((
            Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                row_gap: px(8),
                max_height: vh(66),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            BookScroll,
        ))
        .with_children(|pane| {
            if let Some((name, blurb)) = heading_text {
                pane.spawn(label(name.to_string(), 15.0, TEXT));
                pane.spawn(label(blurb.to_string(), 13.0, TEXT_DIM));
            }

            if visible.is_empty() {
                pane.spawn(label(
                    "Nothing under this heading yet.".to_string(),
                    13.0,
                    TEXT_DIM,
                ));
                return;
            }

            for reaction in visible {
                book_entry(pane, db, knowledge, reaction);
            }
        });
}

/// One recipe, as a compact clickable row. Tap it to open the full formula
/// tree — that is where the method, hints and "Study further" purchase now
/// live, so this only needs enough to help a chemist recognise what they're
/// looking for.
fn book_entry(
    pane: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    reaction: &chem_sim::Reaction,
) {
    let known = knowledge.is_known(reaction.id);
    let title = product_name(db, reaction.id);
    let product = reaction
        .products
        .first()
        .map(|(id, _)| db.reagents.get(*id));

    pane.spawn((Button, section(), PanelAction::OpenRecipe(reaction.id)))
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

            entry.spawn(label(
                if known {
                    "Tap to see the formula.".to_string()
                } else {
                    format!("{} ingredients — tap to research.", reaction.reactants.len())
                },
                12.0,
                TEXT_DIM,
            ));
        });
}

/// How many levels of "what feeds this" the tree draws before giving up and
/// saying so. Current data's deepest chain (arithrazine ← hyronalin ←
/// dylovene) is 2; this leaves generous headroom without being unbounded.
const MAX_TREE_DEPTH: usize = 8;
/// Horizontal shift per nesting level.
const TREE_INDENT: f32 = 22.0;

/// The formula screen for one recipe: itself, then — only while known, so a
/// locked step's own ingredients stay the same spoiler the hint system
/// already withholds everywhere else — everything that feeds it, one level
/// deeper per step back through the chain.
fn spawn_recipe_tree(
    columns: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    root: &chem_sim::Reaction,
) {
    columns
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            row_gap: px(8),
            ..default()
        })
        .with_children(|screen| {
            screen.spawn(row()).with_children(|row| {
                row.spawn(button("‹ Back to list", PanelAction::CloseRecipe));
            });
            screen
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: px(6),
                        max_height: vh(60),
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                    ScrollPosition::default(),
                    BookScroll,
                ))
                .with_children(|pane| {
                    let mut visited = HashSet::new();
                    render_recipe_node(pane, db, knowledge, root, 0, &mut visited);
                });
        });
}

/// One node in the formula tree: itself, then its ingredients one level
/// deeper if it's known.
///
/// `visited` tracks reactions open on the *current branch* only (inserted on
/// entry, removed before returning) — not the whole tree, since two branches
/// legitimately sharing an ingredient (bicaridine and tricordrazine both need
/// dylovene) must both still render it. Nothing in `chem_sim`'s types rules
/// out an actual cycle, so this must not panic if one appears; it prints a
/// line and stops instead.
fn render_recipe_node(
    pane: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    reaction: &chem_sim::Reaction,
    depth: usize,
    visited: &mut HashSet<ReactionId>,
) {
    if depth > MAX_TREE_DEPTH {
        pane.spawn(label("…chain too deep to show.", 12.0, TEXT_DIM));
        return;
    }
    if !visited.insert(reaction.id) {
        pane.spawn(label("…already shown further up this branch.", 12.0, TEXT_DIM));
        return;
    }

    let known = knowledge.is_known(reaction.id);
    let title = product_name(db, reaction.id);
    let product = reaction
        .products
        .first()
        .map(|(id, _)| db.reagents.get(*id));

    let mut node = section();
    node.margin.left = px(depth as f32 * TREE_INDENT);
    node.border = UiRect::left(px(2.0));

    pane.spawn((node, BackgroundColor(SECTION_BG), BorderColor::from(TEXT_DIM)))
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

            if depth == 0 {
                if let Some(treats) = product.and_then(|p| p.treats.as_ref()) {
                    entry.spawn(label(treats.clone(), 13.0, TEXT_DIM));
                }
            }

            if known {
                entry.spawn(label(recipe_line(db, reaction), 14.0, TEXT));
                if let Some(overdose) = product.and_then(|p| p.overdose) {
                    entry.spawn(label(
                        format!("Overdoses above {overdose} in a single dose."),
                        13.0,
                        Color::srgb(0.90, 0.62, 0.45),
                    ));
                }
                return;
            }

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
            // Only offer the purchase when it can actually go through; a button
            // that silently does nothing is worse than none.
            if knowledge.hint_available(db, reaction.id) {
                let affordable = knowledge.research_points >= HINT_COST;
                entry.spawn(row()).with_children(|row| {
                    if affordable {
                        row.spawn(button(
                            format!("Study further  ({HINT_COST} research)"),
                            PanelAction::BuyHint(reaction.id),
                        ));
                    } else {
                        row.spawn(label(
                            format!("Needs {HINT_COST} research to study further."),
                            12.0,
                            TEXT_DIM,
                        ));
                    }
                });
            }
        });

    // A locked node's own ingredients stay hidden — the same spoiler
    // discipline the hint system already enforces everywhere else.
    if known {
        for &(reagent_id, amount) in &reaction.reactants {
            render_ingredient_node(pane, db, knowledge, reagent_id, amount, false, depth + 1, visited);
        }
        for &(reagent_id, amount) in &reaction.catalysts {
            render_ingredient_node(pane, db, knowledge, reagent_id, amount, true, depth + 1, visited);
        }
    }

    visited.remove(&reaction.id);
}

/// One ingredient row under a known node: recurses one level deeper if
/// something produces it, otherwise a leaf naming the raw reagent and,
/// read-only, whether it's still locked at the Dispenser — unlocking still
/// only happens there, so this is a note, not a button.
#[allow(clippy::too_many_arguments)]
fn render_ingredient_node(
    pane: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    reagent: ReagentId,
    amount: Units,
    catalyst: bool,
    depth: usize,
    visited: &mut HashSet<ReactionId>,
) {
    if let Some(producer) = db.reactions.producer_of(reagent) {
        if catalyst {
            let mut note = section();
            note.margin.left = px(depth as f32 * TREE_INDENT);
            pane.spawn(note).with_children(|entry| {
                entry.spawn(label("catalyst, not consumed:", 11.0, TEXT_DIM));
            });
        }
        render_recipe_node(pane, db, knowledge, producer, depth, visited);
        return;
    }

    let definition = db.reagents.get(reagent);
    let mut line = format!("{amount} {}", definition.name);
    if catalyst {
        line.push_str("   (catalyst, not consumed)");
    }

    let mut node = section();
    node.margin.left = px(depth as f32 * TREE_INDENT);
    node.border = UiRect::left(px(2.0));

    pane.spawn((node, BackgroundColor(SECTION_BG), BorderColor::from(TEXT_DIM)))
        .with_children(|entry| {
            entry.spawn(label(line, 14.0, TEXT_DIM));
            if definition.dispensable && !knowledge.is_reagent_unlocked(db, reagent) {
                entry.spawn(label(
                    format!("locked at dispenser  (tier {})", definition.tier),
                    12.0,
                    Color::srgb(0.80, 0.60, 0.45),
                ));
            }
        });
}

/// The recipes filed under a heading, or every recipe when none is chosen.
///
/// Recorded ones come first, then alphabetically: what you can actually make
/// belongs at the top of the page, and within that the list has to be somewhere
/// findable rather than in data-file order.
fn recipes_in<'a>(
    db: &'a ChemDb,
    knowledge: &Knowledge,
    category: Option<Category>,
) -> Vec<&'a chem_sim::Reaction> {
    let mut recipes: Vec<&chem_sim::Reaction> = db
        .reactions
        .iter()
        .filter(|reaction| match category {
            Some(category) => reaction_categories(db, reaction.id).contains(&category),
            None => true,
        })
        .collect();
    recipes.sort_by_key(|reaction| {
        (
            !knowledge.is_known(reaction.id),
            crate::knowledge::product_name(db, reaction.id),
        )
    });
    recipes
}

/// How much of a heading the chemist has written up, for the sidebar.
fn category_counts(
    db: &ChemDb,
    knowledge: &Knowledge,
    category: Option<Category>,
) -> (usize, usize) {
    let recipes = recipes_in(db, knowledge, category);
    let known = recipes
        .iter()
        .filter(|reaction| knowledge.is_known(reaction.id))
        .count();
    (known, recipes.len())
}

/// The scrolling entry list inside the reference book.
#[derive(Component)]
struct BookScroll;

/// Mouse wheel scrolls the book.
///
/// Written straight onto the one scrollable node rather than through Bevy's
/// pointer-hover scroll events: the cursor is grabbed and invisible while the
/// book is open, so there is no hover target to route through, and the book is
/// the only thing on screen that can scroll.
fn scroll_reference_book(
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    mut book: Query<(&mut ScrollPosition, &ComputedNode), With<BookScroll>>,
) {
    let scrolled: f32 = wheel
        .read()
        .map(|event| match event.unit {
            bevy::input::mouse::MouseScrollUnit::Line => event.y * 24.0,
            bevy::input::mouse::MouseScrollUnit::Pixel => event.y,
        })
        .sum();
    if scrolled == 0.0 {
        return;
    }

    for (mut position, computed) in &mut book {
        // Content taller than the box is exactly how far it can travel.
        let limit = (computed.content_size().y - computed.size().y).max(0.0);
        position.y = (position.y - scrolled).clamp(0.0, limit);
    }
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

    let mut line = format!(
        "{}  →  {}",
        part(&reaction.reactants),
        part(&reaction.products)
    );
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

/// The one line that tells the player whether the lab is taking requests.
///
/// Pure so both wordings can be checked at once. There is no shift or phase
/// to report on any more — just the sign the player themselves controls at
/// the standing board.
fn accepting_banner_line(shift: &Shift) -> &'static str {
    if shift.accepting_orders {
        "OPEN — crew are coming in"
    } else {
        "CLOSED — not accepting requests"
    }
}

fn update_phase_banner(shift: Res<Shift>, banner: BannerText) {
    let line = accepting_banner_line(&shift);
    let mut banner = banner.into_inner();
    if banner.0 != line {
        banner.0 = line.to_string();
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
    pending.sort_by(|a, b| a.1.remaining().total_cmp(&b.1.remaining()));

    for (slot, mut text, mut color) in &mut slots {
        let line = pending.get(slot.0).map(|(member, order, route)| {
            // `order.specific` is freely queryable (nothing secret about it —
            // see its own doc comment) and always wins when set. Otherwise
            // this never queries `Has<IllicitOrder>` — see that marker's own
            // doc comment — and instead reads purely from the reagent's own
            // category: a lenient legitimate order's category is always one
            // of the six orderable ones, so it shows a want-phrase; anything
            // else (every antagonist reagent is `Illicit`) falls back to the
            // real name, which is no more a tell than the pretext already is.
            let want = if order.specific {
                db.reagents.get(order.reagent).name.clone()
            } else {
                reference_category(&db, order.reagent)
                    .filter(|cat| cat.is_legitimately_orderable())
                    .map(|cat| cat.want_phrase().to_string())
                    .unwrap_or_else(|| db.reagents.get(order.reagent).name.clone())
            };
            let reagent = &want;
            match route.phase {
                CrewPhase::Arriving => {
                    format!(
                        "{}\n  {} {}  ·  on the way",
                        member.name, order.amount, reagent
                    )
                }
                _ => {
                    let remaining = order.remaining() as u32;
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
            .map(|(_, order, _)| order.remaining() < 30.0)
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

    let summary = format!("delivered {}   botched {}", shift.succeeded, shift.botched);
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

/// Toasts when the "accepting requests" sign flips.
///
/// Watches `Shift` from `Update` rather than the toggle message itself,
/// because this runs on both ends — a joined chemist needs to notice their
/// partner flipping the sign too, and only the replicated resource reaches
/// them, not the client message that caused it.
fn announce_accepting_toggle(
    shift: Res<Shift>,
    mut announced: Local<Option<bool>>,
    mut toasts: MessageWriter<ShowToast>,
) {
    if *announced == Some(shift.accepting_orders) {
        return;
    }
    // Don't toast the very first frame — that would just announce "open" the
    // instant every session starts.
    let first_run = announced.is_none();
    *announced = Some(shift.accepting_orders);
    if first_run {
        return;
    }

    let (kicker, subtitle, background) = if shift.accepting_orders {
        (
            "OPEN",
            "Taking requests again.".to_string(),
            Color::srgba(0.10, 0.20, 0.13, 0.94),
        )
    } else {
        (
            "CLOSED",
            "Not accepting requests for a while.".to_string(),
            Color::srgba(0.18, 0.15, 0.09, 0.94),
        )
    };

    toasts.write(ShowToast {
        kicker,
        title: "Chemistry".to_string(),
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
// Vitals
// ---------------------------------------------------------------------------

/// Reagents listed in the bloodstream readout before it starts summarising.
const BLOOD_SLOTS: usize = 6;

/// Every piece of text in the vitals panel, tagged by what it says.
///
/// One marker with variants rather than five separate marker components: the
/// order queue needed a `Without<>` chain per marker to keep its queries
/// disjoint, and that grows quadratically. This panel writes all of its text
/// through a single query.
#[derive(Component, PartialEq)]
enum VitalsText {
    Damage(DamageKind),
    Blood(usize),
    Status,
    Collapse,
}

/// The coloured fill inside a damage bar.
#[derive(Component)]
struct DamageBar(DamageKind);

/// A damage type's colour is the colour of the medicine that treats it, taken
/// straight from `chem.reagents.ron`.
///
/// Brute is bicaridine red, burn is dermaline orange, toxin is dylovene green,
/// oxygen is dexalin blue. A player who has made those four learns the mapping
/// from the bars without a word of tutorial text.
fn damage_color(kind: DamageKind) -> Color {
    match kind {
        DamageKind::Brute => Color::srgb(0.85, 0.25, 0.25),
        DamageKind::Burn => Color::srgb(0.95, 0.62, 0.20),
        DamageKind::Toxin => Color::srgb(0.42, 0.70, 0.36),
        DamageKind::Oxygen => Color::srgb(0.40, 0.65, 0.95),
    }
}

/// Fixed slots, filled in each frame — the same pattern as the order queue and
/// for the same reason. Four bars that decay every tick would otherwise force a
/// full despawn-and-rebuild of the panel every frame.
fn spawn_vitals_panel(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: px(16),
                right: px(16),
                width: px(280),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(5),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.07, 0.09, 0.82)),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(Color::srgb(0.95, 0.45, 0.45)),
                VitalsText::Collapse,
            ));
            panel.spawn(label("CONDITION", 12.0, TEXT_DIM));

            for kind in DamageKind::ALL {
                panel.spawn(row()).with_children(|line| {
                    line.spawn((
                        Text::new(kind.label()),
                        TextFont::from_font_size(12.0),
                        TextColor(damage_color(kind)),
                        Node {
                            min_width: px(46),
                            ..default()
                        },
                    ));
                    // Track, with the fill as a child. The fill's width is the
                    // only thing the update touches.
                    line.spawn((
                        Node {
                            width: px(150),
                            height: px(9),
                            border_radius: BorderRadius::all(px(4)),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.16, 0.17, 0.20, 0.9)),
                        children![(
                            Node {
                                width: percent(0),
                                height: percent(100),
                                border_radius: BorderRadius::all(px(4)),
                                ..default()
                            },
                            BackgroundColor(damage_color(kind)),
                            DamageBar(kind),
                        )],
                    ));
                    line.spawn((
                        Text::new(""),
                        TextFont::from_font_size(12.0),
                        TextColor(TEXT_DIM),
                        VitalsText::Damage(kind),
                    ));
                });
            }

            panel.spawn((
                Text::new(""),
                TextFont::from_font_size(12.0),
                TextColor(Color::srgb(0.80, 0.72, 0.95)),
                VitalsText::Status,
            ));
            panel.spawn(label("BLOODSTREAM", 12.0, TEXT_DIM));
            for index in 0..BLOOD_SLOTS {
                panel.spawn((
                    Text::new(""),
                    TextFont::from_font_size(13.0),
                    TextColor(TEXT),
                    VitalsText::Blood(index),
                ));
            }
        });
}

/// One bloodstream row: what it is, how much is left, and whether it is past
/// its overdose threshold.
///
/// Pure so the overdose wording can be tested without a running app.
fn blood_line(db: &ChemDb, reagent: ReagentId, quantity: Units) -> (String, Color) {
    let definition = db.reagents.get(reagent);
    let overdosing = matches!(definition.overdose, Some(threshold) if quantity > threshold);
    let line = if overdosing {
        format!("{:<15}{:>7}  OD", definition.name, quantity.to_string())
    } else {
        format!("{:<15}{:>7}", definition.name, quantity.to_string())
    };

    let [r, g, b] = definition.color;
    let color = if overdosing {
        // The book's own warning colour, so an overdose reads the same
        // everywhere it appears.
        Color::srgb(0.90, 0.62, 0.45)
    } else {
        Color::srgb(0.45 + r * 0.55, 0.45 + g * 0.55, 0.45 + b * 0.55)
    };
    (line, color)
}

fn update_vitals_panel(
    db: Res<ChemDb>,
    me: Query<(&Body, &Bloodstream), With<LocalPlayer>>,
    mut bars: Query<(&DamageBar, &mut Node)>,
    mut texts: Query<(&VitalsText, &mut Text, &mut TextColor)>,
) {
    let Ok((body, blood)) = me.single() else {
        return;
    };
    let contents = blood.0.contents();

    for (bar, mut node) in &mut bars {
        let wanted = percent(body.0.fraction(bar.0) * 100.0);
        if node.width != wanted {
            node.width = wanted;
        }
    }

    let statuses: Vec<&str> = blood
        .0
        .active_statuses()
        .map(|(kind, _)| kind.label())
        .collect();

    for (slot, mut text, mut color) in &mut texts {
        let (line, wanted) = match slot {
            VitalsText::Damage(kind) => {
                let amount = body.0.damage.get(*kind);
                let line = if amount.is_positive() {
                    format!("{amount}")
                } else {
                    String::new()
                };
                (line, TEXT_DIM)
            }
            VitalsText::Blood(index) => match contents.get(*index) {
                // The last slot summarises the overflow rather than silently
                // hiding it — a chemist needs to know there is more in them
                // than the panel has room for.
                Some(_) if *index == BLOOD_SLOTS - 1 && contents.len() > BLOOD_SLOTS => (
                    format!("+{} more", contents.len() - (BLOOD_SLOTS - 1)),
                    TEXT_DIM,
                ),
                Some((reagent, quantity)) => blood_line(&db, *reagent, *quantity),
                None => (String::new(), TEXT),
            },
            VitalsText::Status => {
                let line = if statuses.is_empty() {
                    String::new()
                } else {
                    statuses.join(" · ")
                };
                (line, Color::srgb(0.80, 0.72, 0.95))
            }
            VitalsText::Collapse => {
                let line = if body.0.collapsed {
                    "COLLAPSED".to_string()
                } else {
                    String::new()
                };
                (line, Color::srgb(0.95, 0.45, 0.45))
            }
        };

        if text.0 != line {
            text.0 = line;
        }
        if color.0 != wanted {
            color.0 = wanted;
        }
    }
}

// ---------------------------------------------------------------------------
// Radio feed
// ---------------------------------------------------------------------------

/// Which room the chemist is standing in.
///
/// Worth a line of screen for the same reason the rooms are tinted: the lab is
/// five rooms now, and a player who has walked through two doorways looking for
/// the grinder should not have to work out where they ended up from the wall
/// colour. Top-left is the one free corner — orders sit top-right, the radio
/// bottom-left and vitals bottom-right.
#[derive(Component)]
struct RoomLabel;

fn spawn_room_label(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: px(16),
                left: px(16),
                padding: UiRect::axes(px(10), px(6)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.05, 0.06, 0.08, 0.70)),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(TEXT_DIM),
                RoomLabel,
            ));
        });
}

fn update_room_label(
    chemists: Query<&Transform, With<LocalPlayer>>,
    mut labels: Query<&mut Text, With<RoomLabel>>,
) {
    let Ok(chemist) = chemists.single() else {
        return;
    };
    // Doorways fall outside every room rectangle, so mid-stride there is no
    // room to name. Holding the last one beats blinking the label off and on
    // every time the player crosses a threshold.
    let Some(room) = crate::lab::room_at(chemist.translation) else {
        return;
    };
    for mut text in &mut labels {
        if text.0 != room.name {
            text.0 = room.name.to_string();
        }
    }
}

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

pub(crate) fn heading(text: impl Into<String>) -> impl Bundle {
    (
        Text::new(text.into()),
        TextFont::from_font_size(22.0),
        TextColor(TEXT),
    )
}

pub(crate) fn label(text: impl Into<String>, size: f32, color: Color) -> impl Bundle {
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
        Text::new(format!(
            "{:<16} {:>8}",
            definition.name,
            quantity.to_string()
        )),
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

pub(crate) fn row() -> impl Bundle {
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

/// Generic over the action so the menu's buttons look like the lab's without
/// the two sharing an action enum — a panel button dispenses a reagent, a menu
/// button opens a save, and neither wants the other's variants.
pub(crate) fn button<A: Component>(text: impl Into<String>, action: A) -> impl Bundle {
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

pub(crate) type ChangedButtons<'w, 's> = Query<
    'w,
    's,
    (
        &'static Interaction,
        &'static mut BackgroundColor,
        Has<Selected>,
    ),
    (Changed<Interaction>, With<Button>),
>;

pub(crate) fn button_feedback(mut buttons: ChangedButtons) {
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
/// Every message a panel button can send.
///
/// Bundled into one `SystemParam` because Bevy caps a system at sixteen
/// parameters and the panel is the one place that can emit all of them. Adding
/// a machine means adding a writer here, not widening the system signature.
#[derive(SystemParam)]
struct PanelMessages<'w> {
    dispense: MessageWriter<'w, DispenseRequested>,
    eject: MessageWriter<'w, EjectRequested>,
    empty: MessageWriter<'w, EmptyRequested>,
    transfer: MessageWriter<'w, BufferTransferRequested>,
    package: MessageWriter<'w, PackageRequested>,
    analyze: MessageWriter<'w, AnalyzeRequested>,
    grind: MessageWriter<'w, GrindRequested>,
    set_target: MessageWriter<'w, SetTargetTemperature>,
    set_power: MessageWriter<'w, SetHeaterPower>,
    toggle_accepting: MessageWriter<'w, ToggleAcceptingOrders>,
    requisition: MessageWriter<'w, RequisitionRequested>,
    leave_machine: MessageWriter<'w, LeaveMachineRequested>,
    upgrade_dispenser: MessageWriter<'w, UpgradeDispenserRequested>,
    buy_hint: MessageWriter<'w, BuyHintRequested>,
}

#[allow(clippy::too_many_arguments)]
fn handle_panel_clicks(
    buttons: Query<(&Interaction, &PanelAction), Changed<Interaction>>,
    mut modes: Query<(Entity, &mut InteractionMode), With<LocalPlayer>>,
    mut machines: Query<&mut Machine>,
    mut amounts: Query<&mut DispenseAmount>,
    mut out: PanelMessages,
    thermostats: Query<&Thermostat>,
    // No `ResMut<Knowledge>`/`Res<ChemDb>` here any more: buying a hint was the
    // only thing that needed them, and it now goes through the authority like
    // every other career-wide purchase. Holding a `ResMut` every frame for one
    // rare branch was also an exclusive-access constraint against every system
    // that merely reads the notebook.
    mut book: ResMut<BookView>,
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
                out.buy_hint.write(BuyHintRequested {
                    reaction: *reaction,
                });
                continue;
            }
            PanelAction::UpgradeDispenser => {
                out.upgrade_dispenser.write(UpgradeDispenserRequested);
                continue;
            }
            PanelAction::ShowCategory(category) => {
                book.category = *category;
                continue;
            }
            PanelAction::OpenRecipe(reaction) => {
                book.open_recipe = Some(*reaction);
                continue;
            }
            PanelAction::CloseRecipe => {
                book.open_recipe = None;
                continue;
            }
            PanelAction::Close => {
                leave_machine(player, &mut mode, &mut machines, &mut out.leave_machine);
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
                out.dispense.write(DispenseRequested {
                    machine,
                    reagent: *reagent,
                });
            }
            PanelAction::Eject => {
                out.eject.write(EjectRequested { machine });
            }
            PanelAction::Empty => {
                out.empty.write(EmptyRequested { machine });
            }
            PanelAction::ToBuffer(reagent, amount) => {
                out.transfer.write(BufferTransferRequested {
                    machine,
                    reagent: *reagent,
                    amount: *amount,
                    direction: BufferDirection::ToBuffer,
                });
            }
            PanelAction::ToContainer(reagent, amount) => {
                out.transfer.write(BufferTransferRequested {
                    machine,
                    reagent: *reagent,
                    amount: *amount,
                    direction: BufferDirection::ToContainer,
                });
            }
            PanelAction::Package(kind) => {
                out.package.write(PackageRequested {
                    machine,
                    kind: *kind,
                });
            }
            PanelAction::Analyze => {
                out.analyze.write(AnalyzeRequested { machine });
            }
            PanelAction::Grind { all } => {
                out.grind.write(GrindRequested { machine, all: *all });
            }
            // Requests, not writes: the server owns the temperature, and a
            // client that set it locally would be corrected a frame later.
            PanelAction::SetTarget(target) => {
                out.set_target.write(SetTargetTemperature {
                    machine,
                    target: *target,
                });
            }
            PanelAction::TogglePower => {
                let on = thermostats
                    .get(machine)
                    .is_ok_and(|thermostat| !thermostat.powered);
                out.set_power.write(SetHeaterPower { machine, on });
            }
            // The board's two. Requests, not writes: the server owns the
            // standing and the sign, and a client that moved either locally
            // would be corrected out from under the player a frame later.
            PanelAction::ToggleAcceptingOrders => {
                out.toggle_accepting
                    .write(ToggleAcceptingOrders { board: machine });
            }
            PanelAction::Requisition(kind) => {
                out.requisition.write(RequisitionRequested {
                    board: machine,
                    kind: *kind,
                });
            }
            // Handled above, before the machine guard.
            PanelAction::BuyHint(_)
            | PanelAction::UpgradeDispenser
            | PanelAction::ShowCategory(_)
            | PanelAction::OpenRecipe(_)
            | PanelAction::CloseRecipe
            | PanelAction::Close => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_reads_the_accepting_sign() {
        let mut shift = Shift::default();
        assert!(accepting_banner_line(&shift).contains("OPEN"));

        shift.accepting_orders = false;
        assert!(accepting_banner_line(&shift).contains("CLOSED"));
    }

    // -- the reference book's grouping --------------------------------------

    fn book_fixture() -> (ChemDb, Knowledge) {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should load");
        let knowledge = Knowledge::new(&data);
        (ChemDb(data), knowledge)
    }

    #[test]
    fn every_recipe_is_reachable_from_some_tab() {
        // The "All" tab is a convenience, not the only way in. A recipe that
        // appears under no heading is one a player browsing by ailment can
        // never find, and nothing on screen would say so.
        let (db, knowledge) = book_fixture();
        let mut filed: Vec<&str> = Vec::new();
        for category in Category::ALL {
            for reaction in recipes_in(&db, &knowledge, Some(category)) {
                filed.push(&reaction.key);
            }
        }

        for reaction in db.reactions.iter() {
            assert!(
                filed.contains(&reaction.key.as_str()),
                "'{}' appears under no heading",
                reaction.key
            );
        }
    }

    #[test]
    fn a_tab_leads_with_what_the_chemist_can_already_make() {
        // Recorded first, then alphabetical. A fresh chemist knows kelotane and
        // not dermaline, so Burns must open on kelotane.
        let (db, knowledge) = book_fixture();
        let burns = recipes_in(&db, &knowledge, Some(Category::Burns));
        let keys: Vec<&str> = burns.iter().map(|r| r.key.as_str()).collect();

        assert_eq!(keys.first(), Some(&"kelotane"), "{keys:?}");
        assert!(keys.contains(&"dermaline"), "{keys:?}");
        // Tricordrazine treats all four types, so it is filed here as well as
        // under trauma — the count columns deliberately overlap.
        assert!(keys.contains(&"tricordrazine"), "{keys:?}");

        let (known, total) = category_counts(&db, &knowledge, Some(Category::Burns));
        assert_eq!(known, 1, "only kelotane is known at the start");
        assert_eq!(total, keys.len());
    }

    #[test]
    fn the_all_tab_counts_every_recipe_exactly_once() {
        // The header line reads "N of M recorded" off `Knowledge`, and the All
        // tab has to agree with it or one of the two is lying.
        let (db, knowledge) = book_fixture();
        let (known, total) = category_counts(&db, &knowledge, None);

        assert_eq!(total, db.reactions.len());
        assert_eq!(known, knowledge.known_count());
    }
}
