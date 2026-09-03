//! The main menu: how to play, then which save.
//!
//! Three screens, because the two questions are independent. Host and Solo both
//! own a lab, so both ask which save to open it from — one save can be played
//! alone on Monday and hosted on Tuesday, which is why the save screen does not
//! know or care which of the two sent it there. Join asks neither: the host's
//! notebook and career arrive over the wire, so a guest picking a save would be
//! choosing something that is about to be overwritten.
//!
//! The menu is what decides [`LaunchMode`], which until now came from the
//! command line. Those flags still work and still skip the menu — they are how
//! the game gets launched twice from one terminal while testing co-op.

use bevy::ecs::system::SystemParam;
use bevy::input::keyboard::KeyboardInput;
use bevy::input::ButtonState;
use bevy::input_focus::directional_navigation::DirectionalNavigationPlugin;
use bevy::input_focus::{FocusCause, InputFocus, InputFocusVisible};
use bevy::math::CompassOctant;
use bevy::prelude::*;
use bevy::ui::auto_directional_navigation::{AutoDirectionalNavigation, AutoDirectionalNavigator};

use crate::arc::{AntagId, CampaignChoice, Mode};
use crate::net::{self, parse_address, parse_literal_address, ConnectFailed, LaunchMode};
use crate::saves::{self, SaveSlot};
use crate::settings::{self, Rebinding, Settings};
use crate::ui::{
    button, button_feedback, heading, label, row, BUTTON_IDLE, ERROR_TEXT, PANEL_BG, SECTION_BG,
    TEXT, TEXT_DIM,
};
use crate::AppState;

mod backdrop;

/// Which menu screen is up.
///
/// A state rather than a resource so each screen gets `OnEnter`/`OnExit`, which
/// is what keeps building and tearing down the UI from being one system that
/// has to remember what it drew last.
#[derive(States, Debug, Clone, Copy, Default, Eq, PartialEq, Hash)]
pub enum MenuScreen {
    /// Not in the menu — either still loading, or playing.
    #[default]
    Hidden,
    /// Host, Join or Solo.
    Mode,
    /// Steam hosting and the two ways a guest can join.
    Multiplayer,
    /// New save, or one of the existing ones.
    Save,
    Training,
    TrainingLessons,
    /// Which side to play a brand new save from. Only ever reached when at
    /// least one antagonist has been thwarted — see [`show_campaign_screen`].
    Campaign,
    /// Debug-only deterministic campaign selection for a fresh chemist save.
    /// Shipping builds do not compile this screen or any route to it.
    #[cfg(debug_assertions)]
    AntagonistTest,
    /// The address to dial.
    Join,
    /// Waiting on `AppState::Connecting` to resolve — see its doc comment.
    Connecting,
    /// Sensitivity, FOV, volume, display, and a link to `Controls`. Reached
    /// from `Mode` so a player can tune these before ever opening a save —
    /// previously the pause menu was the only way in. Mirrors
    /// `settings::PauseScreen::Settings`; the two screens share their content
    /// via `settings::settings_body` and only differ in how they're framed
    /// and left.
    Settings,
    /// Key bindings, reached from `Settings` rather than a sibling of it off
    /// `Mode` directly — `Mode`'s job is the one primary decision, and the
    /// pause root already has room to spare that this screen does not.
    Controls,
    /// Verified third-party asset and dependency attribution.
    Credits,
}

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(DirectionalNavigationPlugin)
            .init_state::<MenuScreen>()
            .init_resource::<AddressInput>()
            .init_resource::<PendingMode>()
            .init_resource::<ConnectError>()
            .init_resource::<PendingDelete>()
            .init_resource::<MenuReturn>()
            .init_resource::<MenuSyntheticPress>()
            .init_resource::<MenuStickRepeat>()
            .insert_resource(InputFocusVisible(true))
            .add_systems(Startup, backdrop::load_assets)
            .add_systems(
                OnEnter(AppState::MainMenu),
                (backdrop::ensure_environment, open_menu).chain(),
            )
            .add_systems(
                OnEnter(AppState::Connecting),
                (backdrop::ensure_environment, open_connecting).chain(),
            )
            .add_systems(OnEnter(AppState::Playing), backdrop::clear_environment)
            .add_systems(OnEnter(MenuScreen::Mode), show_mode_screen)
            .add_systems(OnEnter(MenuScreen::Multiplayer), show_multiplayer_screen)
            .add_systems(OnEnter(MenuScreen::Save), show_save_screen)
            .add_systems(OnEnter(MenuScreen::Campaign), show_campaign_screen)
            .add_systems(OnEnter(MenuScreen::Join), show_join_screen)
            .add_systems(OnEnter(MenuScreen::Connecting), show_connecting_screen)
            .add_systems(OnEnter(MenuScreen::Settings), show_settings_screen)
            .add_systems(OnEnter(MenuScreen::Controls), show_controls_screen)
            .add_systems(OnEnter(MenuScreen::Credits), show_credits_screen)
            .add_systems(OnExit(MenuScreen::Mode), clear_screen)
            .add_systems(OnExit(MenuScreen::Multiplayer), clear_screen)
            // A stale confirmation must not reappear if the player leaves the
            // save list and comes back — cleared here rather than trusted to
            // reset itself, the same defensive footing `open_connecting`
            // already gives `ConnectError`.
            .add_systems(
                OnExit(MenuScreen::Save),
                (clear_screen, reset_pending_delete),
            )
            .add_systems(OnExit(MenuScreen::Campaign), clear_screen)
            .add_systems(OnExit(MenuScreen::Join), clear_screen)
            .add_systems(OnExit(MenuScreen::Connecting), clear_screen)
            .add_systems(OnExit(MenuScreen::Settings), clear_screen)
            .add_systems(OnExit(MenuScreen::Controls), clear_screen)
            .add_systems(OnExit(MenuScreen::Credits), clear_screen)
            .add_systems(
                Update,
                (
                    prepare_menu_navigation,
                    navigate_menu,
                    type_address.run_if(in_state(MenuScreen::Join)),
                    show_typed_address.run_if(in_state(MenuScreen::Join)),
                    handle_menu_clicks,
                    refresh_save_screen.run_if(in_state(MenuScreen::Save)),
                    handle_connect_failure.run_if(in_state(AppState::Connecting)),
                    button_feedback,
                    show_menu_focus,
                    backdrop::animate_environment.run_if(backdrop::menu_or_connecting),
                )
                    .chain()
                    .run_if(in_state(AppState::MainMenu).or_else(in_state(AppState::Connecting))),
            );
        #[cfg(debug_assertions)]
        app.add_systems(
            OnEnter(MenuScreen::AntagonistTest),
            show_antagonist_test_screen,
        )
        .add_systems(OnExit(MenuScreen::AntagonistTest), clear_screen);
    }
}

/// Root of whichever screen is up. Despawned wholesale on the way out.
#[derive(Component)]
struct MenuRoot;

/// Host or Solo, remembered while the player picks a save.
#[derive(Resource, Default, Clone, Copy)]
struct PendingMode(LaunchMode);

/// Why the last join attempt failed, if it did.
///
/// Shown on the direct-join screen after `handle_connect_failure` bounces back
/// from `AppState::Connecting` — the one place in the menu that has to say
/// something went wrong out loud, since silence is exactly the failure mode
/// this whole feature exists to fix.
#[derive(Resource, Default)]
struct ConnectError(Option<String>);

/// Screen restored after temporarily leaving `MainMenu` to connect.
#[derive(Resource, Default)]
struct MenuReturn(Option<MenuScreen>);

/// A controller/keyboard activation is translated to the same `Interaction`
/// transition a mouse click produces, so every existing button handler stays
/// the single source of truth.
#[derive(Resource, Default)]
struct MenuSyntheticPress(Option<Entity>);

/// Repeat state for an analogue stick held past the navigation threshold.
#[derive(Resource, Default)]
struct MenuStickRepeat {
    direction: IVec2,
    held_for: f32,
    since_repeat: f32,
}

/// The address being typed on the join screen.
#[derive(Resource, Default)]
struct AddressInput {
    text: String,
    /// Set by Enter, cleared by the system that acts on it.
    submitted: bool,
}

/// The save awaiting a delete confirmation on the Save screen, if any.
///
/// Its row is swapped for an inline "delete forever / cancel" card instead of
/// its usual load button — a destructive action must never share a hitbox
/// with the everyday one. Plain resource rather than a screen: `MenuScreen`
/// stays `Save` throughout, only this one row's rendering changes.
#[derive(Resource, Default)]
struct PendingDelete(Option<String>);

fn reset_pending_delete(mut pending: ResMut<PendingDelete>) {
    pending.0 = None;
}

/// Makes every live menu button discoverable by Bevy's spatial navigator and
/// establishes a deterministic first focus after a screen rebuild.
fn prepare_menu_navigation(
    mut commands: Commands,
    buttons: Query<(Entity, Option<&crate::ui::ButtonTone>), With<Button>>,
    added: Query<Entity, Added<Button>>,
    mut focus: ResMut<InputFocus>,
) {
    for entity in &added {
        commands.entity(entity).insert((
            AutoDirectionalNavigation::default(),
            Outline::new(px(2), px(2), Color::NONE),
        ));
    }

    let focus_is_live = focus
        .get()
        .is_some_and(|entity| buttons.get(entity).is_ok());
    if focus_is_live {
        return;
    }

    let mut eligible: Vec<_> = buttons
        .iter()
        .filter(|(_, tone)| tone != &Some(&crate::ui::ButtonTone::Disabled))
        .map(|(entity, _)| entity)
        .collect();
    eligible.sort_by_key(|entity| entity.index());
    if let Some(first) = eligible.first().copied() {
        focus.set(first, FocusCause::Navigated);
    } else {
        focus.clear();
    }
}

