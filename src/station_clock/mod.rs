//! Wall time on the station, and the daily rhythm the crew live by.
//!
//! The station has a day. It is 20 real minutes long, it starts when the save
//! is created, and it keeps running whatever the player does. Crew get hungry
//! faster as a meal window approaches, so the galley fills around noon and
//! empties by mid-afternoon — and the kitchen starts cooking slightly before
//! the rush rather than reacting to one that has already formed.
//!
//! **Deliberately not built on [`crate::orders::Shift::station_age_seconds`].**
//! That value is scaled by `active` in `instability::advance_station` — half
//! rate when the lab is closed, a third of that again during a debrief — so a
//! clock built on it would make the station's noon depend on whether the
//! chemistry counter is accepting orders. This module reads `Res<Time>` and
//! nothing else.
//!
//! The clock is a *shared baseline*, not a schedule. It shapes how fast hunger
//! climbs, never what hunger is, so a resident who ate an hour ago skips the
//! next window and one who worked through arrives late to an emptying room.
//! Individual actions push people off the baseline; that drift is the point.

mod tuning;

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::net::is_authority;
use crate::AppState;
use crate::

AppStateUnusedImportGuard as _;

pub use tuning::{ClockTuning, ClockTuningError, MealWindow, QuietHours};

/// Station-minutes in a day, matching a 24-hour clock face.
const MINUTES_PER_DAY: f64 = 24.0 * 60.0;

pub struct StationClockPlugin;

