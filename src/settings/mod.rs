//! Pausing, and the knobs a player expects to be able to turn.
//!
//! Two things that would normally be separate modules, together because they
//! share a screen: the only way into the settings is the pause menu, and the
//! only thing on the pause menu worth more than one line is the settings.
//!
//! **Pausing is deliberately not an [`AppState`] variant.** Every gameplay
//! system in this game is gated on `in_state(AppState::Playing)`, so a new
//! top-level variant would switch the entire simulation off — including, in
//! co-op, the *authority's* simulation, which is not a thing one peer gets to
//! do to the other. Instead [`Paused`] is a plain local resource, and the two
//! consequences it has are:
//!
//! 1. **Input stops.** Every system that reads the keyboard or the mouse on
//!    the player's behalf is gated on [`not_paused`], so the chemist stands
//!    still and a click lands on the menu rather than on a machine.
//! 2. **In singleplayer only, the clock stops** — `Time<Virtual>::pause()`,
//!    which freezes every timer and zeroes every `delta_secs()` in one move
//!    without a single `run_if` having to know about it. In `Host`/`Join` the
//!    overlay draws and the world keeps turning, because pausing a shared
//!    simulation from one end is not something a peer can do.
//!
//! [`Settings`] lives beside the save slots rather than inside one: which key
//! walks forward is a property of the person playing, not of the career they
//! happen to have open.

use std::path::PathBuf;

use bevy::input::keyboard::KeyboardInput;
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PrimaryWindow, WindowMode, WindowResolution};
use serde::{Deserialize, Serialize};

use crate::menu::{choice, menu_shell};
use crate::net::LaunchMode;
use crate::saves;
use crate::ui::{
    button, button_feedback, label, row, PreserveButtonBackground, ScrollPane, Selected,
    BUTTON_ACTIVE, BUTTON_IDLE, SECTION_BG, TEXT, TEXT_DIM,
};
use crate::AppState;

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Settings::load())
            .init_resource::<Paused>()
            .init_resource::<PauseScreen>()
            .init_resource::<Rebinding>()
            .add_systems(
                Update,
                (
                    // Ordered: the clock has to follow whatever the click
                    // handler just did to `Paused`, and the overlay has to
                    // follow both or it lags a frame behind the state it is
                    // drawing. Everything below that has no business
                    // touching `Paused`/the clock/the overlay at all — the
                    // sliders, the binding capture, and the display buttons
                    // — is left ungated by `AppState::Playing` individually
                    // so it also runs pre-game, on the main menu's own
                    // Settings/Controls screens; only the outer `run_if`
                    // below decides *whether this whole chain runs at all*.
                    handle_pause_clicks.run_if(in_state(AppState::Playing)),
                    disarm_rebind_off_screen,
                    handle_binding_clicks,
                    capture_rebind_key,
                    drag_sliders,
                    handle_display_clicks,
                    apply_pause_to_the_clock.run_if(in_state(AppState::Playing)),
                    sync_pause_overlay.run_if(in_state(AppState::Playing)),
                    // After the rebuild, so a dial drawn this frame is filled
                    // in this frame rather than sitting empty until the next
                    // time the value happens to move.
                    sync_sliders,
                    sync_binding_rows,
                    sync_display_buttons,
                    apply_display_settings,
                    persist_settings,
                    // The main menu's own screens get this from `MenuPlugin`,
                    // which runs it generically over every `Button` regardless
                    // of which plugin spawned it — registering it again here
                    // for `MainMenu` would just run the same system twice.
                    button_feedback.run_if(in_state(AppState::Playing)),
                )
                    .chain()
                    .run_if(in_state(AppState::Playing).or_else(in_state(AppState::MainMenu))),
            )
            // Leaving the lab has to clear this, or quitting to the menu while
            // paused would leave the next session frozen with no overlay on
            // screen to explain why.
            .add_systems(OnExit(AppState::Playing), unpause);
    }
}

// ---------------------------------------------------------------------------
// Pause
// ---------------------------------------------------------------------------

/// Whether this player has the pause menu up.
///
/// Local, never replicated, and deliberately per-process rather than
/// per-chemist: it is a property of the person at the keyboard, not of the
/// body they are driving.
#[derive(Resource, Default)]
pub struct Paused(pub bool);

/// Run condition: the player is free to act.
///
/// What every input-reading system hangs off. Written as its own function
/// rather than inlined so there is one place to look when asking "what does
/// pausing actually stop?".
pub fn not_paused(paused: Res<Paused>) -> bool {
    !paused.0
}

/// Whether this process owns the whole simulation and may therefore stop it.
///
/// Singleplayer only. A host pausing would freeze the guest's world from under
/// them, and a guest pausing would achieve nothing at all — in both cases the
/// overlay still draws and input still stops, which is the part that is
/// actually about the person reading it.
///
/// Takes a plain `Option<&LaunchMode>` rather than being shaped as a run
/// condition, so it is directly testable and so both callers below share one
/// definition of "solo".
fn owns_the_clock(mode: Option<&LaunchMode>) -> bool {
    matches!(mode, None | Some(LaunchMode::Singleplayer))
}

fn apply_pause_to_the_clock(
    paused: Res<Paused>,
    mode: Option<Res<LaunchMode>>,
    mut time: ResMut<Time<Virtual>>,
) {
    let should_stop = paused.0 && owns_the_clock(mode.as_deref());
    if should_stop == time.is_paused() {
        return;
    }
    if should_stop {
        time.pause();
    } else {
        time.unpause();
    }
}

/// Clears the pause on the way out of the lab.
fn unpause(mut paused: ResMut<Paused>, mut time: ResMut<Time<Virtual>>) {
    paused.0 = false;
    time.unpause();
}

// ---------------------------------------------------------------------------
// The overlay
// ---------------------------------------------------------------------------

/// Root of the pause overlay, despawned wholesale when it closes.
///
/// Spawned *with* `crate::until_we_leave_the_lab()` by every `draw_*` below,
/// which is not belt-and-braces: [`sync_pause_overlay`] is gated on
/// `AppState::Playing`, so on the one path that leaves the lab while the menu
/// is up — "Quit to menu", the button that is *only* reachable from here — it
/// stops running before it can tear its own overlay down. The result was a
/// 97%-opaque panel sitting over the main menu with dead buttons on it.
#[derive(Component)]
struct PauseRoot;

/// Which screen of the pause menu is up.
///
/// A field on [`Paused`]'s neighbour rather than a `States`, because it only
/// ever exists while the overlay does and nothing outside this module cares.
#[derive(Resource, Default, Debug, PartialEq, Eq, Clone, Copy)]
pub enum PauseScreen {
    #[default]
    Root,
    Settings,
    Controls,
    Textbook,
    TextbookArticle,
    Training,
    /// The arc has resolved. Raised by `crate::ending`, which owns everything
    /// about what the screen says; this module only owns the fact that it is a
    /// screen, and therefore gets the freed cursor, the stopped input and the
    /// held clock for nothing.
    Ending,
}

/// Where a single Escape press lands, one level up from `screen` — mirroring
/// what the on-screen "Back" button already does on Settings/Controls, rather
/// than the whole overlay closing outright from any sub-screen the way it used
/// to. `None` means Escape should close the overlay entirely instead (`Root`
/// already does; `Ending` is dismissed the same way since there is nowhere
/// under it to step back to).
pub(crate) fn escape_steps_pause_screen_back_to(screen: PauseScreen) -> Option<PauseScreen> {
    match screen {
        PauseScreen::Settings
        | PauseScreen::Controls
        | PauseScreen::Textbook
        | PauseScreen::Training => Some(PauseScreen::Root),
        PauseScreen::TextbookArticle => Some(PauseScreen::Textbook),
        PauseScreen::Root | PauseScreen::Ending => None,
    }
}