fn stick_direction(stick: Vec2) -> IVec2 {
    if stick.length_squared() < 0.36 {
        IVec2::ZERO
    } else if stick.x.abs() > stick.y.abs() {
        IVec2::new(stick.x.signum() as i32, 0)
    } else {
        IVec2::new(0, stick.y.signum() as i32)
    }
}

#[derive(SystemParam)]
struct MenuControls<'w, 's> {
    time: Res<'w, Time>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    gamepads: Query<'w, 's, &'static Gamepad>,
    rebinding: Res<'w, Rebinding>,
}

type MenuButtons<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Interaction,
        Option<&'static MenuAction>,
        Option<&'static crate::tutorial::ui::Action>,
        Has<settings::NavigableSlider>,
    ),
    With<Button>,
>;

/// Keyboard, D-pad, and left-stick navigation for every main-menu screen.
/// Left/right adjusts a focused slider; all other controls use the same
/// automatic spatial relationships as their on-screen layout.
fn navigate_menu(
    input: MenuControls,
    mut repeat: ResMut<MenuStickRepeat>,
    mut synthetic: ResMut<MenuSyntheticPress>,
    mut navigator: AutoDirectionalNavigator,
    mut buttons: MenuButtons,
    mut scroll_panes: Query<(&mut ScrollPosition, &ComputedNode), With<crate::ui::ScrollPane>>,
    mut slider_adjustments: MessageWriter<settings::AdjustFocusedSlider>,
) {
    // Release last frame's synthetic press before looking for a new one.
    if let Some(entity) = synthetic.0.take() {
        if let Ok((_, mut interaction, _, _, _)) = buttons.get_mut(entity) {
            *interaction = Interaction::None;
        }
    }

    // While a key binding is armed, its capture system owns the keyboard.
    // Controller input remains available so the user cannot strand focus.
    let keyboard_enabled = !input.rebinding.is_armed();
    let mut direction = IVec2::ZERO;
    if keyboard_enabled {
        direction.x = i32::from(input.keys.just_pressed(KeyCode::ArrowRight))
            - i32::from(input.keys.just_pressed(KeyCode::ArrowLeft));
        direction.y = i32::from(input.keys.just_pressed(KeyCode::ArrowUp))
            - i32::from(input.keys.just_pressed(KeyCode::ArrowDown));
    }

    let mut select = keyboard_enabled
        && (input.keys.just_pressed(KeyCode::Enter) || input.keys.just_pressed(KeyCode::Space));
    let mut back = keyboard_enabled && input.keys.just_pressed(KeyCode::Escape);
    let mut scroll_delta = if keyboard_enabled {
        520.0
            * (i32::from(input.keys.just_pressed(KeyCode::PageDown))
                - i32::from(input.keys.just_pressed(KeyCode::PageUp))) as f32
    } else {
        0.0
    };
    let scroll_to_start = keyboard_enabled && input.keys.just_pressed(KeyCode::Home);
    let scroll_to_end = keyboard_enabled && input.keys.just_pressed(KeyCode::End);
    let mut analogue = IVec2::ZERO;
    for gamepad in &input.gamepads {
        direction.x += i32::from(gamepad.just_pressed(GamepadButton::DPadRight))
            - i32::from(gamepad.just_pressed(GamepadButton::DPadLeft));
        direction.y += i32::from(gamepad.just_pressed(GamepadButton::DPadUp))
            - i32::from(gamepad.just_pressed(GamepadButton::DPadDown));
        select |= gamepad.just_pressed(GamepadButton::South);
        back |= gamepad.just_pressed(GamepadButton::East);
        scroll_delta += 520.0
            * (i32::from(gamepad.just_pressed(GamepadButton::RightTrigger))
                - i32::from(gamepad.just_pressed(GamepadButton::LeftTrigger))) as f32;
        scroll_delta -= gamepad.right_stick().y * 620.0 * input.time.delta_secs();
        if analogue == IVec2::ZERO {
            analogue = stick_direction(gamepad.left_stick());
        }
    }

    if scroll_delta != 0.0 || scroll_to_start || scroll_to_end {
        for (mut position, computed) in &mut scroll_panes {
            let limit = (computed.content_size().y - computed.size().y).max(0.0);
            position.y = if scroll_to_start {
                0.0
            } else if scroll_to_end {
                limit
            } else {
                (position.y + scroll_delta).clamp(0.0, limit)
            };
        }
    }

    if analogue == IVec2::ZERO {
        repeat.direction = IVec2::ZERO;
        repeat.held_for = 0.0;
        repeat.since_repeat = 0.0;
    } else if analogue != repeat.direction {
        repeat.direction = analogue;
        repeat.held_for = 0.0;
        repeat.since_repeat = 0.0;
        if direction == IVec2::ZERO {
            direction = analogue;
        }
    } else {
        let delta = input.time.delta_secs();
        repeat.held_for += delta;
        repeat.since_repeat += delta;
        if repeat.held_for >= 0.38 && repeat.since_repeat >= 0.12 {
            repeat.since_repeat = 0.0;
            if direction == IVec2::ZERO {
                direction = analogue;
            }
        }
    }

    direction.x = direction.x.signum();
    direction.y = direction.y.signum();

    if let Some(focused) = navigator.input_focus() {
        let focused_slider = direction.y == 0
            && direction.x != 0
            && buttons
                .get(focused)
                .is_ok_and(|(_, _, _, _, slider)| slider);
        if focused_slider {
            slider_adjustments.write(settings::AdjustFocusedSlider {
                entity: focused,
                direction: direction.x as f32,
            });
            direction = IVec2::ZERO;
        }
    }

    let compass = match (direction.x, direction.y) {
        (0, 1) => Some(CompassOctant::North),
        (0, -1) => Some(CompassOctant::South),
        (1, 0) => Some(CompassOctant::East),
        (-1, 0) => Some(CompassOctant::West),
        (1, 1) => Some(CompassOctant::NorthEast),
        (-1, 1) => Some(CompassOctant::NorthWest),
        (1, -1) => Some(CompassOctant::SouthEast),
        (-1, -1) => Some(CompassOctant::SouthWest),
        _ => None,
    };
    if let Some(compass) = compass {
        let _ = navigator.navigate(compass);
    }

    // Hovered mouse controls take focus without hiding the controller ring.
    for (entity, interaction, _, _, _) in &mut buttons {
        if *interaction == Interaction::Hovered && navigator.input_focus() != Some(entity) {
            navigator
                .manual_directional_navigation
                .focus
                .set(entity, FocusCause::Pressed);
        }
    }

    let target = if back {
        buttons
            .iter()
            .find_map(|(entity, _, menu_action, training_action, _)| {
                let menu_back = menu_action
                    .is_some_and(|action| matches!(action, MenuAction::Back | MenuAction::Cancel));
                let training_back = training_action
                    .is_some_and(|action| matches!(action, crate::tutorial::ui::Action::Back));
                (menu_back || training_back).then_some(entity)
            })
    } else if select {
        navigator.input_focus()
    } else {
        None
    };
    if let Some(entity) = target {
        if let Ok((_, mut interaction, _, _, _)) = buttons.get_mut(entity) {
            *interaction = Interaction::Pressed;
            synthetic.0 = Some(entity);
        }
    }
}

/// Draws a slim cyan ring around the focused control without replacing each
/// button's semantic hover/selected border colour.
fn show_menu_focus(
    focus: Res<InputFocus>,
    visible: Res<InputFocusVisible>,
    mut buttons: Query<(Entity, &mut Outline), With<AutoDirectionalNavigation>>,
) {
    for (entity, mut outline) in &mut buttons {
        outline.color = if visible.0 && focus.get() == Some(entity) {
            Color::srgba(0.30, 0.82, 1.0, 0.90)
        } else {
            Color::NONE
        };
    }
}

/// Marks the text node showing what has been typed.
#[derive(Component)]
struct AddressField;

/// Marks the line under the field that says how the address will be read.
#[derive(Component)]
struct AddressHint;