impl Plugin for StationClockPlugin {
    fn build(&self, app: &mut App) {
        // Authored tuning first: `ClockTuning::authored` validates the file and
        // panics on a bad one, so an incoherent day fails the game to start
        // here rather than producing a station whose rhythm is quietly wrong.
        app.insert_resource(ClockTuning::authored().clone())
            .init_resource::<StationClock>()
            .add_server_message::<StationClockSync>(Channel::Ordered)
            .add_systems(
                Update,
                (
                    (
                        advance_station_clock,
                        broadcast_station_clock,
                        sync_station_clock_to_new_clients,
                    )
                        .chain()
                        .run_if(is_authority),
                    apply_station_clock.run_if(in_state(ClientState::Connected)),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// How long this save's station has been running, and therefore what time it is.
///
/// `elapsed` is seconds since the save was **created**, not since it was loaded:
/// it round-trips through `WorldSave` so a career resumed on Tuesday picks up
/// the morning it stopped at. A fresh save starts at day 0, hour 0.
///
/// Stored as `f64` because it accumulates for the life of a career; `f32` starts
/// losing whole seconds after a few real days of play.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct StationClock {
    elapsed: f64,
}

impl StationClock {
    /// Rebuilds the clock from a saved whole-second count.
    pub fn from_saved_seconds(seconds: u32) -> Self {
        Self {
            elapsed: f64::from(seconds),
        }
    }

    /// Whole seconds since the save was created, which is what reaches the disk.
    ///
    /// Deliberately quantized. `WorldSave` is compared with `PartialEq` every
    /// frame by `world_state::write_cached_world` to decide whether to write, so
    /// a raw `f64` in the save would differ on every frame, defeat that dedup,
    /// and fsync a signed save file every two seconds forever. The same trap is
    /// documented at `Order::waited` and `AgitationRun::elapsed_secs`.
    pub fn saved_seconds(&self) -> u32 {
        self.elapsed.max(0.0) as u32
    }

    /// Advances the clock. Authority-only; see [`advance_station_clock`].
    pub fn advance(&mut self, seconds: f64) {
        self.elapsed += seconds.max(0.0);
    }

    /// Which station day this is, counting the first as day 1.
    ///
    /// Presentation only: nothing branches on the day number, exactly as
    /// `Shift::shift_number` is the number on the board rather than a
    /// simulation input.
    pub fn day(&self, tuning: &ClockTuning) -> u32 {
        (self.elapsed / f64::from(tuning.day_seconds)) as u32 + 1
    }

    /// How far through the day it is, on 0..1. `0.0` is midnight, `0.5` noon.
    ///
    /// This is the value every rhythm decision is made against, so it is the one
    /// that must stay in range: `rem_euclid` keeps it there even if `elapsed`
    /// were ever handed something strange.
    pub fn time_of_day(&self, tuning: &ClockTuning) -> f32 {
        let day = f64::from(tuning.day_seconds);
        (self.elapsed.rem_euclid(day) / day) as f32
    }

    /// The hour on a 24-hour face, for the log and the HUD.
    pub fn hour(&self, tuning: &ClockTuning) -> u32 {
        (f64::from(self.time_of_day(tuning)) * 24.0) as u32 % 24
    }

    /// The minute past the hour, for the log and the HUD.
    pub fn minute(&self, tuning: &ClockTuning) -> u32 {
        (f64::from(self.time_of_day(tuning)) * MINUTES_PER_DAY) as u32 % 60
    }

    /// `HH:MM` on a 24-hour face.
    pub fn clock_face(&self, tuning: &ClockTuning) -> String {
        format!("{:02}:{:02}", self.hour(tuning), self.minute(tuning))
    }

    /// How strongly the station wants to eat right now, on 0..1.
    ///
    /// Zero outside every window, ramping to 1.0 at a window's peak. This scales
    /// the *hunger rate*, never hunger itself — see the module docs for why that
    /// distinction is the whole design.
    pub fn meal_pressure(&self, tuning: &ClockTuning) -> f32 {
        tuning.meal_pressure(self.time_of_day(tuning))
    }

    /// Whether the kitchen should be cooking ahead of a rush.
    ///
    /// True from `kitchen_lead` before a window opens until it closes, so
    /// Service prepares and serves before the first hungry resident arrives
    /// instead of starting when they are already queueing at the pass.
    pub fn kitchen_is_busy(&self, tuning: &ClockTuning) -> bool {
        tuning.kitchen_is_busy(self.time_of_day(tuning))
    }

    /// Whether the station is in its small hours. Presentation only.
    pub fn is_quiet_hours(&self, tuning: &ClockTuning) -> bool {
        tuning.quiet_hours.contains(self.time_of_day(tuning))
    }
}

/// Advances the clock by real time, and by nothing else.
///
/// The entire body is one addition on purpose. Every attempt to make a station
/// clock "smarter" — pausing while closed, running faster during a shift —
/// reintroduces the `station_age_seconds` problem this module exists to avoid.
fn advance_station_clock(time: Res<Time>, mut clock: ResMut<StationClock>) {
    clock.advance(f64::from(time.delta_secs()));
}

/// The whole seconds a client needs to draw the same clock face.
#[derive(Message, Clone, Copy, Serialize, Deserialize)]
pub struct StationClockSync(pub u32);

/// Sends the clock only when the displayed minute actually moves.
///
/// A 20-minute day holds 1440 station-minutes, so this is roughly one message
/// per second at worst. Sending every frame instead would re-replicate the
/// clock ~60x more often for a readout that cannot show the difference — the
/// trap `Order::waited` and `AgitationRun::elapsed_secs` were both caught in.
fn broadcast_station_clock(
    clock: Res<StationClock>,
    tuning: Res<ClockTuning>,
    mut last_sent: Local<Option<(u32, u32)>>,
    mut outgoing: MessageWriter<ToClients<StationClockSync>>,
) {
    let shown = (clock.day(&tuning), clock.minute(&tuning));
    if *last_sent == Some(shown) {
        return;
    }
    *last_sent = Some(shown);
    outgoing.write(ToClients {
        targets: SendTargets::CLIENTS_ONLY,
        message: StationClockSync(clock.saved_seconds()),
    });
}

/// A joining chemist needs the time immediately, not at the next minute tick.
fn sync_station_clock_to_new_clients(
    clock: Res<StationClock>,
    joined: Query<Entity, Added<AuthorizedClient>>,
    mut outgoing: MessageWriter<ToClients<StationClockSync>>,
) {
    for client in &joined {
        outgoing.write(ToClients {
            targets: SendTargets::Single(ClientId::Client(client)),
            message: StationClockSync(clock.saved_seconds()),
        });
    }
}

fn apply_station_clock(
    mut clock: ResMut<StationClock>,
    mut incoming: MessageReader<StationClockSync>,
) {
    for sync in incoming.read() {
        *clock = StationClock::from_saved_seconds(sync.0);
    }
}
