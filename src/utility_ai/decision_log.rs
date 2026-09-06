//! Plain-text AI decision log, for handing to someone (or an AI) who was not
//! there when the session happened.
//!
//! The sibling of [`crate::net::diagnostics`], written for the same reason and
//! in the same shape: one file per process, truncated at the start of every
//! run, living next to `saves/` rather than inside it, with the same
//! `+elapsed` column so the two files can be read side by side on one
//! timeline.
//!
//! ## Why this exists
//!
//! Every other view of the utility AI is either a unit test or a body walking
//! across a room. Neither answers the questions that actually go wrong in a
//! long session: *why* did nobody go to Medical, why does one worker keep
//! flipping between two jobs, why is Botany's queue never emptying. The
//! selector already computes exactly that — [`super::DecisionRecord`] carries
//! the winner **and every rejected candidate's score** — and then throws it
//! away into a 16-entry ring buffer nothing reads. This module is that data,
//! written down.
//!
//! ## Volume, and why lines are collapsed
//!
//! An idle agent re-decides every 0.35–0.75s. A *stuck* one therefore produces
//! thousands of identical lines an hour, which is simultaneously the most
//! important signal in the file and the thing most likely to bury everything
//! else. So consecutive identical outcomes per agent are collapsed into one
//! line plus a repeat count. A worker stuck for ten minutes reads as one line
//! saying so, not as ten minutes of scrollback.
//!
//! Nothing here is replicated, and nothing here reads client state. It is
//! authority-only diagnostics, exactly like the decision log it drains.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use bevy::platform::collections::HashMap;
use bevy::prelude::*;

use super::jobs::{JobBoard, JobDomain, JobTicketState};
use super::{ActionResult, ControlOwner, CurrentAction, UtilityActionResolved, UtilityDecisionLog};
use crate::crew::CrewMember;

/// How often the station-wide snapshot line is written.
///
/// Long enough that the file stays readable across an hour-long session, short
/// enough to localise "everything stopped at about the eight-minute mark".
const SNAPSHOT_SECONDS: f32 = 20.0;

/// How many rejected candidates to print beside the winner.
///
/// The point is to see what nearly won, not to dump the whole candidate set:
/// a losing score of 0.79 against a winner of 0.81 is a tuning question, and a
/// candidate that is *always* 0.0 is a broken consideration. Both show up in
/// the top few; the tail is noise.
const RUNNERS_UP: usize = 3;

/// A stuck agent's repeat count is only flushed this often, so the collapse
/// stays honest without the file going quiet about a real problem.
const REPEAT_FLUSH_SECONDS: f32 = 30.0;

fn ailog_path() -> Option<PathBuf> {
    let root = crate::saves::saves_root();
    // One level above `saves/`, matching `net::diagnostics`: throwaway
    // diagnostic text must never go through the signed save path.
    let dir = root.parent().map(PathBuf::from).unwrap_or(root);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        warn!("could not create {}: {error}", dir.display());
        return None;
    }
    Some(dir.join("ailog.txt"))
}

fn shared_file() -> Option<Arc<Mutex<File>>> {
    static FILE: OnceLock<Option<Arc<Mutex<File>>>> = OnceLock::new();
    FILE.get_or_init(|| {
        let path = ailog_path()?;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .inspect_err(|error| eprintln!("could not open {}: {error}", path.display()))
            .ok()
            .map(|file| Arc::new(Mutex::new(file)))
    })
    .clone()
}

/// The open handle, the clock every `+elapsed` column is measured from, and
/// the per-agent state that makes repeat collapsing possible.
#[derive(Resource)]
pub struct AiLog {
    file: Arc<Mutex<File>>,
    started: Instant,
    /// The last line written for each agent, and how many times it has
    /// repeated since. Keyed by entity because a name lookup can fail.
    last: HashMap<Entity, Repeat>,
    /// Highest decision serial already written per agent, so draining the
    /// rolling buffer never re-prints an entry or misses one.
    seen_serial: HashMap<Entity, u64>,
    snapshot_due: f32,
}

