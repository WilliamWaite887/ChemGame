//! The station's hidden, authoritative health and pressure director.
//!
//! The exact value never crosses the wire. Other systems submit
//! [`StabilityEvent`]s, this module prices them, and clients receive only the
//! qualitative [`StabilityBand`] needed for presentation and dialogue.

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::arc::{AntagId, ArcOutcome, Campaign, Mode};
use crate::crew::NotResident;
use crate::net::is_authority;
use crate::orders::{Order, OrderKind, OrderResolved, Outcome, Shift};
use crate::radio::{RadioChannel, RadioEntry, RadioLog};
use crate::AppState;

pub const STABILITY_MAX: f32 = 100.0;
pub const INCOMPETENCE_PER_IGNORED_SHENANIGAN: i32 = 4;
const DIRECTOR_TICK_SECONDS: f32 = 10.0;
const CLOSED_GRACE_SECONDS: f32 = 120.0;

pub struct InstabilityPlugin;

impl Plugin for InstabilityPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StationStability>()
            .init_resource::<StabilityClock>()
            .init_resource::<ArcImpactTracker>()
            .add_message::<StabilityEvent>()
            .add_server_message::<StabilityBandSync>(Channel::Ordered)
            .add_systems(
                Update,
                (
                    (
                        record_order_impacts,
                        record_arc_impact,
                        apply_events,
                        advance_station,
                        update_band,
                        broadcast_band,
                        sync_band_to_new_clients,
                    )
                        .chain()
                        .run_if(is_authority),
                    apply_band.run_if(in_state(ClientState::Connected)),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[derive(Resource, Default)]
pub struct ArcImpactTracker {
    observed: Option<Option<(AntagId, usize, ArcOutcome)>>,
}

fn record_arc_impact(
    campaign: Option<Res<Campaign>>,
    mut tracker: ResMut<ArcImpactTracker>,
    mut stability: MessageWriter<StabilityEvent>,
) {
    let current = campaign.as_deref().and_then(|campaign| {
        campaign
            .outcome
            .map(|outcome| (campaign.antag, campaign.history.len(), outcome))
    });
    let Some(previous) = tracker.observed.replace(current) else {
        return;
    };
    if previous != current
        && current.is_some_and(|(_, _, outcome)| outcome == ArcOutcome::PlotSucceeded)
    {
        stability.write(StabilityEvent::ArcSucceeded);
    }
}

fn order_kind(report: &OrderResolved) -> StabilityOrderKind {
    match report.kind {
        OrderKind::Normal if report.development => StabilityOrderKind::Development,
        OrderKind::Normal => StabilityOrderKind::Normal,
        OrderKind::Illicit => StabilityOrderKind::Illicit,
        OrderKind::Crisis => StabilityOrderKind::Crisis,
        OrderKind::Counter => StabilityOrderKind::Counter,
        OrderKind::Hostile => StabilityOrderKind::Hostile,
    }
}

fn failure(outcome: Outcome) -> Option<StabilityFailure> {
    Some(match outcome {
        Outcome::Success => return None,
        Outcome::Short => StabilityFailure::Short,
        Outcome::Impure => StabilityFailure::Impure,
        Outcome::Overdose => StabilityFailure::Overdose,
        Outcome::Wrong => StabilityFailure::Wrong,
        Outcome::Expired => StabilityFailure::Expired,
    })
}

fn record_order_impacts(
    mut resolved: MessageReader<OrderResolved>,
    mut stability: MessageWriter<StabilityEvent>,
) {
    for report in resolved.read() {
        let kind = order_kind(report);
        if report.outcome.is_good() {
            let quality = report.quality.unwrap_or(crate::orders::DeliveryQuality {
                purity: 1.0,
                potency: 1,
                remaining_fraction: 0.0,
            });
            stability.write(StabilityEvent::OrderSucceeded {
                kind,
                purity: quality.purity,
                potency: quality.potency,
                remaining_fraction: quality.remaining_fraction,
            });
        } else if let Some(failure) = failure(report.outcome) {
            stability.write(StabilityEvent::OrderFailed { kind, failure });
        }
    }
}

/// The only station condition clients and authored dialogue may know.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
pub enum StabilityBand {
    #[default]
    Stable,
    Strained,
    Unstable,
    Critical,
    Evacuating,
}

impl StabilityBand {
    pub fn order_gap_multiplier(self) -> f32 {
        match self {
            Self::Stable => 1.0,
            Self::Strained => 0.85,
            Self::Unstable => 0.70,
            Self::Critical | Self::Evacuating => 0.55,
        }
    }

    pub fn active_order_bonus(self) -> usize {
        match self {
            Self::Stable => 0,
            Self::Strained => 1,
            Self::Unstable => 2,
            Self::Critical | Self::Evacuating => 3,
        }
    }
}

/// Persistent station health. `value` is deliberately authority-only.
#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StationStability {
    pub value: f32,
    pub band: StabilityBand,
    pub station_age: f32,
    pub decay_accumulator: f32,
}

impl Default for StationStability {
    fn default() -> Self {
        Self {
            value: STABILITY_MAX,
            band: StabilityBand::Stable,
            station_age: 0.0,
            decay_accumulator: 0.0,
        }
    }
}

/// Backward-compatible type name while save migration remains supported.
pub type Instability = StationStability;

#[derive(Resource, Default)]
pub struct StabilityClock {
    tick: f32,
    closed_for: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StabilityOrderKind {
    Normal,
    Development,
    Crisis,
    Counter,
    Illicit,
    Hostile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StabilityFailure {
    Short,
    Impure,
    Overdose,
    Wrong,
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostileSeverity {
    Minor,
    Major,
}

/// The complete public write interface for station health.
#[derive(Message, Clone, Debug)]
pub enum StabilityEvent {
    OrderSucceeded {
        kind: StabilityOrderKind,
        purity: f32,
        potency: u32,
        remaining_fraction: f32,
    },
    OrderFailed {
        kind: StabilityOrderKind,
        failure: StabilityFailure,
    },
    HostileSucceeded(HostileSeverity),
    ArcSucceeded,
    ClosureNeglect {
        pressure: u32,
    },
}

/// Pure pricing for the ledger. Positive values can only be produced by a
/// successful legitimate order; callers cannot submit an arbitrary delta.
pub fn event_delta(event: &StabilityEvent) -> f32 {
    match *event {
        StabilityEvent::OrderSucceeded {
            kind: StabilityOrderKind::Counter,
            ..
        } => 0.0,
        StabilityEvent::OrderSucceeded {
            kind: StabilityOrderKind::Illicit,
            ..
        } => -5.0,
        StabilityEvent::OrderSucceeded {
            kind: StabilityOrderKind::Hostile,
            ..
        } => -4.0,
        StabilityEvent::OrderSucceeded {
            kind,
            purity,
            potency,
            remaining_fraction,
        } => {
            let purity_bonus = if purity >= 0.97 {
                2.0
            } else if purity >= 0.85 {
                1.0
            } else {
                0.0
            };
            let potency_bonus = potency.saturating_sub(1).min(3) as f32;
            let speed_bonus = if remaining_fraction > 0.60 {
                2.0
            } else if remaining_fraction >= 0.25 {
                1.0
            } else {
                0.0
            };
            1.0 + purity_bonus
                + potency_bonus
                + speed_bonus
                + if kind == StabilityOrderKind::Crisis {
                    2.0
                } else {
                    0.0
                }
        }
        StabilityEvent::OrderFailed {
            kind: StabilityOrderKind::Illicit,
            ..
        } => 0.0,
        StabilityEvent::OrderFailed {
            kind: StabilityOrderKind::Hostile,
            ..
        } => 0.0,
        StabilityEvent::OrderFailed {
            kind: StabilityOrderKind::Development,
            failure: StabilityFailure::Expired,
        } => -1.0,
        StabilityEvent::OrderFailed { kind, failure } => {
            let base = match failure {
                StabilityFailure::Short | StabilityFailure::Impure => -2.0,
                StabilityFailure::Overdose | StabilityFailure::Wrong => -4.0,
                StabilityFailure::Expired => -5.0,
            };
            base + if kind == StabilityOrderKind::Crisis {
                -4.0
            } else {
                0.0
            }
        }
        StabilityEvent::HostileSucceeded(HostileSeverity::Minor) => -4.0,
        StabilityEvent::HostileSucceeded(HostileSeverity::Major) => -8.0,
        StabilityEvent::ArcSucceeded => -15.0,
        StabilityEvent::ClosureNeglect { pressure } => {
            -((1 + (pressure.saturating_sub(1) / 3).min(2)) as f32)
        }
    }
}

pub fn apply_delta(stability: &mut StationStability, delta: f32) {
    stability.value = (stability.value + delta).clamp(0.0, STABILITY_MAX);
}

/// Compatibility entry point for existing hostile modules. Its argument is
/// danger, not a signed stability delta, so it can only ever lower station
/// health. New event-shaped code should emit [`StabilityEvent`] instead.
pub fn nudge_instability(stability: &mut StationStability, danger: i32) {
    if danger > 0 {
        apply_delta(stability, -(danger as f32));
    }
}

fn chemist_mode(campaign: Option<&Campaign>) -> bool {
    campaign.is_none_or(|campaign| campaign.mode == Mode::Chemist)
}

fn apply_events(
    mut stability: ResMut<StationStability>,
    campaign: Option<Res<Campaign>>,
    mut events: MessageReader<StabilityEvent>,
) {
    if !chemist_mode(campaign.as_deref()) {
        events.clear();
        return;
    }
    for event in events.read() {
        let delta = event_delta(event);
        if delta != 0.0 {
            apply_delta(&mut stability, delta);
            info!("station stability {delta:+} ({event:?})");
        }
    }
}

fn passive_loss_per_minute(age_seconds: f32) -> f32 {
    (0.5 + (age_seconds / (15.0 * 60.0)).floor() * 0.25).min(2.0)
}

fn advance_station(
    time: Res<Time>,
    mut shift: ResMut<Shift>,
    campaign: Option<Res<Campaign>>,
    waiting: Query<(), (With<Order>, NotResident)>,
    mut clock: ResMut<StabilityClock>,
    mut stability: ResMut<StationStability>,
) {
    if !chemist_mode(campaign.as_deref()) || stability.value <= 0.0 {
        return;
    }

    let dt = time.delta_secs();
    let active = shift.accepting_orders || !waiting.is_empty();
    let started = stability.station_age > 0.0 || active;
    if !started {
        return;
    }

    if active {
        clock.closed_for = 0.0;
    } else {
        let slowdown = if shift.called { 3.0 } else { 1.0 };
        clock.closed_for += dt / slowdown;
    }

    clock.tick += dt;
    if clock.tick < DIRECTOR_TICK_SECONDS {
        return;
    }
    let elapsed = std::mem::take(&mut clock.tick);
    let debrief_scale = if shift.called { 1.0 / 3.0 } else { 1.0 };
    let age_scale = if active { 1.0 } else { 0.5 * debrief_scale };
    stability.station_age += elapsed * age_scale;
    let age_seconds = stability.station_age.max(0.0) as u32;
    if shift.station_age_seconds != age_seconds {
        shift.station_age_seconds = age_seconds;
    }

    if !active && clock.closed_for < CLOSED_GRACE_SECONDS {
        return;
    }
    let decay_scale = if active { 1.0 } else { 0.5 * debrief_scale };
    stability.decay_accumulator +=
        passive_loss_per_minute(stability.station_age) * elapsed / 60.0 * decay_scale;
    let whole = stability.decay_accumulator.floor();
    if whole >= 1.0 {
        stability.decay_accumulator -= whole;
        apply_delta(&mut stability, -whole);
    }
}

fn target_band(value: f32, current: StabilityBand) -> StabilityBand {
    if value <= 0.0 {
        return StabilityBand::Evacuating;
    }
    match current {
        StabilityBand::Stable => {
            if value <= 75.0 {
                StabilityBand::Strained
            } else {
                current
            }
        }
        StabilityBand::Strained => {
            if value <= 50.0 {
                StabilityBand::Unstable
            } else if value >= 80.0 {
                StabilityBand::Stable
            } else {
                current
            }
        }
        StabilityBand::Unstable => {
            if value <= 25.0 {
                StabilityBand::Critical
            } else if value >= 55.0 {
                StabilityBand::Strained
            } else {
                current
            }
        }
        StabilityBand::Critical => {
            if value >= 30.0 {
                StabilityBand::Unstable
            } else {
                current
            }
        }
        StabilityBand::Evacuating => StabilityBand::Evacuating,
    }
}

fn update_band(
    mut stability: ResMut<StationStability>,
    mut shift: ResMut<Shift>,
    campaign: Option<Res<Campaign>>,
    mut radio: ResMut<RadioLog>,
) {
    if !chemist_mode(campaign.as_deref()) {
        return;
    }
    let next = target_band(stability.value, stability.band);
    if next == stability.band {
        return;
    }
    let worsening = next > stability.band;
    stability.band = next;
    shift.stability_band = next;
    let text = match next {
        StabilityBand::Stable => "Station condition has returned to normal operating tolerances.",
        StabilityBand::Strained if worsening => "Station services are reporting mounting operational strain.",
        StabilityBand::Strained => "Station condition is improving, though several systems remain strained.",
        StabilityBand::Unstable if worsening => "Multiple departments report unstable station operations. Prioritise outstanding requests.",
        StabilityBand::Unstable => "The immediate station emergency is easing. Operations remain unstable.",
        StabilityBand::Critical => "STATION CONDITION CRITICAL. Chemistry support is required immediately.",
        StabilityBand::Evacuating => return,
    };
    let entry = RadioEntry::new(RadioChannel::Bridge, text.to_string())
        .speaker("Duty Officer")
        .station_wide();
    radio.push(if worsening {
        entry.negative()
    } else {
        entry.positive()
    });
}

#[derive(Message, Clone, Copy, Serialize, Deserialize)]
pub struct StabilityBandSync(pub StabilityBand);

fn broadcast_band(
    stability: Res<StationStability>,
    mut outgoing: MessageWriter<ToClients<StabilityBandSync>>,
) {
    if !stability.is_changed() {
        return;
    }
    outgoing.write(ToClients {
        targets: SendTargets::CLIENTS_ONLY,
        message: StabilityBandSync(stability.band),
    });
}

fn sync_band_to_new_clients(
    stability: Res<StationStability>,
    joined: Query<Entity, Added<AuthorizedClient>>,
    mut outgoing: MessageWriter<ToClients<StabilityBandSync>>,
) {
    for client in &joined {
        outgoing.write(ToClients {
            targets: SendTargets::Single(ClientId::Client(client)),
            message: StabilityBandSync(stability.band),
        });
    }
}

fn apply_band(
    mut stability: ResMut<StationStability>,
    mut incoming: MessageReader<StabilityBandSync>,
) {
    for sync in incoming.read() {
        stability.band = sync.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_legitimate_successes_can_be_positive() {
        let normal = StabilityEvent::OrderSucceeded {
            kind: StabilityOrderKind::Normal,
            purity: 0.98,
            potency: 5,
            remaining_fraction: 0.8,
        };
        assert_eq!(event_delta(&normal), 8.0);
        assert_eq!(event_delta(&StabilityEvent::ArcSucceeded), -15.0);
        assert_eq!(
            event_delta(&StabilityEvent::OrderSucceeded {
                kind: StabilityOrderKind::Counter,
                purity: 1.0,
                potency: 5,
                remaining_fraction: 1.0,
            }),
            0.0
        );
    }

    #[test]
    fn quality_bonus_thresholds_are_exact_and_potency_is_capped() {
        let score = |purity, potency, remaining_fraction| {
            event_delta(&StabilityEvent::OrderSucceeded {
                kind: StabilityOrderKind::Normal,
                purity,
                potency,
                remaining_fraction,
            })
        };
        assert_eq!(score(0.849, 1, 0.249), 1.0);
        assert_eq!(score(0.85, 2, 0.25), 4.0);
        assert_eq!(score(0.97, 99, 0.601), 8.0);
        assert_eq!(
            event_delta(&StabilityEvent::OrderSucceeded {
                kind: StabilityOrderKind::Crisis,
                purity: 0.97,
                potency: 99,
                remaining_fraction: 0.601,
            }),
            10.0
        );
    }

    #[test]
    fn crisis_and_failure_tuning_matches_the_ledger() {
        for (failure, expected) in [
            (StabilityFailure::Short, -2.0),
            (StabilityFailure::Impure, -2.0),
            (StabilityFailure::Wrong, -4.0),
            (StabilityFailure::Overdose, -4.0),
            (StabilityFailure::Expired, -5.0),
        ] {
            assert_eq!(
                event_delta(&StabilityEvent::OrderFailed {
                    kind: StabilityOrderKind::Normal,
                    failure,
                }),
                expected
            );
        }
        assert_eq!(
            event_delta(&StabilityEvent::OrderFailed {
                kind: StabilityOrderKind::Crisis,
                failure: StabilityFailure::Wrong,
            }),
            -8.0
        );
        assert_eq!(
            event_delta(&StabilityEvent::OrderFailed {
                kind: StabilityOrderKind::Development,
                failure: StabilityFailure::Expired,
            }),
            -1.0
        );
        assert_eq!(
            event_delta(&StabilityEvent::OrderSucceeded {
                kind: StabilityOrderKind::Illicit,
                purity: 1.0,
                potency: 5,
                remaining_fraction: 1.0,
            }),
            -5.0
        );
        assert_eq!(
            event_delta(&StabilityEvent::HostileSucceeded(HostileSeverity::Minor)),
            -4.0
        );
        assert_eq!(
            event_delta(&StabilityEvent::HostileSucceeded(HostileSeverity::Major)),
            -8.0
        );
    }

    #[test]
    fn closure_escalation_and_clamping_are_bounded() {
        for (pressure, expected) in [(1, -1.0), (3, -1.0), (4, -2.0), (6, -2.0), (7, -3.0)] {
            assert_eq!(
                event_delta(&StabilityEvent::ClosureNeglect { pressure }),
                expected
            );
        }
        let mut stability = StationStability::default();
        apply_delta(&mut stability, 50.0);
        assert_eq!(stability.value, STABILITY_MAX);
        apply_delta(&mut stability, -500.0);
        assert_eq!(stability.value, 0.0);
    }

    #[test]
    fn band_recovery_has_five_points_of_hysteresis() {
        assert_eq!(
            target_band(75.0, StabilityBand::Stable),
            StabilityBand::Strained
        );
        assert_eq!(
            target_band(79.9, StabilityBand::Strained),
            StabilityBand::Strained
        );
        assert_eq!(
            target_band(80.0, StabilityBand::Strained),
            StabilityBand::Stable
        );
        assert_eq!(
            target_band(25.0, StabilityBand::Unstable),
            StabilityBand::Critical
        );
        assert_eq!(
            target_band(29.9, StabilityBand::Critical),
            StabilityBand::Critical
        );
        assert_eq!(
            target_band(30.0, StabilityBand::Critical),
            StabilityBand::Unstable
        );
    }

    #[test]
    fn passive_loss_caps_at_two_per_minute() {
        assert_eq!(passive_loss_per_minute(0.0), 0.5);
        assert_eq!(passive_loss_per_minute(15.0 * 60.0), 0.75);
        assert_eq!(passive_loss_per_minute(24.0 * 60.0 * 60.0), 2.0);
    }

    #[test]
    fn excellent_late_game_chemistry_can_sustain_a_multi_hour_save() {
        let excellent = StabilityEvent::OrderSucceeded {
            kind: StabilityOrderKind::Normal,
            purity: 0.98,
            potency: 5,
            remaining_fraction: 0.8,
        };
        let mut stability = StationStability::default();
        for minute in 0..(4 * 60) {
            apply_delta(
                &mut stability,
                -passive_loss_per_minute(minute as f32 * 60.0),
            );
            apply_delta(&mut stability, event_delta(&excellent));
        }
        assert_eq!(stability.value, STABILITY_MAX);
    }

    #[test]
    fn prolonged_failures_reliably_reach_evacuation() {
        let wrong = StabilityEvent::OrderFailed {
            kind: StabilityOrderKind::Normal,
            failure: StabilityFailure::Wrong,
        };
        let mut stability = StationStability::default();
        for _ in 0..25 {
            apply_delta(&mut stability, event_delta(&wrong));
        }
        assert_eq!(stability.value, 0.0);
        assert_eq!(
            target_band(stability.value, stability.band),
            StabilityBand::Evacuating
        );
    }

    #[test]
    fn band_cues_fire_once_per_transition_and_again_on_recovery() {
        let mut app = App::new();
        app.init_resource::<StationStability>()
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .add_systems(Update, update_band);
        app.world_mut().resource_mut::<StationStability>().value = 70.0;
        app.update();
        app.update();
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);

        app.world_mut().resource_mut::<StationStability>().value = 82.0;
        app.update();
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 2);
        assert_eq!(
            app.world().resource::<StationStability>().band,
            StabilityBand::Stable
        );
    }
}
