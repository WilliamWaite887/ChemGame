//! Sound. Every `.ogg` in `assets/sounds/` (sourced from tgstation or Space
//! Station 14 — see
//! `CREDITS.md`), and the one place that actually calls
//! [`AudioPlayer`]/[`PlaybackSettings`].
//!
//! Presentation only, the same layering [`crate::fx`] already draws between
//! state and how it looks: this module reads other modules' state and
//! messages, and none of them know how playback works. [`PlaySfx`] is the
//! local channel for UI/radio cues. Physical actions instead write
//! [`EmitWorldSfx`] after authority validation; [`fanout_world_sfx`] resolves
//! pooled variants once, plays them for the host, and sends the exact same
//! positioned [`WorldSfx`] to remote clients.

use std::collections::{HashMap, HashSet};

use bevy::audio::Volume;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::StatusKind;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::body::{Bloodstream, Body};
use crate::containers::{HeldBy, InSlot, InSlotB};
use crate::door::Door;
use crate::ending::FinishedArc;
use crate::hazards::ActiveHazard;
use crate::machines::{AgitationRun, Machine, MachineKind, ReactionsFired, Thermostat};
use crate::net::is_authority;
use crate::orders::{CrisisOrder, Shift};
use crate::radio::{RadioChannel, RadioLog, RadioPriority, RadioTone};
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

/// The rad klaxon runs for the whole length of a leak — forty seconds, on a
/// seven-second loop — so it is mixed well under a one-shot. It is there to
/// keep the room feeling wrong while you dose up, not to drown out the radio
/// telling you what to do about it.
const RADIATION_ALARM_VOLUME: f32 = 0.4;

pub struct SfxPlugin;

impl Plugin for SfxPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PlaySfx>()
            .add_message::<EmitWorldSfx>()
            .add_server_message::<WorldSfx>(Channel::Ordered)
            .init_resource::<RadioCursor>()
            .init_resource::<AmbienceCooldown>()
            .init_resource::<VariantCursor>()
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
                    (fanout_world_sfx.run_if(is_authority), play_world_sfx).chain(),
                    sync_master_volume,
                    play_reaction_sfx.run_if(is_authority),
                    play_collapse_sfx.run_if(is_authority),
                    play_status_sfx.run_if(is_authority),
                    sync_machine_loops,
                    voice_conveyor_hums,
                    voice_parcel_drops,
                    sync_radiation_alarm,
                    play_crisis_alarm_sfx,
                    play_evacuation_sfx,
                    play_radio_sfx,
                    play_ui_click_sfx,
                    play_door_sfx.run_if(is_authority),
                    cycle_ambience,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Every sourced one-shot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    RadioMedical,
    RadioSecurity,
    RadioEngineering,
    RadioCargo,
    RadioService,
    RadioBridge,
    RadioUrgent,
    BridgePriority,
    Announce,
    RedAlert,
    ShuttleCalled,
    RequisitionConfirm,
    UiClick,
    UiRefused,
    DirectPour,
    Splash,
    Drink,
    Inject,
    AnalyzerFinish,
    PackagePop,
    GlassClunk,
    Corrosion,
    Ignite,
    Flash,
    Slip,
    RadiationPulse,
    Cough,
    AssaultImpact,
}

impl Sfx {
    /// Relative loudness against the master volume dial — 1.0 plays the
    /// source file as-is. The doors are the one pair that came in reading
    /// hotter than everything else at the same dial position.
    fn volume(self) -> f32 {
        match self {
            Sfx::DoorOpen | Sfx::DoorClosed => 0.5,
            Sfx::DispensePour
            | Sfx::Eject
            | Sfx::BufferTransfer
            | Sfx::ReactionOccurred
            | Sfx::Fall
            | Sfx::Grinder
            | Sfx::DirectPour
            | Sfx::Splash
            | Sfx::Drink
            | Sfx::Inject
            | Sfx::GlassClunk
            | Sfx::Cough => 0.55,
            Sfx::AnalyzerFinish
            | Sfx::PackagePop
            | Sfx::RadioMedical
            | Sfx::RadioSecurity
            | Sfx::RadioEngineering
            | Sfx::RadioCargo
            | Sfx::RadioService
            | Sfx::RadioBridge
            | Sfx::RadioUrgent => 0.65,
            Sfx::Corrosion
            | Sfx::Ignite
            | Sfx::Flash
            | Sfx::HazardSmoke
            | Sfx::HazardExplosion
            | Sfx::RadiationPulse
            | Sfx::AssaultImpact => 0.8,
            Sfx::Slip => 0.7,
            // The full-length station announcements. Held under the one-shots
            // because each runs for several seconds over whatever else the
            // room is doing, and each lands in the same frame as the radio
            // card that explains it. The PA fanfare sits lowest of the three:
            // it fronts a joke, not an emergency.
            Sfx::RedAlert | Sfx::ShuttleCalled => 0.85,
            Sfx::Announce => 0.6,
            _ => 1.0,
        }
    }

