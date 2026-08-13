//! Steam Networking Sockets transport + lobby matchmaking.
//!
//! Sits *alongside* the direct/LAN transport in `net/mod.rs` rather than
//! replacing it: `--host`/`--join <addr>` (and the existing in-memory test
//! harness) keep using the original `renet`/netcode path untouched. This
//! module is `renet2` + Steam only, reached through
//! `LaunchMode::HostSteam`/`JoinSteam`.
//!
//! Two transports coexist because they solve different problems for
//! different audiences: the direct path is what makes two-terminal LAN
//! testing possible without Steam running at all, and Steam is what a real
//! player actually uses — no address to type, no port to forward, no NAT
//! problem, because Valve's relay network (SDR) handles all of that behind
//! `SteamServerTransport`/`SteamClientTransport`.
//!
//! `bevy_replicon_renet2` is a locally-patched crate — see
//! `crates/bevy_replicon_renet2_patched/PATCH_NOTES.md` for why.
//!
//! # The join flow
//!
//! There is no join code. A friend either accepts a Steam overlay invite —
//! [`handle_lobby_join_requested`] reacts to Steamworks'
//! `GameLobbyJoinRequested` callback from the menu or the loading screen
//! alike, which is the standard Steam convention — or (future work, not
//! built yet) picks a friend from an in-app lobby list. Both hand back a
//! [`LobbyId`], never a `SteamId` or address directly, which is why
//! [`LaunchMode::JoinSteam`] carries a lobby: the host's `SteamId` is looked
//! up from it once joined.
//!
//! The one invite that is refused rather than served is one accepted
//! *mid-game*: this project has no teardown for a running lab, so joining
//! from inside one would stack a second lab on the first — see
//! [`handle_lobby_join_requested`] for the full reasoning. Steam only fires
//! that callback when the process is already running, though; when it is
//! not, Steam relaunches the executable with `+connect_lobby <id>` and fires
//! nothing at all, which is why `LaunchMode::from_args` parses that argument
//! too. Both halves have to exist or accepting an invite works only some of
//! the time, depending on something the player never thinks about.
//!
//! # Async callbacks vs. the ECS
//!
//! `Matchmaking::create_lobby`/`join_lobby` hand back their result through a
//! `FnOnce` callback that Steam invokes while pumping its own callback queue
//! (`bevy_steamworks`'s `run_steam_callbacks`, in `First`) — not as a Bevy
//! system, so it cannot touch `Commands` directly. Each bridges back to a
//! system via a plain [`std::sync::mpsc`] channel stashed in a resource:
//! the callback sends once and is done, a polling system in `Update` drains
//! it the next time it runs.

use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use std::time::Duration;

use bevy::prelude::*;
use bevy_renet2::steam::{
    AccessPermission, SteamClientTransport, SteamServerConfig, SteamServerTransport,
};
use bevy_replicon::prelude::{ClientState, RepliconChannels};
use bevy_replicon_renet2::renet2::{ConnectionConfig, DisconnectReason, RenetClient, RenetServer};
use bevy_replicon_renet2::{RenetChannelsExt, RepliconRenetPlugins};
pub use bevy_steamworks::{Client, LobbyId};
use bevy_steamworks::{CallbackResult, LobbyType, SResult, SteamId, SteamworksEvent};

use super::{hosting_steam, joining_steam, ConnectFailed};
use crate::AppState;

/// Valve's App ID for this game, from the Steamworks partner dashboard.
///
/// `steamworks::Client::init_app` also accepts this via `steam_appid.txt`
/// next to the executable (the dev-only path — see `.gitignore`), but a
/// named constant is what the rest of this module and `main.rs` build
/// against, so both routes agree on the same number.
pub const STEAM_APP_ID: u32 = 5_103_230;

/// How many chemists a lab can hold — matches `ServerConfig::max_clients`
/// on the direct/LAN transport in `net/mod.rs`.
const MAX_CLIENTS: usize = 4;

