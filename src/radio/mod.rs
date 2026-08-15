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
use crate::net::is_authority;
use crate::orders::{OrderResolved, Outcome};
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
            .add_server_message::<RadioSync>(Channel::Ordered)
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (
                    // Chatter is written once, on the server, so both chemists
                    // hear the same station rather than two divergent ones.
                    (
                        promote_script,
                        queue_reports,
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

#[derive(Resource)]
struct PendingScript(Handle<RadioScript>);

#[derive(Resource, Deref)]
struct Script(RadioScript);

/// One line on the feed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RadioEntry {
    pub channel: String,
    pub text: String,
    pub good: bool,
}

#[derive(Resource, Default)]
pub struct RadioLog {
    pub entries: VecDeque<RadioEntry>,
}

impl RadioLog {
    pub fn push(&mut self, entry: RadioEntry) {
        self.entries.push_back(entry);
        while self.entries.len() > LOG_CAPACITY {
            self.entries.pop_front();
        }
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

/// Short department tag shown against each line.
pub fn channel_for(role: &str) -> String {
    match role {
        "Medical" => "MED",
        "Security" => "SEC",
        "Engineering" => "ENG",
        "Cargo" => "CGO",
        "Service" => "SRV",
        _ => "COM",
    }
    .to_string()
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
    let matching: Vec<&RadioLineDef> = lines.iter().filter(|line| line.outcome == outcome).collect();
    let role_specific: Vec<&RadioLineDef> = matching
        .iter()
        .copied()
        .filter(|line| line.role.as_deref() == Some(role))
        .collect();
    let general: Vec<&RadioLineDef> = matching.iter().copied().filter(|line| line.role.is_none()).collect();

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
            let Some(line) = pick_line(&script.lines, report.outcome, &report.role, &mut rng) else {
                warn!("no radio line for outcome {:?}", report.outcome);
                continue;
            };
            let reagent = db.reagents.get(reagent).name.clone();
            line.text
                .replace("{name}", &report.name)
                .replace("{role}", &report.role)
                .replace("{reagent}", &reagent)
        } else {
            let Some(line) = pick_line(&script.category_lines, report.outcome, &report.role, &mut rng)
            else {
                warn!("no category radio line for outcome {:?}", report.outcome);
                continue;
            };
            let phrase = report.category.map(|cat| cat.want_phrase()).unwrap_or_default();
            line.text
                .replace("{name}", &report.name)
                .replace("{role}", &report.role)
                .replace("{category}", phrase)
        };

        let delay = rng.random_range(script.delay_seconds.0..=script.delay_seconds.1);
        pending.push_delayed(
            delay,
            RadioEntry {
                channel: channel_for(&report.role),
                text,
                good: report.outcome == Outcome::Success,
            },
        );
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
        info!("[{}] {}", entry.channel, entry.text);
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
        log.entries = sync.0.iter().cloned().collect();
    }
}

/// Puts an incoming request on the feed. Called when an order is created, so
/// the radio carries both halves of the conversation.
pub fn announce_request(log: &mut RadioLog, name: &str, role: &str, plea: &str) {
    log.push(RadioEntry {
        channel: channel_for(role),
        text: format!("{name}: {plea}"),
        good: false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_only_the_most_recent_lines() {
        let mut log = RadioLog::default();
        for index in 0..LOG_CAPACITY + 4 {
            log.push(RadioEntry {
                channel: "MED".into(),
                text: format!("line {index}"),
                good: true,
            });
        }
        assert_eq!(log.entries.len(), LOG_CAPACITY);
        assert_eq!(log.entries.front().unwrap().text, "line 4");
        assert_eq!(
            log.entries.back().unwrap().text,
            format!("line {}", LOG_CAPACITY + 3)
        );
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
                general > 0,
                "{outcome:?} has no role-agnostic line, so an unmatched department would go silent"
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
                general > 0,
                "{outcome:?} has no role-agnostic category line, so an unmatched \
                 legitimate order would go silent"
            );
        }
    }
}