    fn variants(self) -> u8 {
        match self {
            Sfx::RadiationPulse => 12,
            Sfx::Cough => 4,
            Sfx::GlassClunk => 2,
            _ => 1,
        }
    }
}

/// "Play this, on my machine, now." Local rather than networked — every peer
/// that wants a sound writes its own copy of this on its own `App`, the same
/// way `ShowToast` (`crate::ui`) is a local-only UI message.
#[derive(Message, Clone, Copy)]
pub struct PlaySfx(pub Sfx);

/// An authority-confirmed physical sound before it is fanned out to peers.
/// Gameplay systems write this only after their mutation succeeds.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct EmitWorldSfx {
    pub sound: Sfx,
    pub position: Vec3,
}

impl EmitWorldSfx {
    pub const fn new(sound: Sfx, position: Vec3) -> Self {
        Self { sound, position }
    }
}

/// The exact positional cue sent by the authority. `variant` is resolved
/// before transmission so every listener hears the same member of a pool.
#[derive(Message, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldSfx {
    pub sound: Sfx,
    pub variant: u8,
    pub position: Vec3,
}

#[derive(Resource, Default)]
struct VariantCursor(HashMap<Sfx, u8>);

impl VariantCursor {
    fn next(&mut self, sound: Sfx) -> u8 {
        let count = sound.variants();
        if count <= 1 {
            return 0;
        }
        let cursor = self.0.entry(sound).or_insert_with(|| {
            // Start each pool somewhere other than zero without sacrificing
            // the no-immediate-repeat guarantee of the round-robin advance.
            rand::rng().random_range(0..count)
        });
        let selected = *cursor;
        *cursor = (*cursor + 1) % count;
        selected
    }
}

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
    /// The two-tone chime in front of a station-wide announcement.
    attention: Handle<AudioSource>,
    /// The full PA fanfare, for the station's own bulletins.
    announce: Handle<AudioSource>,
    red_alert: Handle<AudioSource>,
    shuttle_called: Handle<AudioSource>,
    /// Looped, not one-shot — see [`sync_radiation_alarm`].
    radiation_alarm: Handle<AudioSource>,
    requisition_confirm: Handle<AudioSource>,
    ui_click: Handle<AudioSource>,
    ui_refused: Handle<AudioSource>,
    direct_pour: Handle<AudioSource>,
    splash: Handle<AudioSource>,
    drink: Handle<AudioSource>,
    inject: Handle<AudioSource>,
    analyzer_finish: Handle<AudioSource>,
    package_pop: Handle<AudioSource>,
    glass_clunks: [Handle<AudioSource>; 2],
    corrosion: Handle<AudioSource>,
    ignite: Handle<AudioSource>,
    flash: Handle<AudioSource>,
    slip: Handle<AudioSource>,
    radiation_pulses: [Handle<AudioSource>; 12],
    coughs: [Handle<AudioSource>; 4],
    assault_impact: Handle<AudioSource>,
    agitation_loop: Handle<AudioSource>,
    heater_loop: Handle<AudioSource>,
    conveyor_loop: Handle<AudioSource>,
    parcel_drop: Handle<AudioSource>,
    calm_ambience: Vec<Handle<AudioSource>>,
    tense_ambience: Vec<Handle<AudioSource>>,
}

