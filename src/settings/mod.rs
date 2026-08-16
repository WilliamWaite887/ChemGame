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

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::menu::{choice, menu_shell};
use crate::net::LaunchMode;
use crate::ui::{button_feedback, label, SECTION_BG, TEXT, TEXT_DIM};
use crate::AppState;

/// Where the settings file lives — beside `saves/`, not inside a slot.
const SETTINGS_FILE: &str = "saves/settings.ron";

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Settings::load())
            .init_resource::<Paused>()
            .init_resource::<PauseScreen>()
            .add_systems(
                Update,
                (
                    // Ordered: the clock has to follow whatever the click
                    // handler just did to `Paused`, and the overlay has to
                    // follow both or it lags a frame behind the state it is
                    // drawing.
                    handle_pause_clicks,
                    drag_sliders,
                    apply_pause_to_the_clock,
                    sync_pause_overlay,
                    // After the rebuild, so a dial drawn this frame is filled
                    // in this frame rather than sitting empty until the next
                    // time the value happens to move.
                    sync_sliders,
                    apply_fov,
                    persist_settings,
                    button_feedback,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
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
#[derive(Resource, Default, PartialEq, Eq, Clone, Copy)]
pub enum PauseScreen {
    #[default]
    Root,
    Settings,
    Controls,
    /// The arc has resolved. Raised by `crate::ending`, which owns everything
    /// about what the screen says; this module only owns the fact that it is a
    /// screen, and therefore gets the freed cursor, the stopped input and the
    /// held clock for nothing.
    Ending,
}

#[derive(Component, Clone, Copy)]
pub enum PauseAction {
    Resume,
    OpenSettings,
    OpenControls,
    Back,
    /// Leaves the lab for the main menu. `crate::session` unwinds the world
    /// and `crate::net::close_session_transport` hangs up the socket.
    QuitToMenu,
    QuitToDesktop,
    /// Puts every dial back where it shipped.
    RestoreDefaults,
}

/// Draws and tears down the overlay to match [`Paused`].
///
/// One system rather than `OnEnter`/`OnExit` because `Paused` is a resource,
/// not a state — and rebuilding on a signature change is the same shape
/// `ui::sync_panel` already uses for machine panels.
fn sync_pause_overlay(
    mut commands: Commands,
    paused: Res<Paused>,
    mut screen: ResMut<PauseScreen>,
    settings: Res<Settings>,
    mode: Option<Res<LaunchMode>>,
    ending: Res<crate::ending::FinishedArc>,
    roots: Query<Entity, With<PauseRoot>>,
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
        PauseScreen::Root => draw_root(&mut commands, co_op),
        PauseScreen::Settings => draw_settings(&mut commands, &settings),
        PauseScreen::Controls => draw_controls(&mut commands, &settings),
        PauseScreen::Ending => match ending.showing() {
            Some(ending) => crate::ending::draw(
                &mut commands,
                ending,
                (PauseRoot, crate::until_we_leave_the_lab()),
            ),
            // Unreachable: `ending::notice_the_ending` writes the content
            // before it selects this screen. Falling back to the ordinary
            // pause menu beats an empty screen with no way off it.
            None => draw_root(&mut commands, co_op),
        },
    }
}

fn draw_root(commands: &mut Commands, co_op: bool) {
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

fn draw_settings(commands: &mut Commands, settings: &Settings) {
    menu_shell(
        commands,
        (PauseRoot, crate::until_we_leave_the_lab()),
        "Settings",
        "Drag a dial, or click anywhere along it. Kept in saves/settings.ron, \
         shared by every career.",
        |panel| {
            for knob in [Knob::Sensitivity, Knob::Fov, Knob::Volume] {
                slider_row(panel, knob, settings);
            }
            panel.spawn(choice(
                "Restore defaults",
                "Puts every dial on this screen back where it shipped.",
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

fn draw_controls(commands: &mut Commands, settings: &Settings) {
    menu_shell(
        commands,
        (PauseRoot, crate::until_we_leave_the_lab()),
        "Controls",
        "Rebinding is not wired to a key-capture yet — this is what they do today.",
        |panel| {
            for (name, key) in settings.bindings.described() {
                panel.spawn(label(
                    format!("{name:<22}{}", key_label(key)),
                    15.0,
                    TEXT_DIM,
                ));
            }
            panel.spawn(label(
                "Mouse                 look\nEsc                   pause, or step back out of a panel",
                15.0,
                TEXT_DIM,
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
            PauseAction::RestoreDefaults => {
                let bindings = settings.bindings;
                *settings = Settings {
                    // Rebinding is not reachable from this screen, so
                    // "restore defaults" here must not silently undo it.
                    bindings,
                    ..Settings::default()
                };
            }
        }
    }
}

/// Keeps the chemist's camera on the dialled-in field of view.
///
/// Its own system rather than something `player::adopt_my_chemist` sets at
/// spawn, because it has to answer both questions: a camera that appears after
/// the setting was chosen, and a setting changed while the camera is already
/// looking at the room. `Changed<Projection>` on the query is what keeps it
/// from touching the component — and so waking the render world — every frame.
fn apply_fov(
    settings: Res<Settings>,
    mut cameras: Query<&mut Projection, With<crate::player::PlayerCamera>>,
) {
    let wanted = settings.fov_degrees.to_radians();
    for mut projection in &mut cameras {
        let Projection::Perspective(perspective) = &*projection else {
            continue;
        };
        if (perspective.fov - wanted).abs() < f32::EPSILON {
            continue;
        }
        if let Projection::Perspective(perspective) = &mut *projection {
            perspective.fov = wanted;
        }
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
    /// Nothing reads this yet. Carried anyway so the audio pass has somewhere
    /// to land that is already persisted and already on a screen.
    pub master_volume: f32,
    pub bindings: Bindings,
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
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        Path::new(SETTINGS_FILE).to_path_buf()
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
            Ok(Ok(settings)) => settings,
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
        }
    }
}

impl Bindings {
    /// Every binding with the name the controls screen shows it under, in the
    /// order it reads best — movement, then the hands, then the book.
    pub fn described(&self) -> [(&'static str, KeyCode); 10] {
        [
            ("Walk forward", self.forward),
            ("Walk back", self.back),
            ("Step left", self.left),
            ("Step right", self.right),
            ("Sprint", self.sprint),
            ("Use / hand over", self.interact),
            ("Drop what you hold", self.drop),
            ("Drink or swallow", self.drink),
            ("Apply held item", self.apply),
            ("Reference book", self.book),
        ]
    }
}

/// "W", "Left Shift", "F3" — what a key is called on a controls screen.
///
/// `KeyCode`'s own `Debug` is close enough to be tempting and wrong enough to
/// be embarrassing: it prints `KeyW`, `ShiftLeft`, `Digit1`.
fn key_label(key: KeyCode) -> String {
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
    fn every_binding_is_named_on_the_controls_screen() {
        // A binding with no row is one the player cannot discover exists.
        let described = Bindings::default().described();
        assert_eq!(described.len(), 10);
        assert!(described.iter().all(|(name, _)| !name.is_empty()));
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
}
