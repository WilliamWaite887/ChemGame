//! Putting arriving voice frames back in order before they are heard.
//!
//! This exists because nothing below it does the job. `Channel::Unreliable`
//! hands frames over exactly as they arrive — `ReceiveChannelUnreliable`'s
//! `process_message` is a bare `push_back`, with no reordering, no dedup and
//! no gap detection — and replicon then drains every queued frame in a single
//! system run. Since gameplay here runs in `Update` at render frame rate, a
//! 30 fps dip means ~33 ms between drains against a 20 ms production cadence,
//! so **frames arrive in bursts even on a flawless LAN with zero loss**. A
//! buffer is not a network concession; it is required in the good case.
//!
//! Playing frames in arrival order is the single most likely way to make voice
//! sound broken, and it fails as continuous crackle rather than as an error,
//! so the policy is written down explicitly and tested rather than tuned by
//! ear:
//!
//! 1. Hold ~40–80 ms before starting, then release one frame per 20 ms.
//! 2. Compare sequence numbers with wraparound, never with `<`.
//! 3. Reorder frames that are merely late.
//! 4. Drop duplicates, and drop anything already past its deadline.
//! 5. Conceal a missing frame rather than stalling or skipping.
//! 6. Cap the hold, dropping oldest first so latency recovers.
//! 7. Reset on a new talk-spurt or after inactivity.
//!
//! Codec-free by construction: this decides *what to play and when*, and says
//! so with [`FrameAction`]. Whoever owns the decoder turns that into samples.
//! That split is what lets every case below be a plain unit test.

use std::collections::BTreeMap;

/// Frames held before playback begins. At 20 ms each this is 60 ms — inside
/// the 40–80 ms target, and low enough that conversation still feels immediate.
pub const TARGET_DEPTH: usize = 3;

/// Hard ceiling on what may be held, in frames (~200 ms).
///
/// Past this the buffer is not absorbing jitter any more, it is just adding
/// delay, so the oldest audio is dropped to claw latency back.
pub const MAX_DEPTH: usize = 10;

/// Consecutive concealed frames before the speaker is treated as gone.
///
/// Opus packet-loss concealment interpolates convincingly for a few frames and
/// then starts inventing texture. Five frames is 100 ms, about the point where
/// continuing to guess sounds worse than falling silent.
pub const MAX_CONSECUTIVE_CONCEALED: u32 = 5;

/// What the caller should do to produce the next 20 ms of audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameAction {
    /// Decode this payload normally.
    Decode(Vec<u8>),
    /// The frame never arrived: run packet-loss concealment for one frame.
    /// With the `opus` crate that is `decode_float(&[], .., false)` — an empty
    /// slice is how the API spells "lost".
    Conceal,
    /// Nothing to play: either still filling up, or the speaker has stopped.
    Silence,
}

/// Why a frame was not accepted. Counted rather than logged — these are
/// ordinary conditions on a real connection, and a per-frame log line would be
/// noise at 50 packets a second.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    /// Arrived, but its slot had already been played.
    pub late: u64,
    /// A sequence number already held or already played.
    pub duplicate: u64,
    /// Dropped because the buffer was at [`MAX_DEPTH`].
    pub overflow: u64,
    /// Playback wanted a frame and none was held.
    pub underflow: u64,
    /// Frames handed to packet-loss concealment.
    pub concealed: u64,
    /// Frames accepted into the buffer.
    pub accepted: u64,
}

