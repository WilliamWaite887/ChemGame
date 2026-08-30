//! Plain-text co-op debug log, for handing to someone (or an AI) who was not
//! there when the session happened.
//!
//! Runs on both ends — host and client — since a networking bug is usually
//! only visible from one side: a client that silently never receives a
//! spawn looks completely normal from the host's log alone. Both files
//! describe the same session in each peer's own words, so the pair is what
//! reconstructs what actually happened on the wire.
//!
//! Two sources feed the same file, interleaved on one timeline:
//! - Connection lifecycle, read directly off replicon's own state —
//!   [`ConnectedClient`]/[`AuthorizedClient`] add/remove, [`ConnectedAccount`],
//!   [`ClientState`]/[`ServerState`], periodic RTT/packet-loss/replication
//!   stats — so it stays accurate across both the LAN/direct transport and
//!   the Steam one without knowing either backend exists.
//! - Every `warn!`/`error!` logged anywhere in the process, via a
//!   [`tracing_layer`] installed on [`bevy::log::LogPlugin::custom_layer`].
//!   Connection state alone says a client dropped; it takes the deserialize
//!   error or asset failure logged at the same moment to say *why* — and
//!   that line is easy to lose in a console scrollback shared between two
//!   players, but hard to miss in a file made for exactly this handoff.
//!
//! One file per process, truncated at the start of every run:
//! `%LOCALAPPDATA%/ChemGame/netlog-host.txt` on a host or singleplayer
//! launch, `netlog-client.txt` on a join. Both live next to `saves/`, not
//! inside it — this is throwaway diagnostic text, not signed save data.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use bevy::log::tracing::{self, Level, Subscriber};
use bevy::log::tracing_subscriber::layer::Context;
use bevy::log::tracing_subscriber::{registry::LookupSpan, Layer};
use bevy::log::BoxedLayer;
use bevy::prelude::*;
use bevy_replicon::prelude::*;

use crate::net::{ConnectFailed, ConnectedAccount, LaunchMode, LocalAccount};
use crate::AppState;

/// How often a stats snapshot line is appended while connected.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);

/// Which of the two files this process writes, decided the same way
/// [`LaunchMode::from_args`] would — but read straight from `std::env::args`
/// rather than the `LaunchMode` resource, because the tracing layer has to
/// be installed while `LogPlugin` builds, which is before
/// `net::apply_command_line` inserts that resource (see `main.rs`).
/// [`role_name`] re-derives the same string later from the resource, once it
/// exists, purely for the systems that are more natural to write against it
/// — both paths have to agree, which is why this one reads the exact same
/// flags `LaunchMode::parse_args` does.
fn role_from_env_args() -> &'static str {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--host" | "--host-steam" => return "host",
            "--join" | "+connect_lobby" => return "client",
            _ => {}
        }
    }
    "solo"
}

fn netlog_path(role: &str) -> Option<PathBuf> {
    let root = crate::saves::saves_root();
    // One level above `saves/`, i.e. directly under the app data folder —
    // this is not save data and must never go through the signed/
    // recoverable save path.
    let dir = root.parent().map(PathBuf::from).unwrap_or(root);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        warn!("could not create {}: {error}", dir.display());
        return None;
    }
    Some(dir.join(format!("netlog-{role}.txt")))
}

/// The single open handle to this process's log file, shared by every writer.
///
/// [`tracing_layer`] (installed while `LogPlugin` builds, before any plugin's
/// own `build` runs) and [`NetLog`] (opened moments later from `Startup`)
/// used to each open their own independent `File` to the same path — one
/// truncating, one appending. On Windows two handles like that each keep
/// their own file position, so the layer's handle, still sitting at the
/// offset its own truncate-open left it at, would overwrite the header
/// `NetLog` had just written through the *other* handle the moment the first
/// warning came in — the header vanished, silently, only when a warning
/// actually fired early enough to race it. One shared handle behind one
/// lock removes the race instead of trying to out-time it.
fn shared_file(role: &str) -> Option<Arc<Mutex<File>>> {
    static FILE: OnceLock<Option<Arc<Mutex<File>>>> = OnceLock::new();
    FILE.get_or_init(|| {
        let path = netlog_path(role)?;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .inspect_err(|error| {
                // `warn!` would recurse into this same function through
                // `tracing_layer`, and `LogPlugin` may not even be up yet
                // the first time this runs — `eprintln!` is the only sink
                // guaranteed to exist at both call sites.
                eprintln!("could not open {}: {error}", path.display());
            })
            .ok()
            .map(|file| Arc::new(Mutex::new(file)))
    })
    .clone()
}

