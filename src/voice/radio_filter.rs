//! The handset's own sound: a voice on the radio should be unmistakable from
//! a voice in the room the instant it starts, not just quieter or flatter.
//!
//! A classic telephone/radio band — cut below ~300 Hz and above ~3000 Hz,
//! plus a touch of soft clipping — is the cheapest way to get that, and one
//! that reads as "radio" to anyone who has ever used one. Two first-order
//! (one-pole) filters in series rather than anything fancier: they are three
//! multiplies and two adds per sample, cost nothing worth measuring at 48 kHz
//! mono, and a voice codec's job is intelligibility, not a faithful telephone
//! emulation.
//!
//! Deliberately its own tiny module rather than folded into [`super::frame`]:
//! everything in `frame` is stateless per sample, and this — an IIR filter —
//! is not. Keeping the stateful, per-stream thing separate is what makes it
//! obvious at a glance that resetting it between talk-spurts is a real
//! requirement and not an oversight.

/// Below this, the highpass stage removes it.
const HIGHPASS_HZ: f32 = 300.0;

/// Above this, the lowpass stage removes it.
const LOWPASS_HZ: f32 = 3000.0;

/// Where the soft clip starts rounding off rather than passing a sample
/// through linearly — see [`soft_clip`].
const CLIP_THRESHOLD: f32 = 0.6;

/// A one-pole highpass into a one-pole lowpass into a soft clip, with the
/// two filters' own state carried between calls. One instance per talk-spurt
/// stream, the same lifetime as the [`super::codec::VoiceDecoder`] sitting
/// beside it — a filter left running across a gap in speech would smear the
/// end of one utterance into the start of the next exactly the way an unreset
/// Opus decoder would.
pub struct RadioFilter {
    highpass_alpha: f32,
    lowpass_alpha: f32,
    highpass_prev_in: f32,
    highpass_prev_out: f32,
    lowpass_prev_out: f32,
}

impl RadioFilter {
    pub fn new(sample_rate: u32) -> Self {
        let dt = 1.0 / sample_rate as f32;
        let highpass_rc = 1.0 / (std::f32::consts::TAU * HIGHPASS_HZ);
        let lowpass_rc = 1.0 / (std::f32::consts::TAU * LOWPASS_HZ);
        Self {
            highpass_alpha: highpass_rc / (highpass_rc + dt),
            lowpass_alpha: dt / (lowpass_rc + dt),
            highpass_prev_in: 0.0,
            highpass_prev_out: 0.0,
            lowpass_prev_out: 0.0,
        }
    }

    /// Filters one sample in place, returning the result.
    pub fn process(&mut self, sample: f32) -> f32 {
        let highpassed =
            self.highpass_alpha * (self.highpass_prev_out + sample - self.highpass_prev_in);
        self.highpass_prev_in = sample;
        self.highpass_prev_out = highpassed;

        let lowpassed =
            self.lowpass_prev_out + self.lowpass_alpha * (highpassed - self.lowpass_prev_out);
        self.lowpass_prev_out = lowpassed;

        soft_clip(lowpassed)
    }

    /// Filters a whole frame in place. The usual call shape — voice always
    /// arrives as [`super::frame::FRAME_SAMPLES`]-length chunks — but kept as
    /// a thin wrapper over [`Self::process`] rather than the primary
    /// interface, since the per-sample state is what actually needs testing.
    pub fn process_frame(&mut self, samples: &mut [f32]) {
        for sample in samples {
            *sample = self.process(*sample);
        }
    }

    /// Clears filter memory for a new talk-spurt, so its first samples are
    /// not shaped by the tail of whatever silence or speech came before.
    pub fn reset(&mut self) {
        self.highpass_prev_in = 0.0;
        self.highpass_prev_out = 0.0;
        self.lowpass_prev_out = 0.0;
    }
}

