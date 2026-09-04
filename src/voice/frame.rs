//! Turning whatever the microphone hands us into the fixed-size frames a codec
//! expects — and back again on the other side, in order, with the gaps filled.
//!
//! Deliberately free of Bevy, the network, and the codec: everything here is
//! plain data in and plain data out, so all of it is unit-testable without a
//! `World`, a socket, or a device. That is the same shape
//! [`crate::lab::resting_place`] uses for geometry, and it matters more here
//! than usual — a jitter buffer's interesting cases (reordering, wraparound,
//! loss) are miserable to reproduce against a live connection and trivial to
//! write down as a test.

/// Samples per second the encoder is defined at. Opus is specified at 48 kHz;
/// feeding it anything else means resampling first.
pub const SAMPLE_RATE: u32 = 48_000;

/// How much audio travels in one packet, in milliseconds.
///
/// 20 ms is the usual voice compromise: long enough that per-packet overhead
/// stays small, short enough that a lost packet is a blip rather than a
/// syllable. It is also one of the frame sizes Opus natively supports, so this
/// is not a free parameter — changing it means changing what the codec is
/// asked for.
pub const FRAME_MS: u32 = 20;

/// Samples in one mono frame. 960 at 48 kHz.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE * FRAME_MS / 1000) as usize;

/// Collects interleaved device audio into whole mono frames.
///
/// Two jobs, both forced on us by what the hardware actually grants: the dev
/// machine's default input refuses a mono request and hands back 48 kHz
/// *stereo*, so capture has to downmix; and a device delivers whatever block
/// size it likes, which never lines up with 960. This holds the remainder
/// between calls so no sample is dropped at a block boundary.
///
/// Resampling is deliberately *not* done here — every input tried so far
/// grants 48 kHz outright, and a resampler written speculatively would be
/// untested code on a path that never runs. If a device ever refuses the rate,
/// that belongs in front of this, not inside it.
#[derive(Default)]
pub struct FrameAccumulator {
    /// Mono samples not yet handed out as a whole frame.
    pending: Vec<f32>,
}

impl FrameAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds interleaved samples in, and calls `emit` once per completed frame.
    ///
    /// `channels` is what the device actually granted, not what was asked for.
    /// A callback rather than a returned `Vec<Vec<f32>>` so a busy frame does
    /// not allocate per packet.
    pub fn push(&mut self, interleaved: &[f32], channels: u16, mut emit: impl FnMut(&[f32])) {
        debug_assert!(channels > 0, "a device with no channels cannot be read");
        if channels == 0 {
            return;
        }

        if channels == 1 {
            self.pending.extend_from_slice(interleaved);
        } else {
            // Average the channels rather than taking the first. A headset that
            // only wires its capsule to the right channel would otherwise
            // record pure silence, which looks exactly like a muted device and
            // would be diagnosed as one.
            let channels = usize::from(channels);
            self.pending.reserve(interleaved.len() / channels);
            for group in interleaved.chunks_exact(channels) {
                let sum: f32 = group.iter().sum();
                self.pending.push(sum / channels as f32);
            }
        }

        let whole = self.pending.len() / FRAME_SAMPLES;
        for index in 0..whole {
            emit(&self.pending[index * FRAME_SAMPLES..(index + 1) * FRAME_SAMPLES]);
        }
        if whole > 0 {
            self.pending.drain(..whole * FRAME_SAMPLES);
        }
    }

    /// Samples held back waiting for a frame to fill. Only for tests and
    /// diagnostics.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Drops anything half-collected. Called when transmission stops, so the
    /// tail of one talk-spurt cannot lead off the next.
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