/// The instant every `+elapsed` column in the log is measured from, shared
/// by every writer so a warning logged before `Startup` and a connection
/// event logged after land on the same clock instead of two that start
/// seconds apart.
fn log_start_time() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn write_line(file: &Mutex<File>, started: Instant, text: &str) {
    let stamp = format!("+{:>6.3}s", started.elapsed().as_secs_f64());
    let Ok(mut file) = file.lock() else { return };
    // Best-effort: losing the debug log is not worth crashing co-op over.
    let _ = writeln!(file, "{stamp} {text}");
}

/// Resource wrapping the shared file handle plus the clock the log's
/// elapsed-time column is measured from — the ECS-facing half of the two
/// writers onto [`shared_file`], for connection-lifecycle lines.
#[derive(Resource)]
struct NetLog {
    file: Arc<Mutex<File>>,
    started: Instant,
    path: PathBuf,
}

impl NetLog {
    fn open(role: &str) -> Option<Self> {
        let path = netlog_path(role)?;
        let file = shared_file(role)?;
        Some(Self {
            file,
            started: log_start_time(),
            path,
        })
    }

    fn line(&mut self, text: &str) {
        write_line(&self.file, self.started, text);
    }
}

/// A `tracing` layer that mirrors every `WARN`/`ERROR` from anywhere in the
/// process onto the same file [`NetLog`] writes connection events to.
///
/// Pass this to `LogPlugin::custom_layer` in `main.rs` — it cannot be added
/// from this plugin's own `build`, because by the time any plugin builds,
/// `LogPlugin` (part of `DefaultPlugins`) has already installed the global
/// subscriber and it is too late to add a layer to it.
///
/// Only warnings and errors, deliberately: `info!` is what the normal
/// console already shows a player mid-session, and mirroring it too would
/// bury the two or three lines that actually matter (a deserialize failure,
/// a rejected handshake) in routine chatter neither a player nor an AI
/// reading the handoff file needs.
pub fn tracing_layer(_app: &mut App) -> Option<BoxedLayer> {
    let file = shared_file(role_from_env_args())?;
    Some(Box::new(NetLogTracingLayer {
        file,
        started: log_start_time(),
    }))
}

struct NetLogTracingLayer {
    file: Arc<Mutex<File>>,
    started: Instant,
}

impl<S> Layer<S> for NetLogTracingLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        *metadata.level() <= Level::WARN
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut message = String::new();
        let mut visitor = MessageVisitor(&mut message);
        event.record(&mut visitor);

        let level = event.metadata().level();
        let target = event.metadata().target();
        write_line(
            &self.file,
            self.started,
            &format!("{level} {target}: {message}"),
        );
    }
}

/// Pulls just the formatted `message` field out of a `tracing` event —
/// everything else (spans, structured fields) is more than a plain-text
/// handoff file needs and would make it noisier to read, not more useful.
struct MessageVisitor<'a>(&'a mut String);

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            use std::fmt::Write;
            let _ = write!(self.0, "{value:?}");
        }
    }
}

/// Adds host- and client-side co-op debug logging.
///
/// Always installed: the log is only a few lines for a quiet singleplayer
/// session and free until something actually happens on the wire.
pub struct NetDiagnosticsPlugin;

