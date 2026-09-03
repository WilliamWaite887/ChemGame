use super::{requirements, AcceptOrder, AwaitingConversation, OrderConversationOpened};
use crate::{
    crew::CrewMember, interaction::InteractionMode, orders::Order, player::LocalPlayer, AppState,
};
use bevy::prelude::*;

#[derive(Resource, Default)]
pub struct ConversationView(pub Option<OrderConversationOpened>);
#[derive(Component)]
struct RequestPanel;
#[derive(Component, Clone, Copy)]
enum Action {
    Accept(Entity, u64),
    Close,
}
#[derive(Component)]
struct Countdown(Entity);
#[derive(Resource, Default)]
pub(crate) struct Signature(Option<InteractionMode>, Vec<Entity>);

pub struct OrderConversationUiPlugin;
impl Plugin for OrderConversationUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ConversationView>()
            .init_resource::<Signature>()
            .add_systems(
                Update,
                (
                    (opened, buttons, draw, countdowns).chain(),
                    crate::ui::button_feedback,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

fn opened(
    mut messages: MessageReader<OrderConversationOpened>,
    mut view: ResMut<ConversationView>,
    mut modes: Query<&mut InteractionMode, With<LocalPlayer>>,
) {
    for message in messages.read() {
        for mut mode in &mut modes {
            if mode.is_roaming() {
                *mode = InteractionMode::OrderConversation(message.target, message.id);
                view.0 = Some(message.clone());
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn buttons(
    buttons: Query<(&Interaction, &Action), (Changed<Interaction>, With<Button>)>,
    mut modes: Query<&mut InteractionMode, With<LocalPlayer>>,
    mut accepts: MessageWriter<AcceptOrder>,
) {
    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        for mut mode in &mut modes {
            match action {
                Action::Accept(target, id)
                    if *mode == InteractionMode::OrderConversation(*target, *id) =>
                {
                    accepts.write(AcceptOrder {
                        target: *target,
                        id: *id,
                    });
                    *mode = InteractionMode::Roaming;
                }
                Action::Close => {
                    *mode = match *mode {
                        InteractionMode::OrderDirectory {
                            machine,
                            return_to_book,
                        } => InteractionMode::Social {
                            machine,
                            return_to_book,
                        },
                        _ => InteractionMode::Roaming,
                    };
                }
                _ => {}
            }
        }
    }
}

fn text(value: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(value),
        TextFont::from_font_size(size),
        TextColor(color),
    )
}
fn button(value: &str, action: Action) -> impl Bundle {
    (
        Button,
        Node {
            padding: UiRect::axes(px(18), px(12)),
            border_radius: BorderRadius::all(px(5)),
            ..default()
        },
        BackgroundColor(crate::ui::BUTTON_IDLE),
        action,
        children![(
            Text::new(value),
            TextFont::from_font_size(17.0),
            TextColor(Color::WHITE)
        )],
    )
}

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    mut modes: Query<&mut InteractionMode, With<LocalPlayer>>,
    view: Res<ConversationView>,
    pending: Query<&AwaitingConversation>,
    orders: Query<(Entity, &CrewMember, &Order)>,
    db: Res<crate::chem_data::ChemDb>,
    panels: Query<Entity, With<RequestPanel>>,
    mut signature: ResMut<Signature>,
) {
    let Some(mut mode) = modes.iter_mut().next() else {
        return;
    };
    if let InteractionMode::OrderConversation(target, id) = *mode {
        if !pending.get(target).is_ok_and(|p| p.id == id && p.arrived) {
            *mode = InteractionMode::Roaming;
        }
    }
    let visible = matches!(
        *mode,
        InteractionMode::OrderConversation(..) | InteractionMode::OrderDirectory { .. }
    );
    let mut entries: Vec<_> = orders.iter().collect();
    entries.sort_by(|a, b| {
        a.2.remaining()
            .total_cmp(&b.2.remaining())
            .then(a.0.cmp(&b.0))
    });
    let ids: Vec<_> = entries.iter().map(|(e, _, _)| *e).collect();
    let new_mode = visible.then_some(*mode);
    if signature.0 == new_mode && signature.1 == ids {
        return;
    }
    signature.0 = new_mode;
    signature.1 = ids;
    for panel in &panels {
        commands.entity(panel).despawn();
    }
    if !visible {
        return;
    }
    let color = Color::srgb(0.90, 0.94, 0.96);
    let muted = Color::srgb(0.62, 0.73, 0.78);
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.025, 0.72)),
            GlobalZIndex(60),
            RequestPanel,
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    width: percent(90),
                    max_width: px(840),
                    max_height: vh(88),
                    padding: UiRect::all(px(24)),
                    flex_direction: FlexDirection::Column,
                    row_gap: px(18),
                    border: UiRect::all(px(2)),
                    border_radius: BorderRadius::all(px(10)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.055, 0.08, 0.10)),
                BorderColor::all(Color::srgb(0.24, 0.40, 0.50)),
            ))
            .with_children(|panel| match *mode {
                InteractionMode::OrderConversation(target, id) => {
                    let Some(request) =
                        view.0.as_ref().filter(|v| v.target == target && v.id == id)
                    else {
                        return;
                    };
                    panel.spawn(text(
                        format!("{}  /  {}", request.name, request.role),
                        22.0,
                        color,
                    ));
                    panel
                        .spawn((
                            Node {
                                max_height: vh(42),
                                overflow: Overflow::scroll_y(),
                                ..default()
                            },
                            ScrollPosition::default(),
                            crate::ui::ScrollPane,
                        ))
                        .with_children(|p| {
                            p.spawn(text(&request.explanation, 20.0, color));
                        });
                    panel.spawn(text("ORDER REQUEST", 12.0, muted));
                    panel.spawn(text(&request.requirements, 19.0, color));
                    panel.spawn(text(
                        format!(
                            "Preparation time after acceptance: {}:{:02}",
                            request.patience as u32 / 60,
                            request.patience as u32 % 60
                        ),
                        15.0,
                        muted,
                    ));
                    panel
                        .spawn(Node {
                            column_gap: px(12),
                            ..default()
                        })
                        .with_children(|row| {
                            row.spawn(button("I'll handle it", Action::Accept(target, id)));
                            row.spawn(button("Not now  (Esc)", Action::Close));
                        });
                }
                InteractionMode::OrderDirectory { .. } => {
                    panel.spawn(text(
                        format!("ACCEPTED ORDERS  /  {}", entries.len()),
                        22.0,
                        color,
                    ));
                    panel.spawn(text(
                        "Requests your team has agreed to handle. Preparation clocks keep running.",
                        14.0,
                        muted,
                    ));
                    panel
                        .spawn((
                            Node {
                                flex_direction: FlexDirection::Column,
                                row_gap: px(12),
                                overflow: Overflow::scroll_y(),
                                max_height: vh(61),
                                ..default()
                            },
                            ScrollPosition::default(),
                            crate::ui::ScrollPane,
                        ))
                        .with_children(|list| {
                            for (entity, member, order) in &entries {
                                list.spawn((
                                    Node {
                                        flex_direction: FlexDirection::Column,
                                        flex_shrink: 0.0,
                                        row_gap: px(7),
                                        padding: UiRect::all(px(14)),
                                        ..default()
                                    },
                                    BackgroundColor(Color::srgb(0.09, 0.13, 0.16)),
                                ))
                                .with_children(|card| {
                                    card.spawn(text(
                                        format!("{}  /  {}", member.name, member.role),
                                        18.0,
                                        color,
                                    ));
                                    card.spawn(text(&order.plea, 17.0, color));
                                    card.spawn(text(
                                        requirements(order, &db),
                                        16.0,
                                        Color::srgb(0.48, 0.80, 0.92),
                                    ));
                                    card.spawn((text("", 14.0, muted), Countdown(*entity)));
                                });
                            }
                            if entries.is_empty() {
                                list.spawn(text("No accepted orders.", 18.0, muted));
                            }
                        });
                    panel.spawn(button("Back to crew directory", Action::Close));
                }
                _ => {}
            });
        });
}

fn countdowns(
    orders: Query<(&Order, Has<crate::security_case::OrderHold>)>,
    mut labels: Query<(&Countdown, &mut Text)>,
    session: Option<Res<crate::session::SessionKind>>,
) {
    for (countdown, mut label) in &mut labels {
        if let Ok((order, held)) = orders.get(countdown.0) {
            let seconds = order.remaining() as u32;
            let next = if matches!(
                session.as_deref(),
                Some(crate::session::SessionKind::Training)
            ) {
                "Practice request - untimed".into()
            } else if held {
                "Security hold — visit Bex at Security".into()
            } else {
                format!("Remaining  {}:{:02}", seconds / 60, seconds % 60)
            };
            if label.0 != next {
                label.0 = next;
            }
        }
    }
}
