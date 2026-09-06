//! The wire format and the authority relay that decides who hears whom.
//!
//! Follows the split `audio::fanout_world_sfx`/`WorldSfx` already established
//! for positional sound: a client sends what happened, the authority decides
//! who is in range and re-sends only to them. Voice differs from a one-shot
//! sound effect in three ways that shape everything below: it is continuous
//! rather than one-shot, it is attacker-controlled in a way a validated
//! machine action is not, and it rides `Channel::Unreliable` — a first for
//! this codebase, so every assumption about delivery is written down and
//! tested rather than borrowed from the reliable channels everything else
//! uses.
//!
//! Phase 1 audibility is distance-only: a straight-line range cull, no
//! occlusion. Walls, corners and closed doors are Phase 2, added entirely by
//! changing what feeds `gain`/`muffle`/`emitter` here — the wire shape and the
//! relay's shape do not change.

use std::collections::HashMap;

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::net::is_authority;
use crate::player::Chemist;
use crate::AppState;

use super::codec::MAX_PACKET_BYTES;

/// How a frame was spoken. Travels in both directions: the sender is the only
/// one who knows which it was, and the listener needs it back to decide
/// whether to apply the handset's filter. An explicit enum rather than an
/// `Option`, because postcard is positional and non-self-describing — a
/// `None` and a zero-length `Vec` are easy to conflate by accident, and an
/// enum makes the two frame shapes impossible to confuse.
///
/// `VoiceChannel` does not exist yet (Phase 3); the variant is here now so the
/// wire shape does not change again when it lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceMode {
    Proximity,
}

/// One Opus frame, client to server.
///
/// `stream_id` changes on every new push-to-talk press — see
/// [`super::jitter::JitterBuffer`] for why a delayed frame from the last
/// press must never be mistaken for the start of this one. `seq` counts
/// 20 ms frames within that stream.
#[derive(Message, Clone, Debug, Serialize, Deserialize)]
pub struct VoiceFrame {
    pub stream_id: u16,
    pub seq: u16,
    pub mode: VoiceMode,
    pub data: Vec<u8>,
}

/// The same frame, server to a listener in range.
///
/// `speaker` is entity-mapped rather than carrying a `ClientId` — `ClientId`
/// is not serialisable (`player::Chemist`'s own doc explains why), and the
/// entity is what presentation needs to position the sound.
///
/// `emitter`/`gain`/`muffle` are the authority's answer to "where does this
/// sound come from and how loud is it", computed fresh per listener. In
/// Phase 1 `emitter` is simply the speaker's position and `muffle` is always
/// 0.0; Phase 2 changes only how these three are computed; nothing about the
/// wire shape changes.
///
/// Attenuation ownership is split deliberately so the two attenuation stages
/// downstream never double-count the same distance: Bevy's own spatial audio
/// owns the *visible* segment (`emitter` to the listener's ears — panning and
/// distance falloff, for free from `PlaybackSettings::with_spatial(true)`),
/// and `gain` owns everything Bevy cannot see (the speaker-to-emitter leg,
/// and in Phase 2 corner and closed-door penalties).
#[derive(Message, Clone, Debug, Serialize, Deserialize, MapEntities)]
pub struct VoiceHeard {
    #[entities]
    pub speaker: Entity,
    pub stream_id: u16,
    pub seq: u16,
    pub mode: VoiceMode,
    pub emitter: [f32; 3],
    pub gain: f32,
    pub muffle: f32,
    pub data: Vec<u8>,
}

/// Beyond this, a voice is not sent at all — the privacy and bandwidth floor.
/// A client that was never in range never receives the audio, so it cannot be
/// read back out of that client's memory. Chosen to match `speech::EARSHOT`,
/// the range NPC dialogue already established as "audible in the room you are
/// in or the one next to it" — reusing it means voice and NPC speech agree on
/// what counts as nearby rather than each inventing its own answer.
pub const PROXIMITY_RANGE: f32 = 14.0;

/// Packets per second one sender may transmit before the authority starts
/// dropping the excess. 20 ms framing is 50/s in ordinary operation; this
/// allows a margin for scheduling jitter without opening the door to a flood.
const MAX_PACKETS_PER_SECOND: u32 = 80;

/// Consecutive rate-limit violations before a sender is disconnected outright
/// rather than merely having frames dropped. A momentary burst (a frame-rate
/// stall releasing several queued sends at once) must not cost a connection;
/// a sustained flood is not an accident.
const KICK_AFTER_VIOLATIONS: u32 = 200;

/// Registers the wire types.
///
/// Called from [`super::VoicePlugin`] rather than inlined there, so the
/// relay's own systems and constants stay next to the types they operate on.
pub(super) fn register(app: &mut App) {
    app.add_client_message::<VoiceFrame>(Channel::Unreliable)
        .add_mapped_server_message::<VoiceHeard>(Channel::Unreliable)
        .init_resource::<SenderThrottles>()
        .add_systems(
            Update,
            relay_voice_frames
                .run_if(is_authority)
                .run_if(in_state(AppState::Playing)),
        );
}