struct Repeat {
    text: String,
    count: u32,
    since: f32,
    /// False only for the placeholder inserted before an agent's first line,
    /// so that first line is never mistaken for a repeat of an empty string.
    started: bool,
}

impl AiLog {
    fn write(&self, text: &str) {
        let stamp = format!("+{:>8.3}s", self.started.elapsed().as_secs_f64());
        let Ok(mut file) = self.file.lock() else {
            return;
        };
        // Best-effort: losing the diagnostic log is not worth crashing a
        // session over.
        let _ = writeln!(file, "{stamp} {text}");
    }

    /// Writes `text` for `agent`, collapsing an unbroken run of identical
    /// lines into one line plus a count.
    fn write_collapsed(&mut self, agent: Entity, now: f32, text: String) {
        let run = self.last.entry(agent).or_insert(Repeat {
            text: String::new(),
            count: 0,
            since: now,
            started: false,
        });
        for line in collapse(run, text, now) {
            self.write(&line);
        }
    }
}

/// Decides what an incoming line should actually put in the file, given the
/// run already in progress for that agent.
///
/// Pure, so the rule that keeps a stuck worker from burying the file can be
/// tested without opening one. Returns the lines to write, in order — usually
/// one, sometimes none (collapsed), sometimes two (a pending count flushed
/// ahead of a genuinely new line).
fn collapse(run: &mut Repeat, text: String, now: f32) -> Vec<String> {
    if run.started && run.text == text {
        run.count += 1;
        // A long run still says so periodically, so the file never goes quiet
        // about a problem that is ongoing rather than finished.
        if now - run.since >= REPEAT_FLUSH_SECONDS {
            let (count, seconds) = (run.count, now - run.since);
            run.count = 0;
            run.since = now;
            return vec![format!(
                "      ... repeated {count} times over {seconds:.0}s"
            )];
        }
        return Vec::new();
    }

    let mut lines = Vec::new();
    // Never drop a collapsed run silently: without this the file understates
    // how long a worker was stuck the moment they finally do something else.
    if run.started && run.count > 0 {
        lines.push(format!("      ... repeated {} more times", run.count));
    }
    lines.push(text.clone());
    *run = Repeat {
        text,
        count: 0,
        since: now,
        started: true,
    };
    lines
}

fn open_log(mut commands: Commands, time: Res<Time>) {
    let Some(file) = shared_file() else {
        return;
    };
    let Some(path) = ailog_path() else {
        return;
    };
    let log = AiLog {
        file,
        started: Instant::now(),
        last: HashMap::default(),
        seen_serial: HashMap::default(),
        snapshot_due: time.elapsed_secs(),
    };
    log.write("=== ChemGame utility-AI decision log ===");
    log.write(
        "PICK = a decision. '->' is the winner; 'vs' lists the nearest \
         rejected candidates and their scores.",
    );
    log.write(
        "DONE = an action resolved. Anything other than Completed is worth \
         reading: Unreachable and ReservationUnavailable are the two that \
         strand a worker.",
    );
    log.write("STATION = periodic snapshot. tickets are shown available/claimed.");
    log.write("");
    log.write(
        "Reading it: an odd-looking choice is usually character, not a defect.          A cook who breaks for lunch mid-shift, someone who wanders to the bar,          a botanist who would rather chat, are all the system working.",
    );
    log.write(
        "What is actually broken looks different, and looks the same every time:          a line that repeats forever, a DONE! that never becomes Completed, or a          STATION snapshot whose numbers stop moving.",
    );
    log.write("");
    info!("utility-AI decision log: {}", path.display());
    commands.insert_resource(log);
}

fn name_of(names: &Query<&CrewMember>, agent: Entity) -> String {
    names
        .get(agent)
        .map(|member| member.name.clone())
        .unwrap_or_else(|_| format!("{agent}"))
}