impl Plugin for NetDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        // Not added by `RepliconPlugins` unless the `client_diagnostics`
        // feature is on (it isn't, here) — but the counting code in
        // replicon's own receive path runs unconditionally whenever the
        // resource merely exists, so inserting it ourselves is enough to
        // start getting real numbers with no feature/Cargo.toml change.
        app.init_resource::<ClientReplicationStats>();

        app.add_systems(Startup, open_log)
            .add_systems(
                Update,
                (
                    log_server_connections,
                    log_server_account_bindings,
                    log_server_stats_snapshot,
                )
                    .run_if(has_replicon_server),
            )
            .add_systems(
                Update,
                (
                    log_client_state_changes,
                    log_client_connect_failures,
                    log_client_stats_snapshot,
                )
                    .run_if(is_joining),
            )
            .add_systems(OnEnter(AppState::Connecting), log_connect_attempt)
            .add_systems(OnEnter(AppState::Playing), log_entered_playing)
            .add_systems(Last, flush_log);
    }
}

/// Whether this process is running replicon's server half right now, in
/// either transport. Cheaper and simpler than depending on
/// `bevy_replicon_renet`'s `RenetServer` or `bevy_replicon_renet2`'s
/// Steam-flavoured one by name — `ServerState` is the one thing both
/// backends agree on.
fn has_replicon_server(state: Option<Res<State<ServerState>>>) -> bool {
    matches!(state.map(|s| *s.get()), Some(ServerState::Running))
}

fn role_name(mode: Option<&LaunchMode>) -> &'static str {
    match mode {
        Some(LaunchMode::Host) | Some(LaunchMode::HostSteam) => "host",
        Some(LaunchMode::Join(_)) | Some(LaunchMode::JoinSteam(_)) => "client",
        _ => "solo",
    }
}

fn open_log(mut commands: Commands, mode: Option<Res<LaunchMode>>, account: Res<LocalAccount>) {
    let role = role_name(mode.as_deref());
    let Some(mut log) = NetLog::open(role) else {
        return;
    };
    let wall_clock = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    log.line(&format!(
        "=== ChemGame co-op debug log — role: {role}, account: {}, unix time: {wall_clock} ===",
        account.id
    ));
    log.line(
        "columns: +elapsed since this process started. Hand this file (and the \
         other peer's, if you have it) to whoever is diagnosing the session.",
    );
    info!("co-op debug log: {}", log.path.display());
    commands.insert_resource(log);
}

fn flush_log(log: Option<ResMut<NetLog>>) {
    // `File` writes are unbuffered — each `writeln!` is already its own
    // syscall — so this is a formality that costs nothing and closes the gap
    // if that ever changes.
    if let Some(log) = log {
        if let Ok(mut file) = log.file.lock() {
            let _ = file.flush();
        }
    }
}

// --- host side -------------------------------------------------------

fn log_connect_attempt(mode: Res<LaunchMode>, mut log: Option<ResMut<NetLog>>) {
    let Some(log) = log.as_mut() else { return };
    if let LaunchMode::Join(address) = *mode {
        log.line(&format!("connecting: dialling host at {address}"));
    } else if matches!(*mode, LaunchMode::JoinSteam(_)) {
        log.line("connecting: joining a Steam lobby");
    }
}

fn log_entered_playing(mode: Option<Res<LaunchMode>>, mut log: Option<ResMut<NetLog>>) {
    let Some(log) = log.as_mut() else { return };
    log.line(&format!(
        "entered Playing as {}",
        role_name(mode.as_deref())
    ));
}

fn log_server_connections(
    added: Query<Entity, Added<ConnectedClient>>,
    mut removed: RemovedComponents<ConnectedClient>,
    mut log: Option<ResMut<NetLog>>,
) {
    let Some(log) = log.as_mut() else { return };
    for entity in &added {
        log.line(&format!("transport connect: client {entity}"));
    }
    for entity in removed.read() {
        log.line(&format!("transport disconnect: client {entity}"));
    }
}