/// Per-sender rate-limit bookkeeping. Authority-only, never replicated — a
/// listener has no business knowing how close another client is to being
/// kicked.
#[derive(Resource, Default)]
struct SenderThrottles {
    senders: HashMap<Entity, Throttle>,
}

#[derive(Default)]
struct Throttle {
    count_this_second: u32,
    window_started: f32,
    consecutive_violations: u32,
}

/// Reads what every connected client sent this frame, and re-sends whatever
/// is still in range to whoever it is in range of.
///
/// Runs once per client entity, not once per message: a hostile or merely
/// unlucky client can pack many frames into one drain (see
/// [`super::jitter`]'s module doc on `drain_received`), and the byte/rate
/// limits below have to see the whole batch to mean anything.
#[allow(clippy::too_many_arguments)]
fn relay_voice_frames(
    mut incoming: MessageReader<FromClient<VoiceFrame>>,
    mut throttles: ResMut<SenderThrottles>,
    time: Res<Time>,
    speakers: Query<(Entity, &Chemist, &Transform)>,
    listeners: Query<(&Chemist, &Transform), With<Chemist>>,
    mut outgoing: MessageWriter<ToClients<VoiceHeard>>,
    mut commands: Commands,
) {
    let now = time.elapsed_secs();

    for message in incoming.read() {
        // Oversized payloads are rejected before they cost anything else —
        // Opus at 24 kbit/s never produces this much, so this is either a
        // stale/mismatched build or a hostile peer, never ordinary jitter.
        if message.message.data.len() > MAX_PACKET_BYTES {
            continue;
        }

        let Some((speaker_entity, speaker_chemist, speaker_transform)) = speakers
            .iter()
            .find(|(_, chemist, _)| chemist.client == message.client_id)
        else {
            // No live chemist for this client — mid-handshake, or the body
            // despawned while a frame was already in flight. Not an error.
            continue;
        };

        if throttled(&mut throttles, speaker_entity, now, &mut commands) {
            continue;
        }

        if !speaker_transform.translation.is_finite() {
            continue;
        }

        for (listener_chemist, listener_transform) in &listeners {
            if listener_chemist.client == speaker_chemist.client {
                continue; // Never echo a speaker back to themselves.
            }
            if !listener_transform.translation.is_finite() {
                continue;
            }

            let distance_sq = speaker_transform
                .translation
                .distance_squared(listener_transform.translation);
            if distance_sq > PROXIMITY_RANGE * PROXIMITY_RANGE {
                continue;
            }

            // Phase 1: straight-line falloff only, no occlusion. `gain`
            // covers only what Bevy's own spatial audio cannot see — in
            // Phase 1 that is nothing, since the emitter *is* the speaker, so
            // this stays at 1.0 and Bevy's distance attenuation does all the
            // work. Phase 2 changes this function's body, not its signature.
            let heard = VoiceHeard {
                speaker: speaker_entity,
                stream_id: message.message.stream_id,
                seq: message.message.seq,
                mode: message.message.mode,
                emitter: speaker_transform.translation.to_array(),
                gain: 1.0,
                muffle: 0.0,
                data: message.message.data.clone(),
            };

            outgoing.write(ToClients {
                targets: SendTargets::Single(listener_chemist.client),
                message: heard,
            });
        }
    }
}

