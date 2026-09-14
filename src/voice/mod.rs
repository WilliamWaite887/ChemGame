//! Proximity voice chat: hearing the other chemists, positioned in the room —
//! and the handset, for reaching someone proximity voice cannot.
//!
//! Not to be confused with [`crate::radio`], which is a one-way information
//! feed about station stability and has nothing to do with players talking.
//! Player-to-player comms live here.
//!
//! # Where this is up to
//!
//! Built in four phases, so that device and codec failures were diagnosed
//! before the network could be blamed for them, and the wire format settled
//! before occlusion or the handset had to be retrofitted onto it:
//!
//! - **Phase 0** ✅ — capture, framing, the jitter buffer and the codec,
//!   proven locally with no network. `src/bin/mic-probe.rs` is the device
//!   half; [`frame`], [`jitter`] and [`codec`] are the logic half.
//! - **Phase 1** ✅ — push-to-talk over `Channel::Unreliable`, relayed by the
//!   authority, played back through Bevy's own spatial audio, with per-player
//!   mute and a nameplate.
//! - **Phase 2** ✅ — walls, corners and closed doors, over `nav`'s portal
//!   graph — see [`acoustics`]. Landed by changing only what fed
//!   `VoiceHeard`'s `gain`/`muffle`/`emitter`; the wire shape and the relay's
//!   own shape never had to change.
//! - **Phase 3** ✅ — the handset (`VoiceMode::Handset`), station-wide and
//!   unoccluded, filtered by [`radio_filter`] to be unmistakable from a voice
//!   in the room. One channel today (`VoiceChannel::Common`) — every human
//!   co-op player is a chemist in the same lab, so a department picker
//!   mirroring `radio::RadioChannel`'s list would offer channels with no
//!   other human player ever on them; the enum is real so a second channel is
//!   later a variant, not a wire-format change.
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
//! - [`acoustics`] — walls, the portal graph, closed doors. Pure, `World`-free.
//! - [`radio_filter`] — the handset's band-limit/distortion. Pure, stateful.
//! - [`net`] — the wire types and the authority's relay.
//! - [`capture`] — the microphone: opens a device, gates it on whichever of
//!   the two push-to-talk keys is held, encodes, sends.
//! - [`stream`] — playback: decodes, mixes into Bevy's audio graph, and the
//!   nameplate/mute presentation built on top of it.

use bevy::prelude::*;

mod acoustics;
mod capture;
pub mod codec;
pub mod frame;
pub mod jitter;
mod net;
mod radio_filter;
mod stream;

// `VoiceMode`/`VoiceChannel` stay reachable at `net::` for now rather than
// re-exported here — nothing outside `voice` constructs one yet.
pub use net::{VoiceFrame, VoiceHeard, PROXIMITY_RANGE};

pub struct VoicePlugin;

impl Plugin for VoicePlugin {
    fn build(&self, app: &mut App) {
        net::register(app);
        capture::register(app);
        stream::register(app);
    }
}
