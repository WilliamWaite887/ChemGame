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
use crate::interaction::{leave_machine, InteractionMode};
use crate::machines::{
    slotted_container, Buffer, BufferDirection, BufferTransferRequested, DispenseAmount,
    DispenseRequested, EjectRequested, EmptyRequested, Machine, MachineKind, PackageRequested,
};
use crate::player::LocalPlayer;
use crate::AppState;

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
            Update,
            (handle_panel_clicks, button_feedback, sync_panel)
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
#[derive(PartialEq, Default)]
struct PanelSignature {
    machine: Option<Entity>,
    container: Option<Entity>,
    contents: Vec<(ReagentId, Units)>,
    buffer: Vec<(ReagentId, Units)>,
    amount: Option<Units>,
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
    mut previous: Local<PanelSignature>,
) {
    let open_machine = match modes.iter().next() {
        Some(InteractionMode::UsingMachine(machine)) => Some(*machine),
        _ => None,
    };
    let loaded_entity = open_machine.and_then(|machine| slotted_container(machine, &slotted));
    let loaded = loaded_entity.and_then(|entity| containers.get(entity).ok());
    let machine_parts = open_machine.and_then(|machine| machines.get(machine).ok());

    let signature = PanelSignature {
        machine: open_machine,
        container: loaded_entity,
        contents: loaded
            .map(|container| container.solution.iter().collect())
            .unwrap_or_default(),
        buffer: machine_parts
            .and_then(|(_, _, buffer)| buffer)
            .map(|buffer| buffer.0.iter().collect())
            .unwrap_or_default(),
        amount: machine_parts.and_then(|(_, amount, _)| amount).map(|a| a.0),
    };

    if signature == *previous {
        return;
    }
    *previous = signature;

    for panel in &existing {
        commands.entity(panel).despawn();
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