/// Applies capture gain without letting a loud room clip.
///
/// A plain multiply is what makes amplified voice crackle: anything past ±1.0
/// wraps or hard-clips at the device. This keeps the linear response users
/// expect for ordinary speech and bends the last stretch smoothly instead of
/// slamming into a wall, so a shout distorts gracefully rather than tearing.
pub fn apply_gain(sample: f32, gain: f32) -> f32 {
    let amplified = sample * gain.max(0.0);
    // `tanh` is the standard soft-knee here: near-linear well below the limit,
    // asymptotic at it, and cheap enough per sample not to matter.
    if amplified.abs() <= 0.7 {
        amplified
    } else {
        let sign = amplified.signum();
        let over = amplified.abs() - 0.7;
        sign * (0.7 + 0.3 * (over / 0.3).tanh())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mono_stream_is_split_into_whole_frames_and_the_remainder_is_kept() {
        let mut accumulator = FrameAccumulator::new();
        let mut frames = 0;
        // One and a half frames: one comes out, half stays behind.
        let input = vec![0.5_f32; FRAME_SAMPLES + FRAME_SAMPLES / 2];
        accumulator.push(&input, 1, |frame| {
            assert_eq!(frame.len(), FRAME_SAMPLES);
            frames += 1;
        });
        assert_eq!(frames, 1);
        assert_eq!(accumulator.pending(), FRAME_SAMPLES / 2);
    }

    #[test]
    fn samples_split_across_two_pushes_still_make_one_frame() {
        // The case that matters: a device's block size never divides 960
        // evenly, so a frame is routinely assembled from two callbacks. If the
        // remainder were dropped instead of kept, every boundary would click.
        let mut accumulator = FrameAccumulator::new();
        let mut frames = 0;
        accumulator.push(&vec![0.25_f32; 100], 1, |_| frames += 1);
        assert_eq!(frames, 0, "100 samples is not yet a frame");
        accumulator.push(&vec![0.25_f32; FRAME_SAMPLES - 100], 1, |frame| {
            assert!(frame.iter().all(|s| (*s - 0.25).abs() < f32::EPSILON));
            frames += 1;
        });
        assert_eq!(frames, 1);
        assert_eq!(accumulator.pending(), 0);
    }

    #[test]
    fn stereo_is_averaged_down_to_mono() {
        // The dev machine's default input grants 48 kHz stereo despite being
        // asked for mono, so this path is the normal one, not an edge case.
        let mut accumulator = FrameAccumulator::new();
        let mut captured = Vec::new();
        // Left silent, right at 1.0 — a real headset wiring, and the case a
        // "just take channel 0" downmix would turn into silence.
        let interleaved: Vec<f32> = (0..FRAME_SAMPLES)
            .flat_map(|_| [0.0_f32, 1.0_f32])
            .collect();
        accumulator.push(&interleaved, 2, |frame| captured.extend_from_slice(frame));
        assert_eq!(captured.len(), FRAME_SAMPLES);
        assert!(
            captured.iter().all(|s| (*s - 0.5).abs() < 1e-6),
            "a one-sided capsule must survive the downmix, not vanish"
        );
    }

    #[test]
    fn clearing_drops_the_half_collected_tail() {
        let mut accumulator = FrameAccumulator::new();
        accumulator.push(&vec![0.1_f32; 200], 1, |_| unreachable!());
        assert_eq!(accumulator.pending(), 200);
        accumulator.clear();
        assert_eq!(accumulator.pending(), 0);
    }

    #[test]
    fn ordinary_speech_passes_through_gain_untouched_but_a_shout_cannot_clip() {
        // Quiet speech scaled by a modest gain stays exactly linear.
        let quiet = apply_gain(0.2, 1.5);
        assert!((quiet - 0.3).abs() < 1e-6, "got {quiet}");

        // Anything at all, at any gain, stays inside full scale.
        for gain in [1.0, 4.0, 20.0, 100.0] {
            for sample in [-1.0, -0.9, 0.0, 0.9, 1.0] {
                let out = apply_gain(sample, gain);
                assert!(
                    out.abs() <= 1.0,
                    "gain {gain} on {sample} produced {out}, which would clip"
                );
            }
        }
    }

    #[test]
    fn a_frame_is_twenty_milliseconds_of_audio() {
        // Guards the constants against each other: several timing assumptions
        // downstream (jitter depth, packet cadence) are stated in frames and
        // silently mean nothing if this drifts.
        assert_eq!(FRAME_SAMPLES, 960);
        assert_eq!(FRAME_SAMPLES as u32 * 1000 / SAMPLE_RATE, FRAME_MS);
    }
}