/// Rounds off toward ±1.0 instead of hard-clipping, using `tanh` past
/// [`CLIP_THRESHOLD`] — the same shape [`super::frame::apply_gain`] uses for
/// capture, reused here for the "a bit of radio crunch" character rather than
/// as a safety clamp, since the filters above never amplify past unity gain
/// on their own.
fn soft_clip(sample: f32) -> f32 {
    if sample.abs() <= CLIP_THRESHOLD {
        sample
    } else {
        let sign = sample.signum();
        let over = sample.abs() - CLIP_THRESHOLD;
        let headroom = 1.0 - CLIP_THRESHOLD;
        sign * (CLIP_THRESHOLD + headroom * (over / headroom).tanh())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::frame::SAMPLE_RATE;

    /// A steady tone at `hz`, `count` samples of it, for feeding through the
    /// filter one at a time.
    fn tone(hz: f32, count: usize) -> Vec<f32> {
        (0..count)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                (t * hz * std::f32::consts::TAU).sin() * 0.4
            })
            .collect()
    }

    /// RMS energy of a signal, the standard way to compare "how loud" two
    /// differently-shaped signals are without comparing them sample by
    /// sample.
    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Long enough that a one-pole filter's startup transient (its first few
    /// dozen samples, while its state catches up to a steady input) is a
    /// small fraction of what gets measured.
    const SETTLE_SAMPLES: usize = 4800;

    #[test]
    fn a_tone_inside_the_band_survives_close_to_full_strength() {
        let mut filter = RadioFilter::new(SAMPLE_RATE);
        let input = tone(1000.0, SETTLE_SAMPLES);
        let output: Vec<f32> = input.iter().map(|&s| filter.process(s)).collect();
        let ratio = rms(&output) / rms(&input);
        assert!(
            ratio > 0.7,
            "1 kHz sits in the middle of the pass band and should barely be touched, got {ratio}"
        );
    }

    #[test]
    fn a_tone_well_below_the_band_is_cut_hard() {
        let mut filter = RadioFilter::new(SAMPLE_RATE);
        let input = tone(60.0, SETTLE_SAMPLES);
        let output: Vec<f32> = input.iter().map(|&s| filter.process(s)).collect();
        let ratio = rms(&output) / rms(&input);
        assert!(
            ratio < 0.3,
            "60 Hz is well under the 300 Hz highpass corner, got {ratio}"
        );
    }

    #[test]
    fn a_tone_well_above_the_band_is_cut_hard() {
        // A single one-pole stage rolls off gently (-6 dB/octave) by design —
        // this is meant to be cheap enough to run every sample, not a steep
        // brick-wall filter — so 8 kHz against a 3 kHz corner (under two
        // octaves up) only gets to roughly a third, not a tenth.
        let mut filter = RadioFilter::new(SAMPLE_RATE);
        let input = tone(8000.0, SETTLE_SAMPLES);
        let output: Vec<f32> = input.iter().map(|&s| filter.process(s)).collect();
        let ratio = rms(&output) / rms(&input);
        assert!(
            ratio < 0.4,
            "8 kHz is well over the 3 kHz lowpass corner, got {ratio}"
        );
    }

    #[test]
    fn the_filter_never_produces_a_sample_outside_full_scale() {
        let mut filter = RadioFilter::new(SAMPLE_RATE);
        // A hot signal a real capture chain should never send, but a
        // hostile or buggy peer's decoded audio is not otherwise bounded.
        let loud: Vec<f32> = tone(1000.0, 2000).iter().map(|s| s * 3.0).collect();
        for sample in loud {
            let out = filter.process(sample);
            assert!(out.is_finite() && out.abs() <= 1.0, "got {out}");
        }
    }

    #[test]
    fn resetting_clears_state_rather_than_only_the_memory_of_recent_samples() {
        let mut filter = RadioFilter::new(SAMPLE_RATE);
        for sample in tone(1000.0, 2000) {
            filter.process(sample);
        }
        filter.reset();
        // After a reset, silence in must produce silence out — nothing left
        // over from the tone that was playing a moment ago.
        assert_eq!(filter.process(0.0), 0.0);
    }

    #[test]
    fn process_frame_matches_calling_process_once_per_sample() {
        let mut a = RadioFilter::new(SAMPLE_RATE);
        let mut b = RadioFilter::new(SAMPLE_RATE);
        let mut frame = tone(1000.0, 960);
        let expected: Vec<f32> = frame.iter().map(|&s| a.process(s)).collect();
        b.process_frame(&mut frame);
        assert_eq!(frame, expected);
    }
}