/// What a menu button does.
#[derive(Component, Clone)]
pub(crate) enum MenuAction {
    ChooseHost,
    ChooseSolo,
    ChooseJoin,
    OpenMultiplayer,
    NewSave,
    /// Opens deterministic antagonist selection for a fresh chemist save.
    /// Kept out of shipping builds along with its screen and click handler.
    #[cfg(debug_assertions)]
    NewAntagonistTestSave,
    /// Starts a fresh chemist save against this known antagonist.
    #[cfg(debug_assertions)]
    NewTestChemistRun(AntagId),
    /// A new save played the ordinary way: a hidden antagonist, rolled for
    /// you. Only ever reached from the campaign screen — with nothing
    /// unlocked, [`MenuAction::NewSave`] starts one of these directly.
    NewChemistRun,
    /// A new save played from the other side, working for an antagonist this
    /// machine has already beaten.
    NewAntagonistRun(AntagId),
    LoadSave(String),
    Connect,
    /// Gives up on a `Connecting` attempt and returns to the mode screen.
    Cancel,
    Back,
    Quit,
    OpenTraining,
    OpenSettings,
    OpenControls,
    OpenCredits,
    /// Puts every dial and display option back where it shipped. Deliberately
    /// leaves `bindings` untouched — see [`MenuAction::RestoreBindings`].
    RestoreDefaults,
    /// Puts every key on the Controls screen back where it shipped. Split
    /// from [`MenuAction::RestoreDefaults`] for the same reason
    /// `settings::PauseAction` splits the two: bindings are a different
    /// category of preference from the perceptual dials, and neither reset
    /// should be able to silently undo the other.
    RestoreBindings,
    /// Arms the inline "delete forever / cancel" card for one save row.
    RequestDeleteSave(String),
    /// Actually removes the save — only ever sent from that card, never
    /// directly from a load row.
    ConfirmDeleteSave(String),
    CancelDeleteSave,
}

fn open_menu(
    mut commands: Commands,
    mut screen: ResMut<NextState<MenuScreen>>,
    career: Option<Res<crate::tutorial::StartCareer>>,
    lessons: Option<Res<crate::tutorial::ui::ShowLessons>>,
    mut pending: ResMut<PendingMode>,
    mut return_to: ResMut<MenuReturn>,
) {
    // Done here rather than at startup so it happens exactly once, on the path
    // that is about to list the saves.
    saves::migrate_legacy_saves();
    if lessons.is_some() {
        commands.remove_resource::<crate::tutorial::ui::ShowLessons>();
        screen.set(MenuScreen::TrainingLessons);
    } else if career.is_some() {
        pending.0 = LaunchMode::Singleplayer;
        commands.remove_resource::<crate::tutorial::StartCareer>();
        screen.set(MenuScreen::Save);
    } else if let Some(target) = return_to.0.take() {
        screen.set(target);
    } else {
        screen.set(MenuScreen::Mode);
    }
}

/// Opens the waiting-room screen on the way into `AppState::Connecting`.
///
/// The menu environment deliberately remains alive during the handshake.
/// Clear an older error here so a successful retry cannot leave stale failure
/// copy waiting behind the connecting panel.
fn open_connecting(mut screen: ResMut<NextState<MenuScreen>>, mut error: ResMut<ConnectError>) {
    error.0 = None;
    screen.set(MenuScreen::Connecting);
}

