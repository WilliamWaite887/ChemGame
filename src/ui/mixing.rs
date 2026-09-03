use super::*;
use bevy::input::{keyboard::KeyboardInput, ButtonState};

#[derive(Resource, Clone, PartialEq)]
pub(crate) struct PackagingDraft {
    pub machine: Option<Entity>,
    pub kind: ContainerKind,
    pub text: String,
    pub cursor: usize,
    pub editing: bool,
    pub nonce: u64,
}
impl Default for PackagingDraft {
    fn default() -> Self {
        Self {
            machine: None,
            kind: ContainerKind::Bottle,
            text: String::new(),
            cursor: 0,
            editing: false,
            nonce: 0,
        }
    }
}

pub(super) fn type_label(
    mut events: MessageReader<KeyboardInput>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    modes: Query<&InteractionMode, With<LocalPlayer>>,
    machines: Query<(&Machine, &Buffer)>,
    mut draft: ResMut<PackagingDraft>,
) {
    let machine = modes
        .iter()
        .find_map(|mode| mode.claimed_machine())
        .filter(|e| {
            machines
                .get(*e)
                .is_ok_and(|(m, _)| m.kind == MachineKind::MixingChamber)
        });
    if draft.machine != machine {
        let nonce = draft.nonce;
        *draft = PackagingDraft {
            machine,
            nonce,
            ..default()
        };
    }
    if machine
        .and_then(|e| machines.get(e).ok())
        .is_some_and(|(_, b)| b.0.is_empty())
        && !draft.editing
    {
        draft.text.clear();
        draft.cursor = 0;
    }
    if !draft.editing {
        events.clear();
        return;
    }
    for event in events.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        let cursor = draft.cursor.min(draft.text.len());
        match event.key_code {
            KeyCode::Escape | KeyCode::Enter | KeyCode::Tab => {
                draft.editing = false;
            }
            KeyCode::Home => draft.cursor = 0,
            KeyCode::End => draft.cursor = draft.text.len(),
            KeyCode::ArrowLeft | KeyCode::Backspace => {
                if let Some((before, _)) = draft.text[..cursor].char_indices().next_back() {
                    if event.key_code == KeyCode::Backspace {
                        draft.text.replace_range(before..cursor, "");
                    }
                    draft.cursor = before;
                }
            }
            KeyCode::ArrowRight | KeyCode::Delete => {
                if let Some(c) = draft.text[cursor..].chars().next() {
                    let end = cursor + c.len_utf8();
                    if event.key_code == KeyCode::Delete {
                        draft.text.replace_range(cursor..end, "");
                    } else {
                        draft.cursor = end;
                    }
                }
            }
            _ => {
                if let Some(text) = &event.text {
                    let room = crate::labels::MAX_LABEL.saturating_sub(draft.text.chars().count());
                    let text: String = text
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(room)
                        .collect();
                    draft.text.insert_str(cursor, &text);
                    draft.cursor = cursor + text.len();
                }
            }
        }
    }
    // Keyboard input belongs to this field, including Escape and rebound keys.
    keys.reset_all();
}

#[derive(Component)]
pub(super) struct ScrollArea(pub usize);

fn column(width: f32) -> Node {
    Node {
        width: percent(width),
        min_width: px(0),
        flex_direction: FlexDirection::Column,
        row_gap: px(5),
        ..default()
    }
}
fn list(height: f32) -> Node {
    Node {
        height: px(height),
        width: percent(100),
        flex_direction: FlexDirection::Column,
        row_gap: px(7),
        overflow: Overflow::scroll_y(),
        ..default()
    }
}