impl SfxAssets {
    fn handle(&self, sfx: Sfx, variant: u8) -> Handle<AudioSource> {
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
            Sfx::RadioMedical => &self.analyzer_finish,
            Sfx::RadioSecurity => &self.ui_refused,
            Sfx::RadioEngineering => &self.buffer_transfer,
            Sfx::RadioCargo => &self.requisition_confirm,
            Sfx::RadioService => &self.package_pop,
            Sfx::RadioBridge => &self.ui_click,
            Sfx::RadioUrgent => &self.ui_refused,
            Sfx::BridgePriority => &self.attention,
            Sfx::Announce => &self.announce,
            Sfx::RedAlert => &self.red_alert,
            Sfx::ShuttleCalled => &self.shuttle_called,
            Sfx::RequisitionConfirm => &self.requisition_confirm,
            Sfx::UiClick => &self.ui_click,
            Sfx::UiRefused => &self.ui_refused,
            Sfx::DirectPour => &self.direct_pour,
            Sfx::Splash => &self.splash,
            Sfx::Drink => &self.drink,
            Sfx::Inject => &self.inject,
            Sfx::AnalyzerFinish => &self.analyzer_finish,
            Sfx::PackagePop => &self.package_pop,
            Sfx::GlassClunk => &self.glass_clunks[usize::from(variant) % self.glass_clunks.len()],
            Sfx::Corrosion => &self.corrosion,
            Sfx::Ignite => &self.ignite,
            Sfx::Flash => &self.flash,
            Sfx::Slip => &self.slip,
            Sfx::RadiationPulse => {
                &self.radiation_pulses[usize::from(variant) % self.radiation_pulses.len()]
            }
            Sfx::Cough => &self.coughs[usize::from(variant) % self.coughs.len()],
            Sfx::AssaultImpact => &self.assault_impact,
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
        attention: assets.load("sounds/attention.ogg"),
        announce: assets.load("sounds/announce_syndi.ogg"),
        red_alert: assets.load("sounds/redalert.ogg"),
        shuttle_called: assets.load("sounds/shuttlecalled.ogg"),
        radiation_alarm: assets.load("sounds/radiation.ogg"),
        requisition_confirm: assets.load("sounds/requisition_confirm.ogg"),
        ui_click: assets.load("sounds/ui_click.ogg"),
        ui_refused: assets.load("sounds/ui_refused.ogg"),
        direct_pour: assets.load("sounds/ss14/glug.ogg"),
        splash: assets.load("sounds/ss14/splash.ogg"),
        drink: assets.load("sounds/ss14/drink.ogg"),
        inject: assets.load("sounds/ss14/hypospray.ogg"),
        analyzer_finish: assets.load("sounds/ss14/scan_finish.ogg"),
        package_pop: assets.load("sounds/ss14/pop.ogg"),
        glass_clunks: [
            assets.load("sounds/ss14/bottle_clunk.ogg"),
            assets.load("sounds/ss14/bottle_clunk_2.ogg"),
        ],
        corrosion: assets.load("sounds/ss14/sizzle.ogg"),
        ignite: assets.load("sounds/ss14/fire.ogg"),
        flash: assets.load("sounds/ss14/flash_bang.ogg"),
        slip: assets.load("sounds/ss14/slip.ogg"),
        radiation_pulses: [
            assets.load("sounds/ss14/radpulse1.ogg"),
            assets.load("sounds/ss14/radpulse2.ogg"),
            assets.load("sounds/ss14/radpulse3.ogg"),
            assets.load("sounds/ss14/radpulse4.ogg"),
            assets.load("sounds/ss14/radpulse5.ogg"),
            assets.load("sounds/ss14/radpulse6.ogg"),
            assets.load("sounds/ss14/radpulse7.ogg"),
            assets.load("sounds/ss14/radpulse8.ogg"),
            assets.load("sounds/ss14/radpulse9.ogg"),
            assets.load("sounds/ss14/radpulse10.ogg"),
            assets.load("sounds/ss14/radpulse11.ogg"),
            assets.load("sounds/ss14/radpulse12.ogg"),
        ],
        coughs: [
            assets.load("sounds/ss14/female_cough_1.ogg"),
            assets.load("sounds/ss14/female_cough_2.ogg"),
            assets.load("sounds/ss14/male_cough_1.ogg"),
            assets.load("sounds/ss14/male_cough_2.ogg"),
        ],
        assault_impact: assets.load("sounds/ss14/soft_thump.ogg"),
        agitation_loop: assets.load("sounds/ss14/bubbles.ogg"),
        heater_loop: assets.load("sounds/ss14/buzz_loop.ogg"),
        // The same loop the heaters use, deliberately: it is the right
        // texture of noise for a running belt, and shipping a placeholder we
        // already have beats shipping a conveyor line that runs in silence.
        // Swap this for a dedicated file when one turns up.
        conveyor_loop: assets.load("sounds/ss14/buzz_loop.ogg"),
        parcel_drop: assets.load("sounds/ss14/soft_thump.ogg"),
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
            AudioPlayer::new(assets.handle(*sfx, 0)),
            PlaybackSettings::DESPAWN.with_volume(Volume::Linear(sfx.volume())),
            crate::until_we_leave_the_lab(),
        ));
    }
}

/// Plays the authority's cue locally and forwards the exact same variant to
/// remote clients. `CLIENTS_ONLY` is essential: echoing to the listen host
/// would make every physical action play twice there.
fn fanout_world_sfx(
    mut requests: MessageReader<EmitWorldSfx>,
    mut variants: ResMut<VariantCursor>,
    mut local: MessageWriter<WorldSfx>,
    mut outgoing: MessageWriter<ToClients<WorldSfx>>,
) {
    for request in requests.read() {
        if !request.position.is_finite() {
            continue;
        }
        let message = WorldSfx {
            sound: request.sound,
            variant: variants.next(request.sound),
            position: request.position,
        };
        local.write(message);
        outgoing.write(ToClients {
            targets: SendTargets::CLIENTS_ONLY,
            message,
        });
    }
}