#[derive(Component, Clone, Copy)]
pub enum PauseAction {
    Resume,
    OpenSettings,
    OpenControls,
    OpenTextbook,
    Back,
    /// Leaves the lab for the main menu. `crate::session` unwinds the world
    /// and `crate::net::close_session_transport` hangs up the socket.
    QuitToMenu,
    QuitToDesktop,
    /// Puts every dial and display option back where it shipped. Deliberately
    /// leaves `bindings` untouched — see [`PauseAction::RestoreBindings`].
    RestoreDefaults,
    /// Puts every key on the Controls screen back where it shipped. Split from
    /// [`PauseAction::RestoreDefaults`] because bindings are a different
    /// category of preference (muscle memory/accessibility) from the
    /// perceptual dials, and now that both are independently reachable,
    /// neither should be able to silently reset the other.
    RestoreBindings,
}

/// Draws and tears down the overlay to match [`Paused`].
///
/// One system rather than `OnEnter`/`OnExit` because `Paused` is a resource,
/// not a state — and rebuilding on a signature change is the same shape
/// `ui::sync_panel` already uses for machine panels.
#[allow(clippy::too_many_arguments)]
fn sync_pause_overlay(
    mut commands: Commands,
    paused: Res<Paused>,
    mut screen: ResMut<PauseScreen>,
    settings: Res<Settings>,
    rebinding: Res<Rebinding>,
    mode: Option<Res<LaunchMode>>,
    ending: Res<crate::ending::FinishedArc>,
    roots: Query<Entity, With<PauseRoot>>,
    kind: Option<Res<crate::session::SessionKind>>,
) {
    // Deliberately *not* rebuilt when `Settings` changes. It used to be, and
    // that made a draggable dial impossible: every frame of a drag writes the
    // setting, which would despawn the very track the mouse was holding. The
    // live values are updated in place by [`sync_sliders`] instead, which is
    // the same "rebuild on structure, patch on value" split the order queue
    // and the vitals panel already use.
    let open = !roots.is_empty();
    let wants_rebuild = paused.is_changed() || screen.is_changed() || open != paused.0;
    if !wants_rebuild {
        return;
    }
    for root in &roots {
        commands.entity(root).try_despawn();
    }
    if !paused.0 {
        // Whichever screen was up, the next Escape has to open the root one.
        // Escape closes the overlay from `interaction::panel_input`, which has
        // no business knowing this module's screens — so the reset lives here,
        // where the overlay coming down is already observed. Without it,
        // dismissing the ending with Escape meant every later pause reopened
        // the ending.
        if *screen != PauseScreen::Root {
            *screen = PauseScreen::Root;
        }
        return;
    }

    let co_op = !owns_the_clock(mode.as_deref());
    match *screen {
        PauseScreen::Root => draw_root(
            &mut commands,
            co_op,
            matches!(kind.as_deref(), Some(crate::session::SessionKind::Training)),
        ),
        PauseScreen::Textbook | PauseScreen::TextbookArticle | PauseScreen::Training => {}
        PauseScreen::Settings => draw_settings(&mut commands, &settings),
        PauseScreen::Controls => draw_controls(&mut commands, &settings, &rebinding),
        PauseScreen::Ending => match ending.showing() {
            Some(ending) => crate::ending::draw(
                &mut commands,
                ending,
                (PauseRoot, crate::until_we_leave_the_lab()),
            ),
            // Unreachable: `ending::notice_the_ending` writes the content
            // before it selects this screen. Falling back to the ordinary
            // pause menu beats an empty screen with no way off it.
            None => draw_root(&mut commands, co_op, false),
        },
    }
}

fn draw_root(commands: &mut Commands, co_op: bool, training: bool) {
    let subtitle = if co_op {
        "The lab keeps running — you are not the only one in it."
    } else {
        "The lab is holding still."
    };
    menu_shell(
        commands,
        (PauseRoot, crate::until_we_leave_the_lab()),
        "Paused",
        subtitle,
        |panel| {
            panel.spawn(choice("Resume", "Back to the bench.", PauseAction::Resume));
            panel.spawn(choice(
                "Lab Textbook",
                "Short explanations and help with experiments.",
                PauseAction::OpenTextbook,
            ));
            if training {
                crate::tutorial::pause_controls(panel);
            }
            panel.spawn(choice(
                "Settings",
                "Look sensitivity, field of view, volume.",
                PauseAction::OpenSettings,
            ));
            panel.spawn(choice(
                "Controls",
                "What every key does, and how to change it.",
                PauseAction::OpenControls,
            ));
            panel.spawn(choice(
                "Quit to menu",
                "Leaves the lab. Your notebook and career are already saved.",
                PauseAction::QuitToMenu,
            ));
            panel.spawn(choice(
                "Quit to desktop",
                "Same, and closes the game.",
                PauseAction::QuitToDesktop,
            ));
        },
    );
}

/// Which continuous setting a [`Slider`] drives.
///
/// A small enum rather than a boxed accessor so the whole thing stays plain
/// data: the drag system matches on it to read and write, and the label system
/// matches on it to format.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum Knob {
    Sensitivity,
    Fov,
    Volume,
}

impl Knob {
    /// The ends of the dial.
    ///
    /// Sensitivity spans roughly a factor of eight around the 0.0022 the game
    /// shipped with. Field of view is **vertical**, which is what
    /// `PerspectiveProjection::fov` holds — not the horizontal number a
    /// settings screen usually quotes — and 45° (Bevy's own default, and
    /// therefore exactly what this game has always looked like) sits inside
    /// the range rather than at an end, so the shipped value reads as a choice.
    fn range(self) -> (f32, f32) {
        match self {
            Knob::Sensitivity => (0.0005, 0.0075),
            Knob::Fov => (30.0, 90.0),
            Knob::Volume => (0.0, 1.0),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Knob::Sensitivity => "Look sensitivity",
            Knob::Fov => "Field of view",
            Knob::Volume => "Volume",
        }
    }

    fn read(self, settings: &Settings) -> f32 {
        match self {
            Knob::Sensitivity => settings.mouse_sensitivity,
            Knob::Fov => settings.fov_degrees,
            Knob::Volume => settings.master_volume,
        }
    }

    fn write(self, settings: &mut Settings, value: f32) {
        match self {
            Knob::Sensitivity => settings.mouse_sensitivity = value,
            Knob::Fov => settings.fov_degrees = value,
            Knob::Volume => settings.master_volume = value,
        }
    }

    /// How the number reads next to the dial. Each one is quoted in the unit a
    /// player actually thinks in, which is why this is not one shared format.
    fn format(self, value: f32) -> String {
        match self {
            // Scaled up: "0.0022" is a number nobody can compare at a glance,
            // and the underlying radians-per-pixel is an implementation detail.
            Knob::Sensitivity => format!("{:.0}", value * 1000.0 * 10.0),
            Knob::Fov => format!("{value:.0}° vertical"),
            Knob::Volume => format!("{:.0}%", value * 100.0),
        }
    }
}

/// The draggable track of one setting.
#[derive(Component, Clone, Copy)]
struct Slider(Knob);

/// The filled portion of a slider, resized to match the live value.
#[derive(Component, Clone, Copy)]
struct SliderFill(Knob);

/// The number printed beside a slider.
#[derive(Component, Clone, Copy)]
struct SliderReadout(Knob);

const SLIDER_TRACK_HEIGHT: f32 = 18.0;

/// How the window is displayed. A small owned enum rather than
/// `bevy::window::WindowMode` itself, so `Settings`'s serialized shape stays
/// decoupled from window-internals types (`MonitorSelection`,
/// `VideoModeSelection`) — the same reason every other `Settings` field is a
/// plain domain value rather than a bevy type. Doubles as its own button
/// component, the same way [`Knob`] doubles as a slider identifier.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayMode {
    #[default]
    Windowed,
    Fullscreen,
}

