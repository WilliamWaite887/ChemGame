//! Opus encode and decode, and the concealment that covers a lost frame.
//!
//! A thin wrapper, deliberately: it exists so the rest of the module never
//! touches `opus` types directly, so the buffer sizes and the "what does a
//! lost frame mean" convention live in one place, and so a codec failure is a
//! `Result` rather than a panic in the middle of a game frame.
//!
//! Voice settings are the ones the codec is actually specified for — 48 kHz
//! mono, 20 ms frames — so [`frame::FRAME_SAMPLES`] is what goes in and comes
//! out every time.

use opus::{Application, Channels, Decoder, Encoder};

use super::frame::{FRAME_SAMPLES, SAMPLE_RATE};

/// Target bitrate. Opus at 24 kbit/s is comfortably transparent for speech,
/// and works out at roughly 60 bytes per 20 ms frame — so four players all
/// talking at once is a few KB/s, far inside the per-tick budget renet gives
/// a channel.
pub const BITRATE: i32 = 24_000;

/// Ceiling on an encoded frame, used to size buffers and to reject nonsense
/// from the wire before it reaches the decoder.
///
/// Far above what 24 kbit/s actually produces (~60 bytes), because Opus may
/// legitimately spend more on a transient, but low enough that a malicious
/// peer cannot make us allocate anything interesting.
pub const MAX_PACKET_BYTES: usize = 400;

/// Errors are values here rather than panics: a device or codec fault must
/// cost the speaker their voice, never take the session down.
#[derive(Debug)]
pub enum CodecError {
    Opus(opus::Error),
    /// A payload arrived that is larger than any frame we would ever send.
    /// Dropped and counted rather than handed to the decoder.
    TooLarge(usize),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Opus(error) => write!(f, "opus: {error}"),
            Self::TooLarge(size) => {
                write!(f, "voice packet of {size} bytes exceeds {MAX_PACKET_BYTES}")
            }
        }
    }
}

impl From<opus::Error> for CodecError {
    fn from(error: opus::Error) -> Self {
        Self::Opus(error)
    }
}

/// Compresses captured frames.
pub struct VoiceEncoder {
    inner: Encoder,
    /// Reused so encoding a frame does not allocate.
    scratch: Vec<u8>,
}

impl VoiceEncoder {
    pub fn new() -> Result<Self, CodecError> {
        // `Voip` rather than `Audio`: it optimises for speech intelligibility
        // and enables the in-band machinery that makes packet loss recoverable,
        // which is exactly the trade this wants.
        let mut inner = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
        inner.set_bitrate(opus::Bitrate::Bits(BITRATE))?;
        Ok(Self {
            inner,
            scratch: vec![0; MAX_PACKET_BYTES],
        })
    }

    /// Encodes exactly one frame. `samples` must be [`FRAME_SAMPLES`] long —
    /// Opus only accepts its own frame sizes, and every caller here goes
    /// through [`super::frame::FrameAccumulator`], which only ever emits that.
    pub fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>, CodecError> {
        debug_assert_eq!(samples.len(), FRAME_SAMPLES);
        let written = self.inner.encode_float(samples, &mut self.scratch)?;
        Ok(self.scratch[..written].to_vec())
    }
}

/// Decompresses one speaker's frames.
///
/// One per speaker, not one globally: Opus decoders carry state between
/// frames, which is what makes concealment sound like the voice it is
/// covering for. Sharing one across speakers would smear them together.
pub struct VoiceDecoder {
    inner: Decoder,
}