fn clear_screen(mut commands: Commands, roots: Query<Entity, With<MenuRoot>>) {
    for root in &roots {
        commands.entity(root).try_despawn();
    }
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

fn show_mode_screen(mut commands: Commands) {
    landing_shell(&mut commands, |panel| {
        panel.spawn(landing_choice(
            "Solo",
            "Choose or begin a chemistry career.",
            MenuAction::ChooseSolo,
        ));
        panel.spawn(landing_choice(
            "Multiplayer",
            "Host or join a lab for up to four chemists.",
            MenuAction::OpenMultiplayer,
        ));
        panel.spawn(landing_choice(
            "Training",
            "Guided lessons and an open practice laboratory.",
            MenuAction::OpenTraining,
        ));
        panel.spawn(landing_choice(
            "Settings",
            "Display, sound, look, and controls.",
            MenuAction::OpenSettings,
        ));
        panel.spawn(landing_choice("Quit", "Close ChemGame.", MenuAction::Quit));
    });
}

fn show_multiplayer_screen(mut commands: Commands, steam: Option<Res<crate::net::steam::Client>>) {
    menu_panel_shell(
        &mut commands,
        MenuRoot,
        "Multiplayer",
        "One shared career, one shared lab, up to four chemists.",
        |panel| {
            if steam.is_some() {
                panel.spawn(choice(
                    "Host a lab",
                    "Choose a career and open a friends-only Steam lobby.",
                    MenuAction::ChooseHost,
                ));
            } else {
                panel.spawn(disabled_choice(
                    "Host a lab — Steam unavailable",
                    "Start Steam and relaunch ChemGame to open a lobby.",
                ));
            }
            panel.spawn(choice(
                "Join a lab",
                "Accept a Steam invite, or connect directly on a local network.",
                MenuAction::ChooseJoin,
            ));
            panel.spawn(choice("Back", "Return to the main menu.", MenuAction::Back));
        },
    );
}

fn show_settings_screen(mut commands: Commands, settings: Res<Settings>) {
    menu_panel_shell(
        &mut commands,
        MenuRoot,
        "Settings",
        settings::SETTINGS_SUBTITLE,
        |panel| {
            settings::settings_body(panel, &settings);
            panel.spawn(choice(
                "Controls",
                "What every key does, and how to change it.",
                MenuAction::OpenControls,
            ));
            panel.spawn(choice(
                "Restore defaults",
                "Puts every dial and display option on this screen back where it shipped.",
                MenuAction::RestoreDefaults,
            ));
            panel.spawn(choice("Back", "", MenuAction::Back));
        },
    );
}

fn show_controls_screen(
    mut commands: Commands,
    settings: Res<Settings>,
    rebinding: Res<Rebinding>,
) {
    menu_panel_shell(
        &mut commands,
        MenuRoot,
        "Controls",
        settings::CONTROLS_SUBTITLE,
        |panel| {
            settings::controls_body(panel, &settings, &rebinding);
            panel.spawn(choice(
                "Restore bindings",
                "Puts every key on this screen back where it shipped.",
                MenuAction::RestoreBindings,
            ));
            panel.spawn(choice("Back", "", MenuAction::Back));
        },
    );
}

fn show_credits_screen(mut commands: Commands) {
    menu_panel_shell(
        &mut commands,
        MenuRoot,
        "Credits & licenses",
        "Verified third-party attribution. Scroll with the wheel, Page Up/Down, or controller bumpers/right stick.",
        |panel| {
            panel
                .spawn((
                    Node {
                        width: percent(100),
                        max_height: vh(68),
                        padding: UiRect::right(px(12)),
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                    ScrollPosition::default(),
                    crate::ui::ScrollPane,
                ))
                .with_children(|credits| {
                    credits.spawn((
                        Text::new(credits_text()),
                        TextFont::from_font_size(13.0),
                        TextColor(TEXT_DIM),
                        TextLayout {
                            linebreak: LineBreak::WordBoundary,
                            ..default()
                        },
                        Node {
                            width: percent(100),
                            ..default()
                        },
                    ));
                });
            panel.spawn(choice("Back", "Return to the main menu.", MenuAction::Back));
        },
    );
}

/// Turns the repository's single attribution source into readable menu text.
/// Keeping this derived avoids a second credits list silently going stale.
fn credits_text() -> String {
    let mut out = String::new();
    let mut skip_original_work = false;
    for raw in include_str!("../../CREDITS.md").lines() {
        let line = raw.trim();
        if line.starts_with("## Artwork and models") {
            skip_original_work = true;
            continue;
        }
        if skip_original_work && line.starts_with("## ") {
            skip_original_work = false;
        }
        if skip_original_work {
            continue;
        }
        if line.is_empty() {
            out.push('\n');
            continue;
        }
        if line.starts_with("|---") || line.starts_with("| File ") {
            continue;
        }
        if line.starts_with('|') {
            let fields: Vec<_> = line
                .trim_matches('|')
                .split('|')
                .map(|field| clean_credit_markdown(field.trim()))
                .collect();
            if let Some((file, details)) = fields.split_first() {
                out.push_str(file);
                out.push('\n');
                out.push_str("  ");
                out.push_str(&details.join(" - "));
                out.push_str("\n\n");
            }
            continue;
        }
        let heading = line.trim_start_matches('#').trim();
        if heading == "Third-party asset credits" {
            continue;
        }
        if line.starts_with('#') {
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(&heading.to_uppercase());
            out.push_str("\n\n");
        } else {
            out.push_str(&clean_credit_markdown(line));
            out.push('\n');
        }
    }
    crate::ui::font_safe_text(out)
}

fn clean_credit_markdown(input: &str) -> String {
    let mut text = input.replace("**", "").replace('`', "");
    while let Some(open) = text.find('[') {
        let Some(label_end) = text[open + 1..].find("](").map(|at| open + 1 + at) else {
            break;
        };
        let url_start = label_end + 2;
        let Some(url_end) = text[url_start..].find(')').map(|at| url_start + at) else {
            break;
        };
        let label = text[open + 1..label_end].to_string();
        let url = text[url_start..url_end].to_string();
        text.replace_range(open..=url_end, &format!("{label} ({url})"));
    }
    text
}

fn show_save_screen(
    mut commands: Commands,
    pending_mode: Res<PendingMode>,
    pending_delete: Res<PendingDelete>,
) {
    render_save_screen(&mut commands, pending_mode.0, &pending_delete);
}

/// The Save screen's content, factored out of the `OnEnter` system above so
/// `handle_menu_clicks` can rebuild it in place after a delete-flow click —
/// arming or clearing a confirmation is not a `MenuScreen` transition, so
/// there is no `OnEnter` to re-run.
fn render_save_screen(commands: &mut Commands, mode: LaunchMode, pending_delete: &PendingDelete) {
    let (title, subtitle) = match mode {
        LaunchMode::HostSteam => (
            "Host a career",
            "The career you choose becomes the shared Steam lab.",
        ),
        _ => (
            "Solo careers",
            "Choose an existing career or begin a new one.",
        ),
    };
    let slots = saves::list_slots();

    menu_panel_shell(commands, MenuRoot, title, subtitle, |panel| {
        panel
            .spawn((
                Node {
                    width: percent(100),
                    max_height: vh(68),
                    padding: UiRect::right(px(10)),
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
                ScrollPosition::default(),
                crate::ui::ScrollPane,
            ))
            .with_children(|list| {
                list.spawn(choice(
                    "New career",
                    "Start over: shift 1, an empty notebook.",
                    MenuAction::NewSave,
                ));
                #[cfg(debug_assertions)]
                list.spawn(choice(
                    "New antagonist test save",
                    "Testing only: start a fresh chemist career against a chosen antagonist.",
                    MenuAction::NewAntagonistTestSave,
                ));

                if slots.is_empty() {
                    list.spawn(label("No saved games yet.", 14.0, TEXT_DIM));
                } else {
                    for slot in slots {
                        if pending_delete.0.as_deref() == Some(slot.name.as_str()) {
                            delete_confirmation_card(list, &slot.name);
                            continue;
                        }

                        if slot.evacuated {
                            list.spawn(disabled_choice(&slot.name, &slot.detail()));
                        } else {
                            list.spawn(choice(
                                slot.name.clone(),
                                &slot.detail(),
                                MenuAction::LoadSave(slot.name.clone()),
                            ));
                        }
                        list.spawn(row()).with_children(|row| {
                            row.spawn(button(
                                "Delete save",
                                MenuAction::RequestDeleteSave(slot.name.clone()),
                            ));
                        });
                    }
                }
            });

        panel.spawn(row()).with_children(|row| {
            row.spawn(button("Back", MenuAction::Back));
        });
    });
}

/// Rebuilds the Save screen when a delete confirmation is armed or cleared.
///
/// Arming/clearing `PendingDelete` is not a `MenuScreen` transition, so there
/// is no `OnEnter` to redraw the list — the same "rebuild on structure
/// change" idiom `settings::sync_pause_overlay` already documents for its own
/// screen. Gated to `MenuScreen::Save` in `MenuPlugin::build`, so this never
/// runs (and never touches `saves::list_slots()`'s real disk read) anywhere
/// `PendingDelete` cannot possibly be relevant.
fn refresh_save_screen(
    mut commands: Commands,
    pending_mode: Res<PendingMode>,
    pending_delete: Res<PendingDelete>,
    roots: Query<Entity, With<MenuRoot>>,
) {
    if !pending_delete.is_changed() {
        return;
    }
    for root in &roots {
        commands.entity(root).try_despawn();
    }
    render_save_screen(&mut commands, pending_mode.0, &pending_delete);
}

/// Replaces one slot's row while its deletion awaits confirmation. A separate
/// card rather than a repurposed load button: a destructive action must never
/// share a hitbox with the everyday one.
fn delete_confirmation_card(panel: &mut ChildSpawnerCommands, name: &str) {
    panel
        .spawn((
            Node {
                width: percent(100),
                flex_direction: FlexDirection::Column,
                row_gap: px(6),
                padding: UiRect::all(px(10)),
                margin: UiRect::vertical(px(3)),
                border_radius: BorderRadius::all(px(5)),
                ..default()
            },
            BackgroundColor(SECTION_BG),
        ))
        .with_children(|card| {
            card.spawn(label(format!("Delete '{name}'?"), 15.0, ERROR_TEXT));
            card.spawn(label(
                "Its notebook, career, and live lab are gone for good.",
                12.0,
                TEXT_DIM,
            ));
            card.spawn(row()).with_children(|row| {
                row.spawn(button(
                    "Delete forever",
                    MenuAction::ConfirmDeleteSave(name.to_string()),
                ));
                row.spawn(button("Cancel", MenuAction::CancelDeleteSave));
            });
        });
}

/// Which side to play a new save from.
///
/// Only reached once at least one antagonist has been thwarted — see the
/// `MenuAction::NewSave` arm in [`handle_menu_clicks`]. A first-time player
/// never sees this screen at all, because seeing it would give away that
/// there is something behind the ordinary game to find.
fn show_campaign_screen(mut commands: Commands) {
    let unlocked = saves::thwarted_antags();

    menu_panel_shell(
        &mut commands,
        MenuRoot,
        "A new career",
        "You have stopped one of them before. That door swings both ways now.",
        |panel| {
            panel.spawn(choice(
                "Chemistry lab",
                "Work the counter. Something is moving on the station and nobody \
                 has told you what.",
                MenuAction::NewChemistRun,
            ));

            for antag in unlocked {
                panel.spawn(choice(
                    format!("Work for {}", antag.label()),
                    "You know exactly what they want, because you stopped them \
                     doing it once. Supply them, and keep Security off your back \
                     long enough for it to matter.",
                    MenuAction::NewAntagonistRun(antag),
                ));
            }

            panel.spawn(row()).with_children(|row| {
                row.spawn(button("Back", MenuAction::Back));
            });
        },
    );
}

/// Deterministic campaign selection for development and manual testing.
///
/// This is deliberately a separate screen from antagonist-mode unlocks: these
/// buttons create an ordinary chemist campaign and do not grant, consume or
/// imply any cross-save unlock. The whole function is absent from release
/// builds, so the normal new-save flow cannot reveal the hidden roster.
#[cfg(debug_assertions)]
fn show_antagonist_test_screen(mut commands: Commands) {
    menu_panel_shell(
        &mut commands,
        MenuRoot,
        "Antagonist test save",
        "DEBUG TESTING — choose the hidden antagonist for a new chemist career.",
        |panel| {
            for antag in AntagId::ALL {
                panel.spawn(choice(
                    antag.label(),
                    "Force this campaign without changing antagonist-run unlocks.",
                    MenuAction::NewTestChemistRun(antag),
                ));
            }
            panel.spawn(row()).with_children(|row| {
                row.spawn(button("Back", MenuAction::Back));
            });
        },
    );
}

fn show_join_screen(
    mut commands: Commands,
    mut input: ResMut<AddressInput>,
    error: Res<ConnectError>,
) {
    // Prefilled, because on a home network it is the same address every time
    // and retyping it is the worst part of joining.
    if input.text.is_empty() {
        if let Some(remembered) = saves::remembered_host() {
            input.text = remembered;
        }
    }
    let typed = input.text.clone();

    menu_panel_shell(
        &mut commands,
        MenuRoot,
        "Join a lab",
        "Steam invitations connect automatically. Direct addresses are for LAN and development hosts.",
        |panel| {
            panel
                .spawn((
                    Node {
                        width: percent(100),
                        padding: UiRect::all(px(12)),
                        margin: UiRect::bottom(px(6)),
                        flex_direction: FlexDirection::Column,
                        row_gap: px(3),
                        border: UiRect::left(px(3)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.08, 0.24, 0.31, 0.86)),
                    BorderColor::all(Color::srgb(0.30, 0.72, 0.92)),
                ))
                .with_children(|steam| {
                    steam.spawn(label("JOIN THROUGH STEAM", 13.0, TEXT));
                    steam.spawn(label(
                        "Accept an invite or choose Join Game from the Steam friends list.",
                        13.0,
                        TEXT_DIM,
                    ));
                });
            if let Some(reason) = &error.0 {
                panel.spawn(label(
                    format!("Could not connect: {reason}"),
                    13.0,
                    ERROR_TEXT,
                ));
            }
            panel.spawn(label("DIRECT / LAN", 13.0, TEXT_DIM));
            panel
                .spawn((
                    Node {
                        padding: UiRect::axes(px(12), px(10)),
                        border_radius: BorderRadius::all(px(5)),
                        flex_direction: FlexDirection::Column,
                        row_gap: px(6),
                        margin: UiRect::vertical(px(6)),
                        ..default()
                    },
                    BackgroundColor(SECTION_BG),
                ))
                .with_children(|field| {
                    field.spawn((label(field_text(&typed), 20.0, TEXT), AddressField));
                    field.spawn((label(hint_for(&typed), 13.0, TEXT_DIM), AddressHint));
                });

            panel.spawn(label(
                "Type the address and press Enter. The port is optional.",
                13.0,
                TEXT_DIM,
            ));

            panel.spawn(row()).with_children(|row| {
                row.spawn(button("Connect", MenuAction::Connect));
                row.spawn(button("Back", MenuAction::Back));
            });
        },
    );
}

/// The waiting-room screen for `AppState::Connecting`.
///
/// No lab, no gameplay systems, just this — the whole point of the state (see
/// its doc comment on `AppState`) is that a slow or doomed handshake costs
/// nothing more than sitting here with a Cancel button.
fn show_connecting_screen(mut commands: Commands, mode: Res<LaunchMode>) {
    let detail = match *mode {
        LaunchMode::Join(address) => format!("Dialling {address}…"),
        LaunchMode::JoinSteam(_) => "Joining over Steam…".to_string(),
        // Host/HostSteam/Singleplayer never reach this screen; kept only so
        // the match is exhaustive rather than a state nothing should show.
        _ => "Connecting…".to_string(),
    };

    menu_panel_shell(&mut commands, MenuRoot, "Joining a lab", &detail, |panel| {
        panel.spawn(row()).with_children(|row| {
            row.spawn(button("Cancel", MenuAction::Cancel));
        });
    });
}

/// The field's contents, with a caret so an empty field still looks typeable.
fn field_text(typed: &str) -> String {
    format!("{typed}_")
}

/// Says how what has been typed will actually be dialled.
///
/// Shown live rather than on submit because the interesting part is the port
/// being filled in, and because "that is not an address yet" is worth knowing
/// before clicking rather than after a silent failure to connect.
///
/// Uses [`parse_literal_address`], not the full [`parse_address`]: this runs
/// on every keystroke, and `parse_address`'s DNS fallback is a blocking
/// `getaddrinfo` call that would otherwise stall the whole app while typing a
/// hostname one character at a time. A hostname's hint stays "not an address
/// yet" until Connect actually resolves it — that one blocking call, on a
/// deliberate click, is fine.
fn hint_for(typed: &str) -> String {
    if typed.trim().is_empty() {
        return "for example 192.168.1.40".to_string();
    }
    match parse_literal_address(typed.trim()) {
        Some(address) => format!("connects to {address}"),
        None => "not an address yet".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Collects typing into [`AddressInput`].
///
/// Driven by the keypress's own text rather than by key codes so the layout the
/// player actually has decides what a key produces — a numpad dot and a laptop
/// dot both arrive as `.`.
fn type_address(
    mut typed: MessageReader<KeyboardInput>,
    mut input: ResMut<AddressInput>,
    mut screen: ResMut<NextState<MenuScreen>>,
) {
    for key in typed.read() {
        if key.state != ButtonState::Pressed {
            continue;
        }
        match key.key_code {
            KeyCode::Backspace => {
                input.text.pop();
            }
            KeyCode::Enter | KeyCode::NumpadEnter => input.submitted = true,
            KeyCode::Escape => screen.set(MenuScreen::Mode),
            _ => {
                let Some(text) = &key.text else {
                    continue;
                };
                // Everything an address can contain and nothing else, which is
                // also what stops Tab and friends arriving as whitespace in the
                // middle of an IP.
                let allowed: String = text
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_'))
                    .collect();
                // Long enough for `some-machine.local:5327`. Unbounded is a way
                // to make the layout jump.
                for character in allowed.chars() {
                    if input.text.len() < 64 {
                        input.text.push(character);
                    }
                }
            }
        }
    }
}

/// Keeps the field and its hint in step with what has been typed.
fn show_typed_address(
    input: Res<AddressInput>,
    mut fields: Query<&mut Text, (With<AddressField>, Without<AddressHint>)>,
    mut hints: Query<&mut Text, (With<AddressHint>, Without<AddressField>)>,
) {
    if !input.is_changed() {
        return;
    }
    for mut field in &mut fields {
        field.0 = field_text(&input.text);
    }
    for mut hint in &mut hints {
        hint.0 = hint_for(&input.text);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_menu_clicks(
    buttons: Query<(&Interaction, &MenuAction), Changed<Interaction>>,
    mut commands: Commands,
    current: Res<State<MenuScreen>>,
    mut screen: ResMut<NextState<MenuScreen>>,
    mut app_state: ResMut<NextState<AppState>>,
    mut mode: ResMut<LaunchMode>,
    mut pending: ResMut<PendingMode>,
    mut pending_delete: ResMut<PendingDelete>,
    mut input: ResMut<AddressInput>,
    mut connect_error: ResMut<ConnectError>,
    mut return_to: ResMut<MenuReturn>,
    mut settings: ResMut<Settings>,
    mut quit: MessageWriter<AppExit>,
) {
    // Enter on the join screen is the same action as clicking Connect, so it
    // arrives here rather than growing a second copy of the connect path.
    let submitted = input.submitted;
    if submitted {
        // Bypassed so clearing the flag does not read as "the address changed"
        // and rebuild the field's text every frame.
        input.bypass_change_detection().submitted = false;
    }
    let clicked = buttons
        .iter()
        .filter(|(interaction, _)| **interaction == Interaction::Pressed)
        .map(|(_, action)| action.clone());

    for action in clicked.chain(submitted.then_some(MenuAction::Connect)) {
        match action {
            MenuAction::ChooseSolo => {
                pending.0 = LaunchMode::Singleplayer;
                screen.set(MenuScreen::Save);
            }
            MenuAction::ChooseHost => {
                // Steam, not the direct/LAN transport `LaunchMode::Host`
                // still drives — that one stays reachable only via `--host`
                // now, which is how co-op gets tested from one terminal
                // without Steam running (see `net::steam`'s module doc).
                pending.0 = LaunchMode::HostSteam;
                screen.set(MenuScreen::Save);
            }
            MenuAction::ChooseJoin => {
                connect_error.0 = None;
                screen.set(MenuScreen::Join);
            }
            MenuAction::OpenMultiplayer => screen.set(MenuScreen::Multiplayer),
            MenuAction::OpenTraining => screen.set(MenuScreen::Training),
            // With nothing unlocked there is only one kind of new save, and a
            // one-option screen asking which kind you want would both waste a
            // click and imply the existence of something the player has not
            // earned yet.
            MenuAction::NewSave => {
                if saves::thwarted_antags().is_empty() {
                    start(
                        &mut commands,
                        SaveSlot::new(saves::next_slot_name()),
                        pending.0,
                        None,
                        &mut mode,
                        &mut app_state,
                        &mut screen,
                    );
                } else {
                    screen.set(MenuScreen::Campaign);
                }
            }
            #[cfg(debug_assertions)]
            MenuAction::NewAntagonistTestSave => screen.set(MenuScreen::AntagonistTest),
            #[cfg(debug_assertions)]
            MenuAction::NewTestChemistRun(antag) => start(
                &mut commands,
                SaveSlot::new(saves::next_slot_name()),
                pending.0,
                Some(CampaignChoice {
                    mode: Mode::Chemist,
                    antag: Some(antag),
                }),
                &mut mode,
                &mut app_state,
                &mut screen,
            ),
            MenuAction::NewChemistRun => start(
                &mut commands,
                SaveSlot::new(saves::next_slot_name()),
                pending.0,
                // No forced antagonist: `arc::assign_campaign` rolls a hidden
                // one, preferring whoever this machine has not beaten.
                Some(CampaignChoice {
                    mode: Mode::Chemist,
                    antag: None,
                }),
                &mut mode,
                &mut app_state,
                &mut screen,
            ),
            MenuAction::NewAntagonistRun(antag) => start(
                &mut commands,
                SaveSlot::new(saves::next_slot_name()),
                pending.0,
                // Named, not rolled — you are choosing who you are working
                // for, so there is nothing to hide.
                Some(CampaignChoice {
                    mode: Mode::Antagonist,
                    antag: Some(antag),
                }),
                &mut mode,
                &mut app_state,
                &mut screen,
            ),
            MenuAction::LoadSave(name) => {
                // Defense-in-depth: `show_save_screen` already dims an
                // evacuated save's row and does not attach a working
                // `MenuAction`, but a click is trusted input from this
                // client's own UI, not the network — re-checking here means
                // a stale or forged one still cannot bypass the gate.
                let slot = SaveSlot::new(name);
                if crate::shift::is_evacuated(&slot.progress_path()) {
                    continue;
                }
                start(
                    &mut commands,
                    slot,
                    pending.0,
                    // The save carries its own campaign; forcing one here
                    // would overwrite the arc already in progress.
                    None,
                    &mut mode,
                    &mut app_state,
                    &mut screen,
                );
            }
            MenuAction::Connect => {
                let typed = input.text.trim().to_string();
                let Some(address) = parse_address(&typed) else {
                    // The hint under the field already says why; refusing the
                    // click beats dialling something that cannot answer.
                    continue;
                };
                // Remembering the address is `start_joining`'s job, so only one
                // that got as far as opening a socket is offered back.
                //
                // No save slot: a guest reads the host's notebook and career,
                // and writing either locally would be writing someone else's.
                *mode = LaunchMode::Join(address);
                // Not `Playing` — `open_connecting`'s own `OnEnter` owns the
                // screen from here, the same way `open_menu` alone decides
                // the screen on entering `MainMenu`. The lab is not built,
                // and the full simulation does not run, until the handshake
                // actually finishes; see `AppState::Connecting`'s doc comment.
                app_state.set(AppState::Connecting);
            }
            MenuAction::Cancel => {
                net::abandon_connection_attempt(&mut commands, *mode);
                return_to.0 = Some(MenuScreen::Multiplayer);
                app_state.set(AppState::MainMenu);
            }
            // Back goes up one level, not all the way out: the campaign
            // screen is reached *through* the save list and Controls
            // *through* Settings, so leaving either should land one level up
            // rather than all the way back at Mode.
            MenuAction::Back => screen.set(match current.get() {
                MenuScreen::Campaign => MenuScreen::Save,
                #[cfg(debug_assertions)]
                MenuScreen::AntagonistTest => MenuScreen::Save,
                MenuScreen::Controls => MenuScreen::Settings,
                MenuScreen::Save if pending.0 == LaunchMode::HostSteam => MenuScreen::Multiplayer,
                MenuScreen::Join => MenuScreen::Multiplayer,
                _ => MenuScreen::Mode,
            }),
            MenuAction::Quit => {
                quit.write(AppExit::Success);
            }
            MenuAction::OpenSettings => screen.set(MenuScreen::Settings),
            MenuAction::OpenControls => screen.set(MenuScreen::Controls),
            MenuAction::OpenCredits => screen.set(MenuScreen::Credits),
            MenuAction::RestoreDefaults => settings::restore_settings_defaults(&mut settings),
            MenuAction::RestoreBindings => settings::restore_bindings_defaults(&mut settings),
            // These three only mutate `PendingDelete`; `refresh_save_screen`
            // (its own system, gated to `MenuScreen::Save`) is what actually
            // redraws the list to match — the same split `settings_body`'s
            // sliders use between the system that decides a value and the one
            // that patches the screen onto it.
            MenuAction::RequestDeleteSave(name) => {
                pending_delete.0 = Some(name);
            }
            MenuAction::CancelDeleteSave => {
                pending_delete.0 = None;
            }
            MenuAction::ConfirmDeleteSave(name) => {
                if let Err(error) = SaveSlot::new(name.clone()).delete() {
                    warn!("could not delete save '{name}': {error}");
                }
                pending_delete.0 = None;
            }
        }
    }
}

/// Bounces a failed or timed-out join attempt back to the mode screen with a
/// reason on display, instead of leaving `Connecting` with nothing watching.
fn handle_connect_failure(
    mut failed: MessageReader<ConnectFailed>,
    mut commands: Commands,
    mode: Res<LaunchMode>,
    mut app_state: ResMut<NextState<AppState>>,
    mut error: ResMut<ConnectError>,
    mut return_to: ResMut<MenuReturn>,
) {
    // `.last()`: if several arrive the same frame, only the final word matters
    // — but the read still has to drain the whole reader, or an earlier one
    // would still look unread next frame and fire this again for nothing.
    let Some(failure) = failed.read().last() else {
        return;
    };
    net::abandon_connection_attempt(&mut commands, *mode);
    error.0 = Some(failure.reason.clone());
    return_to.0 = Some(MenuScreen::Join);
    app_state.set(AppState::MainMenu);
}

/// Opens a save in whichever mode was picked on the first screen.
///
/// The mode and the save are independent on purpose: this is what lets a career
/// played alone be hosted later, and a hosted one be finished alone.
fn start(
    commands: &mut Commands,
    slot: SaveSlot,
    pending: LaunchMode,
    campaign: Option<CampaignChoice>,
    mode: &mut LaunchMode,
    app_state: &mut NextState<AppState>,
    screen: &mut NextState<MenuScreen>,
) {
    info!("opening save '{}'", slot.name());
    commands.insert_resource(crate::session::SessionKind::Career);
    commands.insert_resource(slot);
    // Only for a brand new save. Loading one leaves this absent, so
    // `arc::assign_campaign` sees the campaign `shift::load_progress` restored
    // and never rolls over the top of it.
    if let Some(campaign) = campaign {
        commands.insert_resource(campaign);
    }
    *mode = pending;
    app_state.set(AppState::Playing);
    screen.set(MenuScreen::Hidden);
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

const MENU_RAIL_BG: Color = Color::srgba(0.035, 0.047, 0.061, 0.94);
const MENU_SCRIM: Color = Color::srgba(0.008, 0.012, 0.018, 0.34);
const MENU_ACCENT: Color = Color::srgb(0.30, 0.72, 0.92);

/// The deliberately asymmetric landing page: a quiet work-order rail over the
/// live lab, not a dialog box floating in the middle of an empty screen.
fn landing_shell(commands: &mut Commands, body: impl FnOnce(&mut ChildSpawnerCommands)) {
    commands
        .spawn((
            MenuRoot,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                ..default()
            },
            BackgroundColor(MENU_SCRIM),
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: vw(42),
                        min_width: px(410),
                        max_width: px(590),
                        height: percent(100),
                        padding: UiRect::axes(px(48), px(42)),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::SpaceBetween,
                        border: UiRect::right(px(1)),
                        ..default()
                    },
                    BackgroundColor(MENU_RAIL_BG),
                    BorderColor::all(Color::srgba(0.30, 0.72, 0.92, 0.28)),
                ))
                .with_children(|rail| {
                    rail.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: px(3),
                        ..default()
                    })
                    .with_children(|brand| {
                        brand.spawn((
                            Text::new("CHEMGAME"),
                            TextFont::from_font_size(42.0),
                            TextColor(TEXT),
                        ));
                        brand.spawn((
                            Node {
                                width: px(112),
                                height: px(3),
                                margin: UiRect::vertical(px(7)),
                                ..default()
                            },
                            BackgroundColor(MENU_ACCENT),
                        ));
                        brand.spawn(label("STATION CHEMISTRY DIVISION", 12.0, MENU_ACCENT));
                        brand.spawn(label("A shift in the chemistry lab.", 14.0, TEXT_DIM));
                    });
                    rail.spawn(Node {
                        width: percent(100),
                        flex_direction: FlexDirection::Column,
                        row_gap: px(7),
                        ..default()
                    })
                    .with_children(body);
                    rail.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: px(3),
                        ..default()
                    })
                    .with_children(|footer| {
                        footer.spawn(label(
                            "ARROWS / D-PAD  NAVIGATE     ENTER / A  SELECT",
                            10.0,
                            TEXT_DIM,
                        ));
                        footer.spawn(label(
                            format!("BUILD {}  /  SELECT A SHIFT", env!("CARGO_PKG_VERSION")),
                            11.0,
                            TEXT_DIM,
                        ));
                    });
                });

            screen
                .spawn(button("Credits", MenuAction::OpenCredits))
                .insert(Node {
                    position_type: PositionType::Absolute,
                    right: px(28),
                    bottom: px(24),
                    padding: UiRect::axes(px(16), px(8)),
                    ..default()
                });
        });
}

