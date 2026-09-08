//! Playback: turning [`VoiceHeard`] into positioned, decoded audio through
//! Bevy's own spatial mixer — plus the nameplate that says whose voice it is.
//!
//! One [`VoiceStream`]/[`AudioPlayer`] pair per speaker, spawned on their
//! first frame. The jitter buffer and the Opus decoder both run here, on the
//! game thread, on a 20 ms tick; the audio thread only ever pops raw `f32`
//! samples from a lock-free ring buffer. That split — not the decoder doing
//! its own timing — is what keeps a lock or an allocation off the audio
//! thread, which is the standard way this kind of feature ends up crackling.
//!
//! Nameplates exist because a voice with no name attached is much less
//! useful than one with a name on it, in both the world and a mute list —
//! and because the game has never had anywhere to show one before. See
//! `player::display_name` for why the name itself is derived rather than
//! player-chosen.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bevy::audio::AddAudioSource;
use bevy::prelude::*;

use crate::net::AccountId;
use crate::player::{display_name, Chemist, LocalPlayer, PlayerAccount, PlayerCamera};
use crate::settings::Settings;
use crate::AppState;

use super::codec::VoiceDecoder;
use super::frame::{FRAME_MS, FRAME_SAMPLES, SAMPLE_RATE};
use super::jitter::{FrameAction, JitterBuffer};
use super::VoiceHeard;

/// Same anchor height `speech::SpeechBubble` uses. Not shared as a `pub`
/// constant across modules — a nameplate and a speech bubble agreeing on
/// where a head is happens to be true today, not a rule either module should
/// have to import to stay true.
const HEAD_HEIGHT: f32 = 0.78;

/// Ring buffer capacity between the decode tick and the audio thread: half a
/// second at 48 kHz mono. Generous relative to the jitter buffer's own
/// ~80 ms depth sitting upstream of it — the audio thread draining on its
/// own clock, slightly faster or slower than real time, must never overrun
/// this before the next 20 ms tick tops it back up.
const RING_CAPACITY: usize = SAMPLE_RATE as usize / 2;

/// How long since a speaker's last frame before they are swept as gone.
/// Comfortably past the jitter buffer's own silence-after-loss threshold, so
/// an ordinary pause in speech is never mistaken for the speaker leaving.
const SPEAKER_TIMEOUT: Duration = Duration::from_secs(2);

/// Within this since the last frame, a speaker reads as "speaking". Longer
/// than one frame period so a single dropped packet never flickers it.
const SPEAKING_WINDOW: Duration = Duration::from_millis(400);

pub(super) fn register(app: &mut App) {
    app.add_audio_source::<VoiceStream>()
        .init_non_send::<VoicePlayers>()
        .init_resource::<MutedSpeakers>()
        .add_systems(
            Update,
            (
                receive_voice_heard,
                tick_voice_playback,
                despawn_stale_voice_players,
                log_voice_jitter_health,
            )
                .chain()
                .run_if(in_state(AppState::Playing)),
        )
        .add_systems(
            Update,
            (
                spawn_nameplates,
                despawn_stale_nameplates,
                place_nameplates,
                handle_nameplate_clicks,
            )
                .chain()
                .run_if(in_state(AppState::Playing)),
        )
        .add_systems(OnExit(AppState::Playing), clear_voice_players);
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// A live voice, playable through Bevy's audio graph like any other source.
///
/// Holds the consumer end of the ring buffer the decode tick fills, behind a
/// `Mutex` purely so the type can be `Sync` (required of an [`Asset`]) —
/// `decoder()` takes it out exactly once, when playback starts, so this is
/// one lock at spawn time, never a per-sample one.
#[derive(Asset, TypePath)]
struct VoiceStream {
    consumer: std::sync::Mutex<Option<rtrb::Consumer<f32>>>,
}

impl Decodable for VoiceStream {
    type Decoder = VoiceStreamDecoder;

    fn decoder(&self) -> Self::Decoder {
        let consumer = self
            .consumer
            .lock()
            .expect("not poisoned")
            .take()
            .expect("decoder() is called once, when AudioPlayer starts this source");
        VoiceStreamDecoder { consumer }
    }
}

/// Runs on the audio thread. Everything here must be non-blocking and
/// non-allocating: `pop` on a lock-free SPSC ring buffer, nothing else.
struct VoiceStreamDecoder {
    consumer: rtrb::Consumer<f32>,
}

impl Iterator for VoiceStreamDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        // An empty buffer plays silence rather than blocking or ending the
        // stream — the jitter buffer feeding it decides when a speaker is
        // truly gone; a momentary gap here is just this tick running a hair
        // ahead of the next 20 ms of decode.
        Some(self.consumer.pop().unwrap_or(0.0))
    }
}