/// Drains newly-made decisions out of the rolling buffer into the file.
fn log_decisions(
    time: Res<Time>,
    decisions: Res<UtilityDecisionLog>,
    names: Query<&CrewMember>,
    agents: Query<Entity, With<super::UtilityAgent>>,
    log: Option<ResMut<AiLog>>,
) {
    let Some(mut log) = log else { return };
    let now = time.elapsed_secs();

    for agent in &agents {
        // The buffer keeps the last 16 per agent; the serial is monotonic, so
        // this prints each exactly once even if several land between frames.
        let seen = log.seen_serial.get(&agent).copied().unwrap_or(0);
        let mut newest = seen;
        let fresh: Vec<String> = decisions
            .entries(agent)
            .filter(|record| record.decision_serial > seen)
            .map(|record| {
                newest = newest.max(record.decision_serial);
                let mut ranked: Vec<_> = record.scores.iter().collect();
                ranked.sort_by(|a, b| b.score.get().total_cmp(&a.score.get()));
                let chosen = match record.selected {
                    Some(key) => {
                        let score = record
                            .scores
                            .iter()
                            .find(|candidate| candidate.key == key)
                            .map(|candidate| candidate.score.get())
                            .unwrap_or(0.0);
                        format!("{:?}/{} score={score:.2}", key.action, key.target_key)
                    }
                    // Every candidate was vetoed. On its own this is normal;
                    // repeated forever it is the signature of a worker who can
                    // see work but can never take it.
                    None => "(nothing)".to_string(),
                };
                let others: Vec<String> = ranked
                    .iter()
                    .filter(|candidate| Some(candidate.key) != record.selected)
                    .take(RUNNERS_UP)
                    .map(|candidate| {
                        format!("{:?} {:.2}", candidate.key.action, candidate.score.get())
                    })
                    .collect();
                let tail = if others.is_empty() {
                    String::new()
                } else {
                    format!("  vs {}", others.join(", "))
                };
                format!("PICK {:<22} -> {chosen}{tail}", name_of(&names, agent))
            })
            .collect();

        for line in fresh {
            log.write_collapsed(agent, now, line);
        }
        if newest > seen {
            log.seen_serial.insert(agent, newest);
        }
    }
}

/// Records how each action actually ended.
///
/// This is the half that catches the stall class: a worker who selects
/// happily and then resolves `Unreachable` every time is invisible in a
/// selection-only log, and is exactly the bug that has bitten this system
/// twice — an off-floor target the walker can never arrive at.
fn log_resolutions(
    time: Res<Time>,
    mut resolved: MessageReader<UtilityActionResolved>,
    names: Query<&CrewMember>,
    log: Option<ResMut<AiLog>>,
) {
    let Some(mut log) = log else { return };
    let now = time.elapsed_secs();
    for message in resolved.read() {
        let marker = if message.result == ActionResult::Completed {
            " "
        } else {
            // Cheap to scan for by eye, and greppable in a long file.
            "!"
        };
        let line = format!(
            "DONE{marker}{:<22} {:?}/{} = {:?}",
            name_of(&names, message.agent),
            message.key.action,
            message.key.target_key,
            message.result,
        );
        log.write_collapsed(message.agent, now, line);
    }
}

/// One line every [`SNAPSHOT_SECONDS`] describing the whole station.
///
/// Answers the question a per-agent log cannot: did the station as a whole
/// stop. A run of snapshots with identical counts and nobody acting is the
/// clearest possible statement that something deadlocked.
fn log_station_snapshot(
    time: Res<Time>,
    board: Option<Res<JobBoard>>,
    controlled: Query<(&ControlOwner, Option<&CurrentAction>)>,
    log: Option<ResMut<AiLog>>,
) {
    let Some(mut log) = log else { return };
    let now = time.elapsed_secs();
    if now < log.snapshot_due {
        return;
    }
    log.snapshot_due = now + SNAPSHOT_SECONDS;

    let mut utility = 0usize;
    let mut acting = 0usize;
    for (owner, action) in &controlled {
        if *owner != ControlOwner::UtilityAction {
            continue;
        }
        utility += 1;
        if action.is_some() {
            acting += 1;
        }
    }

    let tickets = board.map(|board| {
        let mut per_domain: Vec<String> = JobDomain::ALL
            .into_iter()
            .filter_map(|domain| {
                let (mut available, mut claimed) = (0, 0);
                for ticket in board.iter().filter(|ticket| ticket.domain == domain) {
                    match ticket.state {
                        JobTicketState::Available => available += 1,
                        JobTicketState::Claimed(_) => claimed += 1,
                        JobTicketState::Completed(_) => {}
                    }
                }
                (available + claimed > 0).then(|| format!("{domain:?} {available}/{claimed}"))
            })
            .collect();
        if per_domain.is_empty() {
            per_domain.push("none".into());
        }
        per_domain.join("  ")
    });

    log.write(&format!(
        "STATION utility-controlled={utility} acting={acting} idle={} | tickets(avail/claimed): {}",
        utility.saturating_sub(acting),
        tickets.unwrap_or_else(|| "no board".into()),
    ));
}