fn landing_choice(title: &str, detail: &str, action: MenuAction) -> impl Bundle {
    (
        Button,
        Node {
            width: percent(100),
            min_height: px(62),
            padding: UiRect::axes(px(18), px(10)),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexStart,
            justify_content: JustifyContent::Center,
            row_gap: px(1),
            border: UiRect::left(px(4)),
            border_radius: BorderRadius::right(px(5)),
            ..default()
        },
        BackgroundColor(Color::srgb(0.13, 0.16, 0.21)),
        BorderColor::all(Color::srgba(0.30, 0.72, 0.92, 0.48)),
        crate::ui::ButtonTone::Utility,
        action,
        children![
            (
                Text::new(title.to_uppercase()),
                TextFont::from_font_size(19.0),
                TextColor(TEXT),
            ),
            (
                Text::new(detail.to_string()),
                TextFont::from_font_size(12.0),
                TextColor(TEXT_DIM),
            ),
        ],
    )
}

fn disabled_choice(title: &str, detail: &str) -> impl Bundle {
    (
        Node {
            width: percent(100),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexStart,
            row_gap: px(2),
            padding: UiRect::axes(px(14), px(10)),
            margin: UiRect::vertical(px(3)),
            border: UiRect::left(px(3)),
            border_radius: BorderRadius::all(px(5)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.08, 0.09, 0.11, 0.90)),
        BorderColor::all(Color::srgba(0.85, 0.35, 0.35, 0.55)),
        children![
            (
                Text::new(crate::ui::font_safe_text(title)),
                TextFont::from_font_size(17.0),
                TextColor(TEXT_DIM),
            ),
            (
                Text::new(detail.to_string()),
                TextFont::from_font_size(13.0),
                TextColor(ERROR_TEXT),
            ),
        ],
    )
}