impl rodio::Source for VoiceStreamDecoder {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        rodio::ChannelCount::new(1).expect("1 is nonzero")
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        rodio::SampleRate::new(SAMPLE_RATE).expect("48000 is nonzero")
    }

    fn total_duration(&self) -> Option<Duration> {
        // Live and unbounded: this is a stream, not a clip.
        None
    }
}

/// Everything one remote speaker's playback needs. Bundled into a `NonSend`
/// resource rather than components, for the same reason `voice::capture`'s
/// state is `NonSend`: `opus::Decoder` and `rtrb::Producer` are both `Send`
/// but not `Sync`, so neither can live in an ordinary `Resource` or
/// `Component`, both of which Bevy requires to be `Send + Sync`. A `HashMap`
/// keyed by the speaking chemist's entity plays the role a `Component` would
/// have, without fighting that bound.
#[derive(Default)]
struct VoicePlayers {
    entries: HashMap<Entity, PlayerVoice>,
}

struct PlayerVoice {
    playback_entity: Entity,
    producer: rtrb::Producer<f32>,
    jitter: JitterBuffer,
    decoder: VoiceDecoder,
    /// The stream id the decoder was last reset for, so a new talk-spurt is
    /// noticed exactly once rather than every tick.
    decoder_stream: Option<u16>,
    gain: f32,
    muffle: f32,
    last_heard: Instant,
}

/// Accepts arriving frames into each speaker's jitter buffer, and spawns the
/// playback entity the first time a given speaker is heard from.
///
/// Ungated by `is_authority` — this is presentation, built from a replicated…
/// no, from a *received message*, but the principle is the co-op rule all the
/// same: both the host and a joined client receive their own `VoiceHeard`
/// messages and must build the same thing from them.
fn receive_voice_heard(
    mut commands: Commands,
    mut incoming: MessageReader<VoiceHeard>,
    mut players: NonSendMut<VoicePlayers>,
    mut assets: ResMut<Assets<VoiceStream>>,
    mut transforms: Query<&mut Transform>,
) {
    for message in incoming.read() {
        if !message.gain.is_finite()
            || !message.muffle.is_finite()
            || message.emitter.iter().any(|c| !c.is_finite())
        {
            // A NaN or infinite value here would poison the spatial mixer or
            // the volume multiply below; the authority is supposed to have
            // clamped these, but a hostile or buggy peer is not trusted
            // twice. See `audio::fanout_world_sfx`'s own `is_finite` guard.
            continue;
        }

        let entry = match players.entries.entry(message.speaker) {
            std::collections::hash_map::Entry::Occupied(occupied) => occupied.into_mut(),
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let Ok(decoder) = VoiceDecoder::new() else {
                    warn!("voice playback: could not start a decoder for a new speaker");
                    continue;
                };
                let (producer, consumer) = rtrb::RingBuffer::new(RING_CAPACITY);
                let handle = assets.add(VoiceStream {
                    consumer: std::sync::Mutex::new(Some(consumer)),
                });
                let playback_entity = commands
                    .spawn((
                        AudioPlayer::<VoiceStream>(handle),
                        PlaybackSettings::LOOP.with_spatial(true),
                        Transform::from_translation(Vec3::from_array(message.emitter)),
                        crate::until_we_leave_the_lab(),
                    ))
                    .id();
                vacant.insert(PlayerVoice {
                    playback_entity,
                    producer,
                    jitter: JitterBuffer::new(),
                    decoder,
                    decoder_stream: None,
                    gain: 1.0,
                    muffle: 0.0,
                    last_heard: Instant::now(),
                })
            }
        };
        entry
            .jitter
            .push(message.stream_id, message.seq, message.data.clone());
        entry.gain = message.gain;
        entry.muffle = message.muffle;
        entry.last_heard = Instant::now();

        if let Ok(mut transform) = transforms.get_mut(entry.playback_entity) {
            transform.translation = Vec3::from_array(message.emitter);
        }
    }
}