fn play_world_sfx(
    mut commands: Commands,
    assets: Res<SfxAssets>,
    mut requests: MessageReader<WorldSfx>,
) {
    for request in requests.read() {
        if !request.position.is_finite() {
            continue;
        }
        commands.spawn((
            AudioPlayer::new(assets.handle(request.sound, request.variant)),
            PlaybackSettings::DESPAWN
                .with_volume(Volume::Linear(request.sound.volume()))
                .with_spatial(true),
            Transform::from_translation(request.position),
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

/// `ReactionsFired` is authority-only. It becomes an [`EmitWorldSfx`] here,
/// then [`fanout_world_sfx`] selects one variant and forwards it to every
/// remote listener through the shared positional-audio path.
fn play_reaction_sfx(
    mut fired: MessageReader<ReactionsFired>,
    transforms: Query<&Transform>,
    held: Query<&HeldBy>,
    slots: Query<&InSlot>,
    slots_b: Query<&InSlotB>,
    mut play: MessageWriter<EmitWorldSfx>,
) {
    for report in fired.read() {
        let source = held
            .get(report.container)
            .ok()
            .map(|holder| holder.0)
            .or_else(|| slots.get(report.container).ok().map(|slot| slot.0))
            .or_else(|| slots_b.get(report.container).ok().map(|slot| slot.0))
            .unwrap_or(report.container);
        if let Ok(transform) = transforms.get(source) {
            play.write(EmitWorldSfx::new(
                Sfx::ReactionOccurred,
                transform.translation,
            ));
        }
    }
}

/// `Local` tracking rather than `Added<Collapsed>`-style marker: collapse is
/// a field flip on an existing component, not a spawn. This runs only on the
/// authority so a joining client cannot replay a historical collapse.
fn play_collapse_sfx(
    bodies: Query<(Entity, &Body, &Transform), Changed<Body>>,
    mut known_down: Local<HashSet<Entity>>,
    mut play: MessageWriter<EmitWorldSfx>,
) {
    for (entity, body, transform) in &bodies {
        if body.0.collapsed {
            if known_down.insert(entity) {
                play.write(EmitWorldSfx::new(Sfx::Fall, transform.translation));
            }
        } else {
            known_down.remove(&entity);
        }
    }
}

#[derive(Default)]
struct BodyAudioState {
    choking: bool,
    burning: bool,
    unsteady: bool,
    next_cough_at: f32,
    cough_serial: u8,
}

fn cough_interval(entity: Entity, serial: u8) -> f32 {
    10.0 + ((entity.to_bits().wrapping_add(u64::from(serial))) % 5) as f32
}

/// Audible body presentation follows authority bloodstream state and enters
/// the shared positional path. Only choking repeats, on a long deterministic
/// cadence.
fn play_status_sfx(
    time: Res<Time>,
    bodies: Query<(Entity, &Transform, &Bloodstream)>,
    mut states: Local<HashMap<Entity, BodyAudioState>>,
    mut play: MessageWriter<EmitWorldSfx>,
) {
    let now = time.elapsed_secs();
    let mut seen = HashSet::new();
    for (entity, transform, blood) in &bodies {
        seen.insert(entity);
        let choking = blood.0.status(StatusKind::Choking).intensity > 0.0;
        let burning = blood.0.status(StatusKind::Burning).intensity > 0.0;
        let unsteady = blood.0.status(StatusKind::Unsteady).intensity > 0.0;
        let state = states.entry(entity).or_default();

        if choking && (!state.choking || now >= state.next_cough_at) {
            play.write(EmitWorldSfx::new(Sfx::Cough, transform.translation));
            state.cough_serial = state.cough_serial.wrapping_add(1);
            state.next_cough_at = now + cough_interval(entity, state.cough_serial);
        }
        if burning && !state.burning {
            play.write(EmitWorldSfx::new(Sfx::Ignite, transform.translation));
        }
        if unsteady && !state.unsteady {
            play.write(EmitWorldSfx::new(Sfx::Slip, transform.translation));
        }

        state.choking = choking;
        state.burning = burning;
        state.unsteady = unsteady;
        if !choking {
            state.next_cough_at = 0.0;
        }
    }
    states.retain(|entity, _| seen.contains(entity));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MachineLoopKind {
    Agitation,
    Heater,
}

#[derive(Component)]
struct MachineLoop {
    owner: Entity,
    kind: MachineLoopKind,
}

fn desired_machine_loops(kind: MachineKind, agitating: bool, heater_powered: bool) -> (bool, bool) {
    (
        agitating,
        kind == MachineKind::ReactionChamber && heater_powered,
    )
}

fn sync_machine_loops(
    mut commands: Commands,
    assets: Res<SfxAssets>,
    machines: Query<
        (
            Entity,
            &Transform,
            &Machine,
            Option<&AgitationRun>,
            Option<&Thermostat>,
        ),
        Without<MachineLoop>,
    >,
    mut playing: Query<(Entity, &MachineLoop, &mut Transform)>,
) {
    let mut desired = HashMap::new();
    for (entity, transform, machine, agitation, thermostat) in &machines {
        let (agitation_loop, heater_loop) = desired_machine_loops(
            machine.kind,
            agitation.is_some(),
            thermostat.is_some_and(|thermostat| thermostat.powered),
        );
        if agitation_loop {
            desired.insert((entity, MachineLoopKind::Agitation), transform.translation);
        }
        if heater_loop {
            desired.insert((entity, MachineLoopKind::Heater), transform.translation);
        }
    }

    let mut existing = HashSet::new();
    for (entity, looped, mut transform) in &mut playing {
        let key = (looped.owner, looped.kind);
        if let Some(position) = desired.get(&key) {
            transform.translation = *position;
            existing.insert(key);
        } else {
            commands.entity(entity).despawn();
        }
    }

    for ((owner, kind), position) in desired {
        if existing.contains(&(owner, kind)) {
            continue;
        }
        let (handle, volume) = match kind {
            MachineLoopKind::Agitation => (assets.agitation_loop.clone(), 0.28),
            MachineLoopKind::Heater => (assets.heater_loop.clone(), 0.20),
        };
        commands.spawn((
            MachineLoop { owner, kind },
            AudioPlayer::new(handle),
            PlaybackSettings::LOOP
                .with_volume(Volume::Linear(volume))
                .with_spatial(true),
            Transform::from_translation(position),
            crate::until_we_leave_the_lab(),
        ));
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

/// Volume of one conveyor hum source. Low, because a line carries several.
const CONVEYOR_HUM_VOLUME: f32 = 0.11;
/// Volume of a parcel landing in a chute.
const PARCEL_DROP_VOLUME: f32 = 0.30;

/// "There is running machinery here." `freight` puts one of these along each
/// belt run and [`voice_conveyor_hums`] gives it a loop.
///
/// The marker lives here rather than in `freight` so that module can stay clear
/// of `AudioPlayer` entirely — this is still the one place playback is
/// configured, as the module doc claims. `freight` says *where* a sound is;
/// what it sounds like and how loud is this module's business.
#[derive(Component)]
pub struct ConveyorHum;

/// "A parcel just landed here." Spawned per drop and despawned with its own
/// playback, so `freight` never has to clean one up.
#[derive(Component)]
pub struct ParcelDrop;

/// Gives a hum marker its loop, in place, so it dies when `freight` clears the
/// belt rather than needing its own reconciliation pass.
fn voice_conveyor_hums(
    mut commands: Commands,
    assets: Res<SfxAssets>,
    hums: Query<Entity, Added<ConveyorHum>>,
) {
    for entity in &hums {
        commands.entity(entity).insert((
            AudioPlayer::new(assets.conveyor_loop.clone()),
            PlaybackSettings::LOOP
                .with_volume(Volume::Linear(CONVEYOR_HUM_VOLUME))
                .with_spatial(true),
        ));
    }
}

fn voice_parcel_drops(
    mut commands: Commands,
    assets: Res<SfxAssets>,
    drops: Query<Entity, Added<ParcelDrop>>,
) {
    for entity in &drops {
        commands.entity(entity).insert((
            AudioPlayer::new(assets.parcel_drop.clone()),
            PlaybackSettings::DESPAWN
                .with_volume(Volume::Linear(PARCEL_DROP_VOLUME))
                .with_spatial(true),
        ));
    }
}

/// Marks the looping rad klaxon, so [`sync_radiation_alarm`] can tell whether
/// one is already going — the same shape [`MachineLoop`] uses, minus the owner,
/// because the lab has one alarm however many leaks are open at once.
#[derive(Component)]
struct RadiationAlarm;

/// Sounds the rad klaxon for exactly as long as something radiological is
/// leaking into the lab, and cuts it the moment the last one expires.
///
/// Presentation read straight off replicated state, so it needs no message and
/// no authority gate: `ActiveHazard` is an entity on every peer (see its own
/// doc), which means a guest working the same room hears the same alarm start
/// and stop, and one who joins mid-leak walks into an alarm already running.
///
/// Distinct from [`Sfx::RadiationPulse`], which is the geiger counter by the
/// dispenser — positional, per metabolism tick, and about *where* the source
/// is. This is the room's alarm, and is deliberately not spatial.
fn sync_radiation_alarm(
    mut commands: Commands,
    assets: Res<SfxAssets>,
    hazards: Query<&ActiveHazard>,
    playing: Query<Entity, With<RadiationAlarm>>,
) {
    if hazards.iter().any(|hazard| hazard.radiological) {
        if playing.is_empty() {
            commands.spawn((
                RadiationAlarm,
                AudioPlayer::new(assets.radiation_alarm.clone()),
                PlaybackSettings::LOOP.with_volume(Volume::Linear(RADIATION_ALARM_VOLUME)),
                crate::until_we_leave_the_lab(),
            ));
        }
        return;
    }
    for entity in &playing {
        commands.entity(entity).despawn();
    }
}

/// Calls the shuttle the moment a run is lost.
///
/// Reads `ending::FinishedArc` rather than `arc::Campaign` directly, which
/// buys the one rule that module already had to get right: an arc that
/// resolved in some *earlier* session and is only being loaded back in has
/// already finished, and must not be announced again — see
/// `ending::notice_the_ending`. Going through the same resource means there is
/// one answer to "did this run just end, in front of this player", not two
/// that can disagree.
///
/// The `Local` needs no reset of its own: `crate::session` puts `FinishedArc`
/// back to `None` on the way out of the lab, so the first frame of the next
/// save clears it before any ending can be raised.
fn play_evacuation_sfx(
    finished: Res<FinishedArc>,
    mut announced: Local<bool>,
    mut play: MessageWriter<PlaySfx>,
) {
    let ending = finished.showing();
    // `won`, not a particular `ArcOutcome`: the same outcome is a victory from
    // one chair and a defeat from the other, and it is the side that lost who
    // has somewhere to be.
    if let Some(ending) = ending {
        if !*announced && !ending.won() {
            play.write(PlaySfx(Sfx::ShuttleCalled));
        }
    }
    *announced = ending.is_some();
}

/// Baseline for [`play_radio_sfx`] — `None` means "not seen a frame since
/// entering the lab yet", which is what tells that system to baseline
/// silently instead of replaying old history as fresh blips.
#[derive(Resource, Default)]
struct RadioCursor {
    baselined: bool,
    sequence: Option<u64>,
}

fn reset_radio_cursor(mut cursor: ResMut<RadioCursor>) {
    *cursor = RadioCursor::default();
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
    let newest = log.entries.back().map(|entry| entry.sequence);
    if !cursor.baselined {
        cursor.baselined = true;
        cursor.sequence = newest;
        return;
    }
    let last = cursor.sequence;
    for entry in log
        .entries
        .iter()
        .filter(|entry| last.is_none_or(|last| entry.sequence > last))
    {
        // A bulletin over the PA opens with the station's own fanfare instead
        // of a department ident — it is not coming down one channel, and the
        // joke lands better with something in front of it.
        play.write(PlaySfx(if entry.announcement {
            Sfx::Announce
        } else {
            radio_channel_sfx(entry.channel)
        }));
        if entry.tone == RadioTone::Positive {
            play.write(PlaySfx(Sfx::OrderSuccess));
        }
        match entry.priority {
            // The alert level going up, which the station announces with its
            // klaxon rather than the ordinary chime — so this is an `else`
            // against `StationWide`, not an addition on top of it. Only
            // `showdown::arm_showdown` airs at this level.
            RadioPriority::RedAlert => {
                play.write(PlaySfx(Sfx::RedAlert));
            }
            RadioPriority::StationWide => {
                play.write(PlaySfx(Sfx::BridgePriority));
            }
            RadioPriority::Urgent => {
                play.write(PlaySfx(Sfx::RadioUrgent));
            }
            RadioPriority::Ambient | RadioPriority::Routine => {}
        }
    }
    cursor.sequence = newest.or(last);
}

fn radio_channel_sfx(channel: RadioChannel) -> Sfx {
    match channel {
        RadioChannel::Bridge => Sfx::RadioBridge,
        RadioChannel::Medical => Sfx::RadioMedical,
        RadioChannel::Security => Sfx::RadioSecurity,
        RadioChannel::Engineering => Sfx::RadioEngineering,
        RadioChannel::Cargo => Sfx::RadioCargo,
        RadioChannel::Service => Sfx::RadioService,
        RadioChannel::Lab | RadioChannel::Common => Sfx::RadioBlip,
    }
}

/// Door transitions become authority-confirmed positional cues; clients do
/// not replay the initial replicated closed state as a fresh sound.
fn play_door_sfx(
    doors: Query<(&Door, &Transform), Changed<Door>>,
    mut play: MessageWriter<EmitWorldSfx>,
) {
    for (door, transform) in &doors {
        play.write(EmitWorldSfx::new(
            if door.open {
                Sfx::DoorOpen
            } else {
                Sfx::DoorClosed
            },
            transform.translation,
        ));
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    #[derive(Resource, Default)]
    struct HeardRadio(Vec<Sfx>);

    fn collect_radio_sfx(mut messages: MessageReader<PlaySfx>, mut heard: ResMut<HeardRadio>) {
        heard.0.extend(messages.read().map(|message| message.0));
    }

    #[test]
    fn first_arrival_after_an_empty_baseline_plays_once() {
        let mut app = App::new();
        app.init_resource::<RadioLog>()
            .init_resource::<RadioCursor>()
            .init_resource::<HeardRadio>()
            .add_message::<PlaySfx>()
            .add_systems(Update, (play_radio_sfx, collect_radio_sfx).chain());
        app.update();
        app.world_mut()
            .resource_mut::<RadioLog>()
            .push(crate::radio::RadioEntry::new(
                RadioChannel::Cargo,
                "first arrival",
            ));
        app.update();
        assert_eq!(app.world().resource::<HeardRadio>().0, [Sfx::RadioCargo]);
    }

    /// Drives [`play_radio_sfx`] over a fresh log, baselining first so the
    /// pushed entries are the only thing it hears.
    fn heard_from(entries: Vec<crate::radio::RadioEntry>) -> Vec<Sfx> {
        let mut app = App::new();
        app.init_resource::<RadioLog>()
            .init_resource::<RadioCursor>()
            .init_resource::<HeardRadio>()
            .add_message::<PlaySfx>()
            .add_systems(Update, (play_radio_sfx, collect_radio_sfx).chain());
        app.update();
        for entry in entries {
            app.world_mut().resource_mut::<RadioLog>().push(entry);
            app.update();
        }
        app.world().resource::<HeardRadio>().0.clone()
    }

    #[test]
    fn the_alert_level_going_up_sounds_the_klaxon_instead_of_the_chime() {
        // `showdown::arm_showdown` is the only line in the game that airs at
        // `RedAlert`, and the whole reason it has its own priority rather than
        // a second cue stacked on `StationWide` is that they must not both
        // play over the one announcement.
        let heard = heard_from(vec![
            crate::radio::RadioEntry::new(RadioChannel::Bridge, "an ordinary bridge priority")
                .station_wide(),
            crate::radio::RadioEntry::new(RadioChannel::Bridge, "it is coming here").red_alert(),
        ]);
        assert_eq!(
            heard,
            [
                Sfx::RadioBridge,
                Sfx::BridgePriority,
                Sfx::RadioBridge,
                Sfx::RedAlert
            ]
        );
    }

    #[test]
    fn a_pa_bulletin_opens_with_the_fanfare_rather_than_its_channel_ident() {
        // And the same line *without* the flag stays an ordinary bridge blip —
        // the Bridge talking to one department is not an announcement.
        let heard = heard_from(vec![
            crate::radio::RadioEntry::new(RadioChannel::Bridge, "morale remains mandatory")
                .ambient()
                .over_the_pa(),
            crate::radio::RadioEntry::new(RadioChannel::Bridge, "security, status on the thing")
                .ambient(),
        ]);
        assert_eq!(heard, [Sfx::Announce, Sfx::RadioBridge]);
    }

    #[test]
    fn every_pa_bulletin_in_the_data_is_actually_addressed_to_the_station() {
        // The fanfare is nine seconds long. Setting the flag on a line that
        // reads as one department talking to another would make the station
        // sound like it is interrupting itself, so the data is checked rather
        // than trusted.
        let script: crate::radio::RadioScript =
            ron::from_str(include_str!("../../assets/data/station.radio.ron")).unwrap();
        let announcements = script
            .ambient_lines
            .iter()
            .filter(|line| line.announcement)
            .count();
        assert!(announcements > 0, "nothing is read over the PA at all");
        for line in script.ambient_lines.iter().filter(|line| line.announcement) {
            assert_eq!(
                line.channel,
                RadioChannel::Bridge,
                "'{}' is announced from a department channel",
                line.text
            );
        }
        for exchange in &script.exchanges {
            for line in &exchange.lines {
                assert!(
                    !line.announcement,
                    "'{}' is half of a two-hander, not a bulletin",
                    line.text
                );
            }
        }
        // The fanfare should stay an event. Everything else in the quiet-period
        // pool — department chatter and two-handers alike — has to outnumber it.
        let pool = script.ambient_lines.len() + script.exchanges.len();
        assert!(
            announcements * 2 < pool,
            "{announcements} of {pool} quiet-period picks would open with the fanfare"
        );
    }

    const SS14_FILES: [&str; 31] = [
        "bottle_clunk.ogg",
        "bottle_clunk_2.ogg",
        "bubbles.ogg",
        "buzz_loop.ogg",
        "drink.ogg",
        "female_cough_1.ogg",
        "female_cough_2.ogg",
        "fire.ogg",
        "flash_bang.ogg",
        "glug.ogg",
        "hypospray.ogg",
        "male_cough_1.ogg",
        "male_cough_2.ogg",
        "pop.ogg",
        "radpulse1.ogg",
        "radpulse2.ogg",
        "radpulse3.ogg",
        "radpulse4.ogg",
        "radpulse5.ogg",
        "radpulse6.ogg",
        "radpulse7.ogg",
        "radpulse8.ogg",
        "radpulse9.ogg",
        "radpulse10.ogg",
        "radpulse11.ogg",
        "radpulse12.ogg",
        "scan_finish.ogg",
        "sizzle.ogg",
        "slip.ogg",
        "soft_thump.ogg",
        "splash.ogg",
    ];

    const ORIGINAL_ONE_SHOTS: [&str; 21] = [
        "sounds/attention.ogg",
        "sounds/announce_syndi.ogg",
        "sounds/redalert.ogg",
        "sounds/shuttlecalled.ogg",
        "sounds/radiation.ogg",
        "sounds/dispense_pour.ogg",
        "sounds/eject.ogg",
        "sounds/buffer_transfer.ogg",
        "sounds/reaction_occurred.ogg",
        "sounds/hazard_smoke.ogg",
        "sounds/hazard_explosion.ogg",
        "sounds/major_alarm.ogg",
        "sounds/fall.ogg",
        "sounds/grinder.ogg",
        "sounds/door_open.ogg",
        "sounds/door_closed.ogg",
        "sounds/order_success.ogg",
        "sounds/radio_blip.ogg",
        "sounds/requisition_confirm.ogg",
        "sounds/ui_click.ogg",
        "sounds/ui_refused.ogg",
    ];

    #[test]
    fn every_registered_sound_path_exists() {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        for path in ORIGINAL_ONE_SHOTS
            .into_iter()
            .chain(CALM_AMBIENCE)
            .chain(TENSE_AMBIENCE)
        {
            assert!(
                assets.join(path).is_file(),
                "missing registered sound {path}"
            );
        }
        for file in SS14_FILES {
            assert!(
                assets.join("sounds/ss14").join(file).is_file(),
                "missing registered sound sounds/ss14/{file}"
            );
        }
    }

    #[test]
    fn every_imported_ss14_sound_exists_and_has_a_commercial_safe_credit_row() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sound_dir = root.join("assets/sounds/ss14");
        let credits = fs::read_to_string(root.join("CREDITS.md")).expect("CREDITS.md");

        let imported: HashSet<String> = fs::read_dir(&sound_dir)
            .expect("SS14 sound directory")
            .map(|entry| {
                entry
                    .expect("sound entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(imported.len(), SS14_FILES.len());

        for file in SS14_FILES {
            assert!(sound_dir.join(file).is_file(), "missing {file}");
            assert!(
                credits.contains(&format!("`ss14/{file}`")),
                "{file} has no credit row"
            );
        }
        for row in credits.lines().filter(|line| line.starts_with("| `ss14/")) {
            assert!(!row.contains("CC-BY-NC"), "noncommercial row: {row}");
            assert!(!row.contains("| Custom |"), "custom-license row: {row}");
        }
        for excluded in [
            "alien_spitacid.ogg",
            "pill_insert.ogg",
            "pill_remove.ogg",
            "ice_crit.ogg",
            "jet_injector.ogg",
        ] {
            assert!(!sound_dir.join(excluded).exists());
        }
    }

    #[test]
    fn pooled_variants_do_not_repeat_immediately() {
        let mut cursor = VariantCursor::default();
        for sound in [Sfx::RadiationPulse, Sfx::Cough, Sfx::GlassClunk] {
            let first = cursor.next(sound);
            let second = cursor.next(sound);
            assert_ne!(first, second, "{sound:?} repeated immediately");
            assert!(first < sound.variants());
            assert!(second < sound.variants());
        }
    }

    #[test]
    fn authority_plays_once_locally_and_sends_the_same_cue_to_clients_only() {
        let mut app = App::new();
        app.add_message::<EmitWorldSfx>()
            .add_message::<WorldSfx>()
            .add_message::<ToClients<WorldSfx>>()
            .init_resource::<VariantCursor>()
            .add_systems(Update, fanout_world_sfx);
        let request = EmitWorldSfx::new(Sfx::RadiationPulse, Vec3::new(1.0, 2.0, 3.0));
        app.world_mut().write_message(request);

        app.update();

        let local: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<WorldSfx>>()
            .drain()
            .collect();
        let remote: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ToClients<WorldSfx>>>()
            .drain()
            .collect();
        assert_eq!(local.len(), 1);
        assert_eq!(remote.len(), 1);
        assert!(matches!(
            remote[0].targets,
            SendTargets::AllExcept(ClientId::Server)
        ));
        assert_eq!(remote[0].message, local[0]);
    }

    #[test]
    fn machine_loops_follow_only_their_own_live_state() {
        assert_eq!(
            desired_machine_loops(MachineKind::MixingChamber, true, false),
            (true, false)
        );
        assert_eq!(
            desired_machine_loops(MachineKind::ReactionChamber, false, true),
            (false, true)
        );
        assert_eq!(
            desired_machine_loops(MachineKind::ReactionChamber, false, false),
            (false, false)
        );
        assert_eq!(
            desired_machine_loops(MachineKind::Analyzer, false, true),
            (false, false),
            "a stray thermostat must not make another machine hum"
        );
    }

    #[test]
    fn machine_loop_queries_are_disjoint_at_runtime() {
        let mut world = World::new();
        let mut schedule = Schedule::default();
        schedule.add_systems(sync_machine_loops);

        // Bevy validates query aliasing while a system is initialized. This
        // caught the runtime B0001 where the machine query read `Transform`
        // while the loop-player query could mutate the same component.
        schedule.initialize(&mut world).unwrap();
    }

    #[test]
    fn choking_repeats_on_a_long_cooldown_instead_of_every_frame() {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .add_message::<EmitWorldSfx>()
            .add_systems(Update, play_status_sfx);

        let mut blood = Bloodstream::default();
        blood.0.add_status(StatusKind::Choking, 60.0, 1.0);
        app.world_mut()
            .spawn((Transform::from_xyz(2.0, 0.0, 3.0), blood));

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.update();
        let first: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<EmitWorldSfx>>()
            .drain()
            .collect();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].sound, Sfx::Cough);
        assert_eq!(first[0].position, Vec3::new(2.0, 0.0, 3.0));

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.update();
        assert!(app
            .world_mut()
            .resource_mut::<Messages<EmitWorldSfx>>()
            .drain()
            .next()
            .is_none());

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(15));
        app.update();
        let repeated: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<EmitWorldSfx>>()
            .drain()
            .collect();
        assert_eq!(repeated.len(), 1);
        assert_eq!(repeated[0].sound, Sfx::Cough);
    }
}
