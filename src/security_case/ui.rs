use super::*;
use crate::{interaction::InteractionMode, player::LocalPlayer};

#[derive(Resource, Default)]
struct View(Option<CaseConversationOpened>);
#[derive(Component)]
struct Panel;
#[derive(Component)]
struct ButtonAction(Option<CaseAction>);
pub(super) struct SecurityCaseUiPlugin;
impl Plugin for SecurityCaseUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<View>()
            .add_systems(OnExit(AppState::Playing), reset)
            .add_systems(
                Update,
                (
                    (opened, buttons, draw, dress_locker).chain(),
                    crate::ui::button_feedback,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}
fn reset(mut view: ResMut<View>) {
    *view = View::default();
}
fn opened(
    mut messages: MessageReader<CaseConversationOpened>,
    mut view: ResMut<View>,
    mut modes: Query<&mut InteractionMode, With<LocalPlayer>>,
) {
    for message in messages.read() {
        for mut mode in &mut modes {
            if mode.is_roaming() || *mode == InteractionMode::SecurityConversation {
                *mode = InteractionMode::SecurityConversation;
                view.0 = Some(message.clone());
            }
        }
    }
}
fn buttons(
    buttons: Query<(&Interaction, &ButtonAction), (Changed<Interaction>, With<Button>)>,
    view: Res<View>,
    mut modes: Query<&mut InteractionMode, With<LocalPlayer>>,
    mut requests: MessageWriter<CaseActionRequested>,
    mouse: Res<ButtonInput<MouseButton>>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        for mut mode in &mut modes {
            if *mode != InteractionMode::SecurityConversation {
                continue;
            }
            if let (Some(action), Some(view)) = (action.0, view.0.as_ref()) {
                requests.write(CaseActionRequested {
                    target: view.target,
                    case: view.case,
                    action,
                });
            }
            // Grounds stays open so the authority's explanation can replace it.
            if action.0 != Some(CaseAction::Grounds) {
                *mode = InteractionMode::Roaming;
            }
        }
    }
}
fn draw(
    mut commands: Commands,
    mut modes: Query<&mut InteractionMode, With<LocalPlayer>>,
    view: Res<View>,
    summary: Res<SecurityCaseSummary>,
    panels: Query<Entity, With<Panel>>,
    targets: Query<(), With<Interactable>>,
    mut signature: Local<Option<(u64, Entity, String)>>,
) {
    let Some(mut mode) = modes.iter_mut().next() else {
        return;
    };
    if *mode == InteractionMode::SecurityConversation
        && view.0.as_ref().is_none_or(|v| {
            summary.case != Some(v.case)
                || summary.stage != Some(v.stage)
                || targets.get(v.target).is_err()
        })
    {
        *mode = InteractionMode::Roaming;
    }
    let shown = if *mode == InteractionMode::SecurityConversation {
        view.0.as_ref()
    } else {
        None
    };
    let next = shown.map(|v| (v.case, v.target, v.text.clone()));
    if *signature == next && (next.is_none() || !panels.is_empty()) {
        return;
    }
    *signature = next;
    for panel in &panels {
        commands.entity(panel).despawn();
    }
    let Some(view) = shown else {
        return;
    };
    commands
        .spawn((
            Panel,
            crate::until_we_leave_the_lab(),
            GlobalZIndex(60),
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.025, 0.75)),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    width: percent(88),
                    max_width: px(860),
                    max_height: vh(90),
                    padding: UiRect::all(px(24)),
                    flex_direction: FlexDirection::Column,
                    row_gap: px(18),
                    border: UiRect::all(px(2)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.055, 0.08, 0.10)),
                BorderColor::all(Color::srgb(0.24, 0.4, 0.5)),
            ))
            .with_children(|panel| {
                panel.spawn((
                    Text::new(crate::ui::font_safe_text(&view.title)),
                    TextFont::from_font_size(24.0),
                    TextColor(Color::WHITE),
                ));
                panel
                    .spawn((
                        Node {
                            max_height: vh(45),
                            overflow: Overflow::scroll_y(),
                            ..default()
                        },
                        ScrollPosition::default(),
                        crate::ui::ScrollPane,
                    ))
                    .with_children(|body| {
                        body.spawn((
                            Text::new(crate::ui::font_safe_text(&view.text)),
                            TextFont::from_font_size(19.0),
                            TextColor(Color::srgb(0.90, 0.94, 0.96)),
                        ));
                    });
                panel
                    .spawn(Node {
                        flex_wrap: FlexWrap::Wrap,
                        column_gap: px(10),
                        row_gap: px(10),
                        ..default()
                    })
                    .with_children(|row| {
                        for (action, label) in view
                            .actions
                            .iter()
                            .map(|(a, l)| (Some(*a), l.as_str()))
                            .chain(std::iter::once((None, "Not now (Esc)")))
                        {
                            row.spawn((
                                Button,
                                ButtonAction(action),
                                Node {
                                    padding: UiRect::axes(px(16), px(12)),
                                    ..default()
                                },
                                BackgroundColor(crate::ui::BUTTON_IDLE),
                            ))
                            .with_children(|button| {
                                button.spawn((
                                    Text::new(crate::ui::font_safe_text(label)),
                                    TextFont::from_font_size(16.0),
                                    TextColor(Color::WHITE),
                                ));
                            });
                        }
                    });
            });
        });
}
fn dress_locker(
    mut commands: Commands,
    lockers: Query<Entity, Added<CaseLocker>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for locker in &lockers {
        commands
            .entity(locker)
            .insert((
                Mesh3d(meshes.add(Cuboid::new(0.65, 0.85, 0.45))),
                MeshMaterial3d(materials.add(Color::srgb(0.20, 0.27, 0.30))),
            ))
            .with_children(|box_| {
                box_.spawn((
                    Mesh3d(meshes.add(Cuboid::new(0.42, 0.17, 0.01))),
                    MeshMaterial3d(materials.add(Color::srgb(0.76, 0.81, 0.72))),
                    Transform::from_xyz(0.0, 0.12, 0.231),
                ));
            });
    }
}