/// Decodes and mixes one 20 ms frame per speaker, on a fixed cadence.
///
/// A `Local` accumulator rather than `FixedUpdate` — this codebase
/// deliberately has no fixed timestep schedule (see `player::MoveIntent`'s
/// doc), so every periodic system here follows the same
/// `send_move_input`-style pattern instead of introducing the one thing the
/// rest of the game avoids.
fn tick_voice_playback(
    time: Res<Time>,
    settings: Res<Settings>,
    muted: Res<MutedSpeakers>,
    accounts: Query<&PlayerAccount>,
    mut players: NonSendMut<VoicePlayers>,
    mut since_tick: Local<f32>,
) {
    const FRAME_SECONDS: f32 = FRAME_MS as f32 / 1000.0;

    *since_tick += time.delta_secs();
    // A `while` rather than `if`: a stall (a load hitch, a debugger pause)
    // must catch up in frame-sized steps rather than let `since_tick` grow
    // without bound and then dump a burst of silence into the ring buffer.
    while *since_tick >= FRAME_SECONDS {
        *since_tick -= FRAME_SECONDS;

        for (&speaker, entry) in players.entries.iter_mut() {
            let current_stream = entry.jitter.current_stream();
            if current_stream.is_some() && current_stream != entry.decoder_stream {
                if let Err(error) = entry.decoder.reset() {
                    warn!("voice playback: could not reset decoder for a new talk-spurt: {error}");
                }
                entry.decoder_stream = current_stream;
            }

            let action = entry.jitter.next_action();
            let mut pcm = [0.0_f32; FRAME_SAMPLES];
            let decoded = match action {
                FrameAction::Decode(data) => entry.decoder.decode(&data, &mut pcm).is_ok(),
                FrameAction::Conceal => entry.decoder.conceal(&mut pcm).is_ok(),
                FrameAction::Silence => false,
            };
            if !decoded {
                continue;
            }

            let is_muted = accounts
                .get(speaker)
                .is_ok_and(|account| muted.0.contains(&account.0));
            if is_muted {
                continue;
            }

            // Attenuation ownership, per the plan: `gain` is everything Bevy's
            // own spatial falloff cannot see (the speaker-to-emitter leg, and
            // in Phase 2 corner/door penalties); `voice_volume` is the
            // player's own dial; `muffle` softens the signal itself rather
            // than only its volume. Phase 1 sends `muffle == 0.0` always, so
            // this line is presently a no-op multiply by 1.0 — it is written
            // now because Phase 2 changes only what feeds this, never this.
            let volume = entry.gain * settings.voice_volume * (1.0 - entry.muffle);
            for sample in &mut pcm {
                *sample *= volume;
            }
            for sample in pcm {
                // Best-effort: if the audio thread has fallen behind enough
                // to fill half a second of buffer, dropping the newest
                // sample is the right failure — the alternative is growing
                // latency without bound, which is worse than a lost sample.
                let _ = entry.producer.push(sample);
            }
        }
    }
}

/// How often the jitter-buffer counters are worth writing to the log. Once a
/// connection is bad enough to be worth seeing, once every few seconds is
/// still fast enough to catch it — every frame would be pure noise.
const JITTER_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Surfaces each active speaker's jitter-buffer health — the counters the
/// plan calls for so the buffer's depth and thresholds can be tuned from a
/// real connection instead of guesswork. Logs only a speaker whose buffer is
/// currently held or has ever lost/concealed a frame, so a clean LAN
/// connection between two players produces nothing at all.
fn log_voice_jitter_health(
    players: NonSend<VoicePlayers>,
    mut since_log: Local<f32>,
    time: Res<Time>,
) {
    *since_log += time.delta_secs();
    if *since_log < JITTER_LOG_INTERVAL.as_secs_f32() {
        return;
    }
    *since_log = 0.0;

    for (speaker, entry) in &players.entries {
        let stats = entry.jitter.stats();
        let held = entry.jitter.held();
        if held == 0
            && stats.late == 0
            && stats.duplicate == 0
            && stats.overflow == 0
            && stats.underflow == 0
            && stats.concealed == 0
        {
            continue;
        }
        info!(
            "voice jitter {speaker:?}: held={held} accepted={} late={} duplicate={} \
             overflow={} underflow={} concealed={}",
            stats.accepted,
            stats.late,
            stats.duplicate,
            stats.overflow,
            stats.underflow,
            stats.concealed,
        );
    }
}