impl DisplayMode {
    fn label(self) -> &'static str {
        match self {
            DisplayMode::Windowed => "Windowed",
            DisplayMode::Fullscreen => "Fullscreen",
        }
    }

    /// Mapped at apply time only — see the type's own doc comment for why
    /// `Settings` never stores this directly. `BorderlessFullscreen` over
    /// exclusive `Fullscreen`: it needs no display-mode switch, so alt-tabbing
    /// out of the game does not do the thing exclusive fullscreen is known
    /// for on Windows.
    fn to_window_mode(self) -> WindowMode {
        match self {
            DisplayMode::Windowed => WindowMode::Windowed,
            DisplayMode::Fullscreen => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
        }
    }
}

/// A resolution preset button. Not `(u32, u32)` directly — a bare tuple can't
/// implement `Component` under Rust's orphan rules.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
struct ResolutionChoice(u32, u32);

/// The only resolutions offered on screen. There is no dropdown or free-form
/// numeric entry widget anywhere in this UI — every discrete choice here is a
/// button row, same as the dispense-amount buttons in `ui::dispenser_body`.
const RESOLUTION_PRESETS: [(u32, u32); 4] = [(1280, 720), (1600, 900), (1920, 1080), (2560, 1440)];

/// Shared by both ways of reaching Settings — the pause-reached screen and the
/// main menu's own — so the copy on the two can't drift apart.
pub(crate) const SETTINGS_SUBTITLE: &str =
    "Drag a dial, or click anywhere along it. Kept beside your saves, shared by every career.";

