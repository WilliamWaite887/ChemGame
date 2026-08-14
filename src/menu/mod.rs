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

use bevy::input::keyboard::KeyboardInput;
use bevy::input::ButtonState;
use bevy::prelude::*;

use crate::arc::{AntagId, CampaignChoice, Mode};
use crate::net::{self, parse_address, parse_literal_address, ConnectFailed, LaunchMode};
use crate::saves::{self, SaveSlot};
use crate::ui::{
    button, button_feedback, heading, label, row, BUTTON_IDLE, ERROR_TEXT, PANEL_BG, SECTION_BG,
    TEXT, TEXT_DIM,
};
use crate::AppState;

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
    /// New save, or one of the existing ones.
    Save,
    /// Which side to play a brand new save from. Only ever reached when at
    /// least one antagonist has been thwarted — see [`show_campaign_screen`].
    Campaign,
    /// The address to dial.
    Join,
    /// Waiting on `AppState::Connecting` to resolve — see its doc comment.
    Connecting,
}

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<MenuScreen>()
            .init_resource::<AddressInput>()
            .init_resource::<PendingMode>()
            .init_resource::<ConnectError>()
            .add_systems(OnEnter(AppState::MainMenu), open_menu)
            .add_systems(OnExit(AppState::MainMenu), close_menu)
            .add_systems(OnEnter(AppState::Connecting), open_connecting)
            .add_systems(OnExit(AppState::Connecting), close_menu)
            .add_systems(OnEnter(MenuScreen::Mode), show_mode_screen)
            .add_systems(OnEnter(MenuScreen::Save), show_save_screen)
            .add_systems(OnEnter(MenuScreen::Campaign), show_campaign_screen)
            .add_systems(OnEnter(MenuScreen::Join), show_join_screen)
            .add_systems(OnEnter(MenuScreen::Connecting), show_connecting_screen)
            .add_systems(OnExit(MenuScreen::Mode), clear_screen)
            .add_systems(OnExit(MenuScreen::Save), clear_screen)
            .add_systems(OnExit(MenuScreen::Campaign), clear_screen)
            .add_systems(OnExit(MenuScreen::Join), clear_screen)
            .add_systems(OnExit(MenuScreen::Connecting), clear_screen)
            .add_systems(
                Update,
                (
                    type_address.run_if(in_state(MenuScreen::Join)),
                    show_typed_address.run_if(in_state(MenuScreen::Join)),
                    handle_menu_clicks,
                    handle_connect_failure.run_if(in_state(AppState::Connecting)),
                    button_feedback,
                )
                    .chain()
                    .run_if(in_state(AppState::MainMenu).or_else(in_state(AppState::Connecting))),
            );
    }
}

/// Root of whichever screen is up. Despawned wholesale on the way out.
#[derive(Component)]
struct MenuRoot;

/// The menu's own camera.
///
/// The lab's camera belongs to a chemist and does not exist until one has been
/// assigned, which happens well after the menu needs to be on screen — with no
/// camera at all, bevy has nothing to draw the UI onto.
#[derive(Component)]
struct MenuCamera;

/// Host or Solo, remembered while the player picks a save.
#[derive(Resource, Default, Clone, Copy)]
struct PendingMode(LaunchMode);

/// Why the last join attempt failed, if it did.
///
/// Shown on the mode screen after `handle_connect_failure` bounces back from
/// `AppState::Connecting` — the one place in the menu that has to say
/// something went wrong out loud, since silence is exactly the failure mode
/// this whole feature exists to fix.
#[derive(Resource, Default)]
struct ConnectError(Option<String>);

/// The address being typed on the join screen.
#[derive(Resource, Default)]
struct AddressInput {
    text: String,
    /// Set by Enter, cleared by the system that acts on it.
    submitted: bool,
}

/// Marks the text node showing what has been typed.
#[derive(Component)]
struct AddressField;

/// Marks the line under the field that says how the address will be read.
#[derive(Component)]
struct AddressHint;

/// What a menu button does.
#[derive(Component, Clone)]
enum MenuAction {
    ChooseHost,
    ChooseSolo,
    ChooseJoin,
    NewSave,
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
}

fn open_menu(mut commands: Commands, mut screen: ResMut<NextState<MenuScreen>>) {
    // Done here rather than at startup so it happens exactly once, on the path
    // that is about to list the saves.
    saves::migrate_legacy_saves();
    commands.spawn((Camera2d, MenuCamera));
    screen.set(MenuScreen::Mode);
}

/// Opens the waiting-room screen on the way into `AppState::Connecting`.
///
/// Mirrors `open_menu`: a fresh `MenuCamera` (the previous one, if any, was
/// just despawned by `close_menu` on the way out of `MainMenu`), and clears
/// any error left over from an earlier attempt so a successful retry does not
/// show a stale failure the moment it lands back on the mode screen.
fn open_connecting(
    mut commands: Commands,
    mut screen: ResMut<NextState<MenuScreen>>,
    mut error: ResMut<ConnectError>,
) {
    error.0 = None;
    commands.spawn((Camera2d, MenuCamera));
    screen.set(MenuScreen::Connecting);
}

fn close_menu(
    mut commands: Commands,
    cameras: Query<Entity, With<MenuCamera>>,
    roots: Query<Entity, With<MenuRoot>>,
) {
    for camera in &cameras {
        commands.entity(camera).despawn();
    }
    // `try_` because leaving the menu also exits the current screen, and that
    // despawns the same root: both run in this frame's state transition.
    for root in &roots {
        commands.entity(root).try_despawn();
    }
}

