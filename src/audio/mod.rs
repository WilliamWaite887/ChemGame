//! Sound. Every `.ogg` in `assets/sounds/` (sourced from tgstation — see
//! `CREDITS.md`), and the one place that actually calls
//! [`AudioPlayer`]/[`PlaybackSettings`].
//!
//! Presentation only, the same layering [`crate::fx`] already draws between
//! state and how it looks: this module reads other modules' state and
//! messages, and none of them know it exists. [`PlaySfx`] is the one thing
//! other modules write to reach it — a plain local [`Message`], deliberately
//! not networked, because "make a sound on my own machine" needs no wire at
//! all. `handle_panel_clicks` (`crate::ui`) is the one place that writes it
//! directly, for the handful of actions with a specific cue; everything else
//! here derives a sound from a signal that already exists.
//!
//! One real limitation: [`play_reaction_sfx`]'s underlying signal
//! (`ReactionsFired`) is deliberately not networked, so it only ever fires
//! on whichever peer is authority — giving it a proper co-op-wide cue would
//! need a new synced message the same shape as `HazardFelt`.

use std::collections::HashSet;

use bevy::audio::Volume;
use bevy::prelude::*;
use rand::prelude::*;

use crate::body::Body;
use crate::door::Door;
use crate::hazards::{HazardFelt, HazardKind};
use crate::machines::ReactionsFired;
use crate::orders::{CrisisOrder, Shift};
use crate::radio::RadioLog;
use crate::settings::Settings;
use crate::AppState;

/// Ambient loops, cycled at random rather than once on repeat — see
/// `sound/ambience/general` in tgstation, `CREDITS.md` for licensing.
/// Everything but the two reserved for [`tense_moment`].
const CALM_AMBIENCE: [&str; 8] = [
    "sounds/ambience/ambigen1.ogg",
    "sounds/ambience/ambigen2.ogg",
    "sounds/ambience/ambigen3.ogg",
    "sounds/ambience/ambigen4.ogg",
    "sounds/ambience/ambigen9.ogg",
    "sounds/ambience/ambigen12.ogg",
    "sounds/ambience/ambigen13.ogg",
    "sounds/ambience/shipambience.ogg",
];

/// `ambigen10`/`ambigen11` — noticeably more distressing than the rest of the
/// set, so reserved for while something is actually wrong rather than mixed
/// into ordinary rotation. Only eligible while [`tense_moment`] holds, which
/// is the minority of a shift, so the calm pool still plays the most overall
/// with no weighting needed.
const TENSE_AMBIENCE: [&str; 2] = [
    "sounds/ambience/ambigen10.ogg",
    "sounds/ambience/ambigen11.ogg",
];

/// Quieter than any one-shot — this is a bed, not a cue.
const AMBIENCE_VOLUME: f32 = 0.35;

pub struct SfxPlugin;

