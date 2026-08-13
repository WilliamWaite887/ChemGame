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
//! `GameLobbyJoinRequested` callback regardless of what screen is up, which
//! is the standard Steam convention — or (future work, not built yet) picks
//! a friend from an in-app lobby list. Both hand back a [`LobbyId`], never a
//! `SteamId` or address directly, which is why [`LaunchMode::JoinSteam`]
//! carries a lobby: the host's `SteamId` is looked up from it once joined.
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

use bevy::prelude::*;
use bevy_renet2::steam::{
    AccessPermission, SteamClientTransport, SteamServerConfig, SteamServerTransport,
};
use bevy_replicon::prelude::{ClientState, RepliconChannels};
use bevy_replicon_renet2::renet2::{ConnectionConfig, DisconnectReason, RenetClient, RenetServer};
use bevy_replicon_renet2::RenetChannelsExt;
pub use bevy_steamworks::{Client, LobbyId};
use bevy_steamworks::{CallbackResult, LobbyType, SResult, SteamworksEvent};

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

pub struct SteamPlugin;

impl Plugin for SteamPlugin {
    fn build(&self, app: &mut App) {
        // Only ever added by `main.rs` after `SteamworksPlugin` itself
        // initialised successfully, so `SteamworksEvent` is guaranteed
        // registered by the time these systems run — see main.rs.
        app.add_systems(OnEnter(AppState::Playing), start_hosting_steam.run_if(hosting_steam))
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
            .add_systems(
                Update,
                (
                    // Not gated to any state or launch mode: an overlay invite
                    // can be accepted from the main menu, mid-game, anywhere.
                    handle_lobby_join_requested,
                    poll_lobby_creation.run_if(resource_exists::<PendingLobbyCreation>),
                    poll_lobby_join.run_if(resource_exists::<PendingLobbyJoin>),
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
fn report_join_failure_steam(client: Option<Res<RenetClient>>, mut failed: MessageWriter<ConnectFailed>) {
    let reason = match client.and_then(|client| client.disconnect_reason()) {
        Some(DisconnectReason::Transport) | None => {
            "could not reach the host over Steam — check that they're still hosting".to_string()
        }
        Some(reason) => reason.to_string(),
    };
    failed.write(ConnectFailed { reason });
}

/// Drops whatever half-open Steam connection resources exist, for a
/// cancelled or failed join attempt.
pub(crate) fn abandon_join(commands: &mut Commands) {
    commands.remove_resource::<RenetClient>();
    commands.remove_resource::<SteamClientTransport>();
    commands.remove_resource::<PendingLobbyJoin>();
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

/// Reacts to a Steam overlay invite being accepted, from anywhere in the
/// app — not gated to the menu or to any particular `LaunchMode`, because
/// that is exactly when this callback actually fires.
fn handle_lobby_join_requested(
    mut events: MessageReader<SteamworksEvent>,
    mut mode: ResMut<super::LaunchMode>,
    mut app_state: ResMut<NextState<AppState>>,
) {
    for event in events.read() {
        let SteamworksEvent::CallbackResult(CallbackResult::GameLobbyJoinRequested(request)) =
            event
        else {
            continue;
        };
        info!("accepting a Steam invite to lobby {:?}", request.lobby_steam_id);
        *mode = super::LaunchMode::JoinSteam(request.lobby_steam_id);
        app_state.set(AppState::Connecting);
    }
}

fn start_joining_steam(mode: Res<super::LaunchMode>, client: Res<Client>, mut commands: Commands) {
    let super::LaunchMode::JoinSteam(lobby) = *mode else {
        return;
    };
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
    // `false`: Steam Networking Sockets is not one of the reliable-socket
    // cases (in-memory, WebSocket) `RenetClient::new`'s second argument
    // exists for — same as the direct/LAN netcode transport, renet's own
    // reliable channels do the retransmission work. Matches renet2_steam's
    // own echo client example.
    let renet_client = RenetClient::new(
        ConnectionConfig::from_channels(channels.server_configs(), channels.client_configs()),
        false,
    );
    match SteamClientTransport::new(&client, &host) {
        Ok(transport) => {
            commands.insert_resource(renet_client);
            commands.insert_resource(transport);
            info!("joining the lab over Steam (lobby {lobby:?}, host {host:?})");
        }
        Err(e) => {
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