/// The lobby currently being hosted, once Steam has actually created it.
///
/// Exists mainly so a future "invite via Steam" button has something to
/// pass to [`invite_friend`] — Steam already surfaces the active lobby to
/// friends through its own overlay/friends-list UI without one, so nothing
/// reads this yet.
#[allow(dead_code)]
#[derive(Resource, Clone, Copy, Debug)]
pub struct HostedLobby(pub LobbyId);

/// Bridges [`create_lobby`](bevy_steamworks::Matchmaking::create_lobby)'s
/// callback back into a system; see the module doc.
///
/// `Receiver` alone is `Send` but not `Sync` (it assumes one consumer), and
/// a `Resource` needs both — the `Mutex` costs nothing real here since only
/// this module's own polling system ever touches it, one call at a time.
#[derive(Resource)]
struct PendingLobbyCreation(Mutex<Receiver<SResult<LobbyId>>>);

/// Bridges [`join_lobby`](bevy_steamworks::Matchmaking::join_lobby)'s
/// callback back into a system; see the module doc.
#[derive(Resource)]
struct PendingLobbyJoin(Mutex<Receiver<Result<LobbyId, ()>>>);

/// The lobby this process has actually joined as a guest.
///
/// Exists so [`abandon_join`] can leave it again. Dropping the transport
/// closes the P2P connection but says nothing to matchmaking, so without
/// this a cancelled join leaves the player counted as a lobby member —
/// permanently eating one of the host's four seats, and advertising them to
/// friends as being in a lab they walked out of.
#[derive(Resource, Clone, Copy, Debug)]
struct JoinedLobby(LobbyId);

/// How many times to dial the host before giving up.
///
/// More than one because of a race with no race-free alternative: the host
/// gates connections on `AccessPermission::InLobby`, which `renet2_steam`
/// checks against `lobby_members` — the host's *locally propagated* view of
/// who is in the lobby. This process calls `connect_p2p` the instant its own
/// `join_lobby` callback returns, which can be before the host's copy of the
/// member list has caught up, and the host then rejects outright and
/// silently (`renet2_steam`'s reject arm logs nothing). Without a retry that
/// reads to the player as "could not reach the host" about a host who is
/// sitting right there hosting — this module's original bug in a new
/// costume.
const JOIN_ATTEMPTS: u8 = 5;

/// Long enough for a lobby membership change to propagate, short enough that
/// five of them still fit inside the patience of someone watching a
/// Connecting screen.
const JOIN_RETRY_DELAY: Duration = Duration::from_millis(500);

/// How quickly a dial has to fail to count as a *refusal* worth retrying.
///
/// A rejected connection comes back in well under a second — the host
/// answers immediately, it just says no. A host who is not there at all
/// instead runs out Steam's own ~10s connect timeout. Retrying that five
/// times would leave someone staring at the Connecting screen for the better
/// part of a minute before being told the obvious, so anything slower than
/// this is reported straight away.
const REFUSAL_WINDOW: Duration = Duration::from_secs(3);

/// A join in flight, kept so a rejected dial can be tried again rather than
/// reported as a dead host. See [`JOIN_ATTEMPTS`].
#[derive(Resource, Debug)]
struct SteamJoinAttempt {
    lobby: LobbyId,
    host: SteamId,
    /// Dials remaining *after* the one currently in flight.
    attempts_left: u8,
    /// When the outstanding dial was opened, so a refusal can be told from a
    /// timeout — see [`REFUSAL_WINDOW`].
    dialed_at: Duration,
    /// When [`retry_steam_join`] should dial again; `None` while a dial is
    /// still outstanding.
    retry_at: Option<Duration>,
}

pub struct SteamPlugin;