/// Updates one sender's rate-limit window and reports whether this frame
/// should be dropped. Kicks the connection outright on sustained abuse rather
/// than merely dropping forever, so a flooding peer costs the server nothing
/// once ejected.
fn throttled(
    throttles: &mut SenderThrottles,
    speaker: Entity,
    now: f32,
    commands: &mut Commands,
) -> bool {
    let throttle = throttles.senders.entry(speaker).or_default();

    if now - throttle.window_started >= 1.0 {
        throttle.window_started = now;
        throttle.count_this_second = 0;
    }
    throttle.count_this_second += 1;

    if throttle.count_this_second <= MAX_PACKETS_PER_SECOND {
        throttle.consecutive_violations = 0;
        return false;
    }

    throttle.consecutive_violations += 1;
    if throttle.consecutive_violations >= KICK_AFTER_VIOLATIONS {
        // `despawn` rather than reaching into renet directly: removing the
        // chemist is enough to stop the flood mattering, and disconnection
        // bookkeeping already reacts to a chemist disappearing elsewhere in
        // `net`/`player` exactly as it does for any other departure.
        commands.entity(speaker).despawn();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_voice_frame_round_trips_through_the_actual_wire_format() {
        // postcard is not self-describing, so this is the only test that
        // actually proves the type survives what replicon sends it through —
        // see `arc::tests::campaign_roster_round_trips_through_the_actual_wire_format`
        // for the bug this class of test exists to catch.
        let frame = VoiceFrame {
            stream_id: 42,
            seq: 900,
            mode: VoiceMode::Proximity,
            data: vec![1, 2, 3, 4, 5],
        };
        let bytes = postcard::to_allocvec(&frame).expect("postcard serialize");
        let round_tripped: VoiceFrame = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(round_tripped.stream_id, frame.stream_id);
        assert_eq!(round_tripped.seq, frame.seq);
        assert_eq!(round_tripped.mode, frame.mode);
        assert_eq!(round_tripped.data, frame.data);
    }

    #[test]
    fn a_voice_heard_message_round_trips_through_the_actual_wire_format() {
        let heard = VoiceHeard {
            speaker: Entity::PLACEHOLDER,
            stream_id: 7,
            seq: 3,
            mode: VoiceMode::Proximity,
            emitter: [1.0, 2.0, 3.0],
            gain: 0.5,
            muffle: 0.25,
            data: vec![9, 8, 7],
        };
        // `Entity` itself round-trips through postcard independent of
        // replicon's entity-mapping layer (which only rewrites the id after
        // deserialization); this test is only about the byte shape.
        let bytes = postcard::to_allocvec(&heard).expect("postcard serialize");
        let round_tripped: VoiceHeard = postcard::from_bytes(&bytes).expect("postcard deserialize");
        assert_eq!(round_tripped.stream_id, heard.stream_id);
        assert_eq!(round_tripped.seq, heard.seq);
        assert_eq!(round_tripped.mode, heard.mode);
        assert_eq!(round_tripped.emitter, heard.emitter);
        assert_eq!(round_tripped.gain, heard.gain);
        assert_eq!(round_tripped.muffle, heard.muffle);
        assert_eq!(round_tripped.data, heard.data);
    }

    #[test]
    fn a_sender_within_budget_is_never_throttled() {
        let mut throttles = SenderThrottles::default();
        let mut commands_queue = bevy::ecs::world::CommandQueue::default();
        let mut world = World::new();
        let speaker = world.spawn_empty().id();

        for i in 0..MAX_PACKETS_PER_SECOND {
            let mut commands = Commands::new(&mut commands_queue, &world);
            let dropped = throttled(&mut throttles, speaker, 0.0, &mut commands);
            assert!(!dropped, "packet {i} should be within budget");
        }
    }

    #[test]
    fn a_sender_over_budget_within_one_second_is_dropped_but_not_kicked() {
        let mut throttles = SenderThrottles::default();
        let mut commands_queue = bevy::ecs::world::CommandQueue::default();
        let mut world = World::new();
        let speaker = world.spawn_empty().id();

        for _ in 0..MAX_PACKETS_PER_SECOND {
            let mut commands = Commands::new(&mut commands_queue, &world);
            throttled(&mut throttles, speaker, 0.0, &mut commands);
        }
        let mut commands = Commands::new(&mut commands_queue, &world);
        let dropped = throttled(&mut throttles, speaker, 0.0, &mut commands);
        assert!(dropped, "one packet past budget must be dropped");
        commands_queue.apply(&mut world);
        assert!(
            world.get_entity(speaker).is_ok(),
            "a single burst must not disconnect the sender"
        );
    }

    #[test]
    fn sustained_flooding_disconnects_the_sender() {
        let mut throttles = SenderThrottles::default();
        let mut commands_queue = bevy::ecs::world::CommandQueue::default();
        let mut world = World::new();
        let speaker = world.spawn_empty().id();

        // Every one of these calls lands in the same one-second window (the
        // clock never advances), so every packet past the budget counts as a
        // violation — enough of them must eventually disconnect the sender.
        for _ in 0..(MAX_PACKETS_PER_SECOND + KICK_AFTER_VIOLATIONS + 1) {
            let mut commands = Commands::new(&mut commands_queue, &world);
            throttled(&mut throttles, speaker, 0.0, &mut commands);
        }
        commands_queue.apply(&mut world);
        assert!(
            world.get_entity(speaker).is_err(),
            "sustained flooding must disconnect the sender"
        );
    }

    #[test]
    fn the_rate_limit_window_resets_after_a_second_passes() {
        let mut throttles = SenderThrottles::default();
        let mut commands_queue = bevy::ecs::world::CommandQueue::default();
        let mut world = World::new();
        let speaker = world.spawn_empty().id();

        for _ in 0..MAX_PACKETS_PER_SECOND {
            let mut commands = Commands::new(&mut commands_queue, &world);
            throttled(&mut throttles, speaker, 0.0, &mut commands);
        }
        let mut commands = Commands::new(&mut commands_queue, &world);
        assert!(throttled(&mut throttles, speaker, 0.0, &mut commands));

        // A full second later, the window has rolled over.
        let mut commands = Commands::new(&mut commands_queue, &world);
        assert!(!throttled(&mut throttles, speaker, 1.0, &mut commands));
    }
}