fn despawn_stale_voice_players(mut commands: Commands, mut players: NonSendMut<VoicePlayers>) {
    let now = Instant::now();
    players.entries.retain(|_, entry| {
        let alive = now.duration_since(entry.last_heard) < SPEAKER_TIMEOUT;
        if !alive {
            commands.entity(entry.playback_entity).despawn();
        }
        alive
    });
}

/// Drops all live playback state on the way out of a session.
///
/// Does **not** despawn the playback entities itself: each one already
/// carries `crate::until_we_leave_the_lab()`
/// (`DespawnOnExit(AppState::Playing)`), which handles that on the same
/// transition. Despawning them again here would be a double despawn.
fn clear_voice_players(mut players: NonSendMut<VoicePlayers>) {
    players.entries.clear();
}

/// Whether a speaker has been heard from recently enough to count as
/// currently talking. `pub(super)` only — this is a presentation detail, not
/// part of the module's public surface.
fn speaking_now(players: &VoicePlayers, chemist: Entity) -> bool {
    players
        .entries
        .get(&chemist)
        .is_some_and(|entry| entry.last_heard.elapsed() < SPEAKING_WINDOW)
}

// ---------------------------------------------------------------------------
// Mute
// ---------------------------------------------------------------------------

/// Accounts this client has chosen not to hear. Client-local and
/// unreplicated by design — muting is a listener's own choice, not a fact
/// about the world, and with at most three other players
/// (`net::MAX_REMOTE_CLIENTS`) a client-side list is all four friends need.
#[derive(Resource, Default)]
struct MutedSpeakers(std::collections::HashSet<AccountId>);

// ---------------------------------------------------------------------------
// Nameplates
// ---------------------------------------------------------------------------

/// A clickable label floating over one remote chemist's head: their name,
/// and — the reason this exists — whether they are currently talking.
/// Clicking it toggles [`MutedSpeakers`] for their account.
#[derive(Component)]
struct Nameplate {
    chemist: Entity,
    account: AccountId,
}

#[derive(Component)]
struct NameplateLabel;

/// Ensures every remote chemist has exactly one nameplate, following the
/// "presentation built from `Added`/liveness, never mutated back into"
/// pattern `speech::spawn_bubbles`/`despawn_bubbles` already use for the same
/// reason: entities in this game come and go without a removal event this
/// module can rely on, so an explicit sweep is the correct primitive, not
/// defensive padding.
#[allow(clippy::type_complexity)]
fn spawn_nameplates(
    mut commands: Commands,
    chemists: Query<(Entity, &PlayerAccount), (With<Chemist>, Without<LocalPlayer>)>,
    plates: Query<&Nameplate>,
) {
    for (chemist, account) in &chemists {
        if plates.iter().any(|plate| plate.chemist == chemist) {
            continue;
        }
        commands.spawn((
            Button,
            Node {
                position_type: PositionType::Absolute,
                padding: UiRect::axes(px(8), px(3)),
                border_radius: BorderRadius::all(px(4)),
                ..default()
            },
            Visibility::Hidden,
            BackgroundColor(Color::srgba(0.05, 0.06, 0.08, 0.72)),
            GlobalZIndex(29),
            Nameplate {
                chemist,
                account: account.0,
            },
            crate::until_we_leave_the_lab(),
            children![(
                Text::new(display_name(account.0)),
                TextFont::from_font_size(12.0),
                TextColor(Color::srgba(0.85, 0.85, 0.9, 1.0)),
                NameplateLabel,
            )],
        ));
    }

    // A departed chemist's nameplate has no removal event to react to
    // either; the sweep lives in `despawn_stale_nameplates` below, run from
    // the same chain so it never lags a full extra frame behind arrivals.
}

fn despawn_stale_nameplates(
    mut commands: Commands,
    chemists: Query<(), With<Chemist>>,
    plates: Query<(Entity, &Nameplate)>,
) {
    for (entity, plate) in &plates {
        if !chemists.contains(plate.chemist) {
            commands.entity(entity).despawn();
        }
    }
}