/// The content of the Settings screen, shared by both callers. Spawns no
/// action buttons of its own: each caller owns its `menu_shell` call, its root
/// marker, and its own trailing choices, in its own action enum — the same
/// split `choice`/`menu_shell` already draw between what is shared and what
/// is screen-specific.
pub(crate) fn settings_body(panel: &mut ChildSpawnerCommands, settings: &Settings) {
    panel
        .spawn((
            Node {
                width: percent(100),
                flex_direction: FlexDirection::Column,
                max_height: vh(60),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            ScrollPane,
        ))
        .with_children(|pane| {
            for knob in [Knob::Sensitivity, Knob::Fov, Knob::Volume] {
                slider_row(pane, knob, settings);
            }
            display_section(pane, settings);
        });
}

/// "Windowed"/"Fullscreen" and the resolution presets. A section of
/// [`settings_body`] rather than a screen of its own — there is little enough
/// here that a whole extra screen and Back click would cost more than it buys.
fn display_section(panel: &mut ChildSpawnerCommands, settings: &Settings) {
    panel.spawn((
        label("Display", 14.0, TEXT),
        Node {
            margin: UiRect::top(px(10)),
            ..default()
        },
    ));
    panel.spawn(row()).with_children(|row| {
        for mode in [DisplayMode::Windowed, DisplayMode::Fullscreen] {
            let mut entity = row.spawn(button(mode.label(), mode));
            if mode == settings.display_mode {
                entity.insert((Selected, BackgroundColor(crate::ui::BUTTON_ACTIVE)));
            }
        }
    });
    panel.spawn(row()).with_children(|row| {
        for (w, h) in RESOLUTION_PRESETS {
            let mut entity = row.spawn(button(format!("{w}x{h}"), ResolutionChoice(w, h)));
            if (w, h) == settings.resolution {
                entity.insert((Selected, BackgroundColor(crate::ui::BUTTON_ACTIVE)));
            }
        }
    });
}

fn draw_settings(commands: &mut Commands, settings: &Settings) {
    menu_shell(
        commands,
        (PauseRoot, crate::until_we_leave_the_lab()),
        "Settings",
        SETTINGS_SUBTITLE,
        |panel| {
            settings_body(panel, settings);
            panel.spawn(choice(
                "Restore defaults",
                "Puts every dial and display option on this screen back where it shipped.",
                PauseAction::RestoreDefaults,
            ));
            panel.spawn(choice("Back", "", PauseAction::Back));
        },
    );
}

/// A labelled dial: title and live value on one line, the track under it.
fn slider_row(panel: &mut ChildSpawnerCommands, knob: Knob, settings: &Settings) {
    let value = knob.read(settings);
    panel
        .spawn(Node {
            width: percent(100),
            justify_content: JustifyContent::SpaceBetween,
            margin: UiRect::top(px(10)),
            ..default()
        })
        .with_children(|row| {
            row.spawn(label(knob.title(), 14.0, TEXT));
            row.spawn((
                Text::new(knob.format(value)),
                TextFont::from_font_size(14.0),
                TextColor(TEXT_DIM),
                SliderReadout(knob),
            ));
        });

    // `Button` so `Interaction` is tracked for it — that is what tells the
    // drag system a press started on *this* track rather than somewhere else
    // on the screen.
    panel
        .spawn((
            Button,
            Node {
                width: percent(100),
                height: px(SLIDER_TRACK_HEIGHT),
                margin: UiRect::bottom(px(4)),
                border_radius: BorderRadius::all(px(SLIDER_TRACK_HEIGHT / 2.0)),
                ..default()
            },
            BackgroundColor(SECTION_BG),
            Slider(knob),
            PreserveButtonBackground,
        ))
        .with_children(|track| {
            track.spawn((
                Node {
                    width: percent(fraction_of(knob, value) * 100.0),
                    height: percent(100),
                    border_radius: BorderRadius::all(px(SLIDER_TRACK_HEIGHT / 2.0)),
                    ..default()
                },
                BackgroundColor(crate::ui::BUTTON_ACTIVE),
                SliderFill(knob),
            ));
        });
}

/// Where `value` sits along `knob`'s range, as 0..=1.
fn fraction_of(knob: Knob, value: f32) -> f32 {
    let (lo, hi) = knob.range();
    if hi <= lo {
        return 0.0;
    }
    ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// The value `fraction` of the way along `knob`'s range.
fn value_at(knob: Knob, fraction: f32) -> f32 {
    let (lo, hi) = knob.range();
    lo + (hi - lo) * fraction.clamp(0.0, 1.0)
}

/// Drags a dial while the mouse is held down on it.
///
/// Tracks which slider the press *started* on rather than reading `Interaction`
/// every frame, because a drag that leaves the track — which is most of them,
/// once you are pulling toward one end — would otherwise stop dead the moment
/// the cursor crossed the edge.
///
/// The cursor is free here, same as it is over any open machine panel (see
/// `interaction::panel_input`) — `ui::drag_thermostat_slider` on the reaction
/// chamber's dial is built the same way, just scoped to a machine over the
/// network instead of a local resource.
fn drag_sliders(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    sliders: Query<(&Slider, &Interaction, &ComputedNode, &UiGlobalTransform)>,
    mut held: Local<Option<Knob>>,
    mut settings: ResMut<Settings>,
) {
    if !mouse.pressed(MouseButton::Left) {
        *held = None;
        return;
    }
    // A fresh press: whichever track it landed on is the one this drag owns.
    if held.is_none() {
        *held = sliders
            .iter()
            .find(|(_, interaction, _, _)| **interaction != Interaction::None)
            .map(|(slider, _, _, _)| slider.0);
    }
    let Some(knob) = *held else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Some((_, _, node, transform)) = sliders.iter().find(|(slider, _, _, _)| slider.0 == knob)
    else {
        return;
    };

    // `normalize_point` puts the node's centre at the origin and its corners
    // at ±0.5, and it accounts for any transform on the node — worth using
    // over hand-rolled `x - width/2` arithmetic, which silently assumes the
    // translation is a corner and that nothing above the track is scaled.
    let Some(local) = node.normalize_point(*transform, cursor) else {
        return;
    };
    let value = value_at(knob, local.x + 0.5);
    if knob.read(&settings) != value {
        knob.write(&mut settings, value);
    }
}

/// Keeps each dial's fill and printed value on the live setting.
///
/// Separate from the drag so the two other ways a value can move — "Restore
/// defaults", and a settings file edited by hand — redraw exactly the same way.
fn sync_sliders(
    settings: Res<Settings>,
    mut fills: Query<(&SliderFill, &mut Node)>,
    mut readouts: Query<(&SliderReadout, &mut Text)>,
) {
    if !settings.is_changed() {
        return;
    }
    for (fill, mut node) in &mut fills {
        node.width = percent(fraction_of(fill.0, fill.0.read(&settings)) * 100.0);
    }
    for (readout, mut text) in &mut readouts {
        let wanted = readout.0.format(readout.0.read(&settings));
        if text.0 != wanted {
            text.0 = wanted;
        }
    }
}

/// Shared by both ways of reaching Controls.
pub(crate) const CONTROLS_SUBTITLE: &str =
    "Click a key, then press whatever should do it. Esc cancels.";

/// The content of the Controls screen, shared by both callers — see
/// [`settings_body`]'s doc comment for the split this follows.
pub(crate) fn controls_body(
    panel: &mut ChildSpawnerCommands,
    settings: &Settings,
    rebinding: &Rebinding,
) {
    panel
        .spawn((
            Node {
                width: percent(100),
                flex_direction: FlexDirection::Column,
                max_height: vh(60),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            ScrollPane,
        ))
        .with_children(|pane| {
            for slot in BindingSlot::ALL {
                binding_row(pane, slot, settings, rebinding);
            }
            pane.spawn(label(
                "Mouse                 look\nEsc                   pause, or step back out of a panel",
                15.0,
                TEXT_DIM,
            ));
        });
}

/// One rebindable row: the action's name, and a button showing its key (or
/// "press a key..." while armed) that arms/disarms [`Rebinding`] on click.
fn binding_row(
    panel: &mut ChildSpawnerCommands,
    slot: BindingSlot,
    settings: &Settings,
    rebinding: &Rebinding,
) {
    let armed = rebinding.0 == Some(slot);
    let text = if armed {
        "press a key...".to_string()
    } else {
        key_label(slot.read(&settings.bindings))
    };
    panel
        .spawn(Node {
            width: percent(100),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            margin: UiRect::vertical(px(2)),
            ..default()
        })
        .with_children(|row| {
            row.spawn(label(slot.title(), 14.0, TEXT));
            let mut key_button = row.spawn((
                Button,
                Node {
                    padding: UiRect::axes(px(11), px(6)),
                    min_width: px(130),
                    justify_content: JustifyContent::Center,
                    border_radius: BorderRadius::all(px(4)),
                    ..default()
                },
                BackgroundColor(if armed { BUTTON_ACTIVE } else { BUTTON_IDLE }),
                BindingButton(slot),
                children![(
                    Text::new(text),
                    TextFont::from_font_size(14.0),
                    TextColor(TEXT),
                    BindingReadout(slot),
                )],
            ));
            if armed {
                key_button.insert(Selected);
            }
        });
}

fn draw_controls(commands: &mut Commands, settings: &Settings, rebinding: &Rebinding) {
    menu_shell(
        commands,
        (PauseRoot, crate::until_we_leave_the_lab()),
        "Controls",
        CONTROLS_SUBTITLE,
        |panel| {
            controls_body(panel, settings, rebinding);
            panel.spawn(choice(
                "Restore bindings",
                "Puts every key on this screen back where it shipped.",
                PauseAction::RestoreBindings,
            ));
            panel.spawn(choice("Back", "", PauseAction::Back));
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_pause_clicks(
    buttons: Query<(&Interaction, &PauseAction), Changed<Interaction>>,
    mut paused: ResMut<Paused>,
    mut screen: ResMut<PauseScreen>,
    mut settings: ResMut<Settings>,
    mut app_state: ResMut<NextState<AppState>>,
    mut quit: MessageWriter<AppExit>,
    mut textbook: ResMut<crate::textbook::TextbookView>,
) {
    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            PauseAction::Resume => {
                paused.0 = false;
                *screen = PauseScreen::Root;
            }
            PauseAction::OpenSettings => *screen = PauseScreen::Settings,
            PauseAction::OpenControls => *screen = PauseScreen::Controls,
            PauseAction::OpenTextbook => crate::textbook::open(&mut textbook, &mut screen),
            PauseAction::Back => *screen = PauseScreen::Root,
            PauseAction::QuitToMenu => {
                // `OnExit(AppState::Playing)` owns the teardown — see
                // `crate::net::leave_session` and the `DespawnOnExit` markers
                // on everything the lab spawns.
                app_state.set(AppState::MainMenu);
            }
            PauseAction::QuitToDesktop => {
                quit.write(AppExit::Success);
            }
            PauseAction::RestoreDefaults => restore_settings_defaults(&mut settings),
            PauseAction::RestoreBindings => restore_bindings_defaults(&mut settings),
        }
    }
}

/// Puts every dial and display option back where it shipped, preserving
/// `bindings` — shared by the pause-reached and menu-reached Settings
/// screens' "Restore defaults" buttons so the two can't drift.
pub(crate) fn restore_settings_defaults(settings: &mut Settings) {
    let bindings = settings.bindings;
    *settings = Settings {
        bindings,
        ..Settings::default()
    };
}

/// Puts every key back where it shipped — shared by the pause-reached and
/// menu-reached Controls screens' "Restore bindings" buttons.
pub(crate) fn restore_bindings_defaults(settings: &mut Settings) {
    settings.bindings = Bindings::default();
}

/// Writes straight to [`Settings`] on click — the same shape [`drag_sliders`]
/// already uses for the continuous dials, minus the drag: a display button is
/// a discrete choice, not a range. Skips the write entirely when the clicked
/// option already matches, so re-clicking the current mode/resolution does not
/// mark `Settings` changed for nothing (which would otherwise trigger an
/// unnecessary disk write via `persist_settings`).
fn handle_display_clicks(
    modes: Query<(&Interaction, &DisplayMode), Changed<Interaction>>,
    resolutions: Query<(&Interaction, &ResolutionChoice), Changed<Interaction>>,
    mut settings: ResMut<Settings>,
) {
    for (interaction, mode) in &modes {
        if *interaction == Interaction::Pressed && settings.display_mode != *mode {
            settings.display_mode = *mode;
        }
    }
    for (interaction, resolution) in &resolutions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let wanted = (resolution.0, resolution.1);
        if settings.resolution != wanted {
            settings.resolution = wanted;
        }
    }
}

/// Patches each display button's background/[`Selected`] in place — the same
/// "rebuild on structure, patch on value" split [`sync_binding_rows`] and the
/// sliders already follow, so "Restore defaults" or a hand-edited settings
/// file redraws these buttons exactly the same way a click does.
fn sync_display_buttons(
    settings: Res<Settings>,
    mut commands: Commands,
    mut modes: Query<
        (Entity, &DisplayMode, Has<Selected>, &mut BackgroundColor),
        Without<ResolutionChoice>,
    >,
    mut resolutions: Query<
        (
            Entity,
            &ResolutionChoice,
            Has<Selected>,
            &mut BackgroundColor,
        ),
        Without<DisplayMode>,
    >,
) {
    if !settings.is_changed() {
        return;
    }
    for (entity, mode, was_selected, mut background) in &mut modes {
        let selected = *mode == settings.display_mode;
        if selected != was_selected {
            let mut entity = commands.entity(entity);
            if selected {
                entity.insert(Selected);
            } else {
                entity.remove::<Selected>();
            }
        }
        background.0 = if selected { BUTTON_ACTIVE } else { BUTTON_IDLE };
    }
    for (entity, resolution, was_selected, mut background) in &mut resolutions {
        let selected = (resolution.0, resolution.1) == settings.resolution;
        if selected != was_selected {
            let mut entity = commands.entity(entity);
            if selected {
                entity.insert(Selected);
            } else {
                entity.remove::<Selected>();
            }
        }
        background.0 = if selected { BUTTON_ACTIVE } else { BUTTON_IDLE };
    }
}

/// Keeps the real window on the dialled-in display mode and resolution.
///
/// Mirrors the camera settings path's exact shape: bail unless [`Settings`] actually
/// changed, then write only whichever of `mode`/`resolution` differs, so
/// neither wakes the window backend every frame it happens to run.
fn apply_display_settings(
    settings: Res<Settings>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if !settings.is_changed() {
        return;
    }
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let wanted_mode = settings.display_mode.to_window_mode();
    if window.mode != wanted_mode {
        window.mode = wanted_mode;
    }
    let wanted_resolution = settings.resolution;
    let current_resolution = (
        window.resolution.physical_width(),
        window.resolution.physical_height(),
    );
    if current_resolution != wanted_resolution {
        window.resolution = WindowResolution::new(wanted_resolution.0, wanted_resolution.1);
    }
}

// ---------------------------------------------------------------------------
// The settings themselves
// ---------------------------------------------------------------------------

#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Radians of yaw per pixel of mouse movement. Was a hardcoded constant in
    /// `player`.
    pub mouse_sensitivity: f32,
    pub fov_degrees: f32,
    /// Applied to `GlobalVolume` by `audio::sync_master_volume`.
    pub master_volume: f32,
    pub bindings: Bindings,
    pub display_mode: DisplayMode,
    /// Physical pixels. Only one of [`RESOLUTION_PRESETS`] is ever offered on
    /// screen, but nothing here enforces that a hand-edited file stays on the
    /// list — an off-list value still applies, just with no preset shown as
    /// selected.
    pub resolution: (u32, u32),
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            // The value the game shipped with, so an existing player's hands
            // do not have to relearn anything.
            mouse_sensitivity: 0.0022,
            // Bevy's own `PerspectiveProjection` default, in degrees. Adding a
            // FOV setting must not change what the game looks like for anyone
            // who never opens the screen.
            fov_degrees: 45.0,
            master_volume: 0.8,
            bindings: Bindings::default(),
            // Both exactly `WindowResolution`/`WindowMode`'s own bare Bevy
            // defaults, so adding this setting changes nothing for a player
            // who never opens the screen.
            display_mode: DisplayMode::default(),
            resolution: (1280, 720),
        }
    }
}