impl Plugin for SfxPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PlaySfx>()
            .init_resource::<RadioCursor>()
            .init_resource::<AmbienceCooldown>()
            .add_systems(Startup, load_sfx)
            // Baselines against whatever radio history is already there
            // rather than replaying an old save's chatter as fresh blips.
            .add_systems(
                OnEnter(AppState::Playing),
                (
                    reset_radio_cursor,
                    |mut cooldown: ResMut<AmbienceCooldown>| {
                        cooldown.0 = None;
                    },
                ),
            )
            .add_systems(
                Update,
                (
                    play_sfx,
                    sync_master_volume,
                    play_hazard_sfx,
                    play_reaction_sfx,
                    play_collapse_sfx,
                    play_crisis_alarm_sfx,
                    play_radio_sfx,
                    play_ui_click_sfx,
                    play_door_sfx,
                    cycle_ambience,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Every sourced one-shot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sfx {
    DispensePour,
    Eject,
    BufferTransfer,
    ReactionOccurred,
    HazardSmoke,
    HazardExplosion,
    MajorAlarm,
    Fall,
    Grinder,
    DoorOpen,
    DoorClosed,
    OrderSuccess,
    RadioBlip,
    RequisitionConfirm,
    UiClick,
    UiRefused,
}

impl Sfx {
    /// Relative loudness against the master volume dial — 1.0 plays the
    /// source file as-is. The doors are the one pair that came in reading
    /// hotter than everything else at the same dial position.
    fn volume(self) -> f32 {
        match self {
            Sfx::DoorOpen | Sfx::DoorClosed => 0.5,
            _ => 1.0,
        }
    }
}

/// "Play this, on my machine, now." Local rather than networked — every peer
/// that wants a sound writes its own copy of this on its own `App`, the same
/// way `ShowToast` (`crate::ui`) is a local-only UI message.
#[derive(Message, Clone, Copy)]
pub struct PlaySfx(pub Sfx);

#[derive(Resource)]
struct SfxAssets {
    dispense_pour: Handle<AudioSource>,
    eject: Handle<AudioSource>,
    buffer_transfer: Handle<AudioSource>,
    reaction_occurred: Handle<AudioSource>,
    hazard_smoke: Handle<AudioSource>,
    hazard_explosion: Handle<AudioSource>,
    major_alarm: Handle<AudioSource>,
    fall: Handle<AudioSource>,
    grinder: Handle<AudioSource>,
    door_open: Handle<AudioSource>,
    door_closed: Handle<AudioSource>,
    order_success: Handle<AudioSource>,
    radio_blip: Handle<AudioSource>,
    requisition_confirm: Handle<AudioSource>,
    ui_click: Handle<AudioSource>,
    ui_refused: Handle<AudioSource>,
    calm_ambience: Vec<Handle<AudioSource>>,
    tense_ambience: Vec<Handle<AudioSource>>,
}

impl SfxAssets {
    fn handle(&self, sfx: Sfx) -> Handle<AudioSource> {
        match sfx {
            Sfx::DispensePour => &self.dispense_pour,
            Sfx::Eject => &self.eject,
            Sfx::BufferTransfer => &self.buffer_transfer,
            Sfx::ReactionOccurred => &self.reaction_occurred,
            Sfx::HazardSmoke => &self.hazard_smoke,
            Sfx::HazardExplosion => &self.hazard_explosion,
            Sfx::MajorAlarm => &self.major_alarm,
            Sfx::Fall => &self.fall,
            Sfx::Grinder => &self.grinder,
            Sfx::DoorOpen => &self.door_open,
            Sfx::DoorClosed => &self.door_closed,
            Sfx::OrderSuccess => &self.order_success,
            Sfx::RadioBlip => &self.radio_blip,
            Sfx::RequisitionConfirm => &self.requisition_confirm,
            Sfx::UiClick => &self.ui_click,
            Sfx::UiRefused => &self.ui_refused,
        }
        .clone()
    }
}

/// A `Handle<AudioSource>` referring to a still-loading asset plays as soon
/// as it resolves (Bevy's own guarantee — see `AudioPlayer`'s doc), so this
/// needs no loading-state gate: every handle can be requested immediately.
fn load_sfx(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(SfxAssets {
        dispense_pour: assets.load("sounds/dispense_pour.ogg"),
        eject: assets.load("sounds/eject.ogg"),
        buffer_transfer: assets.load("sounds/buffer_transfer.ogg"),
        reaction_occurred: assets.load("sounds/reaction_occurred.ogg"),
        hazard_smoke: assets.load("sounds/hazard_smoke.ogg"),
        hazard_explosion: assets.load("sounds/hazard_explosion.ogg"),
        major_alarm: assets.load("sounds/major_alarm.ogg"),
        fall: assets.load("sounds/fall.ogg"),
        grinder: assets.load("sounds/grinder.ogg"),
        door_open: assets.load("sounds/door_open.ogg"),
        door_closed: assets.load("sounds/door_closed.ogg"),
        order_success: assets.load("sounds/order_success.ogg"),
        radio_blip: assets.load("sounds/radio_blip.ogg"),
        requisition_confirm: assets.load("sounds/requisition_confirm.ogg"),
        ui_click: assets.load("sounds/ui_click.ogg"),
        ui_refused: assets.load("sounds/ui_refused.ogg"),
        calm_ambience: CALM_AMBIENCE
            .iter()
            .map(|path| assets.load(*path))
            .collect(),
        tense_ambience: TENSE_AMBIENCE
            .iter()
            .map(|path| assets.load(*path))
            .collect(),
    });
}

/// The one place anything actually spawns an `AudioPlayer`.
fn play_sfx(mut commands: Commands, assets: Res<SfxAssets>, mut requests: MessageReader<PlaySfx>) {
    for PlaySfx(sfx) in requests.read() {
        commands.spawn((
            AudioPlayer::new(assets.handle(*sfx)),
            PlaybackSettings::DESPAWN.with_volume(Volume::Linear(sfx.volume())),
            crate::until_we_leave_the_lab(),
        ));
    }
}

/// Keeps every sound scaled by the settings dial — `Settings::master_volume`
/// was carried unread since the knob shipped for exactly this.
fn sync_master_volume(settings: Res<Settings>, mut global_volume: ResMut<GlobalVolume>) {
    if !settings.is_changed() {
        return;
    }
    global_volume.volume = Volume::Linear(settings.master_volume);
}

/// Same message `fx::apply_hazard_felt` reads for the camera kick — already
/// networked per-affected-chemist, so this needs no gate of its own.
fn play_hazard_sfx(mut felt: MessageReader<HazardFelt>, mut play: MessageWriter<PlaySfx>) {
    for message in felt.read() {
        let sfx = match message.kind {
            HazardKind::Blast => Sfx::HazardExplosion,
            HazardKind::Smoke => Sfx::HazardSmoke,
            // Radiation, and a rogue officer turning physical, still get the
            // camera kick from `fx` — just no sourced cue of their own yet.
            HazardKind::Radiation | HazardKind::Assault => continue,
        };
        play.write(PlaySfx(sfx));
    }
}

/// `ReactionsFired` is deliberately not networked (see its own doc comment
/// in `crate::machines`) — a server-side consequence, not a request — so
/// this only ever fires on whichever peer is authority (the host, or the
/// only peer in singleplayer). Giving a joining guest their own beaker-fizz
/// cue would need a new synced message shaped like `HazardFelt`; wiring in
/// the sourced files is what this pass is for, not that.
fn play_reaction_sfx(mut fired: MessageReader<ReactionsFired>, mut play: MessageWriter<PlaySfx>) {
    for _ in fired.read() {
        play.write(PlaySfx(Sfx::ReactionOccurred));
    }
}

/// `Body` replicates normally, so this reads the same on every peer with no
/// authority gate — same reasoning as `body::close_panel_on_collapse`.
/// `Local` tracking rather than `Added<Collapsed>`-style marker: collapse is
/// a field flip on an existing component, not a spawn.
fn play_collapse_sfx(
    bodies: Query<(Entity, &Body), Changed<Body>>,
    mut known_down: Local<HashSet<Entity>>,
    mut play: MessageWriter<PlaySfx>,
) {
    for (entity, body) in &bodies {
        if body.0.collapsed {
            if known_down.insert(entity) {
                play.write(PlaySfx(Sfx::Fall));
            }
        } else {
            known_down.remove(&entity);
        }
    }
}

/// `CrisisOrder` replicates for exactly this reason (see its
/// `.replicate::<CrisisOrder>()` registration in `crate::net`, which already
/// leans on the same guarantee for `crisis::pulse_alert_lighting`) — every
/// peer sees the same victim entity appear at the same moment, so the alarm
/// needs no message of its own.
fn play_crisis_alarm_sfx(
    new_crises: Query<(), Added<CrisisOrder>>,
    mut play: MessageWriter<PlaySfx>,
) {
    for () in &new_crises {
        play.write(PlaySfx(Sfx::MajorAlarm));
    }
}

/// Baseline for [`play_radio_sfx`] — `None` means "not seen a frame since
/// entering the lab yet", which is what tells that system to baseline
/// silently instead of replaying old history as fresh blips.
#[derive(Resource, Default)]
struct RadioCursor(Option<usize>);

fn reset_radio_cursor(mut cursor: ResMut<RadioCursor>) {
    cursor.0 = None;
}

/// `RadioLog` is correct on every peer already — a client only ever writes
/// it via `radio::apply_radio`'s replicated snapshot, the host writes it
/// directly — so watching it for growth reaches every peer for free, no new
/// message needed for either the crisis alarm's follow-up chatter or an
/// ordinary report landing.
///
/// A length compare, not a content diff: `RadioLog` trims its oldest entry
/// past `LOG_CAPACITY`, so a push landing in the same tick as a trim could in
/// principle leave the length unchanged and this would miss it. Cheaper to
/// accept one skipped blip on that rare edge than to give `RadioEntry` — with
/// its several dozen construction sites across every antagonist thread — a
/// sequence number just to close it.
fn play_radio_sfx(
    log: Res<RadioLog>,
    mut cursor: ResMut<RadioCursor>,
    mut play: MessageWriter<PlaySfx>,
) {
    let len = log.entries.len();
    let Some(last) = cursor.0 else {
        cursor.0 = Some(len);
        return;
    };
    if len > last {
        for entry in log.entries.iter().skip(last).take(len - last) {
            play.write(PlaySfx(if entry.good {
                Sfx::OrderSuccess
            } else {
                Sfx::RadioBlip
            }));
        }
    }
    cursor.0 = Some(len);
}

/// `Door` replicates normally (see `door`'s module doc), so this reads the
/// same on every peer with no authority gate — same reasoning as
/// `play_collapse_sfx`.
fn play_door_sfx(doors: Query<&Door, Changed<Door>>, mut play: MessageWriter<PlaySfx>) {
    for door in &doors {
        play.write(PlaySfx(if door.open {
            Sfx::DoorOpen
        } else {
            Sfx::DoorClosed
        }));
    }
}

/// Universal click feedback for every plain button — pause/settings/main
/// menu choices, sliders, the machine panel alike. `handle_panel_clicks`
/// layers its own specific cue (a pour, an eject, a chime) on top for the
/// handful of actions that have one; both landing on the same click reads as
/// "press" then "what happened", not as a double-up.
fn play_ui_click_sfx(
    buttons: Query<&Interaction, (Changed<Interaction>, With<Button>)>,
    mut play: MessageWriter<PlaySfx>,
) {
    for interaction in &buttons {
        if *interaction == Interaction::Pressed {
            play.write(PlaySfx(Sfx::UiClick));
        }
    }
}

/// Marks whichever ambience loop is currently playing, so [`cycle_ambience`]
/// knows when it has ended and it is time to pick another.
#[derive(Component)]
struct AmbiencePlayer;

/// A crisis is live, or the shift is on its way out — the two moments the
/// station itself would read as "something is wrong", not just "quiet".
/// `Shift::accepting_orders`/`::called` are the same two fields
/// `ui::board_stage` reads for its own `WrappingUp` stage, without pulling in
/// its `waiting`-count query — that count decides whether *the button* is
/// live, not the general mood of the room.
fn tense_moment(shift: &Shift, active_crises: &Query<(), With<CrisisOrder>>) -> bool {
    !active_crises.is_empty() || (!shift.accepting_orders && !shift.called)
}

/// How long the lab sits in near-silence between one ambience loop ending
/// and the next starting. Rolled fresh for every gap, so the room does not
/// settle into an audible rhythm.
const AMBIENCE_GAP_SECONDS: (f32, f32) = (30.0, 90.0);

/// The countdown to the next ambience pick, while nothing is playing.
/// `None` both before the first gap has been rolled and while a track is
/// actually going — [`cycle_ambience`] rolls a fresh one the moment silence
/// starts rather than keeping a stale countdown from the last one.
#[derive(Resource, Default)]
struct AmbienceCooldown(Option<Timer>);

/// Keeps at most one ambience loop going, with a quiet gap between each and
/// the next — the lab is not meant to have a bed track running back to back,
/// just something in the room now and then. Which pool a pick draws from is
/// decided fresh at that moment, so a track already playing is never cut off
/// mid-loop by a mood change.
fn cycle_ambience(
    time: Res<Time>,
    mut commands: Commands,
    assets: Res<SfxAssets>,
    shift: Res<Shift>,
    active_crises: Query<(), With<CrisisOrder>>,
    playing: Query<(), With<AmbiencePlayer>>,
    mut cooldown: ResMut<AmbienceCooldown>,
) {
    if !playing.is_empty() {
        cooldown.0 = None;
        return;
    }
    let timer = cooldown.0.get_or_insert_with(|| {
        let gap = rand::rng().random_range(AMBIENCE_GAP_SECONDS.0..=AMBIENCE_GAP_SECONDS.1);
        Timer::from_seconds(gap, TimerMode::Once)
    });
    if !timer.tick(time.delta()).is_finished() {
        return;
    }
    cooldown.0 = None;

    let pool = if tense_moment(&shift, &active_crises) {
        &assets.tense_ambience
    } else {
        &assets.calm_ambience
    };
    let Some(handle) = pool.choose(&mut rand::rng()) else {
        return;
    };
    commands.spawn((
        AudioPlayer::new(handle.clone()),
        PlaybackSettings::DESPAWN.with_volume(Volume::Linear(AMBIENCE_VOLUME)),
        AmbiencePlayer,
        crate::until_we_leave_the_lab(),
    ));
}