/// Projects each nameplate above its chemist's head, hides it if they are
/// behind the camera or too far to matter, colours it while they talk, and
/// reflects whether the local player has muted them.
///
/// Deliberately skips the wall-occlusion check `speech::place_bubbles` does:
/// a nameplate is not the mechanism that makes voice quieter through a wall
/// — the audio itself, via `VoiceHeard.muffle`, is. Gating the *label* on
/// line of sight too would just be the same answer computed twice.
#[allow(clippy::too_many_arguments)]
fn place_nameplates(
    camera: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    chemists: Query<&Transform, (With<Chemist>, Without<LocalPlayer>)>,
    muted: Res<MutedSpeakers>,
    players: NonSend<VoicePlayers>,
    mut plates: Query<(&Nameplate, &mut Node, &mut Visibility, &Children)>,
    mut labels: Query<&mut TextColor, With<NameplateLabel>>,
) {
    let Ok((camera, camera_transform)) = camera.single() else {
        return;
    };
    let eye = camera_transform.translation();

    for (plate, mut node, mut visibility, children) in &mut plates {
        let shown = chemists
            .get(plate.chemist)
            .ok()
            .filter(|transform| {
                eye.distance_squared(transform.translation)
                    <= super::PROXIMITY_RANGE * super::PROXIMITY_RANGE
            })
            .and_then(|transform| {
                camera
                    .world_to_viewport(
                        camera_transform,
                        transform.translation + Vec3::Y * HEAD_HEIGHT,
                    )
                    .ok()
            });

        let Some(at) = shown else {
            if *visibility != Visibility::Hidden {
                *visibility = Visibility::Hidden;
            }
            continue;
        };
        if *visibility != Visibility::Visible {
            *visibility = Visibility::Visible;
        }
        node.left = px(at.x - 40.0);
        node.top = px(at.y - 34.0);

        let is_muted = muted.0.contains(&plate.account);
        let is_speaking = speaking_now(&players, plate.chemist);
        for child in children.iter() {
            if let Ok(mut color) = labels.get_mut(child) {
                color.0 = if is_muted {
                    Color::srgba(0.5, 0.3, 0.3, 1.0)
                } else if is_speaking {
                    Color::srgba(0.5, 0.95, 0.6, 1.0)
                } else {
                    Color::srgba(0.85, 0.85, 0.9, 1.0)
                };
            }
        }
    }
}

fn handle_nameplate_clicks(
    plates: Query<(&Interaction, &Nameplate), Changed<Interaction>>,
    mut muted: ResMut<MutedSpeakers>,
) {
    for (interaction, plate) in &plates {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if !muted.0.remove(&plate.account) {
            muted.0.insert(plate.account);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_frame_reads_as_speaking() {
        let mut players = VoicePlayers::default();
        let chemist = Entity::from_raw_u32(1).unwrap();
        players.entries.insert(
            chemist,
            PlayerVoice {
                playback_entity: Entity::from_raw_u32(2).unwrap(),
                producer: rtrb::RingBuffer::new(1).0,
                jitter: JitterBuffer::new(),
                decoder: VoiceDecoder::new().expect("decoder"),
                decoder_stream: None,
                gain: 1.0,
                muffle: 0.0,
                last_heard: Instant::now(),
            },
        );
        assert!(speaking_now(&players, chemist));
    }

    #[test]
    fn a_stale_frame_no_longer_reads_as_speaking() {
        let mut players = VoicePlayers::default();
        let chemist = Entity::from_raw_u32(1).unwrap();
        players.entries.insert(
            chemist,
            PlayerVoice {
                playback_entity: Entity::from_raw_u32(2).unwrap(),
                producer: rtrb::RingBuffer::new(1).0,
                jitter: JitterBuffer::new(),
                decoder: VoiceDecoder::new().expect("decoder"),
                decoder_stream: None,
                gain: 1.0,
                muffle: 0.0,
                last_heard: Instant::now() - SPEAKING_WINDOW - Duration::from_millis(1),
            },
        );
        assert!(!speaking_now(&players, chemist));
    }

    #[test]
    fn a_speaker_never_heard_from_is_not_speaking() {
        let players = VoicePlayers::default();
        assert!(!speaking_now(&players, Entity::from_raw_u32(99).unwrap()));
    }

    #[test]
    fn muting_and_unmuting_is_a_plain_toggle() {
        let mut muted = MutedSpeakers::default();
        let account = AccountId::from_bytes([3; 16]);
        assert!(!muted.0.contains(&account));
        muted.0.insert(account);
        assert!(muted.0.contains(&account));
        muted.0.remove(&account);
        assert!(!muted.0.contains(&account));
    }
}
