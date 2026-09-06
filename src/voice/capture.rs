//! The microphone half: opening a device, gating it on push-to-talk, and
//! turning what it hears into [`VoiceFrame`]s.
//!
//! Runs unconditionally on every peer, host included — the same way
//! `player::send_move_input` never special-cases the listen host, because
//! replicon delivers a client message to the local `FromClient<M>` queue too.
//! `net::relay_voice_frames` is the only place that decides who a frame
//! actually reaches.

use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::microphone::{available_inputs, Microphone, MicrophoneBuilder};
use rodio::Source;

use crate::settings::{Paused, Settings};
use crate::AppState;

use super::codec::VoiceEncoder;
use super::frame::{apply_gain, FrameAccumulator};
use super::net::VoiceMode;
use super::VoiceFrame;

/// How long to wait after a failed device open before trying again.
///
/// Long enough that a genuinely absent microphone does not spend the whole
/// session re-probing `cpal::default_host()` every frame; short enough that
/// plugging one in mid-session is picked up without a restart.
const REOPEN_COOLDOWN: Duration = Duration::from_secs(2);

pub(super) fn register(app: &mut App) {
    app.init_non_send::<CaptureState>()
        .add_systems(
            Update,
            (maintain_capture_device, capture_and_send_voice)
                .chain()
                .run_if(in_state(AppState::Playing)),
        )
        .add_systems(OnExit(AppState::Playing), stop_capture);
}

/// The open device and everything the capture pipeline needs to keep
/// running, all in one `NonSend` resource rather than split across several.
///
/// `NonSend`, not merely a design preference: `opus::Encoder` is `Send` but
/// **not** `Sync` (the crate's own `unsafe impl Send for Encoder` has no
/// matching `Sync`), so a plain `Resource` — which Bevy requires to be
/// `Send + Sync` — cannot hold one at all. `Microphone` carries the same
/// constraint through its `cpal::Stream`. There is exactly one system that
/// ever touches this, so pinning it to the main thread costs nothing.
#[derive(Default)]
struct CaptureState {
    microphone: Option<Microphone>,
    accumulator: FrameAccumulator,
    encoder: Option<VoiceEncoder>,
    stream_id: u16,
    seq: u16,
    was_transmitting: bool,
    /// Mirrors `Settings.voice_input_device` as of the last successful open,
    /// so a change to the setting is noticed without re-opening every frame.
    open_device_id: Option<String>,
    last_open_attempt: Option<Instant>,
    /// The display name of whichever device is actually open, for future
    /// diagnostics/UI — not read by anything yet, but plumbing this through
    /// now is free and saves a second pass through `cpal` later.
    status: DeviceStatus,
}

/// Public so a future settings/diagnostics screen can show it without this
/// module growing a UI dependency of its own.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeviceStatus {
    #[default]
    Closed,
    /// Open, and the display name it was opened under. Never the persisted
    /// id — a human reads this, not a config file.
    Open(String),
    /// Opened, but not the device the setting asked for.
    Fallback {
        wanted: String,
        using: String,
    },
    Error(String),
}

/// Picks which available device id to open for a saved preference.
///
/// Pure and hardware-free on purpose: this is the one piece of device
/// selection with real decision logic (exact match, or fall back to
/// default), so it is the one piece worth testing without a real microphone.
/// `None` means "use the system default input" — both when nothing was ever
/// saved and when the saved id no longer resolves to a present device.
fn choose_device_id<'a>(preferred: Option<&str>, available: &'a [String]) -> Option<&'a str> {
    let preferred = preferred?;
    available.iter().find(|id| id.as_str() == preferred).map(String::as_str)
}