impl Plugin for SteamPlugin {
    fn build(&self, app: &mut App) {
        // The renet2 backend itself. Nothing below works without it, and its
        // absence is exactly the bug this module shipped with: `RepliconRenet
        // ServerPlugin` is what installs `bevy_renet2::steam::SteamServer
        // Plugin`, and *that* is the only thing that ever calls
        // `SteamServerTransport::update()` — the only place the listen socket
        // is drained and an incoming connection is `accept()`ed. Without it a
        // host opened a lobby, printed "lab open", and then ignored every
        // knock at the door in complete silence. The client half is just as
        // total: `SteamClientPlugin` is what calls `SteamClientTransport::
        // update()`, which is what calls `RenetClient::set_connected()`, which
        // is what moves replicon's `ClientState` — so `net::finish_joining`
        // and `report_join_failure_steam` below both sat waiting on a
        // transition that could never happen. No connection, and no error
        // either: the guest hung on the Connecting screen forever.
        //
        // Added here rather than beside the direct/LAN group in `NetPlugin`
        // for three reasons. `main.rs` only adds `SteamPlugin` once Steam
        // actually initialised, and these plugins are dead weight otherwise.
        // Every resource they drive (`RenetServer`, `SteamServerTransport`,
        // `RenetClient`, `SteamClientTransport`) is created in this file.
        // And the group's name collides with `bevy_replicon_renet`'s
        // identically-named one — two of those a line apart in `net/mod.rs`
        // is the exact shape of confusion that lost this plugin in the first
        // place. Ordering against `RepliconPlugins` does not matter; the sets
        // it configures need not already exist.
        app.add_plugins(RepliconRenetPlugins)
            // Only ever added by `main.rs` after `SteamworksPlugin` itself
            // initialised successfully, so `SteamworksEvent` is guaranteed
            // registered by the time these systems run — see main.rs.
            .add_systems(Startup, prewarm_relay_network)
            .add_systems(OnEnter(AppState::Playing), start_hosting_steam.run_if(hosting_steam))
            // A joining process stops at `AppState::Connecting` — see its doc
            // comment on `AppState` — instead of running the full simulation
            // against a lobby that may never answer.
            .add_systems(
                OnEnter(AppState::Connecting),
                start_joining_steam.run_if(joining_steam),
            )
            .add_systems(
                OnEnter(ClientState::Disconnected),
                report_join_failure_steam
                    .run_if(in_state(AppState::Connecting).and_then(joining_steam)),
            )
            // The handshake landed, so the retry bookkeeping below has nothing
            // left to watch for.
            .add_systems(
                OnEnter(ClientState::Connected),
                forget_join_attempt.run_if(joining_steam),
            )
            .add_systems(
                Update,
                (
                    // Not gated to any state or launch mode: an overlay invite
                    // can be accepted from the menu or while loading, which is
                    // exactly when this callback actually fires. The one case
                    // it refuses is mid-game — see the system itself.
                    handle_lobby_join_requested,
                    poll_lobby_creation.run_if(resource_exists::<PendingLobbyCreation>),
                    poll_lobby_join.run_if(resource_exists::<PendingLobbyJoin>),
                    retry_steam_join.run_if(resource_exists::<SteamJoinAttempt>),
                ),
            );
    }
}

/// The Steam client's handshake failed or timed out — the Steam-typed
/// counterpart of `net::report_join_failure`. Kept separate because the
/// Steam transport's `RenetClient` (`bevy_replicon_renet2::renet2::
/// RenetClient`) is a different type from the LAN transport's, despite the
/// shared name, and each is gated so only one ever fires off the one shared
/// `ClientState` transition.
///
/// Retries first, though: see [`JOIN_ATTEMPTS`] for the race that makes a
/// single refused dial say nothing about whether the host is actually there.
fn report_join_failure_steam(
    mut commands: Commands,
    client: Option<Res<RenetClient>>,
    time: Res<Time<Real>>,
    attempt: Option<ResMut<SteamJoinAttempt>>,
    mut failed: MessageWriter<ConnectFailed>,
) {
    if let Some(mut attempt) = attempt {
        let refused = time.elapsed().saturating_sub(attempt.dialed_at) < REFUSAL_WINDOW;
        if refused && attempt.attempts_left > 0 {
            attempt.attempts_left -= 1;
            attempt.retry_at = Some(time.elapsed() + JOIN_RETRY_DELAY);
            // Both must go, not just be overwritten later: `resource_added::
            // <RenetClient>` is what puts `ClientState` back into
            // `Connecting`, and inserting over a live resource counts as
            // *changed*, not *added* — so a redial on top of the old client
            // would never re-arm this system for the attempt after it.
            commands.remove_resource::<RenetClient>();
            commands.remove_resource::<SteamClientTransport>();
            warn!(
                "the host refused or dropped the connection; retrying ({} left)",
                attempt.attempts_left + 1
            );
            return;
        }
        commands.remove_resource::<SteamJoinAttempt>();
    }

    let reason = match client.and_then(|client| client.disconnect_reason()) {
        Some(DisconnectReason::Transport) | None => {
            "could not reach the host over Steam — check that they're still hosting".to_string()
        }
        Some(reason) => reason.to_string(),
    };
    failed.write(ConnectFailed { reason });
}