fn ingredients(
    parent: &mut ChildSpawnerCommands,
    db: &ChemDb,
    solution: Option<&chem_sim::Solution>,
    slot: MachineSlot,
    locked: bool,
    chamber: bool,
    scroll: f32,
) {
    parent
        .spawn((
            list(if chamber { 95.0 } else { 150.0 }),
            ScrollPosition(Vec2::new(0.0, scroll)),
            ScrollPane,
            ScrollArea(if chamber {
                2
            } else if slot == MachineSlot::A {
                0
            } else {
                1
            }),
        ))
        .with_children(|list| {
            let Some(solution) = solution else {
                list.spawn(label("No beaker loaded", 13.0, TEXT_DIM));
                return;
            };
            if solution.is_empty() {
                list.spawn(label("Empty", 13.0, TEXT_DIM));
            }
            for (reagent, amount) in solution.iter() {
                list.spawn(label(
                    format!(
                        "{}  {}  {:.0}%",
                        db.reagents.get(reagent).name,
                        amount,
                        solution.purity_of(reagent) * 100.0
                    ),
                    13.0,
                    TEXT,
                ));
                if !locked {
                    list.spawn(wrap_row()).with_children(|controls| {
                        if chamber {
                            controls.spawn(styled_button(
                                "To A",
                                PanelAction::ToContainer(reagent, amount, MachineSlot::A),
                                ButtonTone::Primary,
                            ));
                            controls.spawn(styled_button(
                                "To B",
                                PanelAction::ToContainer(reagent, amount, MachineSlot::B),
                                ButtonTone::Primary,
                            ));
                        } else {
                            for n in [5, 10] {
                                controls.spawn(styled_button(
                                    format!("{n}u"),
                                    PanelAction::ToBuffer(reagent, Units::whole(n), slot),
                                    ButtonTone::Primary,
                                ));
                            }
                            controls.spawn(styled_button(
                                "All",
                                PanelAction::ToBuffer(reagent, amount, slot),
                                ButtonTone::Primary,
                            ));
                        }
                    });
                }
            }
        });
}
fn readings(parent: &mut ChildSpawnerCommands, solution: Option<&chem_sim::Solution>) {
    if let Some(s) = solution {
        parent.spawn(label(
            format!("{} / {}", s.total_volume(), s.max_volume()),
            13.0,
            TEXT,
        ));
        parent.spawn(label(
            format!("pH {:.2}  |  {:.1}K", s.ph(), s.temperature.0),
            13.0,
            TEXT,
        ));
        parent.spawn(label(
            format!("Purity {:.1}%", s.average_purity() * 100.0),
            12.0,
            TEXT_DIM,
        ));
    }
}
fn beaker(
    parent: &mut ChildSpawnerCommands,
    title: &str,
    entity: Option<Entity>,
    loaded: Option<&Container>,
    slot: MachineSlot,
    locked: bool,
) {
    parent.spawn(column(21.0)).with_children(|p| {
        p.spawn(label(title, 15.0, TEXT));
        beaker_preview_sized(p, entity, 72.0, 88.0);
        readings(p, loaded.map(|c| &c.solution));
        if !locked && entity.is_some() {
            p.spawn(styled_button(
                "Eject",
                PanelAction::Eject(slot),
                ButtonTone::Utility,
            ));
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    machine: Entity,
    buffer: Option<&Buffer>,
    a: Option<Entity>,
    ca: Option<&Container>,
    b: Option<Entity>,
    cb: Option<&Container>,
    agitation: Option<&AgitationRun>,
    draft: &PackagingDraft,
    scroll: [f32; 5],
) {
    let locked = agitation.is_some();
    panel
        .spawn(Node {
            width: percent(100),
            column_gap: px(10),
            ..default()
        })
        .with_children(|top| {
            top.spawn(column(29.0)).with_children(|p| {
                p.spawn(label("A CONTENTS", 12.0, TEXT_DIM));
                ingredients(
                    p,
                    db,
                    ca.map(|c| &c.solution),
                    MachineSlot::A,
                    locked,
                    false,
                    scroll[0],
                );
            });
            beaker(top, "BEAKER A", a, ca, MachineSlot::A, locked);
            beaker(top, "BEAKER B", b, cb, MachineSlot::B, locked);
            top.spawn(column(29.0)).with_children(|p| {
                p.spawn(label("B CONTENTS", 12.0, TEXT_DIM));
                ingredients(
                    p,
                    db,
                    cb.map(|c| &c.solution),
                    MachineSlot::B,
                    locked,
                    false,
                    scroll[1],
                );
            });
        });
    let preparations = ca.zip(cb).filter(|(a, b)| {
        matches!(a.kind, ContainerKind::Beaker | ContainerKind::LargeBeaker)
            && matches!(b.kind, ContainerKind::Beaker | ContainerKind::LargeBeaker)
            && !chem_sim::is_reacting(&a.solution, &db.reactions)
            && !chem_sim::is_reacting(&b.solution, &db.reactions)
            && !db
                .reactions
                .activate_agitation(&a.solution, &b.solution)
                .is_empty()
    });
    panel.spawn(wrap_row()).with_children(|buttons| {
        for (direction, title) in [
            (AgitateDirection::AToB, "Agitate A -> B"),
            (AgitateDirection::BToA, "Agitate B -> A"),
            (AgitateDirection::ToMixer, "Agitate to Mixer"),
        ] {
            let available = !locked
                && preparations.is_some_and(|(a, b)| match direction {
                    AgitateDirection::AToB => {
                        a.solution.total_volume().is_positive()
                            && b.solution.available_volume() >= a.solution.total_volume()
                    }
                    AgitateDirection::BToA => {
                        b.solution.total_volume().is_positive()
                            && a.solution.available_volume() >= b.solution.total_volume()
                    }
                    AgitateDirection::ToMixer => buffer.is_some_and(|buffer| {
                        buffer.0.is_empty()
                            && buffer.0.available_volume()
                                >= a.solution.total_volume() + b.solution.total_volume()
                    }),
                });
            if available {
                buttons.spawn(styled_button(
                    title,
                    PanelAction::Agitate(direction),
                    ButtonTone::Primary,
                ));
            } else {
                buttons
                    .spawn((
                        Node {
                            padding: UiRect::all(px(9)),
                            ..default()
                        },
                        BackgroundColor(SECTION_BG),
                    ))
                    .with_children(|b| {
                        b.spawn(label(title, 13.0, TEXT_DIM));
                    });
            }
        }
    });
    if let Some(run) = agitation {
        panel.spawn(label(
            format!(
                "{}   {:.0}%   {:.1}s remaining",
                run.direction.label(),
                run.progress() * 100.0,
                run.remaining_secs()
            ),
            13.0,
            GOOD_TEXT,
        ));
    } else if preparations.is_none() {
        panel.spawn(label(
            "Prepare both recipe sides separately to agitate.",
            12.0,
            TEXT_DIM,
        ));
    }
    panel
        .spawn((
            Node {
                width: percent(100),
                column_gap: px(16),
                padding: UiRect::top(px(12)),
                ..default()
            },
            BackgroundColor(SECTION_BG),
        ))
        .with_children(|bottom| {
            bottom.spawn(column(34.0)).with_children(|chamber| {
                chamber.spawn(label("MIXING CHAMBER", 13.0, TEXT_DIM));
                chamber.spawn(row()).with_children(|preview| {
                    beaker_preview_sized(preview, Some(machine), 58.0, 78.0);
                    preview
                        .spawn(column(65.0))
                        .with_children(|p| readings(p, buffer.map(|b| &b.0)));
                });
                if let Some(run) =
                    agitation.filter(|run| run.direction == AgitateDirection::ToMixer)
                {
                    chamber.spawn(label(
                        format!("Agitating {:.0}%", run.progress() * 100.0),
                        13.0,
                        GOOD_TEXT,
                    ));
                }
                ingredients(
                    chamber,
                    db,
                    buffer.map(|b| &b.0),
                    MachineSlot::C,
                    locked,
                    true,
                    scroll[2],
                );
            });
            bottom.spawn(column(37.0)).with_children(|packages| {
                packages.spawn(label("PACKAGE", 13.0, TEXT_DIM));
                packages
                    .spawn((
                        list(200.0),
                        ScrollPosition(Vec2::new(0.0, scroll[3])),
                        ScrollPane,
                        ScrollArea(3),
                    ))
                    .with_children(|options| {
                        options.spawn(wrap_row()).with_children(|grid| {
                            for kind in [
                                ContainerKind::Pill,
                                ContainerKind::Bottle,
                                ContainerKind::Syringe,
                                ContainerKind::Patch,
                                ContainerKind::SprayBottle,
                                ContainerKind::ChemicalCharge5,
                                ContainerKind::ChemicalCharge10,
                                ContainerKind::ChemicalCharge20,
                                ContainerKind::PhPaper,
                                ContainerKind::SmokeProjector,
                            ] {
                                let mut option = grid.spawn(styled_button(
                                    format!("{} ({})", kind.label(), kind.capacity()),
                                    PanelAction::Package(kind),
                                    ButtonTone::Choice,
                                ));
                                if draft.kind == kind {
                                    option.insert(Selected);
                                }
                            }
                        });
                    });
            });
            bottom.spawn(column(29.0)).with_children(|output| {
                output.spawn(styled_button(
                    "Label",
                    PanelAction::FocusPackageLabel,
                    ButtonTone::Utility,
                ));
                let auto = if draft.kind == ContainerKind::PhPaper {
                    "pH paper".into()
                } else {
                    buffer.map_or(String::new(), |b| {
                        crate::machines::automatic_label(&b.0, db)
                    })
                };
                let shown = if draft.text.is_empty() {
                    if auto.is_empty() {
                        "Move chemicals into the chamber".into()
                    } else {
                        auto
                    }
                } else if draft.editing {
                    let mut text = draft.text.clone();
                    text.insert(draft.cursor.min(text.len()), '|');
                    text
                } else {
                    draft.text.clone()
                };
                output
                    .spawn((
                        Button,
                        Node {
                            width: percent(100),
                            height: px(130),
                            padding: UiRect::all(px(10)),
                            border: UiRect::all(px(1)),
                            overflow: Overflow::scroll_y(),
                            ..default()
                        },
                        BackgroundColor(PANEL_BG),
                        BorderColor::all(if draft.editing { LABEL_INK } else { TEXT_DIM }),
                        PreserveButtonBackground,
                        PanelAction::FocusPackageLabel,
                        ScrollPosition(Vec2::new(0.0, scroll[4])),
                        ScrollPane,
                        ScrollArea(4),
                    ))
                    .with_children(|field| {
                        field.spawn(label(
                            shown,
                            15.0,
                            if draft.text.is_empty() {
                                TEXT_DIM
                            } else {
                                LABEL_INK
                            },
                        ));
                    });
                let amount = buffer.map_or(Units::ZERO, |b| {
                    b.0.total_volume().min(draft.kind.capacity())
                });
                output.spawn(label(
                    format!("{}  |  {}", draft.kind.label(), amount),
                    13.0,
                    TEXT,
                ));
                let valid = !locked
                    && (draft.kind == ContainerKind::PhPaper
                        || (amount.is_positive()
                            && (draft.kind.charge_fuse().is_none()
                                || buffer.is_some_and(|b| {
                                    b.0.iter()
                                        .any(|(r, _)| db.reagents.get(r).explosive.is_some())
                                }))));
                if valid {
                    output.spawn(styled_button(
                        "Package",
                        PanelAction::FinishPackage,
                        ButtonTone::Primary,
                    ));
                } else {
                    output.spawn(label("Package unavailable", 14.0, TEXT_DIM));
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::keyboard::{Key, NativeKey};

    fn setup() -> (App, Entity, Entity) {
        let mut app = App::new();
        app.init_resource::<PackagingDraft>()
            .init_resource::<ButtonInput<KeyCode>>()
            .add_message::<KeyboardInput>()
            .add_systems(Update, type_label);
        let mut solution = chem_sim::Solution::new(Units::whole(300));
        let db = ChemDb(
            chem_sim::ChemData::from_ron(
                include_str!("../../assets/data/chem.reagents.ron"),
                include_str!("../../assets/data/chem.reactions.ron"),
            )
            .unwrap(),
        );
        let _ = solution.add(db.reagent("water"), Units::whole(20));
        let machine = app
            .world_mut()
            .spawn((Machine::new(MachineKind::MixingChamber), Buffer(solution)))
            .id();
        let player = app
            .world_mut()
            .spawn((LocalPlayer, InteractionMode::UsingMachine(machine)))
            .id();
        app.update();
        app.world_mut().resource_mut::<PackagingDraft>().editing = true;
        (app, player, machine)
    }
    fn press(app: &mut App, key_code: KeyCode, text: &str) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key_code);
        app.world_mut().write_message(KeyboardInput {
            key_code,
            logical_key: Key::Unidentified(NativeKey::Unidentified),
            state: ButtonState::Pressed,
            text: (!text.is_empty()).then(|| text.into()),
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();
    }
    #[test]
    fn label_typing_consumes_shortcuts_and_edits_unicode_at_the_cursor() {
        let (mut app, player, _) = setup();
        press(&mut app, KeyCode::KeyI, "eau é");
        assert!(!app
            .world()
            .resource::<ButtonInput<KeyCode>>()
            .pressed(KeyCode::KeyI));
        press(&mut app, KeyCode::ArrowLeft, "");
        press(&mut app, KeyCode::KeyW, "très ");
        assert_eq!(app.world().resource::<PackagingDraft>().text, "eau très é");
        press(&mut app, KeyCode::Delete, "");
        assert_eq!(app.world().resource::<PackagingDraft>().text, "eau très ");
        press(&mut app, KeyCode::Escape, "");
        assert!(!app.world().resource::<PackagingDraft>().editing);
        assert!(!app
            .world()
            .resource::<ButtonInput<KeyCode>>()
            .just_pressed(KeyCode::Escape));
        assert!(matches!(
            app.world().get::<InteractionMode>(player),
            Some(InteractionMode::UsingMachine(_))
        ));
    }
    #[test]
    fn draft_survives_live_updates_and_inspection_but_clears_after_leaving() {
        let (mut app, player, machine) = setup();
        press(&mut app, KeyCode::KeyW, "Fresh water");
        press(&mut app, KeyCode::Enter, "");
        app.world_mut()
            .get_mut::<Buffer>(machine)
            .unwrap()
            .0
            .temperature
            .0 += 5.0;
        app.update();
        let item = app.world_mut().spawn_empty().id();
        app.world_mut()
            .entity_mut(player)
            .insert(InteractionMode::Inspecting {
                item,
                machine: Some(machine),
            });
        app.update();
        assert_eq!(app.world().resource::<PackagingDraft>().text, "Fresh water");
        app.world_mut()
            .entity_mut(player)
            .insert(InteractionMode::Roaming);
        app.update();
        let draft = app.world().resource::<PackagingDraft>();
        assert!(draft.text.is_empty());
        assert_eq!(draft.kind, ContainerKind::Bottle);
    }
}