impl VoiceDecoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            inner: Decoder::new(SAMPLE_RATE, Channels::Mono)?,
        })
    }

    /// Decodes a received frame into `out`, which must hold [`FRAME_SAMPLES`].
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize, CodecError> {
        debug_assert_eq!(out.len(), FRAME_SAMPLES);
        if packet.len() > MAX_PACKET_BYTES {
            return Err(CodecError::TooLarge(packet.len()));
        }
        Ok(self.inner.decode_float(packet, out, false)?)
    }

    /// Produces one frame of packet-loss concealment.
    ///
    /// An **empty slice** is how the `opus` crate spells "this frame is
    /// missing" — not a `None`, and not a buffer of zeros, which would be
    /// heard as a click. libopus interpolates from the frames either side,
    /// which is why a single dropped packet can pass unnoticed.
    pub fn conceal(&mut self, out: &mut [f32]) -> Result<usize, CodecError> {
        debug_assert_eq!(out.len(), FRAME_SAMPLES);
        Ok(self.inner.decode_float(&[], out, false)?)
    }

    /// Returns the decoder to a clean state, for a new talk-spurt or after a
    /// speaker has been silent. Without this the first frame of a new
    /// utterance is coloured by the end of the last one.
    pub fn reset(&mut self) -> Result<(), CodecError> {
        Ok(self.inner.reset_state()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second of 440 Hz at 48 kHz, in 20 ms frames — a stand-in for speech
    /// that is deterministic and easy to compare against.
    fn tone(frames: usize) -> Vec<Vec<f32>> {
        (0..frames)
            .map(|frame| {
                (0..FRAME_SAMPLES)
                    .map(|i| {
                        let t = (frame * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
                        (t * 440.0 * std::f32::consts::TAU).sin() * 0.5
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_frame_survives_the_round_trip_and_compresses_hard() {
        let mut encoder = VoiceEncoder::new().expect("encoder");
        let mut decoder = VoiceDecoder::new().expect("decoder");
        let frames = tone(10);

        let mut out = vec![0.0; FRAME_SAMPLES];
        let mut encoded_total = 0;
        for frame in &frames {
            let packet = encoder.encode(frame).expect("encode");
            encoded_total += packet.len();
            let samples = decoder.decode(&packet, &mut out).expect("decode");
            assert_eq!(samples, FRAME_SAMPLES);
        }

        // 10 frames of raw f32 mono would be 38400 bytes on the wire.
        let raw = frames.len() * FRAME_SAMPLES * std::mem::size_of::<f32>();
        assert!(
            encoded_total * 20 < raw,
            "expected better than 20x compression, got {encoded_total} vs {raw} raw"
        );
        // And every packet stays inside the bound the wire validation uses.
        assert!(encoded_total / frames.len() < MAX_PACKET_BYTES);
    }

    #[test]
    fn the_decoder_reproduces_the_signal_rather_than_noise() {
        // Opus is lossy, so this cannot compare sample-for-sample. It checks
        // the decoded frame carries roughly the energy that went in — enough
        // to catch a wrong sample rate, a channel mix-up, or silence.
        let mut encoder = VoiceEncoder::new().expect("encoder");
        let mut decoder = VoiceDecoder::new().expect("decoder");
        let frames = tone(25);
        let mut out = vec![0.0; FRAME_SAMPLES];

        // Skip the first few: the codec needs a moment to converge, and
        // asserting on frame 0 would be asserting on the warm-up.
        let mut checked = 0;
        for (index, frame) in frames.iter().enumerate() {
            let packet = encoder.encode(frame).expect("encode");
            decoder.decode(&packet, &mut out).expect("decode");
            if index < 5 {
                continue;
            }
            let input_rms =
                (frame.iter().map(|s| s * s).sum::<f32>() / FRAME_SAMPLES as f32).sqrt();
            let output_rms = (out.iter().map(|s| s * s).sum::<f32>() / FRAME_SAMPLES as f32).sqrt();
            assert!(
                (output_rms - input_rms).abs() < 0.15,
                "frame {index}: in {input_rms:.3} vs out {output_rms:.3}"
            );
            checked += 1;
        }
        assert!(checked > 15, "should have checked most of the frames");
    }

    #[test]
    fn concealment_produces_a_real_frame_rather_than_silence_or_an_error() {
        // The behaviour the jitter buffer depends on for every dropped packet.
        let mut encoder = VoiceEncoder::new().expect("encoder");
        let mut decoder = VoiceDecoder::new().expect("decoder");
        let frames = tone(10);
        let mut out = vec![0.0; FRAME_SAMPLES];

        // Establish context so concealment has something to interpolate from.
        for frame in frames.iter().take(5) {
            let packet = encoder.encode(frame).expect("encode");
            decoder.decode(&packet, &mut out).expect("decode");
        }

        let samples = decoder.conceal(&mut out).expect("conceal must not fail");
        assert_eq!(samples, FRAME_SAMPLES);
        let energy = out.iter().map(|s| s * s).sum::<f32>();
        assert!(
            energy > 0.0,
            "concealment should interpolate the missing audio, not emit a silent \
             frame — silence is exactly the click it exists to avoid"
        );
    }

    #[test]
    fn an_oversized_packet_is_refused_before_it_reaches_the_decoder() {
        // Payloads are attacker-controlled; this is the guard that keeps a
        // hostile peer from reaching libopus with something absurd.
        let mut decoder = VoiceDecoder::new().expect("decoder");
        let mut out = vec![0.0; FRAME_SAMPLES];
        let huge = vec![0_u8; MAX_PACKET_BYTES + 1];
        assert!(matches!(
            decoder.decode(&huge, &mut out),
            Err(CodecError::TooLarge(_))
        ));
    }

    #[test]
    fn a_malformed_packet_is_an_error_rather_than_a_panic() {
        // Random bytes are not valid Opus. The host must survive them.
        let mut decoder = VoiceDecoder::new().expect("decoder");
        let mut out = vec![0.0; FRAME_SAMPLES];
        let garbage = vec![0xFF_u8; 50];
        // Either it decodes to something harmless or it errors; it must not
        // panic, and it must not be able to take the session with it.
        let _ = decoder.decode(&garbage, &mut out);
    }

    #[test]
    fn a_decoder_can_be_reset_for_a_new_talk_spurt() {
        let mut encoder = VoiceEncoder::new().expect("encoder");
        let mut decoder = VoiceDecoder::new().expect("decoder");
        let mut out = vec![0.0; FRAME_SAMPLES];
        for frame in tone(5) {
            let packet = encoder.encode(&frame).expect("encode");
            decoder.decode(&packet, &mut out).expect("decode");
        }
        decoder.reset().expect("reset must succeed");
        // Still usable afterwards.
        let packet = encoder.encode(&tone(1)[0]).expect("encode");
        assert_eq!(
            decoder.decode(&packet, &mut out).expect("decode"),
            FRAME_SAMPLES
        );
    }
}