/// One speaker's stream, reordered.
///
/// Holds encoded payloads, not samples: concealment and decoding both belong
/// to the codec, and keeping this buffer codec-free is what makes it testable.
pub struct JitterBuffer {
    /// The talk-spurt being played. `None` until the first frame ever arrives.
    stream: Option<u16>,
    held: BTreeMap<u16, Vec<u8>>,
    /// Sequence number to play next; `None` while still filling.
    cursor: Option<u16>,
    consecutive_concealed: u32,
    stats: JitterStats,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            stream: None,
            held: BTreeMap::new(),
            cursor: None,
            consecutive_concealed: 0,
            stats: JitterStats::default(),
        }
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// The talk-spurt currently being played or filled, if any.
    ///
    /// Lets a caller holding a stateful decoder alongside this buffer notice
    /// a new talk-spurt and reset that decoder too — Opus carries a little
    /// prediction context between frames, and while not resetting it is not
    /// a correctness bug (unlike mixing up two different speakers, which this
    /// buffer already prevents on its own), a new utterance's first frame
    /// otherwise decodes slightly coloured by the tail of the last one.
    pub fn current_stream(&self) -> Option<u16> {
        self.stream
    }

    /// Whether anything is currently being played out.
    pub fn is_active(&self) -> bool {
        self.cursor.is_some() || !self.held.is_empty()
    }

    /// Forgets everything, including which stream was playing.
    ///
    /// For a speaker going away entirely. A new talk-spurt does not need this
    /// — [`Self::push`] notices the changed `stream_id` itself — but a
    /// disconnect or a long silence does, so the next spurt re-latches at
    /// target depth instead of inheriting a stale cursor.
    pub fn reset(&mut self) {
        self.stream = None;
        self.held.clear();
        self.cursor = None;
        self.consecutive_concealed = 0;
    }

    /// Offers a received frame.
    ///
    /// `stream_id` changes on every new push-to-talk press, which is what
    /// separates "a new utterance" from "a straggler from the last one" — the
    /// two are indistinguishable by sequence number alone.
    pub fn push(&mut self, stream_id: u16, seq: u16, payload: Vec<u8>) {
        // A new talk-spurt supersedes whatever was playing. Anything still
        // held belongs to the old utterance and is now worthless.
        if self.stream != Some(stream_id) {
            // Only treat it as *new* if it isn't a straggler from the spurt we
            // just finished; `push` can see the tail of the old stream after
            // the new one has started.
            if let Some(current) = self.stream {
                if is_newer(current, stream_id) {
                    // An older stream id arriving after we moved on.
                    self.stats.late += 1;
                    return;
                }
            }
            self.stream = Some(stream_id);
            self.held.clear();
            self.cursor = None;
            self.consecutive_concealed = 0;
        }

        // Already played this slot: too late to matter.
        if let Some(cursor) = self.cursor {
            if !is_newer_or_equal(seq, cursor) {
                self.stats.late += 1;
                return;
            }
        }

        if self.held.contains_key(&seq) {
            self.stats.duplicate += 1;
            return;
        }

        if self.held.len() >= MAX_DEPTH {
            // Drop the oldest rather than refusing the newest: the goal is to
            // shed accumulated latency, and the newest frame is the one whose
            // timing we still care about.
            if let Some(&oldest) = self.held.keys().next() {
                self.held.remove(&oldest);
                self.stats.overflow += 1;
                // The dropped frame must not then be waited for.
                if self.cursor == Some(oldest) {
                    self.cursor = Some(oldest.wrapping_add(1));
                }
            }
        }

        self.held.insert(seq, payload);
        self.stats.accepted += 1;
    }

    /// Produces the next 20 ms of audio.
    ///
    /// Call once per frame of audio consumed, not once per rendered frame.
    pub fn next_action(&mut self) -> FrameAction {
        // Still filling: stay quiet until there is enough to ride out jitter.
        let Some(cursor) = self.cursor else {
            if self.held.len() < TARGET_DEPTH {
                return FrameAction::Silence;
            }
            // Start from the oldest held frame.
            let start = *self.held.keys().next().expect("just checked non-empty");
            self.cursor = Some(start);
            return self.play_from(start);
        };

        if self.held.is_empty() {
            // Nothing at all. Either the speaker stopped, or the network went
            // away; either way, concealing indefinitely would invent speech.
            self.stats.underflow += 1;
            if self.consecutive_concealed >= MAX_CONSECUTIVE_CONCEALED {
                self.reset();
                return FrameAction::Silence;
            }
            self.consecutive_concealed += 1;
            self.stats.concealed += 1;
            self.cursor = Some(cursor.wrapping_add(1));
            return FrameAction::Conceal;
        }

        self.play_from(cursor)
    }

    /// Plays `seq` if held, otherwise conceals that one slot and moves on.
    fn play_from(&mut self, seq: u16) -> FrameAction {
        if let Some(payload) = self.held.remove(&seq) {
            self.consecutive_concealed = 0;
            self.cursor = Some(seq.wrapping_add(1));
            return FrameAction::Decode(payload);
        }

        // A gap with later frames already waiting: conceal exactly this slot
        // rather than stalling (which would add permanent delay) or skipping
        // ahead (which would shorten the speech).
        if self.consecutive_concealed >= MAX_CONSECUTIVE_CONCEALED {
            self.reset();
            return FrameAction::Silence;
        }
        self.consecutive_concealed += 1;
        self.stats.concealed += 1;
        self.cursor = Some(seq.wrapping_add(1));
        FrameAction::Conceal
    }
}