/// (Re)opens the capture device when nothing is open, the setting changed, or
/// the last attempt is old enough to be worth retrying.
fn maintain_capture_device(settings: Res<Settings>, mut state: NonSendMut<CaptureState>) {
    let setting_changed = state.open_device_id != settings.voice_input_device;
    let device_present = state.microphone.is_some();
    let cooldown_elapsed = state
        .last_open_attempt
        .is_none_or(|at| at.elapsed() >= REOPEN_COOLDOWN);

    if device_present && !setting_changed {
        return;
    }
    if !device_present && !cooldown_elapsed {
        return;
    }

    state.last_open_attempt = Some(Instant::now());
    state.microphone = None;

    match open_device(settings.voice_input_device.as_deref()) {
        Ok((microphone, opened)) => {
            state.status = opened;
            state.open_device_id = settings.voice_input_device.clone();
            state.microphone = Some(microphone);
        }
        Err(message) => {
            warn!("voice capture: {message}");
            state.status = DeviceStatus::Error(message);
            // Leave `open_device_id` alone: if the setting itself is what is
            // broken, retrying it every cooldown tick is the right behaviour
            // (the device may come back), not a change that needs re-noticing.
        }
    }
}

/// Voice's own preferred config, applied identically to whichever device
/// path opened the builder. Not shared as a generic helper: the builder's
/// typestate marker types (`DeviceIsSet`/`ConfigIsSet`) live in a private
/// `rodio` module and cannot be named outside it, so the only way to reach
/// `open_stream()` on either path is to write the tail inline on the
/// concrete value the compiler already has in hand.
macro_rules! finish_opening {
    ($builder:expr) => {
        $builder
            .default_config()
            .map_err(|error| format!("could not read device config: {error}"))?
            .prefer_sample_rates(
                super::frame::SAMPLE_RATE
                    .try_into()
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
            .prefer_channel_counts([1_u16.try_into().expect("nonzero")])
            .open_stream()
            .map_err(|error| format!("could not open stream: {error}"))?
    };
}

/// The actual `cpal`/`rodio` device-open path. Not unit-testable without real
/// hardware — [`choose_device_id`] carries the part that is.
fn open_device(preferred: Option<&str>) -> Result<(Microphone, DeviceStatus), String> {
    let inputs = available_inputs().map_err(|error| format!("could not list inputs: {error}"))?;
    if inputs.is_empty() {
        return Err("no input devices are available".to_string());
    }

    let ids: Vec<String> = inputs
        .iter()
        .filter_map(|input| input.clone().into_inner().id().ok())
        .map(|id| id.to_string())
        .collect();

    if let Some(id) = choose_device_id(preferred, &ids) {
        let index = ids
            .iter()
            .position(|candidate| candidate == id)
            .expect("just matched");
        let display_name = inputs[index].to_string();
        let builder = MicrophoneBuilder::new()
            .device(inputs[index].clone())
            .map_err(|error| format!("could not select device: {error}"))?;
        let microphone: Microphone = finish_opening!(builder);
        return Ok((microphone, DeviceStatus::Open(display_name)));
    }

    // No exact match: the setting is unset, or names a device that vanished.
    // Either way, fall back to the system default rather than leaving voice
    // capture dead — a stale device id must degrade, not disable, capture.
    let builder = MicrophoneBuilder::new()
        .default_device()
        .map_err(|error| format!("no default input device: {error}"))?;
    let display_name = available_inputs()
        .ok()
        .and_then(|inputs| {
            let default_device = cpal::default_host().default_input_device()?;
            let default_id = default_device.id().ok()?.to_string();
            inputs
                .into_iter()
                .find(|input| {
                    input
                        .clone()
                        .into_inner()
                        .id()
                        .map(|id| id.to_string() == default_id)
                        .unwrap_or(false)
                })
                .map(|input| input.to_string())
        })
        .unwrap_or_else(|| "system default".to_string());
    let microphone: Microphone = finish_opening!(builder);

    let status = match preferred {
        Some(wanted) => DeviceStatus::Fallback {
            wanted: wanted.to_string(),
            using: display_name,
        },
        None => DeviceStatus::Open(display_name),
    };
    Ok((microphone, status))
}

fn stop_capture(mut state: NonSendMut<CaptureState>) {
    *state = CaptureState::default();
}

/// Drains whatever the device has buffered, feeds it through gain and framing
/// while transmitting, and writes one [`VoiceFrame`] per completed 20 ms
/// frame.
///
/// Drains unconditionally — even while not transmitting — so the device's own
/// ring buffer never backs up and so the very first frame after a push
/// contains fresh audio rather than a moment already a hundred milliseconds
/// stale.
fn capture_and_send_voice(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<Settings>,
    paused: Res<Paused>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut state: NonSendMut<CaptureState>,
    mut outgoing: MessageWriter<VoiceFrame>,
) {
    let state = &mut *state;
    let Some(microphone) = state.microphone.as_mut() else {
        return;
    };

    // A closed window (headless test, or a moment during teardown) is not a
    // reason to stop transmitting; only an actually-unfocused real window is.
    let focused = windows.iter().next().is_none_or(|window| window.focused);

    let should_transmit = !paused.0 && focused && keys.pressed(settings.bindings.push_to_talk);

    if should_transmit && !state.was_transmitting {
        // A new press. `wrapping_add` rather than a plain `+= 1`: the stream
        // id is meant to wrap, and this is the only place it advances.
        state.stream_id = state.stream_id.wrapping_add(1);
        state.seq = 0;
        state.accumulator.clear();
        if state.encoder.is_none() {
            state.encoder = VoiceEncoder::new()
                .inspect_err(|error| warn!("voice capture: could not start encoder: {error}"))
                .ok();
        }
    } else if !should_transmit && state.was_transmitting {
        // Drop whatever partial frame was mid-collection so a later press
        // does not open with a fragment of silence stitched onto real audio.
        state.accumulator.clear();
    }
    state.was_transmitting = should_transmit;

    let available = microphone.size_hint().0;
    let mut scratch = Vec::with_capacity(available);
    for _ in 0..available {
        match Iterator::next(microphone) {
            Some(sample) => scratch.push(sample),
            None => {
                // The stream ended — device removed, or a driver error. Drop
                // it; `maintain_capture_device` will notice and retry.
                state.microphone = None;
                state.status = DeviceStatus::Error("input stream ended".to_string());
                return;
            }
        }
    }

    if !should_transmit || scratch.is_empty() {
        return;
    }

    let channels = state.microphone.as_ref().expect("checked above").channels().get();
    let gain = settings.mic_gain;

    let mut completed = Vec::new();
    state.accumulator.push(&scratch, channels, |frame| {
        completed.push(frame.to_vec());
    });

    let Some(encoder) = state.encoder.as_mut() else {
        return;
    };
    for frame in completed {
        let gained: Vec<f32> = frame.iter().map(|s| apply_gain(*s, gain)).collect();
        match encoder.encode(&gained) {
            Ok(data) => {
                outgoing.write(VoiceFrame {
                    stream_id: state.stream_id,
                    seq: state.seq,
                    mode: VoiceMode::Proximity,
                    data,
                });
                state.seq = state.seq.wrapping_add(1);
            }
            Err(error) => warn!("voice capture: encode failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_nothing_saved_the_system_default_is_used() {
        assert_eq!(choose_device_id(None, &["a".into(), "b".into()]), None);
    }

    #[test]
    fn a_saved_id_that_is_present_is_selected() {
        let available = vec!["wasapi:a".to_string(), "wasapi:b".to_string()];
        assert_eq!(
            choose_device_id(Some("wasapi:b"), &available),
            Some("wasapi:b")
        );
    }

    #[test]
    fn a_saved_id_no_longer_present_falls_back_to_default() {
        // The exact scenario the plan calls out: a saved device unplugged
        // between sessions must not leave capture permanently broken.
        let available = vec!["wasapi:a".to_string()];
        assert_eq!(choose_device_id(Some("wasapi:missing"), &available), None);
    }

    #[test]
    fn an_empty_device_list_never_panics_and_falls_back() {
        assert_eq!(choose_device_id(Some("anything"), &[]), None);
    }
}