pub(super) fn register(app: &mut App) {
    app.add_systems(
        OnEnter(crate::AppState::Playing),
        open_log.run_if(crate::net::is_authority),
    )
    .add_systems(
        Update,
        (log_decisions, log_resolutions, log_station_snapshot)
            .chain()
            // After everything that could produce a decision or a resolution,
            // so a line never lands a frame before the thing it describes.
            .after(super::UtilityAiSet::Resolve)
            .run_if(crate::net::is_authority)
            .run_if(in_state(crate::AppState::Playing)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Repeat {
        Repeat {
            text: String::new(),
            count: 0,
            since: 0.0,
            started: false,
        }
    }

    #[test]
    fn the_first_line_for_an_agent_is_always_written() {
        // The placeholder starts holding an empty string. Without the
        // `started` flag, an agent's first line would be compared against it
        // and a genuinely empty line would vanish.
        let mut run = fresh();
        assert_eq!(collapse(&mut run, "PICK first".into(), 0.0), ["PICK first"]);
    }

    #[test]
    fn an_unchanged_line_is_collapsed_rather_than_repeated() {
        // The property the whole module depends on: a worker stuck for ten
        // minutes must not bury the rest of the file.
        let mut run = fresh();
        collapse(&mut run, "PICK stuck -> (nothing)".into(), 0.0);
        let mut written = 0;
        for frame in 1..300 {
            written += collapse(
                &mut run,
                "PICK stuck -> (nothing)".into(),
                frame as f32 * 0.05,
            )
            .len();
        }
        assert!(
            written <= 1,
            "300 identical decisions produced {written} lines",
        );
        assert!(run.count > 0, "the repeats were counted, not discarded");
    }

    #[test]
    fn a_long_run_still_reports_itself_periodically() {
        // Collapsing must not make the file go silent about a problem that is
        // ongoing rather than finished.
        let mut run = fresh();
        collapse(&mut run, "PICK stuck".into(), 0.0);
        collapse(&mut run, "PICK stuck".into(), 1.0);
        let flushed = collapse(&mut run, "PICK stuck".into(), REPEAT_FLUSH_SECONDS + 1.0);
        assert_eq!(flushed.len(), 1);
        assert!(flushed[0].contains("repeated"), "{flushed:?}");
    }

    #[test]
    fn a_changed_line_flushes_the_pending_count_first() {
        // Otherwise a collapsed run is silently dropped the moment the agent
        // does something else, and the file understates how long it was stuck.
        let mut run = fresh();
        collapse(&mut run, "PICK a".into(), 0.0);
        collapse(&mut run, "PICK a".into(), 0.1);
        let lines = collapse(&mut run, "PICK b".into(), 0.2);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("repeated 1 more"), "{lines:?}");
        assert_eq!(lines[1], "PICK b");
        assert_eq!(run.count, 0, "the count reset with the new line");
    }

    #[test]
    fn alternating_between_two_actions_is_never_collapsed() {
        // Thrashing is one of the things the log exists to expose. Collapsing
        // it away would hide exactly the pattern we want to see.
        let mut run = fresh();
        let mut written = 0;
        for frame in 0..20 {
            let text = if frame % 2 == 0 { "PICK a" } else { "PICK b" };
            written += collapse(&mut run, text.into(), frame as f32).len();
        }
        assert_eq!(written, 20, "an alternating run must print every line");
    }
}
