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
use crate::machines::{
    slotted_container, Buffer, BufferDirection, BufferTransferRequested, DispenseAmount,
    DispenseRequested, EjectRequested, EmptyRequested, Machine, MachineKind, PackageRequested,
};
use crate::knowledge::Knowledge;
use crate::orders::{Order, Shift};
use crate::player::LocalPlayer;
use crate::radio::RadioLog;
use crate::AppState;

/// How many orders the queue can show at once. Matches `max_active` in
/// `station.orders.ron`.
const ORDER_SLOTS: usize = 3;
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
                update_order_queue,
                update_radio_feed,
            )
                .chain()
                .run_if(in_state(AppState::Playing)),
        );
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
    amount: Option<Units>,
    known_recipes: usize,
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
            amount: None,
            known_recipes: usize::MAX,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sync_panel(
    mut commands: Commands,
    db: Res<ChemDb>,
    existing: Query<Entity, With<PanelRoot>>,
    modes: Query<&InteractionMode, With<LocalPlayer>>,
    machines: Query<(&Machine, Option<&DispenseAmount>, Option<&Buffer>)>,
    slotted: Query<(Entity, &InSlot)>,
    containers: Query<&Container>,
    knowledge: Res<Knowledge>,
    mut previous: Local<PanelSignature>,
) {
    let mode = modes.iter().next().copied().unwrap_or_default();
    let open_machine = match mode {
        InteractionMode::UsingMachine(machine) => Some(machine),
        _ => None,
    };
    let loaded_entity = open_machine.and_then(|machine| slotted_container(machine, &slotted));
    let loaded = loaded_entity.and_then(|entity| containers.get(entity).ok());
    let machine_parts = open_machine.and_then(|machine| machines.get(machine).ok());

    let signature = PanelSignature {
        mode,
        container: loaded_entity,
        contents: loaded
            .map(|container| container.solution.iter().collect())
            .unwrap_or_default(),
        buffer: machine_parts
            .and_then(|(_, _, buffer)| buffer)
            .map(|buffer| buffer.0.iter().collect())
            .unwrap_or_default(),
        amount: machine_parts.and_then(|(_, amount, _)| amount).map(|a| a.0),
        known_recipes: knowledge.known_count(),
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
    let Some((machine, amount, buffer)) = machine_parts else {
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
                        other => {
                            panel.spawn(label(
                                format!("{} is not installed yet.", other.label()),
                                15.0,
                                TEXT_DIM,
                            ));
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
                            "{} of {} recipes recorded   ·   B or Esc to close",
                            knowledge.known_count(),
                            db.reactions.len()
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

// The disjointness filters are noisy inline; naming them keeps the queue
// system's signature readable.
type ShiftText<'w, 's> = Single<
    'w,
    's,
    &'static mut Text,
    (With<ShiftReadout>, Without<OrderSlot>, Without<PleaLine>),
>;
type PleaText<'w, 's> = Single<
    'w,
    's,
    &'static mut Text,
    (With<PleaLine>, Without<OrderSlot>, Without<ShiftReadout>),
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
) {
    let Some((player, mut mode)) = modes.iter_mut().next() else {
        return;
    };
    let InteractionMode::UsingMachine(machine) = *mode else {
        return;
    };

    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
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
            PanelAction::Close => {
                leave_machine(player, &mut mode, &mut machines);
                return;
            }
        }
    }
}