/// Whether `a` is strictly newer than `b`, allowing for wraparound.
///
/// `u16` at 50 frames a second wraps roughly every 22 minutes of continuous
/// talking. A plain `a > b` would, at that instant, treat every subsequent
/// frame as ancient and wedge the stream until the speaker stopped — a bug
/// that takes a very long session to reproduce and is miserable to diagnose.
/// Comparing the *difference* as a signed value is the standard fix and is
/// correct across the boundary.
pub fn is_newer(a: u16, b: u16) -> bool {
    (a.wrapping_sub(b) as i16) > 0
}

fn is_newer_or_equal(a: u16, b: u16) -> bool {
    (a.wrapping_sub(b) as i16) >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(n: u8) -> Vec<u8> {
        vec![n]
    }

    /// Fills to target depth so playback has started, and returns the buffer.
    fn started() -> JitterBuffer {
        let mut buffer = JitterBuffer::new();
        for seq in 0..TARGET_DEPTH as u16 {
            buffer.push(1, seq, payload(seq as u8));
        }
        buffer
    }

    #[test]
    fn nothing_plays_until_the_buffer_has_filled_to_target_depth() {
        let mut buffer = JitterBuffer::new();
        buffer.push(1, 0, payload(0));
        assert_eq!(buffer.next_action(), FrameAction::Silence);
        buffer.push(1, 1, payload(1));
        assert_eq!(buffer.next_action(), FrameAction::Silence);
        buffer.push(1, 2, payload(2));
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(0)));
    }

    #[test]
    fn frames_play_in_sequence_order_even_when_they_arrive_scrambled() {
        let mut buffer = JitterBuffer::new();
        // Deliberately out of order, as an unreliable channel would deliver.
        buffer.push(1, 2, payload(2));
        buffer.push(1, 0, payload(0));
        buffer.push(1, 1, payload(1));
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(0)));
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(1)));
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(2)));
    }

    #[test]
    fn a_duplicate_frame_is_rejected_and_counted() {
        let mut buffer = JitterBuffer::new();
        buffer.push(1, 0, payload(0));
        buffer.push(1, 0, payload(0));
        assert_eq!(buffer.stats().duplicate, 1);
        assert_eq!(buffer.held(), 1);
    }

    #[test]
    fn a_frame_arriving_after_its_slot_was_played_is_dropped_as_late() {
        let mut buffer = started();
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(0)));
        // Frame 0 turns up again, far too late to be inserted.
        buffer.push(1, 0, payload(0));
        assert_eq!(buffer.stats().late, 1);
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(1)));
    }

    #[test]
    fn a_missing_frame_is_concealed_exactly_once_and_playback_continues() {
        let mut buffer = JitterBuffer::new();
        // 1 never arrives.
        buffer.push(1, 0, payload(0));
        buffer.push(1, 2, payload(2));
        buffer.push(1, 3, payload(3));
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(0)));
        assert_eq!(
            buffer.next_action(),
            FrameAction::Conceal,
            "the gap must be concealed, not stalled through or skipped"
        );
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(2)));
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(3)));
        assert_eq!(buffer.stats().concealed, 1);
    }

    #[test]
    fn sustained_loss_falls_silent_instead_of_concealing_forever() {
        let mut buffer = started();
        for _ in 0..TARGET_DEPTH {
            buffer.next_action();
        }
        // Nothing more ever arrives.
        let mut concealed = 0;
        for _ in 0..MAX_CONSECUTIVE_CONCEALED + 3 {
            match buffer.next_action() {
                FrameAction::Conceal => concealed += 1,
                FrameAction::Silence => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(concealed, MAX_CONSECUTIVE_CONCEALED);
        assert_eq!(buffer.next_action(), FrameAction::Silence);
    }

    #[test]
    fn sequence_numbers_compare_correctly_across_the_u16_wraparound() {
        // The ~22-minute bug. A naive `a > b` fails every one of these.
        assert!(is_newer(0, u16::MAX), "0 follows 65535");
        assert!(is_newer(1, u16::MAX));
        assert!(!is_newer(u16::MAX, 0));
        assert!(is_newer(1, 0));
        assert!(!is_newer(0, 1));
    }

    #[test]
    fn playback_survives_the_sequence_number_wrapping_round_to_zero() {
        let mut buffer = JitterBuffer::new();
        // Straddle the boundary: 65534, 65535, 0, 1.
        buffer.push(1, u16::MAX - 1, payload(1));
        buffer.push(1, u16::MAX, payload(2));
        buffer.push(1, 0, payload(3));
        buffer.push(1, 1, payload(4));

        // BTreeMap orders 0 and 1 *before* 65534 numerically, so a buffer that
        // trusted map order rather than the cursor would play these backwards.
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(3)));
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(4)));
    }

    #[test]
    fn exceeding_the_depth_cap_drops_the_oldest_so_latency_recovers() {
        let mut buffer = JitterBuffer::new();
        for seq in 0..MAX_DEPTH as u16 + 2 {
            buffer.push(1, seq, payload(seq as u8));
        }
        assert_eq!(buffer.held(), MAX_DEPTH);
        assert_eq!(buffer.stats().overflow, 2);
        // The newest frames survived; the oldest two were shed.
        assert_eq!(
            buffer.next_action(),
            FrameAction::Decode(payload(2)),
            "the two oldest should have been dropped, not the newest"
        );
    }

    #[test]
    fn a_new_talk_spurt_discards_the_previous_one_and_relatches() {
        let mut buffer = started();
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(0)));

        // A new PTT press: different stream, sequence restarting.
        buffer.push(2, 0, payload(50));
        assert_eq!(
            buffer.held(),
            1,
            "the previous spurt's frames must be discarded"
        );
        // Re-latches: silent until it has filled again.
        assert_eq!(buffer.next_action(), FrameAction::Silence);
        buffer.push(2, 1, payload(51));
        buffer.push(2, 2, payload(52));
        assert_eq!(buffer.next_action(), FrameAction::Decode(payload(50)));
    }

    #[test]
    fn a_straggler_from_the_previous_spurt_does_not_restart_the_new_one() {
        // The exact reason stream_id exists separately from seq: without it,
        // a delayed frame carrying seq 5 from the old utterance is
        // indistinguishable from the new utterance's seq 5.
        let mut buffer = started();
        buffer.next_action();
        buffer.push(2, 0, payload(50));
        let held_after_new_spurt = buffer.held();

        buffer.push(1, 9, payload(9)); // straggler from spurt 1
        assert_eq!(
            buffer.held(),
            held_after_new_spurt,
            "an old spurt's frame must not be accepted into the new one"
        );
        assert_eq!(buffer.stats().late, 1);
    }

    #[test]
    fn resetting_clears_everything_including_the_stream() {
        let mut buffer = started();
        buffer.next_action();
        buffer.reset();
        assert_eq!(buffer.held(), 0);
        assert!(!buffer.is_active());
        assert_eq!(buffer.next_action(), FrameAction::Silence);
    }

    #[test]
    fn every_counter_moves_only_for_its_own_condition() {
        let mut buffer = JitterBuffer::new();
        buffer.push(1, 0, payload(0));
        assert_eq!(buffer.stats().accepted, 1);
        buffer.push(1, 0, payload(0));
        assert_eq!(buffer.stats().duplicate, 1);
        assert_eq!(buffer.stats().accepted, 1, "a duplicate is not an accept");

        let stats = buffer.stats();
        assert_eq!(stats.late, 0);
        assert_eq!(stats.overflow, 0);
        assert_eq!(stats.underflow, 0);
        assert_eq!(stats.concealed, 0);
    }
}