/// Dials the host again once the delay [`report_join_failure_steam`] set has
/// elapsed.
///
/// Redialling from a later frame rather than inside the failure handler is
/// deliberate: the removals above are queued commands, so a same-frame
/// re-insert would land in the same flush and `resource_added` would never
/// see the gap.
fn retry_steam_join(
    mut commands: Commands,
    client: Res<Client>,
    channels: Res<RepliconChannels>,
    time: Res<Time<Real>>,
    existing: Option<Res<RenetClient>>,
    mut attempt: ResMut<SteamJoinAttempt>,
    mut failed: MessageWriter<ConnectFailed>,
) {
    if existing.is_some() {
        return;
    }
    let Some(retry_at) = attempt.retry_at else {
        return;
    };
    if time.elapsed() < retry_at {
        return;
    }
    attempt.retry_at = None;
    attempt.dialed_at = time.elapsed();
    dial_host(
        &mut commands,
        &client,
        &channels,
        attempt.lobby,
        attempt.host,
        &mut failed,
    );
}

/// The handshake finished, so there is nothing left to retry.
fn forget_join_attempt(mut commands: Commands) {
    commands.remove_resource::<SteamJoinAttempt>();
}

/// Drops whatever half-open Steam connection resources exist, for a
/// cancelled or failed join attempt.
pub(crate) fn abandon_join(commands: &mut Commands) {
    commands.remove_resource::<RenetClient>();
    commands.remove_resource::<SteamClientTransport>();
    commands.remove_resource::<PendingLobbyJoin>();
    commands.remove_resource::<SteamJoinAttempt>();
    // Queued rather than done inline because `Commands` has no world access,
    // and the lobby is only known if the join got far enough to enter one.
    // See [`JoinedLobby`] for what leaving actually costs to skip.
    commands.queue(|world: &mut World| {
        let Some(JoinedLobby(lobby)) = world.remove_resource::<JoinedLobby>() else {
            return;
        };
        if let Some(client) = world.get_resource::<Client>() {
            client.matchmaking().leave_lobby(lobby);
            info!("left Steam lobby {lobby:?}");
        }
    });
}

/// Asks Steam to fetch the relay map and start pinging relays now, rather
/// than lazily on the first `connect_p2p`/`create_listen_socket_p2p`.
///
/// Valve's own recommendation, and it matters here more than most: the only
/// P2P connection a player ever opens is the first one, so paying several
/// seconds of relay discovery *inside* the Connecting screen is
/// indistinguishable from the silent hang this module's plugin wiring
/// already caused once.
fn prewarm_relay_network(client: Res<Client>) {
    client.networking_utils().init_relay_network_access();
}

fn start_hosting_steam(client: Res<Client>, mut commands: Commands) {
    let (tx, rx) = std::sync::mpsc::channel();
    client
        .matchmaking()
        .create_lobby(LobbyType::FriendsOnly, MAX_CLIENTS as u32, move |result| {
            // The receiver may already be gone if the app is shutting down;
            // nothing to do about that but drop the result.
            let _ = tx.send(result);
        });
    commands.insert_resource(PendingLobbyCreation(Mutex::new(rx)));
    info!("opening a Steam lobby...");
}