/// The main-menu work surface used by every screen below the landing page.
pub(crate) fn menu_panel_shell(
    commands: &mut Commands,
    root: impl Bundle,
    title: &str,
    subtitle: &str,
    body: impl FnOnce(&mut ChildSpawnerCommands),
) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                padding: UiRect::left(vw(4)),
                ..default()
            },
            BackgroundColor(MENU_SCRIM),
            root,
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: vw(52),
                        min_width: px(480),
                        max_width: px(760),
                        max_height: vh(92),
                        padding: UiRect::all(px(30)),
                        flex_direction: FlexDirection::Column,
                        row_gap: px(10),
                        border: UiRect {
                            left: px(3),
                            right: px(1),
                            top: px(1),
                            bottom: px(1),
                        },
                        border_radius: BorderRadius::right(px(8)),
                        ..default()
                    },
                    BackgroundColor(MENU_RAIL_BG),
                    BorderColor::all(Color::srgba(0.30, 0.72, 0.92, 0.46)),
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new(crate::ui::font_safe_text(title)),
                        TextFont::from_font_size(30.0),
                        TextColor(TEXT),
                    ));
                    panel.spawn(label(subtitle, 14.0, TEXT_DIM));
                    panel.spawn((
                        Node {
                            width: percent(100),
                            height: px(2),
                            margin: UiRect::vertical(px(3)),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.30, 0.72, 0.92, 0.38)),
                    ));
                    body(panel);
                });
        });
}