fn clear_screen(mut commands: Commands, roots: Query<Entity, With<MenuRoot>>) {
    for root in &roots {
        commands.entity(root).try_despawn();
    }
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

fn show_mode_screen(mut commands: Commands, error: Res<ConnectError>) {
    menu_shell(
        &mut commands,
        MenuRoot,
        "ChemGame",
        "A shift in the chemistry lab.",
        |panel| {
            if let Some(reason) = &error.0 {
                panel.spawn(label(format!("Could not connect: {reason}"), 13.0, ERROR_TEXT));
            }
            panel.spawn(choice(
                "Solo",
                "One chemist. No networking.",
                MenuAction::ChooseSolo,
            ));
            panel.spawn(choice(
                "Host",
                "Play your save and open the lab to a second chemist, over Steam.",
                MenuAction::ChooseHost,
            ));
            panel.spawn(choice(
                "Join",
                "Work someone else's lab. Their save, their shift. \
                 If they clicked Host here, accept their Steam invite \
                 instead — you don't need this screen.",
                MenuAction::ChooseJoin,
            ));
            panel.spawn(row()).with_children(|row| {
                row.spawn(button("Quit", MenuAction::Quit));
            });
        },
    );
}

fn show_save_screen(mut commands: Commands, pending: Res<PendingMode>) {
    let subtitle = match pending.0 {
        LaunchMode::HostSteam => "Hosting — the save you pick is the lab you both work.",
        _ => "Playing alone.",
    };
    let slots = saves::list_slots();

    menu_shell(&mut commands, MenuRoot, "Choose a save", subtitle, |panel| {
        panel.spawn(choice(
            "New save",
            "Start over: shift 1, an empty notebook.",
            MenuAction::NewSave,
        ));

        if slots.is_empty() {
            panel.spawn(label("No saved games yet.", 14.0, TEXT_DIM));
        } else {
            for slot in slots {
                let action = MenuAction::LoadSave(slot.name.clone());
                panel.spawn(choice(slot.name.clone(), &slot.detail(), action));
            }
        }

        panel.spawn(row()).with_children(|row| {
            row.spawn(button("Back", MenuAction::Back));
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

    menu_shell(
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

fn show_join_screen(mut commands: Commands, mut input: ResMut<AddressInput>) {
    // Prefilled, because on a home network it is the same address every time
    // and retyping it is the worst part of joining.
    if input.text.is_empty() {
        if let Some(remembered) = saves::remembered_host() {
            input.text = remembered;
        }
    }
    let typed = input.text.clone();

    menu_shell(
        &mut commands,
        MenuRoot,
        "Join a lab",
        "For a host running chemgame --host on the same network. \
         A host who clicked Host in the menu is using Steam, not this — \
         accept their overlay invite instead.",
        |panel| {
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

    menu_shell(&mut commands, MenuRoot, "Joining a lab", &detail, |panel| {
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
    mut input: ResMut<AddressInput>,
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
            MenuAction::ChooseJoin => screen.set(MenuScreen::Join),
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
            MenuAction::LoadSave(name) => start(
                &mut commands,
                SaveSlot::new(name),
                pending.0,
                // The save carries its own campaign; forcing one here would
                // overwrite the arc already in progress.
                None,
                &mut mode,
                &mut app_state,
                &mut screen,
            ),
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
                app_state.set(AppState::MainMenu);
            }
            // Back goes up one level, not all the way out: the campaign screen
            // is reached *through* the save list, so leaving it should land
            // back on the save list.
            MenuAction::Back => screen.set(match current.get() {
                MenuScreen::Campaign => MenuScreen::Save,
                _ => MenuScreen::Mode,
            }),
            MenuAction::Quit => {
                quit.write(AppExit::Success);
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
) {
    // `.last()`: if several arrive the same frame, only the final word matters
    // — but the read still has to drain the whole reader, or an earlier one
    // would still look unread next frame and fire this again for nothing.
    let Some(failure) = failed.read().last() else {
        return;
    };
    net::abandon_connection_attempt(&mut commands, *mode);
    error.0 = Some(failure.reason.clone());
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
                    row_gap: px(8),
                    ..default()
                })
                .with_children(|panel| {
                    panel.spawn(heading(title));
                    panel.spawn(label(subtitle, 14.0, TEXT_DIM));
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
            .init_resource::<LaunchMode>()
            .init_resource::<ConnectError>()
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
    fn cancel_drops_the_attempt_and_returns_to_the_mode_screen() {
        // The escape hatch that did not exist before this feature: a doomed
        // or just slow handshake used to have no way out but force-quitting.
        let mut app = menu_app();
        click(&mut app, MenuAction::ChooseJoin);
        type_line(&mut app, "192.168.1.40");
        click(&mut app, MenuAction::Connect);
        assert_eq!(state(&app), AppState::Connecting);

        click(&mut app, MenuAction::Cancel);
        assert_eq!(state(&app), AppState::MainMenu);
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
        assert_eq!(hint_for("192.168.1.40:9999"), "connects to 192.168.1.40:9999");
        assert_eq!(hint_for("nonsense"), "not an address yet");
        assert_eq!(hint_for("  "), "for example 192.168.1.40");
    }
}