fn poll_lobby_creation(
    mut commands: Commands,
    pending: Res<PendingLobbyCreation>,
    client: Res<Client>,
    channels: Res<RepliconChannels>,
) {
    let Ok(result) = pending.0.lock().unwrap().try_recv() else {
        return;
    };
    commands.remove_resource::<PendingLobbyCreation>();

    let lobby = match result {
        Ok(lobby) => lobby,
        Err(e) => {
            error!("could not create a Steam lobby: {e:?}");
            return;
        }
    };

    let server = RenetServer::new(ConnectionConfig::from_channels(
        channels.server_configs(),
        channels.client_configs(),
    ));
    let config = SteamServerConfig {
        max_clients: MAX_CLIENTS,
        // Only players who are in the lobby can open a connection, so an
        // invite (or a future in-app lobby list) is what actually gates who
        // can join — nobody can dial in from outside it.
        access_permission: AccessPermission::InLobby(lobby),
    };
    match SteamServerTransport::new(&client, config) {
        Ok(transport) => {
            commands.insert_resource(server);
            commands.insert_resource(transport);
            commands.insert_resource(HostedLobby(lobby));
            info!("lab open — Steam lobby {lobby:?}; invite a friend from the Steam overlay");
        }
        Err(e) => error!("could not start the Steam transport: {e:?}"),
    }
}

/// Reacts to a Steam overlay invite being accepted.
///
/// Deliberately not gated by a run condition: this callback fires whenever
/// Steam decides it does, which is why the two cases that cannot be served
/// are handled *inside* and say so out loud, the same way
/// `net::warn_if_steam_unavailable` does. Silently doing nothing is the one
/// response this module has already been punished for.
fn handle_lobby_join_requested(
    mut commands: Commands,
    mut events: MessageReader<SteamworksEvent>,
    mut mode: ResMut<super::LaunchMode>,
    // Optional so the timing rules below can be tested with no Steam at all;
    // in the real app `main.rs` only adds this plugin once Steam is up.
    client: Option<Res<Client>>,
    current: Res<State<AppState>>,
    mut app_state: ResMut<NextState<AppState>>,
) {
    for event in events.read() {
        let SteamworksEvent::CallbackResult(CallbackResult::GameLobbyJoinRequested(request)) =
            event
        else {
            continue;
        };

        // Refused mid-game, because there is no way back out of a running
        // lab: nothing in this project has an `OnExit(AppState::Playing)`,
        // so the machines, glassware, HUD and camera would all still be
        // standing when `OnEnter(Playing)` built the host's replicated set
        // on top of them. Worse, a `HostSteam` process would be holding a
        // `RenetServer` while acquiring a `RenetClient`, which is the
        // replication loop replicon warns about by name.
        if *current.get() == AppState::Playing {
            warn!(
                "ignoring a Steam invite to lobby {:?}: already in a lab — \
                 quit to the menu first",
                request.lobby_steam_id
            );
            continue;
        }

        // A second invite while the first is still in flight re-targets it,
        // so the half-open client and the lobby already joined have to go.
        abandon_join(&mut commands);

        info!("accepting a Steam invite to lobby {:?}", request.lobby_steam_id);
        *mode = super::LaunchMode::JoinSteam(request.lobby_steam_id);

        match *current.get() {
            // Not while the chemistry is still parsing: `chem_data::finish_
            // loading` only runs `in_state(AppState::Loading)`, so moving the
            // state here would strand it and the lab would come up with no
            // `ChemDb` at all. `finish_loading` routes a `JoinSteam` mode
            // straight to `Connecting` itself, so the hand-off just happens a
            // few frames later instead.
            AppState::Loading => {}
            // Already dialling somebody. The state is not changing, so
            // `OnEnter(AppState::Connecting)` — and with it
            // `start_joining_steam` — will not fire again, and the newly
            // accepted lobby would never be dialled. Do it directly instead.
            AppState::Connecting => {
                if let Some(client) = client.as_deref() {
                    begin_lobby_join(&mut commands, client, request.lobby_steam_id);
                }
            }
            _ => app_state.set(AppState::Connecting),
        }
    }
}

fn start_joining_steam(mode: Res<super::LaunchMode>, client: Res<Client>, mut commands: Commands) {
    let super::LaunchMode::JoinSteam(lobby) = *mode else {
        return;
    };
    begin_lobby_join(&mut commands, &client, lobby);
}