/// The frame every screen shares: title, subtitle, and a column of controls.
///
/// `root` is whatever marks the screen for teardown — [`MenuRoot`] here, and
/// its own marker for the in-game pause menu, which reuses this shell so the
/// two never drift apart visually.
pub(crate) fn menu_shell(
    commands: &mut Commands,
    root: impl Bundle,
    title: &str,
    subtitle: &str,
    body: impl FnOnce(&mut ChildSpawnerCommands),
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
            BackgroundColor(PANEL_BG),
            root,
        ))
        .with_children(|screen| {
            screen
                .spawn(Node {
                    width: px(520),
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(px(24)),
                    row_gap: px(10),
                    ..default()
                })
                .with_children(|panel| {
                    panel.spawn(heading(title));
                    panel.spawn(label(subtitle, 14.0, TEXT_DIM));
                    // A thin rule between the subtitle and the body, so the
                    // title block reads as its own group rather than just the
                    // first two rows of the same list as everything under it.
                    panel.spawn((
                        Node {
                            width: percent(100),
                            height: px(1),
                            margin: UiRect::vertical(px(2)),
                            ..default()
                        },
                        BackgroundColor(SECTION_BG),
                    ));
                    body(panel);
                });
        });
}

/// A full-width button with a line of explanation under it.
///
/// Wider and taller than a panel button because this is the one screen the
/// player uses with a free cursor and no hurry.
///
/// Generic over the action for the same reason [`menu_shell`] is generic over
/// its root: the pause menu wants exactly this button with an action type of
/// its own, and two copies would drift.
pub(crate) fn choice<A: Component>(
    title: impl Into<String>,
    detail: &str,
    action: A,
) -> impl Bundle {
    (
        Button,
        Node {
            width: percent(100),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexStart,
            row_gap: px(2),
            padding: UiRect::axes(px(14), px(10)),
            margin: UiRect::vertical(px(3)),
            border_radius: BorderRadius::all(px(5)),
            ..default()
        },
        BackgroundColor(BUTTON_IDLE),
        action,
        children![
            (
                Text::new(title.into()),
                TextFont::from_font_size(17.0),
                TextColor(TEXT),
            ),
            (
                Text::new(detail.to_string()),
                TextFont::from_font_size(13.0),
                TextColor(TEXT_DIM),
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use bevy::input::keyboard::{Key, NativeKey};
    use bevy::state::app::StatesPlugin;

    use super::*;

    /// The menu's decision-making without a window, a camera or a renderer.
    ///
    /// Deliberately not exercising the two paths that touch the disk — writing
    /// into the player's real `saves/` from a test run is the trap
    /// `ProgressPlugin` was split out to avoid. Everything up to the write is
    /// here; `saves` covers the layout itself.
    fn menu_app() -> App {
        let mut app = App::new();
        app.add_plugins(StatesPlugin)
            .init_state::<AppState>()
            .init_state::<MenuScreen>()
            .init_resource::<AddressInput>()
            .init_resource::<PendingMode>()
            .init_resource::<PendingDelete>()
            .init_resource::<LaunchMode>()
            .init_resource::<ConnectError>()
            .init_resource::<MenuReturn>()
            .init_resource::<Settings>()
            .add_message::<AppExit>()
            .add_message::<KeyboardInput>()
            .add_message::<ConnectFailed>()
            .add_systems(
                Update,
                (type_address, handle_menu_clicks, handle_connect_failure).chain(),
            );
        app
    }

    /// Clicks a button, the way the UI would.
    ///
    /// `Interaction` counts as changed the frame it is spawned, which is what
    /// `handle_menu_clicks` filters on. Twice, because `StateTransition` runs
    /// before `Update`: a state set while handling the click is not the current
    /// state until the following frame.
    fn click(app: &mut App, action: MenuAction) {
        app.world_mut().spawn((Interaction::Pressed, action));
        app.update();
        app.update();
    }

    fn screen(app: &App) -> MenuScreen {
        *app.world().resource::<State<MenuScreen>>().get()
    }

    fn state(app: &App) -> AppState {
        *app.world().resource::<State<AppState>>().get()
    }

    /// Types a line, one keypress per character.
    fn type_line(app: &mut App, line: &str) {
        let window = app.world_mut().spawn_empty().id();
        for character in line.chars() {
            let text = character.to_string();
            app.world_mut().write_message(KeyboardInput {
                key_code: KeyCode::KeyA,
                logical_key: Key::Character(text.clone().into()),
                state: ButtonState::Pressed,
                text: Some(text.into()),
                repeat: false,
                window,
            });
        }
        app.update();
    }

    #[test]
    fn host_and_solo_both_lead_to_the_same_save_screen() {
        // The whole point of separating the two questions: a career is a
        // career, and how many people are working it is a different decision.
        for (chosen, expected) in [
            (MenuAction::ChooseSolo, LaunchMode::Singleplayer),
            (MenuAction::ChooseHost, LaunchMode::HostSteam),
        ] {
            let mut app = menu_app();
            app.world_mut()
                .resource_mut::<NextState<MenuScreen>>()
                .set(MenuScreen::Mode);
            app.update();

            click(&mut app, chosen);
            assert_eq!(screen(&app), MenuScreen::Save);
            assert_eq!(
                *app.world().resource::<LaunchMode>(),
                LaunchMode::Singleplayer,
                "picking a mode must not commit to it until a save is chosen"
            );
            assert_eq!(app.world().resource::<PendingMode>().0, expected);
        }
    }

    #[test]
    fn a_save_started_solo_can_be_hosted() {
        // The requirement in one test: the same slot, opened in either mode,
        // and nothing about the save itself decides which.
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseHost);
        click(&mut app, MenuAction::LoadSave("Chemist".into()));

        assert_eq!(state(&app), AppState::Playing);
        assert_eq!(screen(&app), MenuScreen::Hidden);
        assert_eq!(*app.world().resource::<LaunchMode>(), LaunchMode::HostSteam);
        assert_eq!(
            app.world().resource::<SaveSlot>().name(),
            "Chemist",
            "the lab both chemists work is the save the host picked"
        );
    }

    // -- campaigns ---------------------------------------------------------

    #[test]
    fn an_antagonist_run_carries_its_choice_into_the_session() {
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseSolo);
        click(&mut app, MenuAction::NewAntagonistRun(AntagId::Blob));

        assert_eq!(state(&app), AppState::Playing);
        let choice = app.world().resource::<CampaignChoice>();
        assert_eq!(choice.mode, Mode::Antagonist);
        assert_eq!(
            choice.antag,
            Some(AntagId::Blob),
            "an antagonist run names who you are working for — there is nothing to hide"
        );
    }

    #[test]
    fn an_ordinary_new_save_never_names_its_antagonist() {
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseSolo);
        click(&mut app, MenuAction::NewChemistRun);

        let choice = app.world().resource::<CampaignChoice>();
        assert_eq!(choice.mode, Mode::Chemist);
        assert_eq!(
            choice.antag, None,
            "the menu must not decide, or the whole reveal is spoiled before the lab loads"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn every_debug_antagonist_choice_starts_an_ordinary_chemist_campaign() {
        for antag in AntagId::ALL {
            let mut app = menu_app();
            click(&mut app, MenuAction::ChooseSolo);
            click(&mut app, MenuAction::NewTestChemistRun(antag));

            assert_eq!(state(&app), AppState::Playing);
            let choice = app.world().resource::<CampaignChoice>();
            assert_eq!(choice.mode, Mode::Chemist);
            assert_eq!(choice.antag, Some(antag));
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_picker_is_separate_from_antagonist_mode_unlocks() {
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseHost);
        click(&mut app, MenuAction::NewAntagonistTestSave);
        assert_eq!(screen(&app), MenuScreen::AntagonistTest);

        click(&mut app, MenuAction::NewTestChemistRun(AntagId::Cult));
        assert_eq!(*app.world().resource::<LaunchMode>(), LaunchMode::HostSteam);
        assert_eq!(app.world().resource::<CampaignChoice>().mode, Mode::Chemist);
    }

    #[test]
    fn loading_a_save_never_forces_a_campaign_over_the_one_it_has() {
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseSolo);
        click(&mut app, MenuAction::LoadSave("Chemist".into()));

        assert!(
            app.world().get_resource::<CampaignChoice>().is_none(),
            "a loaded save's own arc must survive being loaded"
        );
    }

    #[test]
    fn back_from_the_campaign_screen_returns_to_the_save_list() {
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseSolo);
        assert_eq!(screen(&app), MenuScreen::Save);

        // Reached directly rather than through `NewSave`, which branches on
        // what is on disk — the trap `menu_app` exists to stay clear of.
        app.world_mut()
            .resource_mut::<NextState<MenuScreen>>()
            .set(MenuScreen::Campaign);
        app.update();
        assert_eq!(screen(&app), MenuScreen::Campaign);

        click(&mut app, MenuAction::Back);

        assert_eq!(
            screen(&app),
            MenuScreen::Save,
            "back should go up one level, not all the way out"
        );
    }

    #[test]
    fn a_guest_gets_no_save_of_their_own() {
        // A joining chemist reads the host's notebook and career. Handing them
        // a slot as well would mean writing someone else's lab to their disk.
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseJoin);
        assert_eq!(screen(&app), MenuScreen::Join);

        type_line(&mut app, "192.168.1.40");
        // What pressing Enter in the field does.
        app.world_mut().resource_mut::<AddressInput>().submitted = true;
        app.update();
        app.update();

        // Not `Playing` yet — Connect only starts dialling; the lab and the
        // full simulation wait for `AppState::Connecting` to resolve. See its
        // doc comment.
        assert_eq!(state(&app), AppState::Connecting);
        assert!(
            app.world().get_resource::<SaveSlot>().is_none(),
            "a guest must not open a save of their own"
        );
        assert!(matches!(
            *app.world().resource::<LaunchMode>(),
            LaunchMode::Join(_)
        ));
    }

    #[test]
    fn an_address_that_cannot_be_dialled_is_refused() {
        // Silence is the failure mode of everything about joining, so the menu
        // is the one place that can still say no out loud.
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseJoin);
        type_line(&mut app, "the lab");

        click(&mut app, MenuAction::Connect);
        assert_eq!(
            state(&app),
            AppState::Loading,
            "an unparseable address must not start the game"
        );
        assert_eq!(screen(&app), MenuScreen::Join, "and must leave you here");
    }

    #[test]
    fn cancel_drops_the_attempt_and_returns_to_multiplayer() {
        // The escape hatch that did not exist before this feature: a doomed
        // or just slow handshake used to have no way out but force-quitting.
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseJoin);
        type_line(&mut app, "192.168.1.40");
        click(&mut app, MenuAction::Connect);
        assert_eq!(state(&app), AppState::Connecting);

        click(&mut app, MenuAction::Cancel);
        assert_eq!(state(&app), AppState::MainMenu);
        assert_eq!(
            app.world().resource::<MenuReturn>().0,
            Some(MenuScreen::Multiplayer)
        );
    }

    #[test]
    fn a_connect_failure_bounces_back_with_a_reason_on_display() {
        // Without this, a failed or timed-out handshake has nothing watching
        // it: the whole point of `ConnectFailed` is that `Connecting` never
        // just hangs with no way for the player to find out why.
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseJoin);
        type_line(&mut app, "192.168.1.40");
        click(&mut app, MenuAction::Connect);
        assert_eq!(state(&app), AppState::Connecting);

        app.world_mut().write_message(ConnectFailed {
            reason: "could not reach the host".to_string(),
        });
        app.update();
        app.update();

        assert_eq!(state(&app), AppState::MainMenu);
        assert_eq!(
            app.world().resource::<ConnectError>().0.as_deref(),
            Some("could not reach the host")
        );
        assert_eq!(
            app.world().resource::<MenuReturn>().0,
            Some(MenuScreen::Join),
            "a failed direct connection should reopen the field and its error"
        );
    }

    #[test]
    fn typing_only_accepts_what_an_address_can_contain() {
        let mut app = menu_app();
        app.world_mut()
            .resource_mut::<NextState<MenuScreen>>()
            .set(MenuScreen::Join);
        app.update();

        // A space would otherwise sit invisibly in the middle of an IP.
        type_line(&mut app, "192.168.1.40 x");
        assert_eq!(app.world().resource::<AddressInput>().text, "192.168.1.40x");

        // Hostnames and explicit ports have to survive.
        app.world_mut().resource_mut::<AddressInput>().text.clear();
        type_line(&mut app, "some-machine.local:5327");
        assert_eq!(
            app.world().resource::<AddressInput>().text,
            "some-machine.local:5327"
        );
    }

    #[test]
    fn backspace_deletes_and_the_field_has_a_limit() {
        let mut app = menu_app();
        let window = app.world_mut().spawn_empty().id();
        type_line(&mut app, "10.0.0.5");
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::Backspace,
            logical_key: Key::Unidentified(NativeKey::Unidentified),
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window,
        });
        app.update();
        assert_eq!(app.world().resource::<AddressInput>().text, "10.0.0.");

        type_line(&mut app, &"9".repeat(200));
        assert_eq!(
            app.world().resource::<AddressInput>().text.len(),
            64,
            "an unbounded field is a way to make the layout jump"
        );
    }

    #[test]
    fn the_hint_shows_the_port_being_filled_in() {
        // The port filling itself in is the part nobody expects, so the field
        // says what it will actually dial before the click rather than after.
        assert_eq!(hint_for("192.168.1.40"), "connects to 192.168.1.40:5327");
        assert_eq!(
            hint_for("192.168.1.40:9999"),
            "connects to 192.168.1.40:9999"
        );
        assert_eq!(hint_for("nonsense"), "not an address yet");
        assert_eq!(hint_for("  "), "for example 192.168.1.40");
    }

    // -- settings, reached before a save is ever chosen --------------------

    #[test]
    fn landing_routes_multiplayer_and_credits_to_their_own_screens() {
        let mut app = menu_app();
        click(&mut app, MenuAction::OpenMultiplayer);
        assert_eq!(screen(&app), MenuScreen::Multiplayer);

        click(&mut app, MenuAction::Back);
        assert_eq!(screen(&app), MenuScreen::Mode);

        click(&mut app, MenuAction::OpenCredits);
        assert_eq!(screen(&app), MenuScreen::Credits);
    }

    #[test]
    fn a_host_backtracks_to_multiplayer_not_the_landing_page() {
        let mut app = menu_app();
        click(&mut app, MenuAction::OpenMultiplayer);
        click(&mut app, MenuAction::ChooseHost);
        assert_eq!(screen(&app), MenuScreen::Save);

        click(&mut app, MenuAction::Back);
        assert_eq!(screen(&app), MenuScreen::Multiplayer);
    }

    #[test]
    fn credits_are_derived_from_third_party_attribution_only() {
        let credits = credits_text();
        assert!(credits.contains("SOUND EFFECTS - SOURCED FROM TGSTATION"));
        assert!(credits.contains("CODE - VENDORED DEPENDENCY"));
        assert!(!credits.contains("ARTWORK AND MODELS - ORIGINAL WORK"));
        assert!(credits.contains("Freesound 203281 (https://freesound.org"));
    }

    #[test]
    fn settings_is_reachable_from_the_mode_screen() {
        let mut app = menu_app();
        app.world_mut()
            .resource_mut::<NextState<MenuScreen>>()
            .set(MenuScreen::Mode);
        app.update();

        click(&mut app, MenuAction::OpenSettings);
        assert_eq!(screen(&app), MenuScreen::Settings);
    }

    #[test]
    fn controls_is_reachable_from_the_settings_screen() {
        let mut app = menu_app();
        click(&mut app, MenuAction::OpenSettings);
        assert_eq!(screen(&app), MenuScreen::Settings);

        click(&mut app, MenuAction::OpenControls);
        assert_eq!(screen(&app), MenuScreen::Controls);
    }

    #[test]
    fn back_from_controls_returns_to_settings_not_all_the_way_out() {
        let mut app = menu_app();
        click(&mut app, MenuAction::OpenSettings);
        click(&mut app, MenuAction::OpenControls);

        click(&mut app, MenuAction::Back);
        assert_eq!(screen(&app), MenuScreen::Settings);
    }

    #[test]
    fn back_from_settings_returns_to_mode() {
        let mut app = menu_app();
        click(&mut app, MenuAction::OpenSettings);

        click(&mut app, MenuAction::Back);
        assert_eq!(screen(&app), MenuScreen::Mode);
    }

    #[test]
    fn restoring_defaults_and_restoring_bindings_leave_each_other_untouched() {
        let mut app = menu_app();
        app.world_mut().resource_mut::<Settings>().fov_degrees = 90.0;
        app.world_mut().resource_mut::<Settings>().bindings.forward = KeyCode::ArrowUp;

        click(&mut app, MenuAction::RestoreDefaults);
        let settings = app.world().resource::<Settings>();
        assert_eq!(settings.fov_degrees, Settings::default().fov_degrees);
        assert_eq!(
            settings.bindings.forward,
            KeyCode::ArrowUp,
            "restoring the dials must not touch bindings"
        );

        click(&mut app, MenuAction::RestoreBindings);
        assert_eq!(
            app.world().resource::<Settings>().bindings.forward,
            Settings::default().bindings.forward
        );
    }

    // -- deleting a save -----------------------------------------------------
    //
    // Deliberately never exercises `MenuAction::ConfirmDeleteSave`: that arm
    // calls `SaveSlot::delete()` against the real `saves_root()`, exactly the
    // disk-touching trap `menu_app`'s own doc comment above says this harness
    // stays clear of. Only the pure `PendingDelete` state transitions are
    // covered here.

    #[test]
    fn requesting_a_delete_arms_the_confirmation() {
        let mut app = menu_app();
        click(&mut app, MenuAction::RequestDeleteSave("Chemist".into()));
        assert_eq!(
            app.world().resource::<PendingDelete>().0.as_deref(),
            Some("Chemist")
        );
    }

    #[test]
    fn cancelling_a_delete_request_clears_it() {
        let mut app = menu_app();
        click(&mut app, MenuAction::RequestDeleteSave("Chemist".into()));
        click(&mut app, MenuAction::CancelDeleteSave);
        assert_eq!(app.world().resource::<PendingDelete>().0, None);
    }
}