impl Settings {
    /// Beside the real save slots, not a literal relative path — so a
    /// packaged build's settings land under `%LOCALAPPDATA%\ChemGame\saves`
    /// alongside the saves themselves (and under Steam Cloud) instead of
    /// wherever the executable's current directory happens to be. In a dev or
    /// test build this resolves to the same relative `saves/` it always has,
    /// since `saves::saves_root()`'s own fallback is that literal path.
    fn path() -> PathBuf {
        saves::saves_root().join("settings.ron")
    }

    /// Reads the file, falling back to defaults for anything missing.
    ///
    /// A corrupt settings file costs the player their preferences, never the
    /// session — the same rule `knowledge::read_save` and
    /// `shift::read_progress` already follow.
    pub fn load() -> Settings {
        let path = Settings::path();
        if !path.exists() {
            return Settings::default();
        }
        match std::fs::read_to_string(&path).map(|text| ron::from_str::<Settings>(&text)) {
            Ok(Ok(mut settings)) => {
                settings.bindings.migrate_inspect_binding();
                settings
            }
            Ok(Err(error)) => {
                warn!("ignoring unreadable {}: {error}", path.display());
                Settings::default()
            }
            Err(error) => {
                warn!("could not read {}: {error}", path.display());
                Settings::default()
            }
        }
    }
}

/// Writes the file whenever anything actually changed.
fn persist_settings(settings: Res<Settings>, mut written: Local<Option<Settings>>) {
    if written.as_ref() == Some(&*settings) {
        return;
    }
    let Ok(text) = ron::ser::to_string_pretty(&*settings, default()) else {
        return;
    };
    *written = Some(settings.clone());

    let path = Settings::path();
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            warn!("could not create {}: {error}", parent.display());
            return;
        }
    }
    if let Err(error) = std::fs::write(&path, text) {
        warn!("could not write {}: {error}", path.display());
    }
}

/// Every key the game reads on the player's behalf.
///
/// Routed through here rather than read as literals at eight call sites, which
/// is what makes rebinding a data change instead of a refactor. Doing it while
/// there are only eight is far cheaper than doing it later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bindings {
    pub forward: KeyCode,
    pub back: KeyCode,
    pub left: KeyCode,
    pub right: KeyCode,
    pub sprint: KeyCode,
    pub interact: KeyCode,
    pub drop: KeyCode,
    pub drink: KeyCode,
    pub apply: KeyCode,
    pub book: KeyCode,
    /// Shared crew relationships, dialogue, and department shops.
    pub social: KeyCode,
    /// Write on whatever is in hand — see [`crate::labels`].
    pub label: KeyCode,
    pub inspect: KeyCode,
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings {
            forward: KeyCode::KeyW,
            back: KeyCode::KeyS,
            left: KeyCode::KeyA,
            right: KeyCode::KeyD,
            sprint: KeyCode::ShiftLeft,
            interact: KeyCode::KeyE,
            drop: KeyCode::KeyQ,
            drink: KeyCode::KeyR,
            apply: KeyCode::KeyF,
            book: KeyCode::KeyB,
            social: KeyCode::Tab,
            label: KeyCode::KeyL,
            inspect: KeyCode::KeyI,
        }
    }
}

impl Bindings {
    fn migrate_inspect_binding(&mut self) {
        let old: Vec<_> = BindingSlot::ALL
            .iter()
            .filter(|s| **s != BindingSlot::Inspect)
            .map(|s| s.read(self))
            .collect();
        if old.contains(&self.inspect) {
            if let Some(key) = [
                KeyCode::KeyI,
                KeyCode::KeyO,
                KeyCode::KeyP,
                KeyCode::KeyK,
                KeyCode::F6,
                KeyCode::F7,
                KeyCode::F8,
                KeyCode::F9,
                KeyCode::F10,
                KeyCode::F11,
                KeyCode::F12,
                KeyCode::F5,
                KeyCode::F4,
            ]
            .into_iter()
            .find(|k| !old.contains(k))
            {
                self.inspect = key;
            }
        }
    }

    /// Assigns `key` to `slot`, swapping rather than refusing if another slot
    /// already holds it. With 12 bindings, forcing every key to stay distinct
    /// avoids two actions silently firing off one keypress, and a swap lets a
    /// player freely reorganise a whole layout (WASD for the arrow keys, say)
    /// without hitting a "that key is already taken" dead end.
    fn rebind(&mut self, slot: BindingSlot, key: KeyCode) {
        let previous = slot.read(self);
        if let Some(displaced) = BindingSlot::ALL
            .into_iter()
            .find(|&other| other != slot && other.read(self) == key)
        {
            displaced.write(self, previous);
        }
        slot.write(self, key);
    }
}

/// Which field of [`Bindings`] a row on the Controls screen drives. A small
/// enum mirroring [`Knob`]'s own shape: the click system matches on it to read
/// and rebind, the row builder matches on it to title and format.
///
/// Replaces the former `Bindings::described()`, which returned only 10 of the
/// 11 fields — `label` (see `crate::labels`) had no row on the Controls
/// screen and so no way for a player to ever discover it existed.
/// [`BindingSlot::ALL`] enumerating every field fixes that by construction.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
enum BindingSlot {
    Forward,
    Back,
    Left,
    Right,
    Sprint,
    Interact,
    Drop,
    Drink,
    Apply,
    Book,
    Social,
    Label,
    Inspect,
}

impl BindingSlot {
    /// In the order the Controls screen reads best — movement, then the
    /// hands, then the book.
    const ALL: [BindingSlot; 13] = [
        BindingSlot::Forward,
        BindingSlot::Back,
        BindingSlot::Left,
        BindingSlot::Right,
        BindingSlot::Sprint,
        BindingSlot::Interact,
        BindingSlot::Drop,
        BindingSlot::Drink,
        BindingSlot::Apply,
        BindingSlot::Book,
        BindingSlot::Social,
        BindingSlot::Label,
        BindingSlot::Inspect,
    ];