/// Asks Steam to put us in the lobby; [`poll_lobby_join`] picks it up from
/// there.
///
/// Split out of [`start_joining_steam`] because
/// [`handle_lobby_join_requested`] has to be able to do this itself: an
/// invite accepted while *already* on the Connecting screen does not change
/// `AppState`, and Bevy does not run enter/exit schedules for a same-state
/// transition — so `OnEnter(AppState::Connecting)` would never fire and the
/// second lobby would never be dialled at all.
fn begin_lobby_join(commands: &mut Commands, client: &Client, lobby: LobbyId) {
    let (tx, rx) = std::sync::mpsc::channel();
    client.matchmaking().join_lobby(lobby, move |result| {
        let _ = tx.send(result);
    });
    commands.insert_resource(PendingLobbyJoin(Mutex::new(rx)));
    info!("joining Steam lobby {lobby:?}...");
}

fn poll_lobby_join(
    mut commands: Commands,
    pending: Res<PendingLobbyJoin>,
    client: Res<Client>,
    channels: Res<RepliconChannels>,
    time: Res<Time<Real>>,
    mut failed: MessageWriter<ConnectFailed>,
) {
    let Ok(result) = pending.0.lock().unwrap().try_recv() else {
        return;
    };
    commands.remove_resource::<PendingLobbyJoin>();

    let lobby = match result {
        Ok(lobby) => lobby,
        Err(()) => {
            error!("could not join the Steam lobby — it may be full or no longer open");
            failed.write(ConnectFailed {
                reason: "could not join the lobby — it may be full or no longer open".to_string(),
            });
            return;
        }
    };

    let host = client.matchmaking().lobby_owner(lobby);
    commands.insert_resource(JoinedLobby(lobby));
    commands.insert_resource(SteamJoinAttempt {
        lobby,
        host,
        attempts_left: JOIN_ATTEMPTS - 1,
        dialed_at: time.elapsed(),
        retry_at: None,
    });
    dial_host(&mut commands, &client, &channels, lobby, host, &mut failed);
}

/// Opens the P2P connection to the host and hands it to renet.
///
/// Split out of [`poll_lobby_join`] because [`retry_steam_join`] has to do
/// exactly the same thing again — see [`JOIN_ATTEMPTS`].
fn dial_host(
    commands: &mut Commands,
    client: &Client,
    channels: &RepliconChannels,
    lobby: LobbyId,
    host: SteamId,
    failed: &mut MessageWriter<ConnectFailed>,
) {
    // `false`: Steam Networking Sockets is not one of the reliable-socket
    // cases (in-memory, WebSocket) `RenetClient::new`'s second argument
    // exists for — same as the direct/LAN netcode transport, renet's own
    // reliable channels do the retransmission work. Matches renet2_steam's
    // own echo client example.
    let renet_client = RenetClient::new(
        ConnectionConfig::from_channels(channels.server_configs(), channels.client_configs()),
        false,
    );
    match SteamClientTransport::new(client, &host) {
        Ok(transport) => {
            commands.insert_resource(renet_client);
            commands.insert_resource(transport);
            info!("joining the lab over Steam (lobby {lobby:?}, host {host:?})");
        }
        Err(e) => {
            // A synchronous failure here is `connect_p2p` refusing to hand
            // back a handle at all, which no amount of retrying fixes —
            // unlike the host-side rejection `report_join_failure_steam`
            // retries through.
            error!("could not reach the host over Steam: {e:?}");
            failed.write(ConnectFailed {
                reason: format!("could not reach the host over Steam: {e:?}"),
            });
        }
    }
}

