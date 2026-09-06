//! Proximity voice chat: hearing the other chemists, positioned in the room.
//!
//! Not to be confused with [`crate::radio`], which is a one-way information
//! feed about station stability and has nothing to do with players talking.
//! Player-to-player comms live here, and the handset is called a handset.
//!
//! # Where this is up to
//!
//! Being built in phases, so that device and codec failures are diagnosed
//! before the network can be blamed for them:
//!
//! - **Phase 0** ✅ — capture, framing, the jitter buffer and the codec,
//!   proven locally with no network. `src/bin/mic-probe.rs` is the device
//!   half; [`frame`], [`jitter`] and [`codec`] are the logic half.
//! - **Phase 1** (this module) — push-to-talk over `Channel::Unreliable`,
//!   relayed by the authority to whoever is in range, played back through
//!   Bevy's own spatial audio, with per-player mute and a nameplate.
//! - **Phase 2** — walls, corners and closed doors, over `nav`'s portal
//!   graph. Changes only what feeds `VoiceHeard`'s `gain`/`muffle`/`emitter`
//!   — the wire shape and the relay's shape do not change.
//! - **Phase 3** — the handset (`VoiceMode::Handset`, already reserved on the
//!   wire so Phase 3 does not need to touch it again).
//!
//! # The split this follows
//!
//! The same one [`crate::speech`] already uses, for the same reason: the
//! authority decides *who can hear whom* (a simulation question both peers
//! must agree on, in [`net::relay_voice_frames`]), while presentation happens
//! on both ends (in [`stream`]). A client's world is a view — anything built
//! only on the host fails silently for the guest, which is how every co-op
//! bug in this project has presented.
//!
//! # File layout
//!
//! - [`frame`] — downmix, 20 ms framing, capture gain. Pure, hardware-free.
//! - [`jitter`] — reordering, loss concealment policy. Pure, codec-free.
//! - [`codec`] — the Opus wrapper both [`capture`] and [`stream`] use.
//! - [`net`] — the wire types and the authority's relay.
//! - [`capture`] — the microphone: opens a device, gates it on push-to-talk,
//!   encodes, sends.
//! - [`stream`] — playback: decodes, mixes into Bevy's audio graph, and the
//!   nameplate/mute presentation built on top of it.

use bevy::prelude::*;

mod capture;
pub mod codec;
pub mod frame;
pub mod jitter;
mod net;
mod stream;

// `VoiceMode` stays reachable at `net::VoiceMode` for now rather than
// re-exported here — nothing outside `voice` constructs one yet. Phase 3's
// handset item is expected to be the first, at which point it belongs here.
pub use net::{VoiceFrame, VoiceHeard, PROXIMITY_RANGE};

pub struct VoicePlugin;

impl Plugin for VoicePlugin {
    fn build(&self, app: &mut App) {
        net::register(app);
        capture::register(app);
        stream::register(app);
    }
}