    fn title(self) -> &'static str {
        match self {
            BindingSlot::Forward => "Walk forward",
            BindingSlot::Back => "Walk back",
            BindingSlot::Left => "Step left",
            BindingSlot::Right => "Step right",
            BindingSlot::Sprint => "Sprint",
            BindingSlot::Interact => "Use / hand over",
            BindingSlot::Drop => "Drop what you hold",
            BindingSlot::Drink => "Drink or swallow",
            BindingSlot::Apply => "Apply held item",
            BindingSlot::Book => "Reference book",
            BindingSlot::Social => "Crew relationships / shops",
            BindingSlot::Label => "Write on what you hold",
            BindingSlot::Inspect => "Inspect held item",
        }
    }

    fn read(self, bindings: &Bindings) -> KeyCode {
        match self {
            BindingSlot::Forward => bindings.forward,
            BindingSlot::Back => bindings.back,
            BindingSlot::Left => bindings.left,
            BindingSlot::Right => bindings.right,
            BindingSlot::Sprint => bindings.sprint,
            BindingSlot::Interact => bindings.interact,
            BindingSlot::Drop => bindings.drop,
            BindingSlot::Drink => bindings.drink,
            BindingSlot::Apply => bindings.apply,
            BindingSlot::Book => bindings.book,
            BindingSlot::Social => bindings.social,
            BindingSlot::Label => bindings.label,
            BindingSlot::Inspect => bindings.inspect,
        }
    }

    fn write(self, bindings: &mut Bindings, key: KeyCode) {
        match self {
            BindingSlot::Forward => bindings.forward = key,
            BindingSlot::Back => bindings.back = key,
            BindingSlot::Left => bindings.left = key,
            BindingSlot::Right => bindings.right = key,
            BindingSlot::Sprint => bindings.sprint = key,
            BindingSlot::Interact => bindings.interact = key,
            BindingSlot::Drop => bindings.drop = key,
            BindingSlot::Drink => bindings.drink = key,
            BindingSlot::Apply => bindings.apply = key,
            BindingSlot::Book => bindings.book = key,
            BindingSlot::Social => bindings.social = key,
            BindingSlot::Label => bindings.label = key,
            BindingSlot::Inspect => bindings.inspect = key,
        }
    }
}

/// Which binding row is waiting for a keypress, if any.
///
/// Local UI state on the same footing as [`Paused`]/[`PauseScreen`] — never
/// replicated, never saved.
#[derive(Resource, Default)]
pub(crate) struct Rebinding(Option<BindingSlot>);

impl Rebinding {
    pub(crate) fn is_armed(&self) -> bool {
        self.0.is_some()
    }
}

/// A clickable row on the Controls screen. Hand-rolled rather than
/// `ui::button()`: the row's text has to be patched in place while armed (see
/// [`BindingReadout`]) — the same reason `SliderReadout` is a separate marker
/// from `Slider` rather than reusing `ui::button()`'s opaque bundle, which
/// gives no handle to the text child underneath.
#[derive(Component, Clone, Copy)]
struct BindingButton(BindingSlot);

/// The text child of one [`BindingButton`], patched in place by
/// [`sync_binding_rows`].
#[derive(Component, Clone, Copy)]
struct BindingReadout(BindingSlot);

/// Arms, re-arms, or cancels a binding capture on click.
///
/// Clicking the already-armed row cancels it (a toggle); clicking a different
/// row replaces whichever was armed — no explicit cancel-first step required.
fn handle_binding_clicks(
    buttons: Query<(&Interaction, &BindingButton), Changed<Interaction>>,
    mut rebinding: ResMut<Rebinding>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        rebinding.0 = if rebinding.0 == Some(button.0) {
            None
        } else {
            Some(button.0)
        };
    }
}

/// Consumes the next key press while a row is armed.
///
/// The same raw-`KeyboardInput`-reading idiom `menu::type_address` already
/// uses for the join screen's text field: arm on click, consume the next
/// relevant key event, disarm.
fn capture_rebind_key(
    mut keys: MessageReader<KeyboardInput>,
    mut rebinding: ResMut<Rebinding>,
    mut settings: ResMut<Settings>,
) {
    let Some(slot) = rebinding.0 else {
        // Still has to drain the reader, or an unarmed frame leaves keys
        // queued up to be misread as the *next* arming's capture.
        keys.clear();
        return;
    };
    for key in keys.read() {
        if key.state != ButtonState::Pressed {
            continue;
        }
        // A key already held down before the row was clicked must not be
        // silently captured — only a fresh press counts.
        if key.repeat {
            continue;
        }
        if key.key_code == KeyCode::Escape {
            rebinding.0 = None;
            return;
        }
        // Modifiers are valid targets, not excluded — `Sprint`'s own default
        // is `ShiftLeft`.
        settings.bindings.rebind(slot, key.key_code);
        rebinding.0 = None;
        return;
    }
}

/// Clears an armed row the instant Controls is no longer the screen showing,
/// regardless of *how* it was left — Back, Escape, quitting to the main menu.
/// Centralised here so every navigation path gets this for free instead of
/// each one having to remember to clear it.
fn disarm_rebind_off_screen(
    menu_screen: Res<State<crate::menu::MenuScreen>>,
    pause_screen: Res<PauseScreen>,
    mut rebinding: ResMut<Rebinding>,
) {
    if !rebinding.is_armed() {
        return;
    }
    let showing_controls = *menu_screen.get() == crate::menu::MenuScreen::Controls
        || *pause_screen == PauseScreen::Controls;
    if !showing_controls {
        rebinding.0 = None;
    }
}

/// Patches each row's background, [`Selected`] and text in place — the same
/// "rebuild on structure, patch on value" split [`sync_pause_overlay`] already
/// documents for its own sliders, so an armed row's own button is never
/// despawned out from under a keypress it is waiting on.
fn sync_binding_rows(
    settings: Res<Settings>,
    rebinding: Res<Rebinding>,
    mut commands: Commands,
    mut buttons: Query<(Entity, &BindingButton, Has<Selected>, &mut BackgroundColor)>,
    mut readouts: Query<(&BindingReadout, &mut Text)>,
) {
    if !settings.is_changed() && !rebinding.is_changed() {
        return;
    }
    for (entity, button, was_selected, mut background) in &mut buttons {
        let armed = rebinding.0 == Some(button.0);
        if armed != was_selected {
            let mut entity = commands.entity(entity);
            if armed {
                entity.insert(Selected);
            } else {
                entity.remove::<Selected>();
            }
        }
        background.0 = if armed { BUTTON_ACTIVE } else { BUTTON_IDLE };
    }
    for (readout, mut text) in &mut readouts {
        let armed = rebinding.0 == Some(readout.0);
        let wanted = if armed {
            "press a key...".to_string()
        } else {
            key_label(readout.0.read(&settings.bindings))
        };
        if text.0 != wanted {
            text.0 = wanted;
        }
    }
}

/// "W", "Left Shift", "F3" — what a key is called on a controls screen.
///
/// `KeyCode`'s own `Debug` is close enough to be tempting and wrong enough to
/// be embarrassing: it prints `KeyW`, `ShiftLeft`, `Digit1`.
pub(crate) fn key_label(key: KeyCode) -> String {
    let raw = format!("{key:?}");
    if let Some(letter) = raw.strip_prefix("Key") {
        return letter.to_string();
    }
    if let Some(digit) = raw.strip_prefix("Digit") {
        return digit.to_string();
    }
    match key {
        KeyCode::ShiftLeft => "Left Shift".to_string(),
        KeyCode::ShiftRight => "Right Shift".to_string(),
        KeyCode::ControlLeft => "Left Ctrl".to_string(),
        KeyCode::ControlRight => "Right Ctrl".to_string(),
        KeyCode::AltLeft => "Left Alt".to_string(),
        KeyCode::AltRight => "Right Alt".to_string(),
        KeyCode::Space => "Space".to_string(),
        _ => raw,
    }
}