/// Opens Steam's own invite dialog for the lobby currently being hosted.
///
/// Not wired to any button yet — a friend can already be invited through
/// Steam's own overlay/friends-list UI without this, since `create_lobby`
/// alone is enough for Steam to know the lobby exists. This is here for the
/// in-game "invite" button that's a natural follow-up.
#[allow(dead_code)]
pub fn invite_friend(client: &Client, lobby: LobbyId) {
    client.friends().activate_invite_dialog(lobby);
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use bevy_steamworks::{GameLobbyJoinRequested, SteamId};

    use super::*;
    use crate::net::LaunchMode;

    const LOBBY: u64 = 109_775_241_234_567_890;

    /// The invite handler alone, with no Steam anywhere.
    ///
    /// Possible only because `handle_lobby_join_requested` never touches
    /// `Res<Client>` — the callback has already been turned into a plain
    /// message by the time it gets here, which is exactly what makes the
    /// timing rules below testable at all.
    fn invite_app(state: AppState) -> App {
        let mut app = App::new();
        app.add_plugins(StatesPlugin)
            .init_state::<AppState>()
            .init_resource::<LaunchMode>()
            .add_message::<SteamworksEvent>()
            .add_systems(Update, handle_lobby_join_requested);
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(state);
        // One to apply the state, one for the handler to run in.
        app.update();
        app
    }

    fn accept_invite(app: &mut App) {
        app.world_mut()
            .write_message(SteamworksEvent::CallbackResult(
                CallbackResult::GameLobbyJoinRequested(GameLobbyJoinRequested {
                    lobby_steam_id: LobbyId::from_raw(LOBBY),
                    friend_steam_id: SteamId::from_raw(7),
                }),
            ));
        app.update();
        app.update();
    }

    fn state(app: &App) -> AppState {
        *app.world().resource::<State<AppState>>().get()
    }

    #[test]
    fn an_invite_accepted_while_loading_waits_for_the_chemistry() {
        // Moving to `Connecting` here would strand
        // `chem_data::finish_loading`, which only runs `in_state(Loading)` —
        // the lab would come up with no `ChemDb` at all. The mode is still
        // recorded, because `next_state_after_loading` is what picks the
        // deferred hand-off up.
        let mut app = invite_app(AppState::Loading);
        accept_invite(&mut app);

        assert_eq!(
            *app.world().resource::<LaunchMode>(),
            LaunchMode::JoinSteam(LobbyId::from_raw(LOBBY)),
            "the invite must still be remembered"
        );
        assert_eq!(
            state(&app),
            AppState::Loading,
            "the chemistry must be allowed to finish parsing first"
        );
    }

    #[test]
    fn an_invite_accepted_at_the_menu_starts_connecting() {
        let mut app = invite_app(AppState::MainMenu);
        accept_invite(&mut app);

        assert_eq!(
            *app.world().resource::<LaunchMode>(),
            LaunchMode::JoinSteam(LobbyId::from_raw(LOBBY))
        );
        assert_eq!(state(&app), AppState::Connecting);
    }

    #[test]
    fn a_second_invite_while_connecting_re_targets_the_first() {
        // The dialling itself needs Steam and cannot be checked here, but the
        // re-target must at least be recorded — and the state must stay put,
        // since a same-state transition would not re-run
        // `OnEnter(AppState::Connecting)` anyway. That is exactly why
        // `handle_lobby_join_requested` calls `begin_lobby_join` itself in
        // this case rather than leaning on the state machine.
        let mut app = invite_app(AppState::Connecting);
        app.world_mut()
            .insert_resource(LaunchMode::JoinSteam(LobbyId::from_raw(1)));
        accept_invite(&mut app);

        assert_eq!(
            *app.world().resource::<LaunchMode>(),
            LaunchMode::JoinSteam(LobbyId::from_raw(LOBBY)),
            "the newer invite must win"
        );
        assert_eq!(state(&app), AppState::Connecting);
    }

    #[test]
    fn an_invite_accepted_mid_game_is_refused() {
        // Nothing here has an `OnExit(AppState::Playing)`, so accepting would
        // stack the host's replicated lab on top of the one already standing;
        // and a host would hold a `RenetServer` and a `RenetClient` at once,
        // which is replicon's replication loop by name.
        let mut app = invite_app(AppState::Playing);
        app.world_mut()
            .insert_resource(LaunchMode::HostSteam);
        accept_invite(&mut app);

        assert_eq!(
            *app.world().resource::<LaunchMode>(),
            LaunchMode::HostSteam,
            "a host must not be turned into a guest of someone else's lab"
        );
        assert_eq!(state(&app), AppState::Playing);
    }
}