fn log_server_account_bindings(
    added: Query<(Entity, &ConnectedAccount), Added<ConnectedAccount>>,
    mut log: Option<ResMut<NetLog>>,
) {
    let Some(log) = log.as_mut() else { return };
    for (entity, account) in &added {
        log.line(&format!(
            "account bound: client {entity} -> account {}",
            account.0
        ));
    }
}

fn log_server_stats_snapshot(
    clients: Query<(Entity, &ConnectedClientStats), With<ConnectedClient>>,
    mut since_last: Local<Duration>,
    time: Res<Time>,
    mut log: Option<ResMut<NetLog>>,
) {
    let Some(log) = log.as_mut() else { return };
    *since_last += time.delta();
    if *since_last < SNAPSHOT_INTERVAL {
        return;
    }
    *since_last = Duration::ZERO;
    if clients.is_empty() {
        return;
    }
    let mut line = String::from("stats:");
    for (entity, stats) in &clients {
        line.push_str(&format!(
            " [{entity} rtt={:.0}ms loss={:.1}% up={:.0}B/s down={:.0}B/s]",
            stats.rtt * 1000.0,
            stats.packet_loss * 100.0,
            stats.sent_bps,
            stats.received_bps,
        ));
    }
    log.line(&line);
}

// --- client side -------------------------------------------------------

/// Run condition: this process joined someone else's lab, over either
/// transport. `ClientState` exists on a host and in singleplayer too — it is
/// just always `Disconnected` there — so without this every launch would log
/// a meaningless "client state -> Disconnected" line that has nothing to do
/// with a co-op session at all.
fn is_joining(mode: Option<Res<LaunchMode>>) -> bool {
    matches!(
        mode.as_deref(),
        Some(LaunchMode::Join(_)) | Some(LaunchMode::JoinSteam(_))
    )
}

fn log_client_state_changes(
    state: Option<Res<State<ClientState>>>,
    mut last: Local<Option<ClientState>>,
    mut log: Option<ResMut<NetLog>>,
) {
    let Some(state) = state else { return };
    let current = *state.get();
    if *last == Some(current) {
        return;
    }
    *last = Some(current);
    let Some(log) = log.as_mut() else { return };
    log.line(&format!("client state -> {current:?}"));
}

fn log_client_connect_failures(
    mut failures: MessageReader<ConnectFailed>,
    mut log: Option<ResMut<NetLog>>,
) {
    let Some(log) = log.as_mut() else { return };
    for failure in failures.read() {
        log.line(&format!("connect failed: {}", failure.reason));
    }
}

fn log_client_stats_snapshot(
    stats: Option<Res<ClientStats>>,
    replication: Option<Res<ClientReplicationStats>>,
    mut last_replication: Local<ClientReplicationStats>,
    client_state: Option<Res<State<ClientState>>>,
    mut since_last: Local<Duration>,
    time: Res<Time>,
    mut log: Option<ResMut<NetLog>>,
) {
    let Some(log) = log.as_mut() else { return };
    let Some(true) = client_state.map(|s| *s.get() == ClientState::Connected) else {
        return;
    };
    *since_last += time.delta();
    if *since_last < SNAPSHOT_INTERVAL {
        return;
    }
    *since_last = Duration::ZERO;
    let Some(stats) = stats else { return };
    let mut replication_line = String::new();
    if let Some(replication) = replication {
        replication_line = format!(
            " | replication: {} entities changed, {} components changed, {} despawns, {} messages, {} bytes (deltas)",
            replication.entities_changed.saturating_sub(last_replication.entities_changed),
            replication.components_changed.saturating_sub(last_replication.components_changed),
            replication.despawns.saturating_sub(last_replication.despawns),
            replication.messages.saturating_sub(last_replication.messages),
            replication.bytes.saturating_sub(last_replication.bytes),
        );
        *last_replication = *replication;
    }
    log.line(&format!(
        "stats: rtt={:.0}ms loss={:.1}% up={:.0}B/s down={:.0}B/s{replication_line}",
        stats.rtt * 1000.0,
        stats.packet_loss * 100.0,
        stats.sent_bps,
        stats.received_bps,
    ));
}