#[cfg(test)]
mod tests {
    use bevy::input::keyboard::{Key, NativeKey};

    use super::*;

    #[test]
    fn the_defaults_are_exactly_what_the_game_shipped_with() {
        // Introducing a settings file must not silently re-tune a game that
        // was already played with these numbers.
        let settings = Settings::default();
        assert_eq!(settings.mouse_sensitivity, 0.0022);
        assert_eq!(settings.bindings.forward, KeyCode::KeyW);
        assert_eq!(settings.bindings.interact, KeyCode::KeyE);
        assert_eq!(settings.bindings.drop, KeyCode::KeyQ);
        assert_eq!(settings.bindings.drink, KeyCode::KeyR);
        assert_eq!(settings.bindings.apply, KeyCode::KeyF);
        assert_eq!(settings.bindings.book, KeyCode::KeyB);
        assert_eq!(settings.bindings.social, KeyCode::Tab);
    }

    #[test]
    fn every_dial_can_actually_reach_the_value_it_ships_at() {
        // A default outside its own dial's range would open the settings
        // screen showing a fill that does not match the setting, and there
        // would be no way to drag back to what the player has been using.
        let settings = Settings::default();
        for knob in [Knob::Sensitivity, Knob::Fov, Knob::Volume] {
            let (lo, hi) = knob.range();
            let value = knob.read(&settings);
            assert!(
                value >= lo && value <= hi,
                "{} ships at {value}, outside its own {lo}..={hi} dial",
                knob.title()
            );
        }
    }

    #[test]
    fn a_dial_reads_back_what_was_dragged_onto_it() {
        for knob in [Knob::Sensitivity, Knob::Fov, Knob::Volume] {
            for fraction in [0.0, 0.25, 0.5, 1.0] {
                let value = value_at(knob, fraction);
                assert!(
                    (fraction_of(knob, value) - fraction).abs() < 1e-5,
                    "{} lost the value at {fraction}",
                    knob.title()
                );
            }
        }
    }

    #[test]
    fn dragging_past_either_end_of_a_dial_clamps() {
        // The drag reads a raw cursor position, which is routinely outside the
        // track — pulling toward an end is how you reach it.
        for knob in [Knob::Sensitivity, Knob::Fov, Knob::Volume] {
            let (lo, hi) = knob.range();
            assert_eq!(value_at(knob, -3.0), lo);
            assert_eq!(value_at(knob, 4.0), hi);
        }
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let mut settings = Settings {
            mouse_sensitivity: 0.0045,
            fov_degrees: 100.0,
            ..Settings::default()
        };
        settings.bindings.interact = KeyCode::KeyG;

        let text = ron::ser::to_string_pretty(&settings, default()).unwrap();
        let back: Settings = ron::from_str(&text).unwrap();

        assert_eq!(back, settings);
    }

    #[test]
    fn a_settings_file_missing_every_field_still_loads() {
        // `#[serde(default)]` on both structs is what keeps adding a knob from
        // costing an existing player the ones they already set.
        let sparse: Settings = ron::from_str("(mouse_sensitivity: 0.005)").unwrap();
        assert_eq!(sparse.mouse_sensitivity, 0.005);
        assert_eq!(sparse.fov_degrees, Settings::default().fov_degrees);
        assert_eq!(sparse.bindings, Bindings::default());
    }

    #[test]
    fn a_corrupt_settings_file_is_ignored_rather_than_fatal() {
        // Exercised through `ron` directly: `Settings::load` reads a fixed
        // path, and a test that wrote to it would fight every other test run.
        assert!(ron::from_str::<Settings>("this is not ron at all").is_err());
    }

    #[test]
    fn keys_are_named_the_way_a_player_would_name_them() {
        assert_eq!(key_label(KeyCode::KeyW), "W");
        assert_eq!(key_label(KeyCode::Digit1), "1");
        assert_eq!(key_label(KeyCode::ShiftLeft), "Left Shift");
        assert_eq!(key_label(KeyCode::Space), "Space");
    }

    #[test]
    fn every_binding_including_the_label_key_has_a_slot() {
        // `label` had no row on the old `described()`-based Controls screen —
        // no way for a player to ever discover it existed. `BindingSlot::ALL`
        // enumerating every field of `Bindings` fixes that by construction.
        assert_eq!(BindingSlot::ALL.len(), 13);
        assert!(BindingSlot::ALL.iter().all(|slot| !slot.title().is_empty()));
        assert!(
            BindingSlot::ALL.contains(&BindingSlot::Label),
            "the label-writing key must be discoverable on the Controls screen"
        );
        assert!(BindingSlot::ALL.contains(&BindingSlot::Social));
    }

    #[test]
    fn rebind_swaps_rather_than_duplicates() {
        let mut bindings = Bindings::default();
        // `Forward` is `KeyW`; `Back`'s own key is what we ask `Forward` to
        // take, so the swap has something to displace.
        let backs_key = bindings.back;
        bindings.rebind(BindingSlot::Forward, backs_key);

        assert_eq!(bindings.forward, backs_key);
        assert_eq!(
            bindings.back,
            KeyCode::KeyW,
            "the displaced slot must receive the other's old key, not go unbound"
        );

        // No two slots ever share a key after a rebind.
        for a in BindingSlot::ALL {
            for b in BindingSlot::ALL {
                if a != b {
                    assert_ne!(a.read(&bindings), b.read(&bindings), "{a:?}/{b:?} collide");
                }
            }
        }
    }

    /// Builds the one `KeyboardInput` field that actually matters to
    /// [`capture_rebind_key`] — it only ever reads `key_code`/`state`/
    /// `repeat` — with a throwaway window entity and logical key, the same
    /// shape `menu`'s own `backspace_deletes_and_the_field_has_a_limit` test
    /// uses for a non-character key.
    fn key_press(app: &mut App, key_code: KeyCode, repeat: bool) -> KeyboardInput {
        let window = app.world_mut().spawn_empty().id();
        KeyboardInput {
            key_code,
            logical_key: Key::Unidentified(NativeKey::Unidentified),
            state: ButtonState::Pressed,
            text: None,
            repeat,
            window,
        }
    }

    #[test]
    fn escape_cancels_an_armed_rebind_without_writing_anything() {
        let mut app = App::new();
        app.add_message::<KeyboardInput>()
            .init_resource::<Settings>()
            .insert_resource(Rebinding(Some(BindingSlot::Forward)))
            .add_systems(Update, capture_rebind_key);
        let key = key_press(&mut app, KeyCode::Escape, false);
        app.world_mut().write_message(key);
        app.update();

        assert!(!app.world().resource::<Rebinding>().is_armed());
        assert_eq!(
            app.world().resource::<Settings>().bindings,
            Bindings::default(),
            "cancelling must not write anything"
        );
    }

    #[test]
    fn a_repeat_of_an_already_held_key_is_not_captured() {
        let mut app = App::new();
        app.add_message::<KeyboardInput>()
            .init_resource::<Settings>()
            .insert_resource(Rebinding(Some(BindingSlot::Forward)))
            .add_systems(Update, capture_rebind_key);
        let key = key_press(&mut app, KeyCode::KeyP, true);
        app.world_mut().write_message(key);
        app.update();

        assert_eq!(
            app.world().resource::<Rebinding>().0,
            Some(BindingSlot::Forward),
            "a key already held before the row was clicked must stay unconsumed"
        );
        assert_eq!(
            app.world().resource::<Settings>().bindings.forward,
            KeyCode::KeyW
        );
    }

