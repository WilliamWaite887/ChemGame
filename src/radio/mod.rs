//! Station radio chatter.
//!
//! The delay is the whole point. A report that lands the instant you hand a
//! beaker over is a score popup; one that arrives half a minute later, while
//! you are elbow-deep in the next order, reads as the station getting on with
//! its day around you.

use std::collections::VecDeque;

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chem_data::ChemDb;
use crate::hazards::ActiveHazard;
use crate::net::is_authority;
use crate::orders::{CrisisOrder, Order, OrderResolved, Outcome, Shift};
use crate::showdown::Showdown;
use crate::AppState;

/// Lines kept in the log before the oldest scrolls off.
///
/// Bigger than the small always-on HUD feed shows at once (`ui::RADIO_SLOTS`,
/// 6) — this is the depth of history the standing board's scrollable radio
/// section can show. `RadioEntry` is small and `broadcast_radio` already
/// resends the whole buffer as one snapshot on every change, so 40 stays
/// trivial next to everything else already crossing the wire per frame.
const LOG_CAPACITY: usize = 40;

pub struct RadioPlugin;

impl Plugin for RadioPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<RadioScript>::new(&["radio.ron"]))
            .init_resource::<RadioLog>()
            .init_resource::<PendingBroadcasts>()
            .init_resource::<AmbientRadio>()
            .add_server_message::<RadioSync>(Channel::Ordered)
            .add_systems(Startup, start_loading)
            .add_systems(OnEnter(AppState::Playing), reset_ambient_radio)
            .add_systems(
                Update,
                (
                    // Chatter is written once, on the server, so both chemists
                    // hear the same station rather than two divergent ones.
                    (
                        promote_script,
                        queue_reports,
                        tick_ambient_radio,
                        deliver_broadcasts,
                        broadcast_radio,
                    )
                        .chain()
                        .run_if(is_authority),
                    apply_radio.run_if(in_state(ClientState::Connected)),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[derive(Asset, TypePath, Deserialize)]
pub struct RadioScript {
    pub delay_seconds: (f32, f32),
    pub lines: Vec<RadioLineDef>,
    /// Lines for a legitimate order that resolved with nothing matching its
    /// category (`Wrong`/`Expired` only — every other outcome always has a
    /// matched reagent to name). Uses `{category}`, never `{reagent}`: naming
    /// the order's actual reference reagent here would leak the one thing the
    /// player was never told to look for. See `OrderResolved::reagent`.
    #[serde(default)]
    pub category_lines: Vec<RadioLineDef>,
    /// Quiet-period station colour. These are server-authored exactly like
    /// order reports, so every chemist hears the same joke in the same order.
    #[serde(default)]
    pub ambient_gap_seconds: Option<(f32, f32)>,
    #[serde(default)]
    pub ambient_lines: Vec<AmbientLineDef>,
    #[serde(default)]
    pub exchanges: Vec<RadioExchangeDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RadioLineDef {
    pub outcome: Outcome,
    /// Restricts the line to one department. Role-specific lines are preferred
    /// where they exist, so departments keep their own voice.
    #[serde(default)]
    pub role: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AmbientLineDef {
    pub channel: RadioChannel,
    pub speaker: String,
    pub text: String,
    /// This one goes out over the PA — see [`RadioEntry::announcement`]. Opt
    /// in per line rather than derived from the channel: the Bridge talks to
    /// departments on the radio like anyone else (see `exchanges`), and only
    /// some of what it says is an announcement to the whole station.
    #[serde(default)]
    pub announcement: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RadioExchangeDef {
    pub lines: Vec<AmbientLineDef>,
}

#[derive(Resource)]
struct PendingScript(Handle<RadioScript>);

#[derive(Resource, Deref)]
struct Script(RadioScript);

/// A semantic station radio channel. Keeping this typed prevents presentation
/// code from having to infer that `BRG` deserves a stronger treatment than an
/// ordinary department line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RadioChannel {
    Bridge,
    Medical,
    Security,
    Engineering,
    Cargo,
    Service,
    Lab,
    #[default]
    Common,
}

impl RadioChannel {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Bridge => "BRG",
            Self::Medical => "MED",
            Self::Security => "SEC",
            Self::Engineering => "ENG",
            Self::Cargo => "CGO",
            Self::Service => "SRV",
            Self::Lab => "LAB",
            Self::Common => "COM",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bridge => "Bridge",
            Self::Medical => "Medical",
            Self::Security => "Security",
            Self::Engineering => "Engineering",
            Self::Cargo => "Cargo",
            Self::Service => "Service",
            Self::Lab => "Chemistry Lab",
            Self::Common => "Station Common",
        }
    }

    pub fn default_speaker(self) -> &'static str {
        match self {
            Self::Bridge => "Duty Officer",
            Self::Medical => "Medbay Dispatch",
            Self::Security => "Security Dispatch",
            Self::Engineering => "Engineering Control",
            Self::Cargo => "Cargo Desk",
            Self::Service => "Service Desk",
            Self::Lab => "Lab Annunciator",
            Self::Common => "Station Relay",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RadioTone {
    Positive,
    Negative,
    #[default]
    Neutral,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RadioPriority {
    Ambient,
    #[default]
    Routine,
    Urgent,
    StationWide,
    /// The station's alert level itself going up.
    ///
    /// Above [`StationWide`](Self::StationWide) rather than beside it: it
    /// outranks every other line for the dispatch card, and it is the one
    /// priority that sounds the alert klaxon instead of the ordinary
    /// announcement chime. Exactly one line in the game airs at this level —
    /// `showdown::arm_showdown`'s, the moment the main antagonist stops
    /// working through other people.
    RedAlert,
}

/// One delivered transmission. `sequence` is assigned only by `RadioLog`,
/// never by callers, which makes rollover and network snapshots reliable.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RadioEntry {
    pub sequence: u64,
    pub channel: RadioChannel,
    pub speaker: Option<String>,
    pub text: String,
    pub tone: RadioTone,
    pub priority: RadioPriority,
    /// Went out over the station's public-address system rather than as
    /// traffic on a department channel, so it opens with the PA jingle instead
    /// of the channel ident (`audio::play_radio_sfx`).
    ///
    /// Orthogonal to [`priority`](Self::priority), and deliberately so: the
    /// morale bulletins and the suggestion-box updates are the *least* urgent
    /// lines in the game — [`RadioPriority::Ambient`] — and are exactly what
    /// the PA is for. The two priorities that outrank an announcement already
    /// carry a cue of their own and do not want a second one in front of it.
    pub announcement: bool,
}

impl RadioEntry {
    pub fn new(channel: RadioChannel, text: impl Into<String>) -> Self {
        Self {
            sequence: 0,
            channel,
            speaker: Some(channel.default_speaker().to_string()),
            text: text.into(),
            tone: RadioTone::Neutral,
            priority: RadioPriority::Routine,
            announcement: false,
        }
    }

    pub fn speaker(mut self, speaker: impl Into<String>) -> Self {
        self.speaker = Some(speaker.into());
        self
    }

    pub fn tone(mut self, tone: RadioTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn positive(self) -> Self {
        self.tone(RadioTone::Positive)
    }

    pub fn negative(self) -> Self {
        self.tone(RadioTone::Negative)
    }

    pub fn priority(mut self, priority: RadioPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn urgent(self) -> Self {
        self.priority(RadioPriority::Urgent)
    }

    pub fn station_wide(self) -> Self {
        self.priority(RadioPriority::StationWide)
    }

    /// See [`RadioPriority::RedAlert`] for why this is not just a louder
    /// [`station_wide`](Self::station_wide).
    pub fn red_alert(self) -> Self {
        self.priority(RadioPriority::RedAlert)
    }

    pub fn ambient(self) -> Self {
        self.priority(RadioPriority::Ambient)
    }

    /// See [`RadioEntry::announcement`].
    pub fn over_the_pa(mut self) -> Self {
        self.announcement = true;
        self
    }
}

#[derive(Resource, Default)]
pub struct RadioLog {
    pub entries: VecDeque<RadioEntry>,
    next_sequence: u64,
}

impl RadioLog {
    pub fn push(&mut self, mut entry: RadioEntry) {
        entry.sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.entries.push_back(entry);
        while self.entries.len() > LOG_CAPACITY {
            self.entries.pop_front();
        }
    }

    fn replace_snapshot(&mut self, entries: impl IntoIterator<Item = RadioEntry>) {
        self.entries = entries.into_iter().collect();
        self.next_sequence = self
            .entries
            .back()
            .map_or(0, |entry| entry.sequence.saturating_add(1));
    }
}

#[derive(Resource, Default)]
pub struct PendingBroadcasts(Vec<(Timer, RadioEntry)>);

impl PendingBroadcasts {
    /// Schedules `entry` to land in `delay` seconds.
    ///
    /// The one write path onto the queue — `queue_reports` uses this
    /// internally too, so a delayed line always looks the same to
    /// `deliver_broadcasts` regardless of what caused it.
    pub fn push_delayed(&mut self, delay: f32, entry: RadioEntry) {
        self.0
            .push((Timer::from_seconds(delay.max(0.0), TimerMode::Once), entry));
    }

    /// How many lines are queued but not yet delivered. Test-only: other
    /// modules' tests use this to confirm something was actually scheduled,
    /// without reaching into the queue's contents.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
}

/// Server-side quiet-period scheduler. `bag` addresses the combined ambient
/// line/exchange pool and is exhausted before it is refilled, preventing the
/// same gag from immediately repeating.
#[derive(Resource)]
struct AmbientRadio {
    timer: Timer,
    bag: Vec<usize>,
    last_log_sequence: Option<u64>,
    urgent_quiet_seconds: f32,
}

impl Default for AmbientRadio {
    fn default() -> Self {
        Self {
            timer: Timer::from_seconds(90.0, TimerMode::Once),
            bag: Vec::new(),
            last_log_sequence: None,
            urgent_quiet_seconds: 0.0,
        }
    }
}

fn reset_ambient_radio(mut ambient: ResMut<AmbientRadio>) {
    *ambient = AmbientRadio::default();
}

/// Short department tag shown against each line.
pub fn channel_for(role: &str) -> RadioChannel {
    match role {
        "Medical" => RadioChannel::Medical,
        "Security" => RadioChannel::Security,
        "Engineering" => RadioChannel::Engineering,
        "Cargo" => RadioChannel::Cargo,
        "Service" => RadioChannel::Service,
        _ => RadioChannel::Common,
    }
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingScript(assets.load("data/station.radio.ron")));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingScript>>,
    mut scripts: ResMut<Assets<RadioScript>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingScript>();
}

/// Picks a line for `outcome`, preferring one written for `role` — role-
/// specific lines win 60% of the time when one exists, so departments keep
/// their own voice, and a general fallback exists so a new role can never
/// silence a whole outcome.
fn pick_line<'a>(
    lines: &'a [RadioLineDef],
    outcome: Outcome,
    role: &str,
    rng: &mut impl Rng,
) -> Option<&'a RadioLineDef> {
    let matching: Vec<&RadioLineDef> = lines
        .iter()
        .filter(|line| line.outcome == outcome)
        .collect();
    let role_specific: Vec<&RadioLineDef> = matching
        .iter()
        .copied()
        .filter(|line| line.role.as_deref() == Some(role))
        .collect();
    let general: Vec<&RadioLineDef> = matching
        .iter()
        .copied()
        .filter(|line| line.role.is_none())
        .collect();

    if !role_specific.is_empty() && rng.random_bool(0.6) {
        role_specific.choose(rng).copied()
    } else if !general.is_empty() {
        general.choose(rng).copied()
    } else {
        matching.first().copied()
    }
}

/// Turns a resolved order into a line, scheduled for later.
fn queue_reports(
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    mut resolved: MessageReader<OrderResolved>,
    mut pending: ResMut<PendingBroadcasts>,
) {
    let Some(script) = script else {
        // Drain anyway; a report with no script is better lost than delivered
        // in a burst once the asset finally lands.
        resolved.clear();
        return;
    };

    let mut rng = rand::rng();
    for report in resolved.read() {
        // A matched reagent (every antagonist order, and any legitimate order
        // that resolved as Success/Short/Impure/Overdose) reads from the
        // ordinary `{reagent}` pool. A legitimate order with nothing matching
        // its category (Wrong/Expired) reads from `category_lines` instead,
        // which never gets a real chemical name to substitute in.
        let text = if let Some(reagent) = report.reagent {
            let Some(line) = pick_line(&script.lines, report.outcome, &report.role, &mut rng)
            else {
                warn!("no radio line for outcome {:?}", report.outcome);
                continue;
            };
            let reagent = db.reagents.get(reagent).name.clone();
            line.text
                .replace("{name}", &report.name)
                .replace("{role}", &report.role)
                .replace("{reagent}", &reagent)
        } else {
            let Some(line) = pick_line(
                &script.category_lines,
                report.outcome,
                &report.role,
                &mut rng,
            ) else {
                warn!("no category radio line for outcome {:?}", report.outcome);
                continue;
            };
            let phrase = report
                .category
                .map(|cat| cat.want_phrase())
                .unwrap_or_default();
            line.text
                .replace("{name}", &report.name)
                .replace("{role}", &report.role)
                .replace("{category}", phrase)
        };

        let delay = rng.random_range(script.delay_seconds.0..=script.delay_seconds.1);
        let tone = if report.outcome == Outcome::Success {
            RadioTone::Positive
        } else {
            RadioTone::Negative
        };
        pending.push_delayed(
            delay,
            RadioEntry::new(channel_for(&report.role), text)
                .speaker(&report.name)
                .tone(tone),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn tick_ambient_radio(
    time: Res<Time>,
    script: Option<Res<Script>>,
    shift: Res<Shift>,
    orders: Query<&Order>,
    crises: Query<(), With<CrisisOrder>>,
    hazards: Query<(), With<ActiveHazard>>,
    showdown: Option<Res<Showdown>>,
    mut ambient: ResMut<AmbientRadio>,
    mut pending: ResMut<PendingBroadcasts>,
    mut log: ResMut<RadioLog>,
) {
    let Some(script) = script else {
        return;
    };
    let newest = log.entries.back();
    if newest.map(|entry| entry.sequence) != ambient.last_log_sequence {
        ambient.last_log_sequence = newest.map(|entry| entry.sequence);
        if newest.is_some_and(|entry| entry.priority >= RadioPriority::Urgent) {
            ambient.urgent_quiet_seconds = 20.0;
        }
    }
    ambient.urgent_quiet_seconds = (ambient.urgent_quiet_seconds - time.delta_secs()).max(0.0);

    let near_expiry = orders.iter().any(|order| order.remaining() < 30.0);
    let suppressed = !shift.accepting_orders
        || near_expiry
        || !crises.is_empty()
        || !hazards.is_empty()
        || showdown.is_some()
        || ambient.urgent_quiet_seconds > 0.0;
    if suppressed || !ambient.timer.tick(time.delta()).just_finished() {
        return;
    }

    let choices = script.ambient_lines.len() + script.exchanges.len();
    if choices == 0 {
        return;
    }
    let mut rng = rand::rng();
    if ambient.bag.is_empty() {
        ambient.bag.extend(0..choices);
        ambient.bag.shuffle(&mut rng);
    }
    let choice = ambient.bag.pop().unwrap_or_default();
    if let Some(line) = script.ambient_lines.get(choice) {
        log.push(ambient_entry(line));
    } else if let Some(exchange) = script.exchanges.get(choice - script.ambient_lines.len()) {
        let mut delay = 0.0;
        for line in &exchange.lines {
            let entry = ambient_entry(line);
            if delay == 0.0 {
                log.push(entry);
            } else {
                pending.push_delayed(delay, entry);
            }
            delay += rng.random_range(1.5..=3.0);
        }
    }

    let gap = script.ambient_gap_seconds.unwrap_or((75.0, 120.0));
    ambient.timer = Timer::from_seconds(rng.random_range(gap.0..=gap.1), TimerMode::Once);
}

/// One authored quiet-period line, as it lands in the log. Shared by the
/// standalone lines and the exchanges so a two-hander can front its opening
/// line with the PA jingle exactly like a standalone bulletin does.
fn ambient_entry(line: &AmbientLineDef) -> RadioEntry {
    let entry = RadioEntry::new(line.channel, line.text.clone())
        .speaker(&line.speaker)
        .ambient();
    if line.announcement {
        entry.over_the_pa()
    } else {
        entry
    }
}

fn deliver_broadcasts(
    time: Res<Time>,
    mut pending: ResMut<PendingBroadcasts>,
    mut log: ResMut<RadioLog>,
) {
    let mut due = Vec::new();
    pending.0.retain_mut(|(timer, entry)| {
        if timer.tick(time.delta()).just_finished() {
            due.push(entry.clone());
            false
        } else {
            true
        }
    });
    for entry in due {
        info!("[{}] {}", entry.channel.tag(), entry.text);
        log.push(entry);
    }
}

/// The visible feed, pushed to clients whenever a line lands.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct RadioSync(Vec<RadioEntry>);

fn broadcast_radio(log: Res<RadioLog>, mut outgoing: MessageWriter<ToClients<RadioSync>>) {
    if !log.is_changed() {
        return;
    }
    outgoing.write(ToClients {
        targets: SendTargets::CLIENTS_ONLY,
        message: RadioSync(log.entries.iter().cloned().collect()),
    });
}

fn apply_radio(mut log: ResMut<RadioLog>, mut incoming: MessageReader<RadioSync>) {
    for sync in incoming.read() {
        log.replace_snapshot(sync.0.iter().cloned());
    }
}

/// Puts an incoming request on the feed. Called when an order is created, so
/// the radio carries both halves of the conversation.
pub fn announce_request(log: &mut RadioLog, name: &str, role: &str, plea: &str) {
    log.push(RadioEntry::new(channel_for(role), plea).speaker(name));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_only_the_most_recent_lines() {
        let mut log = RadioLog::default();
        for index in 0..LOG_CAPACITY + 4 {
            log.push(
                RadioEntry::new(RadioChannel::Medical, format!("line {index}"))
                    .tone(RadioTone::Positive),
            );
        }
        assert_eq!(log.entries.len(), LOG_CAPACITY);
        assert_eq!(log.entries.front().unwrap().text, "line 4");
        assert_eq!(
            log.entries.back().unwrap().text,
            format!("line {}", LOG_CAPACITY + 3)
        );
        assert_eq!(log.entries.front().unwrap().sequence, 4);
        assert_eq!(log.entries.back().unwrap().sequence, 43);
    }

    #[test]
    fn every_outcome_has_at_least_one_line() {
        // A missing outcome means silence exactly when the player most needs
        // feedback, and it would only show up during play.
        let script: RadioScript =
            ron::from_str(include_str!("../../assets/data/station.radio.ron")).unwrap();

        for outcome in [
            Outcome::Success,
            Outcome::Short,
            Outcome::Impure,
            Outcome::Overdose,
            Outcome::Wrong,
            Outcome::Expired,
        ] {
            let general = script
                .lines
                .iter()
                .filter(|line| line.outcome == outcome && line.role.is_none())
                .count();
            assert!(
                general >= 2,
                "{outcome:?} needs two role-agnostic fallbacks so an unmatched department stays varied"
            );
        }
    }

    #[test]
    fn every_category_outcome_has_at_least_one_line() {
        // Wrong and Expired are the only outcomes a legitimate order can
        // resolve to with nothing matched — the only two that ever read from
        // `category_lines` instead of `lines`.
        let script: RadioScript =
            ron::from_str(include_str!("../../assets/data/station.radio.ron")).unwrap();

        for outcome in [Outcome::Wrong, Outcome::Expired] {
            let general = script
                .category_lines
                .iter()
                .filter(|line| line.outcome == outcome && line.role.is_none())
                .count();
            assert!(
                general >= 2,
                "{outcome:?} needs two role-agnostic category fallbacks"
            );
        }
    }

    #[test]
    fn every_department_has_three_lines_for_every_report_path() {
        let script: RadioScript =
            ron::from_str(include_str!("../../assets/data/station.radio.ron")).unwrap();
        for role in ["Medical", "Security", "Engineering", "Cargo", "Service"] {
            for outcome in [
                Outcome::Success,
                Outcome::Short,
                Outcome::Impure,
                Outcome::Overdose,
                Outcome::Wrong,
                Outcome::Expired,
            ] {
                assert!(
                    script
                        .lines
                        .iter()
                        .filter(|line| {
                            line.outcome == outcome && line.role.as_deref() == Some(role)
                        })
                        .count()
                        >= 3,
                    "{role} needs three {outcome:?} lines"
                );
            }
            for outcome in [Outcome::Wrong, Outcome::Expired] {
                assert!(
                    script
                        .category_lines
                        .iter()
                        .filter(|line| {
                            line.outcome == outcome && line.role.as_deref() == Some(role)
                        })
                        .count()
                        >= 3,
                    "{role} needs three category {outcome:?} lines"
                );
            }
        }
    }

    #[test]
    fn ambient_pool_has_the_promised_cast_and_exchange_depth() {
        let script: RadioScript =
            ron::from_str(include_str!("../../assets/data/station.radio.ron")).unwrap();
        for channel in [
            RadioChannel::Medical,
            RadioChannel::Security,
            RadioChannel::Engineering,
            RadioChannel::Cargo,
            RadioChannel::Service,
        ] {
            assert!(
                script
                    .ambient_lines
                    .iter()
                    .filter(|line| line.channel == channel)
                    .count()
                    >= 6,
                "{} needs six standalone ambient lines",
                channel.label()
            );
        }
        assert!(
            script
                .ambient_lines
                .iter()
                .filter(|line| line.channel == RadioChannel::Bridge)
                .count()
                >= 12
        );
        assert!(script.exchanges.len() >= 10);
        assert!(script
            .exchanges
            .iter()
            .all(|exchange| exchange.lines.len() >= 2));
        assert!(script
            .ambient_lines
            .iter()
            .chain(script.exchanges.iter().flat_map(|exchange| &exchange.lines))
            .all(|line| !line.speaker.trim().is_empty() && !line.text.trim().is_empty()));
    }
}
