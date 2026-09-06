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
//! - **Phase 0** (in progress) — capture, framing, and the jitter buffer,
//!   proven locally with no network. `src/bin/mic-probe.rs` is the device
//!   half; [`frame`] and [`jitter`] are the logic half.
//! - **Phase 1** — push-to-talk over `Channel::Unreliable`, relayed by the
//!   authority to whoever is in range.
//! - **Phase 2** — walls, corners and closed doors, over `nav`'s portal graph.
//! - **Phase 3** — the handset.
//!
//! # The split this follows
//!
//! The same one [`crate::speech`] already uses, for the same reason: the
//! authority decides *who can hear whom* (a simulation question both peers
//! must agree on), while presentation happens on both ends. A client's world
//! is a view — anything built only on the host fails silently for the guest,
//! which is how every co-op bug in this project has presented.

// Phase 0 builds and proves the pieces before Phase 1 wires them to a
// microphone and a socket, so nothing here has a caller yet and every item
// reads as dead. The tests exercise all of it. **Remove this the moment
// `VoicePlugin` exists** — after that, an unused item here is a real signal.
#![allow(dead_code)]

pub mod codec;
pub mod frame;
pub mod jitter;
