//! Lists the microphones this machine offers and captures a short sample from
//! one, reporting its level.
//!
//! Voice chat's first failure mode is never the network — it is a device that
//! does not exist, is muted at the OS level, or hands back a config nothing
//! expected. Diagnosing that from inside the game means starting a session,
//! joining someone, and guessing. This answers it in two seconds without
//! Bevy, the map, or a second player, and is equally the right thing to ask a
//! player to run when they report hearing nothing.
//!
//! `cargo run --bin mic-probe` lists devices;
//! `cargo run --bin mic-probe -- 2` records ~2 s from the default input.

use std::time::{Duration, Instant};

use rodio::cpal::traits::DeviceTrait;
use rodio::microphone::{available_inputs, MicrophoneBuilder};
use rodio::Source;

/// What voice capture actually wants: Opus is defined at 48 kHz, and speech is
/// mono, so asking for these up front avoids a resample/downmix stage. The
/// device is free to refuse both — `prefer_*` is a preference, not a demand —
/// which is exactly what this probe is for finding out.
const WANTED_SAMPLE_RATE: u32 = 48_000;
const WANTED_CHANNELS: u16 = 1;

fn main() {
    let inputs = match available_inputs() {
        Ok(inputs) => inputs,
        Err(error) => {
            eprintln!("could not list input devices: {error}");
            std::process::exit(1);
        }
    };

    if inputs.is_empty() {
        println!("No input devices. Voice chat will have nothing to capture.");
        return;
    }

    // Both the human name and the stable id: on this machine three separate
    // devices are all called "Microphone", so the display name alone cannot
    // say which one a saved setting meant. `DeviceId` round-trips through a
    // string for exactly this purpose, so it — not the name — is what a
    // persisted device preference has to key on.
    println!("{} input device(s):", inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        let id = input
            .clone()
            .into_inner()
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|error| format!("<no id: {error}>"));
        println!("  [{index}] {input}");
        println!("        id: {id}");
    }

    // No duration argument means the caller only wanted the list.
    let Some(seconds) = std::env::args().nth(1) else {
        println!("\nPass a number of seconds to record from the default input,");
        println!("e.g. `cargo run --bin mic-probe -- 2`.");
        return;
    };
    let seconds: f32 = match seconds.parse() {
        Ok(value) if value > 0.0 => value,
        _ => {
            eprintln!("`{seconds}` is not a positive number of seconds");
            std::process::exit(1);
        }
    };

    record(seconds);
}

/// Opens the default input and drains it for `seconds`, reporting the config
/// actually granted and the peak/RMS level heard.
fn record(seconds: f32) {
    let builder = match MicrophoneBuilder::new().default_device() {
        Ok(builder) => builder,
        Err(error) => {
            eprintln!("no default input device: {error}");
            std::process::exit(1);
        }
    };
    let builder = match builder.default_config() {
        Ok(builder) => builder,
        Err(error) => {
            eprintln!("could not read the device's default config: {error}");
            std::process::exit(1);
        }
    };

    let builder = builder
        .prefer_sample_rates(
            WANTED_SAMPLE_RATE
                .try_into()
                .into_iter()
                .collect::<Vec<_>>(),
        )
        .prefer_channel_counts(WANTED_CHANNELS.try_into().into_iter().collect::<Vec<_>>());

    let mut mic = match builder.open_stream() {
        Ok(mic) => mic,
        Err(error) => {
            eprintln!("could not open the input stream: {error}");
            std::process::exit(1);
        }
    };

    let rate = mic.sample_rate().get();
    let channels = mic.channels().get();
    println!("\nrecording {seconds}s at {rate} Hz, {channels} channel(s)");
    if rate != WANTED_SAMPLE_RATE || channels != u32::from(WANTED_CHANNELS) as u16 {
        println!(
            "note: not the preferred {WANTED_SAMPLE_RATE} Hz mono — capture will \
             have to resample/downmix before encoding."
        );
    }

    // Drain only what is already buffered, then yield. `Microphone`'s
    // `Iterator::next` *sleeps* when the buffer is empty, so a bare
    // `for sample in mic` would block; inside Bevy that would stall the frame.
    // `size_hint().0` reports the samples actually available right now, which
    // is the same bounded-drain shape the real capture system has to use.
    let deadline = Instant::now() + Duration::from_secs_f32(seconds);
    let mut peak = 0.0_f32;
    let mut sum_squares = 0.0_f64;
    let mut counted = 0_u64;

    while Instant::now() < deadline {
        let available = mic.size_hint().0;
        if available == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        for _ in 0..available {
            let Some(sample) = mic.next() else {
                eprintln!("the input stream ended early (device removed?)");
                return;
            };
            peak = peak.max(sample.abs());
            sum_squares += f64::from(sample) * f64::from(sample);
            counted += 1;
        }
    }

    if counted == 0 {
        println!("captured nothing at all — the device is present but silent.");
        return;
    }

    let rms = (sum_squares / counted as f64).sqrt();
    println!("captured {counted} samples; peak {peak:.4}, rms {rms:.4}");
    if peak < 0.001 {
        println!("that is effectively silence — check the OS mute/privacy settings.");
    } else if peak >= 0.999 {
        println!("clipping at full scale — turn the input gain down.");
    }
}
