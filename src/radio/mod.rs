//! Station radio chatter.
//!
//! The delay is the whole point. A report that lands the instant you hand a
//! beaker over is a score popup; one that arrives half a minute later, while
//! you are elbow-deep in the next order, reads as the station getting on with
//! its day around you.

use std::collections::VecDeque;

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::orders::{Outcome, OrderResolved};
use crate::AppState;

/// Lines kept in the feed before the oldest scrolls off.
const LOG_CAPACITY: usize = 6;

pub struct RadioPlugin;

impl Plugin for RadioPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<RadioScript>::new(&["radio.ron"]))
            .init_resource::<RadioLog>()
            .init_resource::<PendingBroadcasts>()
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (promote_script, queue_reports, deliver_broadcasts)
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[derive(Asset, TypePath, Deserialize)]
pub struct RadioScript {
    pub delay_seconds: (f32, f32),
    pub lines: Vec<RadioLineDef>,
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
#[derive(Clone, Debug)]
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
struct PendingBroadcasts(Vec<(Timer, RadioEntry)>);

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
        // Prefer a line written for this department, fall back to a general
        // one. Without the fallback a new role would silence the radio.
        let matching: Vec<&RadioLineDef> = script
            .lines
            .iter()
            .filter(|line| line.outcome == report.outcome)
            .collect();
        let role_specific: Vec<&RadioLineDef> = matching
            .iter()
            .copied()
            .filter(|line| line.role.as_deref() == Some(report.role.as_str()))
            .collect();
        let general: Vec<&RadioLineDef> = matching
            .iter()
            .copied()
            .filter(|line| line.role.is_none())
            .collect();

        let chosen = if !role_specific.is_empty() && rng.random_bool(0.6) {
            role_specific.choose(&mut rng).copied()
        } else if !general.is_empty() {
            general.choose(&mut rng).copied()
        } else {
            matching.first().copied()
        };

        let Some(line) = chosen else {
            warn!("no radio line for outcome {:?}", report.outcome);
            continue;
        };

        let reagent = db.reagents.get(report.reagent).name.clone();
        let text = line
            .text
            .replace("{name}", &report.name)
            .replace("{role}", &report.role)
            .replace("{reagent}", &reagent);

        let delay = rng.random_range(script.delay_seconds.0..=script.delay_seconds.1);
        pending.0.push((
            Timer::from_seconds(delay, TimerMode::Once),
            RadioEntry {
                channel: channel_for(&report.role),
                text,
                good: report.outcome == Outcome::Success,
            },
        ));
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
        assert_eq!(log.entries.back().unwrap().text, "line 9");
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
}