    #[test]
    fn a_fresh_key_press_rebinds_and_disarms() {
        let mut app = App::new();
        app.add_message::<KeyboardInput>()
            .init_resource::<Settings>()
            .insert_resource(Rebinding(Some(BindingSlot::Forward)))
            .add_systems(Update, capture_rebind_key);
        let key = key_press(&mut app, KeyCode::KeyP, false);
        app.world_mut().write_message(key);
        app.update();

        assert!(!app.world().resource::<Rebinding>().is_armed());
        assert_eq!(
            app.world().resource::<Settings>().bindings.forward,
            KeyCode::KeyP
        );
    }

    #[test]
    fn leaving_controls_disarms_whatever_was_armed() {
        // Exercises the pause-screen half; the menu-screen half is the same
        // check against `crate::menu::MenuScreen` and is covered by
        // `menu`'s own tests reaching Settings/Controls.
        let mut app = App::new();
        app.add_plugins(bevy::state::app::StatesPlugin)
            .init_state::<crate::menu::MenuScreen>()
            .insert_resource(PauseScreen::Root)
            .insert_resource(Rebinding(Some(BindingSlot::Forward)))
            .add_systems(Update, disarm_rebind_off_screen);
        app.update();

        assert!(
            !app.world().resource::<Rebinding>().is_armed(),
            "leaving Controls for Root must disarm"
        );
    }

    #[test]
    fn staying_on_controls_keeps_the_capture_armed() {
        let mut app = App::new();
        app.add_plugins(bevy::state::app::StatesPlugin)
            .init_state::<crate::menu::MenuScreen>()
            .insert_resource(PauseScreen::Controls)
            .insert_resource(Rebinding(Some(BindingSlot::Forward)))
            .add_systems(Update, disarm_rebind_off_screen);
        app.update();

        assert!(app.world().resource::<Rebinding>().is_armed());
    }

    #[test]
    fn escape_steps_back_from_settings_and_controls_to_root() {
        assert_eq!(
            escape_steps_pause_screen_back_to(PauseScreen::Settings),
            Some(PauseScreen::Root)
        );
        assert_eq!(
            escape_steps_pause_screen_back_to(PauseScreen::Controls),
            Some(PauseScreen::Root)
        );
    }

    #[test]
    fn escape_closes_the_overlay_entirely_from_root_and_ending() {
        assert_eq!(escape_steps_pause_screen_back_to(PauseScreen::Root), None);
        assert_eq!(escape_steps_pause_screen_back_to(PauseScreen::Ending), None);
    }

    #[test]
    fn restoring_settings_defaults_leaves_bindings_untouched() {
        let mut settings = Settings {
            fov_degrees: 90.0,
            ..Settings::default()
        };
        settings.bindings.forward = KeyCode::ArrowUp;

        restore_settings_defaults(&mut settings);

        assert_eq!(settings.fov_degrees, Settings::default().fov_degrees);
        assert_eq!(settings.bindings.forward, KeyCode::ArrowUp);
    }

    #[test]
    fn restoring_bindings_leaves_everything_else_untouched() {
        let mut settings = Settings {
            fov_degrees: 90.0,
            ..Settings::default()
        };
        settings.bindings.forward = KeyCode::ArrowUp;

        restore_bindings_defaults(&mut settings);

        assert_eq!(settings.fov_degrees, 90.0);
        assert_eq!(
            settings.bindings.forward,
            Settings::default().bindings.forward
        );
    }

    #[test]
    fn applying_display_settings_writes_the_dialled_in_mode_and_resolution() {
        let mut app = App::new();
        app.insert_resource(Settings {
            display_mode: DisplayMode::Fullscreen,
            resolution: (1920, 1080),
            ..Settings::default()
        });
        let window = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();
        app.add_systems(Update, apply_display_settings);
        app.update();

        let window = app.world().entity(window).get::<Window>().unwrap();
        assert_eq!(
            window.mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        );
        assert_eq!(window.resolution.physical_width(), 1920);
        assert_eq!(window.resolution.physical_height(), 1080);
    }

    #[test]
    fn applying_display_settings_leaves_the_window_alone_once_it_matches() {
        // Same shape the live camera settings path relies on: a redundant write every
        // frame would wake the window backend for nothing once the two
        // already agree — exercised by marking `Settings` changed again with
        // no value actually different, the same as a slider drag or a
        // hand-edited file that happens to resolve to the identical number.
        // `Ref<Window>::is_changed()`, read by a system chained right after
        // `apply_display_settings`, is the standard way to ask "did the
        // previous system in this frame actually write to it" without
        // reaching into raw `ComponentTicks` arithmetic by hand.
        #[derive(Resource, Default)]
        struct WasChanged(bool);

        fn observe(mut was_changed: ResMut<WasChanged>, windows: Query<Ref<Window>>) {
            was_changed.0 = windows.single().is_ok_and(|window| window.is_changed());
        }

        let mut app = App::new();
        app.insert_resource(Settings::default())
            .init_resource::<WasChanged>();
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app.add_systems(Update, (apply_display_settings, observe).chain());

        // First run: `Settings` was just inserted, so it reads as changed
        // regardless of value — spend that one before the assertion.
        app.update();

        app.world_mut().resource_mut::<Settings>().set_changed();
        app.update();

        assert!(
            !app.world().resource::<WasChanged>().0,
            "Window already matched Settings, so a same-valued change must not touch it"
        );
    }

    #[test]
    fn fullscreen_maps_to_borderless_fullscreen_on_the_current_monitor() {
        assert_eq!(
            DisplayMode::Fullscreen.to_window_mode(),
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        );
        assert_eq!(DisplayMode::Windowed.to_window_mode(), WindowMode::Windowed);
    }

    // Bevy validates query aliasing while a system is initialized — the same
    // technique `audio::machine_loop_queries_are_disjoint_at_runtime` already
    // uses, and the one that would have caught the B0001
    // `sync_display_buttons` shipped with before its `modes` query gained its
    // own `Without<ResolutionChoice>`: both it and `resolutions` mutate
    // `BackgroundColor`, and only one side carried a disjointing filter.
    #[test]
    fn display_button_queries_are_disjoint_at_runtime() {
        let mut world = World::new();
        let mut schedule = Schedule::default();
        schedule.add_systems(sync_display_buttons);
        schedule.initialize(&mut world).unwrap();
    }

    #[test]
    fn binding_row_queries_are_disjoint_at_runtime() {
        let mut world = World::new();
        let mut schedule = Schedule::default();
        schedule.add_systems(sync_binding_rows);
        schedule.initialize(&mut world).unwrap();
    }

    #[test]
    fn settings_live_beside_the_real_save_slots() {
        assert_eq!(Settings::path(), saves::saves_root().join("settings.ron"));
    }

    #[test]
    fn defaults_ship_at_the_same_window_bevy_would() {
        assert_eq!(Settings::default().display_mode, DisplayMode::Windowed);
        assert_eq!(Settings::default().resolution, (1280, 720));
    }

    #[test]
    fn only_singleplayer_stops_the_clock() {
        // A host pausing would freeze the guest's world from under them, and a
        // guest pausing would achieve nothing at all. Both still get the
        // overlay and both still stop taking input — that half is about the
        // person reading it, not about the simulation.
        assert!(owns_the_clock(Some(&LaunchMode::Singleplayer)));
        assert!(owns_the_clock(None));
        assert!(!owns_the_clock(Some(&LaunchMode::Host)));
        assert!(!owns_the_clock(Some(&LaunchMode::HostSteam)));
    }
    #[test]
    fn adding_inspect_keeps_existing_rebound_keys_distinct() {
        let mut old: Bindings = ron::from_str("(interact:KeyI)").unwrap();
        old.migrate_inspect_binding();
        assert_eq!(old.interact, KeyCode::KeyI);
        assert_ne!(old.inspect, old.interact);
        assert_eq!(old.inspect, KeyCode::KeyO);
    }
}
