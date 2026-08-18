//! Co-op networking.
//!
//! Follows replicon's recommended shape rather than running two worlds: there
//! is one set of game systems, and authority is decided by a run condition.
//! `ClientState::Disconnected` reads as "not a remote client", which is true
//! for both a dedicated server *and* singleplayer — so the singleplayer game
//! is the same code path with nobody connected, not a special case.
//!
//! What is replicated is the physical lab: containers and what is in them,
//! machine occupancy, and the crew at the counter. That is what two chemists
//! need to see the same version of.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::SystemTime;

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_replicon_renet::netcode::{
    ClientAuthentication, NetcodeClientTransport, NetcodeServerTransport, ServerAuthentication,
    ServerConfig,
};
use bevy_replicon_renet::renet::{ConnectionConfig, DisconnectReason};
// `RenetServer`/`RenetClient` come from bevy_renet's re-export, not raw renet:
// only the bevy wrappers are resources.
use bevy_replicon_renet::{RenetChannelsExt, RenetClient, RenetServer, RepliconRenetPlugins};

use crate::body::{Bloodstream, Body};
use crate::chem_world::ChemicalPuddle;
use crate::containers::{Container, HeldBy, InSlot, InSlotB, Stored};
use crate::crew::{AtCounter, CrewMember, NeedsMedicalEvacuation};
use crate::door::{Corroded, Door};
use crate::hazards::{ActiveHazard, SmokeCloud, SmokeOwner, SmokePayload};
use crate::interaction::Interactable;
use crate::machines::{AgitationRun, Buffer, DispenseAmount, Hopper, Machine, Thermostat};
use crate::orders::{CounterOrder, CrisisOrder, DevelopmentOrder, Order};
use crate::player::Player;
use crate::produce::Produce;
use crate::rogue_security::Deterrent;
use crate::showdown::{Assailant, Breach};
use crate::AppState;

pub mod steam;

/// Arbitrary; both ends must agree. The low byte is an explicit schema
/// revision so replicated chemistry additions cannot accidentally keep an old
/// handshake compatible.
const PROTOCOL_REVISION: u64 = 5;
const PROTOCOL_ID: u64 = 0x43_48_45_4d_00_00_00_00 | PROTOCOL_REVISION;
const DEFAULT_PORT: u16 = 5327;
/// The host is a local chemist, leaving three network seats in a four-person
/// lab. Both direct and Steam transports use this same value.
const MAX_REMOTE_CLIENTS: usize = 3;

/// How this process was launched.
///
/// `Host`/`Join` dial an address directly over the LAN/dev transport
/// (`start_hosting`/`start_joining` below); `HostSteam`/`JoinSteam` go
/// through Steam Networking Sockets instead (`steam::start_hosting_steam`/
/// `steam::start_joining_steam`) — see `steam`'s module doc for why the two
/// transports coexist rather than one replacing the other.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LaunchMode {
    /// One chemist, no sockets. Server logic runs locally.
    #[default]
    Singleplayer,
    /// Listen server: plays the game and accepts up to three more chemists.
    Host,
    /// Joins a lab hosted elsewhere.
    Join(SocketAddr),
    /// Listen server, reached through a Steam lobby instead of an address.
    HostSteam,
    /// Joins a lab hosted over Steam. Carries the lobby, not the host's
    /// `SteamId` directly, because that is what both ways of getting here —
    /// accepting an overlay invite, or picking a friend from a lobby list —
    /// actually hand back; the host's id is looked up from the lobby once
    /// joined.
    JoinSteam(steam::LobbyId),
}

impl LaunchMode {
    /// Reads `--solo`, `--host`, `--join [addr]`, `--host-steam` or Steam's
    /// own `+connect_lobby <id>` from this process's command line.
    ///
    /// `None` means the command line said nothing, which is the signal to show
    /// the menu instead of guessing.
    ///
    /// No `--join-steam` flag of our own: there is no address or code to type
    /// for the Steam path, only a lobby id, which is not something a person
    /// reads out loud the way an IP is — see `steam`'s module doc.
    /// `+connect_lobby` is not that flag under another name; it is Steam
    /// talking to us, never a human.
    pub fn from_args() -> Option<Self> {
        Self::parse_args(std::env::args().skip(1))
    }

    /// The testable half of [`from_args`].
    ///
    /// Split for the same reason [`parse_literal_address`] was split out of
    /// [`parse_address`]: `std::env::args()` is process-global, so anything
    /// that reads it directly cannot be pinned by a test.
    pub fn parse_args(args: impl IntoIterator<Item = String>) -> Option<Self> {
        let args: Vec<String> = args.into_iter().collect();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--solo" => return Some(LaunchMode::Singleplayer),
                "--host" => return Some(LaunchMode::Host),
                "--host-steam" => return Some(LaunchMode::HostSteam),
                "--join" => {
                    let address = iter
                        .next()
                        .and_then(|value| parse_address(value))
                        .unwrap_or_else(loopback_address);
                    return Some(LaunchMode::Join(address));
                }
                // Steam's argument, not ours — no leading `--`, and never
                // typed by hand. Steam appends this when a friend accepts an
                // overlay invite (or hits Join in the friends list) while the
                // game is *not* already running; when it is running there is
                // no relaunch and `steam::handle_lobby_join_requested` gets a
                // callback instead. Both routes have to exist, or accepting an
                // invite works only when the game happens to be open — which
                // is not something the player is thinking about when they
                // click it.
                "+connect_lobby" => {
                    let Some(lobby) = iter.next().and_then(|value| value.parse::<u64>().ok())
                    else {
                        // Falling through to the menu beats dialling lobby 0.
                        warn!("ignoring a malformed +connect_lobby argument");
                        return None;
                    };
                    return Some(LaunchMode::JoinSteam(steam::LobbyId::from_raw(lobby)));
                }
                _ => {}
            }
        }
        None
    }
}

/// Present when the command line already chose, so the menu is skipped.
///
/// Keeping the flags is what makes testing co-op bearable: two windows from one
/// terminal, no clicking through a menu twice on every rebuild.
#[derive(Resource)]
pub struct LaunchedFromArgs;

/// Applies whatever the command line asked for, before any plugin builds.
///
/// Also picks the save, because `--host` and `--solo` have no menu to pick one
/// in and would otherwise start a brand new career on every launch. A `--join`
/// or a `+connect_lobby` gets no slot: the guest reads the host's notebook and
/// career.
pub fn apply_command_line(app: &mut App) {
    let Some(mode) = LaunchMode::from_args() else {
        return;
    };
    app.insert_resource(mode).insert_resource(LaunchedFromArgs);
    if !matches!(mode, LaunchMode::Join(_) | LaunchMode::JoinSteam(_)) {
        crate::saves::migrate_legacy_saves();
        app.insert_resource(crate::saves::SaveSlot::default_slot());
    }
}

/// The literal-syntax subset of [`parse_address`]: an IP, or an IP and port.
/// No DNS — cheap enough to run on every keystroke, which is exactly why
/// `menu::hint_for` uses this and not `parse_address`. A hostname typed
/// character by character would otherwise trigger a blocking resolve on
/// almost every partial string in between.
pub fn parse_literal_address(value: &str) -> Option<SocketAddr> {
    value
        .parse::<SocketAddr>()
        .ok()
        // Bare IP: fall back to the default port rather than refusing.
        .or_else(|| {
            value
                .parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
        })
}

/// Parses what someone reads out loud: an IP, an IP and port, or a hostname.
///
/// Only called once per deliberate action (the Connect click, or
/// `LaunchMode::from_args`) — never per keystroke, since the DNS fallback
/// below blocks. See [`parse_literal_address`] for the cheap subset that is
/// safe to run on every keystroke.
pub fn parse_address(value: &str) -> Option<SocketAddr> {
    parse_literal_address(value)
        // A name, with or without a port. Worth resolving because on a home
        // network the other machine usually has one and its address is a
        // DHCP lease that changes.
        .or_else(|| resolve(value))
        .or_else(|| resolve(&format!("{value}:{DEFAULT_PORT}")))
}

/// First IPv4 address a name resolves to.
///
/// IPv4 only, deliberately: the host binds an IPv4 socket, and netcode will
/// not match a v6 client address against it, which fails as a silent timeout
/// rather than an error.
fn resolve(value: &str) -> Option<SocketAddr> {
    value
        .to_socket_addrs()
        .ok()?
        .find(|address| address.is_ipv4())
}

fn loopback_address() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)
}

/// The address other machines on this network should dial.
///
/// Found by asking the OS which interface it would route out of. This costs
/// nothing and sends no traffic — connecting a UDP socket only fixes the peer
/// address locally — and it beats enumerating interfaces, which picks the
/// wrong one as soon as there is a VPN or a container bridge in the list.
fn lan_address() -> Option<SocketAddr> {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // Any routable address will do; nothing is sent to it.
    probe.connect((Ipv4Addr::new(203, 0, 113, 1), 9)).ok()?;
    let address = probe.local_addr().ok()?;
    Some(SocketAddr::new(address.ip(), DEFAULT_PORT))
}

/// Run condition: this process owns the simulation.
///
/// True for singleplayer, and for a host — which is why those two are one code
/// path rather than a special case. False only when launched with `--join`.
///
/// Deliberately taken from the launch mode rather than replicon's
/// `ClientState`. A joining client reads `Disconnected` for the second or two
/// it spends handshaking, and the lab is built on entering `Playing`, so a
/// `ClientState` test loses that race whenever assets finish loading first:
/// the client builds its own glassware and machines, then receives the
/// server's on top of them.
/// Absent resource reads as authority: that is a headless test driving the
/// simulation directly, which is exactly the singleplayer case.
pub fn is_authority(mode: Option<Res<LaunchMode>>) -> bool {
    !matches!(
        mode.as_deref(),
        Some(LaunchMode::Join(_)) | Some(LaunchMode::JoinSteam(_))
    )
}

/// Run condition: this process is opening its lab to others over the
/// direct/LAN transport specifically — `hosting_steam` below is the Steam
/// equivalent, deliberately kept separate so each only ever drives its own
/// transport's startup.
fn hosting(mode: Option<Res<LaunchMode>>) -> bool {
    matches!(mode.as_deref(), Some(LaunchMode::Host))
}

/// Run condition: this process is dialling someone else's lab directly.
fn joining(mode: Option<Res<LaunchMode>>) -> bool {
    matches!(mode.as_deref(), Some(LaunchMode::Join(_)))
}

/// Run condition: this process is opening its lab to others over Steam.
fn hosting_steam(mode: Option<Res<LaunchMode>>) -> bool {
    matches!(mode.as_deref(), Some(LaunchMode::HostSteam))
}

/// Run condition: this process is joining a lab over Steam.
fn joining_steam(mode: Option<Res<LaunchMode>>) -> bool {
    matches!(mode.as_deref(), Some(LaunchMode::JoinSteam(_)))
}

/// Says why nothing happened, for the case `hosting_steam`/`joining_steam`
/// wanted to start a Steam transport but Steam never actually came up (not
/// running, `steam_appid.txt`/App ID mismatch, SDK missing). Without this,
/// `LaunchMode::HostSteam`/`JoinSteam` under a failed Steam init look
/// exactly like every other silent co-op bug this project has hit before:
/// the menu accepts the click, the state moves to `Playing`, and nothing
/// else ever happens.
fn warn_if_steam_unavailable(
    mode: Option<Res<LaunchMode>>,
    client: Option<Res<steam::Client>>,
    mut failed: MessageWriter<ConnectFailed>,
) {
    if client.is_some() {
        return;
    }
    match mode.as_deref() {
        Some(LaunchMode::HostSteam) => {
            error!("Steam is not available, so a Steam lobby cannot be opened. Is Steam running?")
        }
        Some(LaunchMode::JoinSteam(_)) => {
            error!(
                "Steam is not available, so this Steam lobby cannot be joined. Is Steam running?"
            );
            failed.write(ConnectFailed {
                reason: "Steam is not available. Is Steam running?".to_string(),
            });
        }
        _ => {}
    }
}

/// A join attempt that did not end in `ClientState::Connected` — read by
/// `menu` while `AppState::Connecting`. Without this, a failed or timed-out
/// handshake has no way to reach the player: nothing else in this module
/// watches for it, and the client would otherwise sit in `Connecting`
/// forever while the socket quietly keeps retrying underneath.
#[derive(Message, Debug, Clone)]
pub struct ConnectFailed {
    pub reason: String,
}

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        // Only if the command line has not already inserted one: the mode is
        // now a menu decision, and singleplayer is the right thing to assume
        // for anything that never reaches the menu, such as a test.
        //
        // `RepliconRenetPlugins` here is the direct/LAN backend *only*. The
        // Steam backend has its own, identically-named group
        // (`bevy_replicon_renet2::RepliconRenetPlugins`), added by
        // `steam::SteamPlugin` — forgetting it is precisely the bug that let
        // a Steam host open a lobby and then ignore every incoming
        // connection without a word.
        app.init_resource::<LaunchMode>()
            .add_plugins((RepliconPlugins, RepliconRenetPlugins))
            .add_message::<ConnectFailed>();

        register_replication(app);
        app.add_systems(Update, resync_on_join.run_if(is_authority))
            // Backend-agnostic: whichever group above is live sets
            // `ServerState`, so this line appearing is proof the transport is
            // actually running. Its absence is what a silent host looks like.
            .add_systems(OnEnter(ServerState::Running), announce_serving)
            // Hangs up on the way out to the menu. Paired with
            // `crate::session`, which unwinds everything the session put in
            // the world; this is the half that unwinds what it put on a
            // socket.
            .add_systems(OnExit(AppState::Playing), close_session_transport)
            // A host or singleplayer game owns the simulation immediately —
            // there is nothing to wait for.
            .add_systems(
                OnEnter(AppState::Playing),
                (
                    start_hosting.run_if(hosting),
                    warn_if_steam_unavailable.run_if(hosting_steam),
                ),
            )
            // A joining process stops here — see `AppState::Connecting`'s doc
            // comment — instead of building the lab and running the full
            // simulation against a socket that may never answer.
            .add_systems(
                OnEnter(AppState::Connecting),
                (
                    start_joining.run_if(joining),
                    warn_if_steam_unavailable.run_if(joining_steam),
                ),
            )
            // `ClientState::Connected` is produced identically by both
            // transports (see `steam`'s module doc), so one system here
            // covers a LAN join and a Steam join alike.
            .add_systems(
                OnEnter(ClientState::Connected),
                finish_joining.run_if(in_state(AppState::Connecting)),
            )
            .add_systems(
                OnEnter(ClientState::Disconnected),
                report_join_failure.run_if(in_state(AppState::Connecting).and_then(joining)),
            );
    }
}

/// The client's handshake finished — hand off from the waiting room to the
/// lab.
fn finish_joining(mut app_state: ResMut<NextState<AppState>>) {
    app_state.set(AppState::Playing);
}

/// Says out loud that the transport is up and listening.
///
/// Worth a line of its own because the hardest part of every co-op bug this
/// project has hit was a host that printed *nothing*: "lab open" only means
/// a socket was bound or a lobby was created, which both happened perfectly
/// well while the backend that answers connections was missing entirely.
/// `ServerState::Running` is set by whichever backend is actually installed,
/// so this is the line that distinguishes the two.
fn announce_serving() {
    info!("the lab is accepting connections");
}

/// The direct/LAN client's handshake failed or timed out — surfaces it
/// instead of leaving `Connecting` with nothing watching.
///
/// Reads this module's own `RenetClient` specifically: the Steam transport's
/// client is a different type of the same name
/// (`bevy_replicon_renet2::renet2::RenetClient`), so `steam` has its own
/// copy of this system, gated on `joining_steam` instead of `joining` so the
/// two never both fire off the one shared `ClientState` transition.
fn report_join_failure(client: Option<Res<RenetClient>>, mut failed: MessageWriter<ConnectFailed>) {
    let reason = match client.and_then(|client| client.disconnect_reason()) {
        // The ordinary case: the netcode handshake's own ~15s timeout gave
        // up with no reply, which renet reports as a transport-level
        // disconnect rather than anything more specific.
        Some(DisconnectReason::Transport) | None => {
            "could not reach the host — check the address, and that it's \
             still open"
                .to_string()
        }
        Some(reason) => reason.to_string(),
    };
    failed.write(ConnectFailed { reason });
}

/// Drops whatever half-open connection resources exist for `mode`, so a
/// cancelled or failed join attempt does not leave a socket or transport
/// behind for the next one to trip over.
pub fn abandon_connection_attempt(commands: &mut Commands, mode: LaunchMode) {
    match mode {
        LaunchMode::Join(_) => {
            commands.remove_resource::<RenetClient>();
            commands.remove_resource::<NetcodeClientTransport>();
        }
        LaunchMode::JoinSteam(_) => steam::abandon_join(commands),
        _ => {}
    }
}

/// Closes the session's transport on the way out of the lab.
///
/// Quitting to the menu has to hang up as well as tear the world down: a host
/// that kept listening would have guests still connected to a lab that no
/// longer exists, and a client that kept its socket open would be dialled into
/// a session it has left. Either one leaves the *next* launch fighting a
/// half-open connection for a port, and neither says anything about it.
///
/// Singleplayer holds none of these resources, so this is a no-op there.
fn close_session_transport(mut commands: Commands, mode: Option<Res<LaunchMode>>) {
    let Some(mode) = mode else {
        return;
    };
    match *mode {
        LaunchMode::Singleplayer => {}
        LaunchMode::Host => {
            commands.remove_resource::<RenetServer>();
            commands.remove_resource::<NetcodeServerTransport>();
        }
        LaunchMode::Join(_) => {
            commands.remove_resource::<RenetClient>();
            commands.remove_resource::<NetcodeClientTransport>();
        }
        LaunchMode::HostSteam => steam::close_host(&mut commands),
        LaunchMode::JoinSteam(_) => steam::abandon_join(&mut commands),
    }
    // Back to the default the menu chooses from again, so a Steam co-op
    // session followed by a solo one does not run solo through the Steam path.
    commands.insert_resource(LaunchMode::default());
}

/// Pushes the shared resources again whenever a chemist joins.
///
/// `Knowledge`, `Shift` and `RadioLog` are resources, which replicon does not
/// replicate, so each is broadcast as a whole snapshot when it changes. That
/// is right in steady state and wrong at the moment of joining: a client that
/// arrives between two discoveries would keep running on whatever its *own*
/// `save.ron` held until the host next changed something. Two chemists reading
/// different books is the worst kind of this bug, because nothing about it
/// looks broken — the second player simply cannot make a recipe the first one
/// can see, and neither of them can tell why.
///
/// Marking them changed rather than sending directly reuses the existing
/// broadcasts, so there is one place that knows how to serialise each.
fn resync_on_join(
    joined: Query<(), Added<AuthorizedClient>>,
    knowledge: Option<ResMut<crate::knowledge::Knowledge>>,
    shift: Option<ResMut<crate::orders::Shift>>,
    radio: Option<ResMut<crate::radio::RadioLog>>,
) {
    if joined.is_empty() {
        return;
    }
    // `AuthorizedClient` only ever appears on a real server, so singleplayer
    // stays quiet. See `announce_serving` for why the host saying anything at
    // all matters this much.
    info!("a chemist joined the lab");
    // A client can authorise before the lab has finished loading, so none of
    // these are guaranteed to exist yet.
    if let Some(mut knowledge) = knowledge {
        knowledge.set_changed();
    }
    if let Some(mut shift) = shift {
        shift.set_changed();
    }
    if let Some(mut radio) = radio {
        radio.set_changed();
    }
}

/// Declares what crosses the wire.
///
/// Only shared lab state is listed. Anything a client can derive for itself —
/// meshes, materials, HUD, the camera — stays local, because replicating
/// presentation would cost bandwidth to tell a client something it already
/// knows.
fn register_replication(app: &mut App) {
    app.replicate::<Transform>()
        .replicate::<Container>()
        .replicate::<HeldBy>()
        .replicate::<InSlot>()
        // The Mixing Chamber's second beaker slot. Same reasoning as `InSlot`
        // itself: both chemists have to see which beaker is in which slot.
        .replicate::<InSlotB>()
        // What is shut in the locker, and which one. Both ends need it: the
        // guest's copy is what hides a stored beaker and what fills the panel
        // when they open the locker themselves.
        .replicate::<Stored>()
        .replicate::<Machine>()
        .replicate::<Buffer>()
        // Carries the Mixing Chamber's otherwise non-derivable preparation
        // provenance plus its visible batch clock.
        .replicate::<AgitationRun>()
        .replicate::<Hopper>()
        .replicate::<DispenseAmount>()
        // `Order` used to carry a `Timer`, which is not `Serialize` — that is
        // the whole reason it was never on this list, and a joining client's
        // order queue sat empty. `patience`/`waited` are plain `f32`, so it
        // can finally ride the wire like everything else here.
        .replicate::<Order>()
        .replicate::<DevelopmentOrder>()
        // The one bit of the (deliberately server-side) `CrewRoute::phase` a
        // client's order queue HUD needs — see `crew::AtCounter`'s own doc
        // comment. Without this a joining client's queue populated with
        // `Order`/`CrewMember` but could never tell arriving from waiting, and
        // `update_order_queue`'s query for a `CrewRoute` that no client entity
        // ever carries left the whole panel permanently blank.
        .replicate::<AtCounter>()
        // Unlike `IllicitOrder`, a crisis has nothing to hide — see its own
        // doc comment. Replicated so `crisis::pulse_alert_lighting` reads the
        // same "is one active" answer on every peer with no sync message.
        .replicate::<CrisisOrder>()
        // Same reasoning as `CrisisOrder`: a department's countermeasure
        // request is public by design, and both peers need to see the same
        // one open so either chemist can fill it.
        .replicate::<CounterOrder>()
        .replicate::<Produce>()
        .replicate::<CrewMember>()
        // Interaction labels are gameplay affordances, not decoration: a
        // guest cannot hand over an order or evacuate an incapacitated
        // resident if their focus ray is unable to recognise that entity as
        // usable. Evacuation's marker selects the dedicated request path.
        .replicate::<Interactable>()
        .replicate::<NeedsMedicalEvacuation>()
        // The marker itself, so a client can tell a chemist from any other
        // replicated entity and give them a body to look at.
        .replicate::<Player>()
        // What the chamber is set to. Without this the second chemist to walk
        // up cannot see it is running and cooks the batch.
        .replicate::<Thermostat>()
        // Whether a door is open. `door::decide_door_state` is the only
        // writer; every peer's leaves, `Solid` and `WalkableAreas` bridge
        // follow this one bool.
        .replicate::<Door>()
        .replicate::<Corroded>()
        // Clouds are entities, so replication carries them and no snapshot
        // message is needed — which is exactly why they are entities.
        .replicate::<SmokeCloud>()
        .replicate::<SmokePayload>()
        .replicate::<SmokeOwner>()
        .replicate::<ActiveHazard>()
        // Floor chemistry is shared state: composition, reach, remaining
        // lifetime, attribution and ignition must agree for every player.
        .replicate::<ChemicalPuddle>()
        // A chemist's condition is shared lab state too: the whole point of
        // the second pair of hands is being able to see that the first pair is
        // in trouble.
        .replicate::<Body>()
        .replicate::<Bloodstream>()
        // Rogue Security's reward — a pickable prop, shared lab state like
        // any other, so both peers see it appear on the counter.
        .replicate::<Deterrent>()
        // The showdown, both forms. A breach needs its own marker because it
        // is not crew and `dress_crew` cannot draw it; the assailant needs one
        // so a guest is not watching an ordinary-looking crew member walk
        // through the lab taking chemical damage for no visible reason.
        .replicate::<Breach>()
        .replicate::<Assailant>();
}

fn start_hosting(mut commands: Commands, channels: Res<RepliconChannels>) {
    // Bind every interface, not loopback. Binding 127.0.0.1 accepts a second
    // window on this machine and nothing else on the network, which is the
    // failure that looks exactly like a firewall problem.
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT);
    let Ok(socket) = UdpSocket::bind(bind) else {
        error!("could not bind {bind}; staying singleplayer");
        return;
    };
    let Ok(since_epoch) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        return;
    };

    // Netcode matches the address the client dialled against this list, so
    // both routes to this host have to be on it: the LAN address for the
    // other machine, loopback for a second window here.
    let lan = lan_address();
    let mut public = Vec::new();
    public.extend(lan);
    public.push(loopback_address());

    let server = RenetServer::new(ConnectionConfig {
        server_channels_config: channels.server_configs(),
        client_channels_config: channels.client_configs(),
        ..Default::default()
    });
    let config = ServerConfig {
        current_time: since_epoch,
        // The host is the first chemist; three remote clients make the
        // supported four-person lab.
        max_clients: MAX_REMOTE_CLIENTS,
        protocol_id: PROTOCOL_ID,
        public_addresses: public,
        authentication: ServerAuthentication::Unsecure,
    };
    let Ok(transport) = NetcodeServerTransport::new(config, socket) else {
        error!("could not start the netcode transport");
        return;
    };

    commands.insert_resource(server);
    commands.insert_resource(transport);

    match lan {
        Some(address) => info!("lab open — the other chemist runs: chemgame --join {address}"),
        // Not fatal: a second window on this machine still works, and this is
        // the normal reading when there is no network at all.
        None => warn!(
            "lab open on {DEFAULT_PORT}, but no network address was found — \
             only this machine can join, via --join 127.0.0.1:{DEFAULT_PORT}"
        ),
    }
}

fn start_joining(
    mut commands: Commands,
    channels: Res<RepliconChannels>,
    mode: Res<LaunchMode>,
    mut failed: MessageWriter<ConnectFailed>,
) {
    let LaunchMode::Join(address) = *mode else {
        return;
    };
    let Ok(since_epoch) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        failed.write(ConnectFailed {
            reason: "the system clock is set before 1970".to_string(),
        });
        return;
    };

    let client = RenetClient::new(ConnectionConfig {
        server_channels_config: channels.server_configs(),
        client_channels_config: channels.client_configs(),
        ..Default::default()
    });
    // Any local port will do; the id only needs to be unique per connection.
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        error!("could not open a local socket to join {address}");
        failed.write(ConnectFailed {
            reason: "could not open a local network socket".to_string(),
        });
        return;
    };
    let authentication = ClientAuthentication::Unsecure {
        client_id: since_epoch.as_millis() as u64,
        protocol_id: PROTOCOL_ID,
        server_addr: address,
        user_data: None,
    };
    let Ok(transport) = NetcodeClientTransport::new(since_epoch, authentication, socket) else {
        error!("could not reach {address}");
        failed.write(ConnectFailed {
            reason: format!("could not reach {address}"),
        });
        return;
    };

    commands.insert_resource(client);
    commands.insert_resource(transport);
    // Offered back as the prefilled address next time. Remembered here rather
    // than when it was typed so only one that got as far as a socket is kept,
    // and stored resolved so the port is visible in the field.
    crate::saves::remember_host(&address.to_string());
    info!("joining the lab at {address}");
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use bevy_replicon::test_app::ServerTestAppExt;
    use chem_sim::{Solution, Units};

    use super::*;
    use crate::containers::ContainerKind;

    /// Two apps wired together in memory, with no sockets and no renderer.
    ///
    /// `dress` decides whether each end can also *build* what it is sent.
    /// Plugins have to go on before `finish`, which is why this is one builder
    /// rather than a pair that gets extended afterwards.
    fn connected_pair_inner(dress: bool) -> (App, App) {
        let mut server = App::new();
        let mut client = App::new();
        for app in [&mut server, &mut client] {
            app.add_plugins((
                MinimalPlugins,
                StatesPlugin,
                RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
            ));
            register_replication(app);
            if dress {
                app.add_plugins(AssetPlugin::default())
                    .init_asset::<Mesh>()
                    .init_asset::<StandardMaterial>()
                    .init_asset::<WorldAsset>()
                    .add_systems(
                        Startup,
                        (
                            crate::lab::load_machine_assets,
                            crate::containers::load_container_assets,
                            crate::player::load_chemist_assets,
                        ),
                    )
                    .add_systems(
                        Update,
                        (
                            crate::lab::dress_machines,
                            crate::containers::dress_containers,
                            crate::player::dress_chemists,
                        ),
                    );
            }
            app.finish();
        }
        server.connect_client(&mut client);
        (server, client)
    }

    fn connected_pair() -> (App, App) {
        connected_pair_inner(false)
    }

    /// A pair where each end can build what it is sent.
    ///
    /// This is the two-windows check made automatic: the server spawns lab
    /// state carrying no presentation at all, and the client has to arrive at
    /// a room it can see and use from the replicated components alone.
    fn connected_pair_with_visuals() -> (App, App) {
        connected_pair_inner(true)
    }

    /// Runs enough frames for a spawn to reach the client and be built out.
    fn settle(server: &mut App, client: &mut App) {
        for _ in 0..3 {
            server.update();
            server.exchange_with_client(client);
            client.update();
        }
    }

    #[test]
    fn a_joining_client_ends_up_with_a_lab_it_can_see_and_use() {
        // The bug this pins is the one that made co-op look finished and play
        // as an empty room: everything the server spawned got its mesh at
        // spawn time, and meshes do not replicate. The client connected fine,
        // was assigned a chemist, and stood in a lab with no machines, no
        // glassware and nobody else in it.
        let (mut server, mut client) = connected_pair_with_visuals();

        let machine = server
            .world_mut()
            .spawn((
                Replicated,
                Machine::new(crate::machines::MachineKind::ChemMaster5000),
            ))
            .id();
        let beaker = server
            .world_mut()
            .spawn((Replicated, Container::new(ContainerKind::LargeBeaker)))
            .id();
        let chemist = server
            .world_mut()
            .spawn((Replicated, Player, Transform::default()))
            .id();

        settle(&mut server, &mut client);

        // Each has to be found by what it *is*, because the client's entity
        // ids are its own.
        let mut machines = client.world_mut().query_filtered::<Entity, With<Machine>>();
        let machines: Vec<Entity> = machines.iter(client.world()).collect();
        assert_eq!(
            machines.len(),
            1,
            "the dispenser must arrive: {machine} on the server"
        );
        assert!(
            client
                .world()
                .get::<Children>(machines[0])
                .is_some_and(|children| children
                    .iter()
                    .any(|child| client.world().get::<WorldAssetRoot>(child).is_some())),
            "the replicated dispenser must receive its authored GLB visual",
        );

        let mut beakers = client.world_mut().query::<(&Container, &Mesh3d)>();
        assert_eq!(
            beakers.iter(client.world()).count(),
            1,
            "the beaker must arrive and be built: {beaker} on the server"
        );

        let mut chemists = client.world_mut().query::<(&Player, &Visibility)>();
        assert_eq!(
            chemists.iter(client.world()).count(),
            1,
            "the other chemist must arrive and be drawable: {chemist} on the server"
        );

        let mut parts = client.world_mut().query::<&crate::player::ChemistBody>();
        assert_eq!(
            parts.iter(client.world()).count(),
            2,
            "a body and a head, or the other chemist is an invisible pair of hands"
        );
    }

    #[test]
    fn a_beakers_contents_survive_the_wire() {
        // The real risk in replicating chemistry is the solution itself:
        // fixed-point quantities, interned reagent ids and capacity all have
        // to arrive intact, or the two chemists are dosing off different
        // numbers without either of them knowing.
        let (mut server, mut client) = connected_pair();

        let mut solution = Solution::new(Units::whole(100));
        let _ = solution.add(chem_sim::ReagentId(3), Units::from_f64(15.25));
        let _ = solution.add(chem_sim::ReagentId(7), Units::whole(30));
        server.world_mut().spawn((
            Replicated,
            Container {
                kind: ContainerKind::LargeBeaker,
                solution: solution.clone(),
            },
        ));

        server.update();
        server.exchange_with_client(&mut client);
        client.update();
        server.exchange_with_client(&mut client);
        client.update();

        let mut query = client.world_mut().query::<&Container>();
        let received: Vec<&Container> = query.iter(client.world()).collect();
        assert_eq!(received.len(), 1, "client should see the beaker");

        let received = received[0];
        assert_eq!(received.kind, ContainerKind::LargeBeaker);
        assert_eq!(
            received.solution, solution,
            "contents must arrive exactly; fixed-point quantities make this checkable"
        );
        assert_eq!(
            received.solution.volume_of(chem_sim::ReagentId(3)),
            Units::from_f64(15.25),
            "fractional units must not be rounded in transit"
        );
    }

    #[test]
    fn remote_clients_receive_order_and_medical_evacuation_prompts() {
        let (mut server, mut client) = connected_pair();

        server.world_mut().spawn((
            Replicated,
            CrewMember {
                name: "Order Patient".into(),
                role: "Medical".into(),
            },
            Interactable::new("Order Patient — hand over 5u Bicaridine"),
        ));
        server.world_mut().spawn((
            Replicated,
            CrewMember {
                name: "Down Patient".into(),
                role: "Engineering".into(),
            },
            Interactable::new("Evacuate Down Patient to Medical"),
            NeedsMedicalEvacuation,
        ));

        settle(&mut server, &mut client);

        let mut prompts = client.world_mut().query::<&Interactable>();
        let labels: Vec<&str> = prompts
            .iter(client.world())
            .map(|prompt| prompt.label.as_str())
            .collect();
        assert!(labels.contains(&"Order Patient — hand over 5u Bicaridine"));
        assert!(labels.contains(&"Evacuate Down Patient to Medical"));

        let mut evacuation = client
            .world_mut()
            .query_filtered::<&Interactable, With<NeedsMedicalEvacuation>>();
        assert_eq!(
            evacuation.single(client.world()).unwrap().label,
            "Evacuate Down Patient to Medical",
            "the remote input router needs both the visible prompt and its dedicated action marker",
        );
    }

    #[test]
    fn machine_occupancy_replicates_with_a_mapped_entity() {
        // `in_use_by` holds a server entity id, which is meaningless to a
        // client until mapped. This is the check that `#[entities]` is doing
        // its job — without it the client would show the wrong chemist, or a
        // dangling id.
        let (mut server, mut client) = connected_pair();

        let chemist = server.world_mut().spawn(Replicated).id();
        server.world_mut().spawn((
            Replicated,
            Machine {
                kind: crate::machines::MachineKind::ChemMaster5000,
                in_use_by: Some(chemist),
            },
        ));

        server.update();
        server.exchange_with_client(&mut client);
        client.update();
        server.exchange_with_client(&mut client);
        client.update();

        let mut query = client.world_mut().query::<&Machine>();
        let machines: Vec<&Machine> = query.iter(client.world()).collect();
        assert_eq!(machines.len(), 1, "client should see the machine");

        let occupant = machines[0].in_use_by.expect("occupancy should replicate");
        assert!(
            client.world().get_entity(occupant).is_ok(),
            "the mapped entity must exist on the client, not dangle at the server's id"
        );
    }

    /// `parse_args` takes owned `String`s, the way `std::env::args()` yields
    /// them.
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn steams_own_connect_lobby_argument_is_a_join() {
        // Accepting an invite with the game closed is not a callback at all:
        // Steam relaunches the executable with this argument and fires
        // nothing. Ignoring it — which is what an unknown flag used to do —
        // drops the player on the main menu having clicked Join, with no
        // indication anything was even attempted.
        assert_eq!(
            LaunchMode::parse_args(args(&["+connect_lobby", "109775241234567890"])),
            Some(LaunchMode::JoinSteam(steam::LobbyId::from_raw(
                109775241234567890
            )))
        );
        // A lobby id that is not a number is not a reason to dial lobby 0.
        assert_eq!(
            LaunchMode::parse_args(args(&["+connect_lobby", "not-a-lobby"])),
            None
        );
        assert_eq!(LaunchMode::parse_args(args(&["+connect_lobby"])), None);
    }

    #[test]
    fn the_existing_flags_still_parse() {
        // A regression net around splitting `from_args` in two.
        assert_eq!(
            LaunchMode::parse_args(args(&["--solo"])),
            Some(LaunchMode::Singleplayer)
        );
        assert_eq!(
            LaunchMode::parse_args(args(&["--host"])),
            Some(LaunchMode::Host)
        );
        assert_eq!(
            LaunchMode::parse_args(args(&["--host-steam"])),
            Some(LaunchMode::HostSteam)
        );
        assert_eq!(
            LaunchMode::parse_args(args(&["--join", "192.168.1.40"])),
            Some(LaunchMode::Join(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 40)),
                DEFAULT_PORT
            )))
        );
        // `--join` with nothing after it is the second-window case.
        assert_eq!(
            LaunchMode::parse_args(args(&["--join"])),
            Some(LaunchMode::Join(loopback_address()))
        );
        assert_eq!(LaunchMode::parse_args(args(&[])), None);
        assert_eq!(LaunchMode::parse_args(args(&["--unrecognised"])), None);
    }

    #[test]
    fn launch_mode_defaults_to_singleplayer() {
        // Nothing on the command line must mean a normal single-chemist shift;
        // networking is opt-in.
        assert_eq!(LaunchMode::default(), LaunchMode::Singleplayer);
    }

    /// Runs `is_authority` for a given mode the way a run condition would.
    fn authority_for(mode: Option<LaunchMode>) -> bool {
        let mut world = World::new();
        if let Some(mode) = mode {
            world.insert_resource(mode);
        }
        world
            .run_system_cached(is_authority)
            .expect("run condition")
    }

    #[test]
    fn only_a_joining_process_gives_up_authority() {
        // The whole simulation hangs off this: singleplayer and a host both
        // run the lab, a joining client never does. Getting it from the launch
        // mode rather than the connection state is what stops a client that is
        // still handshaking from building its own copy of the room and then
        // receiving the server's on top.
        assert!(authority_for(Some(LaunchMode::Singleplayer)));
        assert!(authority_for(Some(LaunchMode::Host)));
        assert!(authority_for(Some(LaunchMode::HostSteam)));
        assert!(!authority_for(Some(LaunchMode::Join(loopback_address()))));
        assert!(!authority_for(Some(LaunchMode::JoinSteam(
            steam::LobbyId::from_raw(1)
        ))));
        assert!(
            authority_for(None),
            "a headless test with no launch mode is driving the simulation itself"
        );
    }

    /// Records whether a reader saw the shared state change this frame, which
    /// is what the real broadcasts key off.
    #[derive(Resource, Default)]
    struct SawChange(bool);

    fn watch_shift(shift: Res<crate::orders::Shift>, mut saw: ResMut<SawChange>) {
        if shift.is_changed() {
            saw.0 = true;
        }
    }

    #[test]
    fn a_joining_chemist_is_sent_the_shared_state_again() {
        // The three snapshot broadcasts only fire on change. Without this the
        // second chemist runs on their own `save.ron` until the host happens
        // to discover something — so the two of them read different books,
        // and the only symptom is a recipe one can make and the other cannot.
        let mut app = App::new();
        app.init_resource::<crate::orders::Shift>()
            .init_resource::<SawChange>()
            .add_systems(Update, (resync_on_join, watch_shift).chain());

        // First frame: inserting the resource counts as a change on its own.
        app.update();
        app.world_mut().resource_mut::<SawChange>().0 = false;

        app.update();
        assert!(
            !app.world().resource::<SawChange>().0,
            "a quiet frame with nobody joining must not resend anything"
        );

        app.world_mut().spawn(AuthorizedClient);
        app.update();
        assert!(
            app.world().resource::<SawChange>().0,
            "a chemist joining must trigger a fresh snapshot"
        );
    }

    #[test]
    fn a_bare_address_picks_up_the_default_port() {
        // What someone actually types when the host reads their IP out loud.
        assert_eq!(
            parse_address("192.168.1.40"),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 40)),
                DEFAULT_PORT
            ))
        );
        // An explicit port still wins.
        assert_eq!(
            parse_address("192.168.1.40:9999"),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 40)),
                9999
            ))
        );
        assert_eq!(parse_address("not an address at all"), None);
    }

    #[test]
    fn the_host_never_advertises_only_loopback() {
        // Binding or advertising loopback alone is the bug that looks exactly
        // like a firewall problem: a second window on this machine works, and
        // nothing else on the network can get in.
        let mut public = Vec::new();
        public.extend(lan_address());
        public.push(loopback_address());

        assert!(
            public.contains(&loopback_address()),
            "a second window on this machine must still be able to join"
        );
        if let Some(lan) = lan_address() {
            assert_ne!(
                lan.ip(),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                "the routable address must not be loopback"
            );
            assert_eq!(lan.port(), DEFAULT_PORT);
        }
    }

    #[test]
    fn a_bare_ip_hints_without_touching_dns() {
        // The cheap subset `menu::hint_for` runs on every keystroke — must
        // never fall through to a resolve, or typing would block on DNS.
        assert_eq!(
            parse_literal_address("192.168.1.40"),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 40)),
                DEFAULT_PORT
            ))
        );
        assert_eq!(
            parse_literal_address("192.168.1.40:9999"),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 40)),
                9999
            ))
        );
        // A hostname is not a literal — this must say so rather than resolve.
        assert_eq!(parse_literal_address("some-machine.local"), None);
    }

    /// Records the last reason `report_join_failure` wrote, the way the real
    /// menu-side handler reads it — reused by both cases below rather than
    /// each reaching into `Messages<ConnectFailed>` directly.
    #[derive(Resource, Default)]
    struct SawFailure(Option<String>);

    fn watch_failure(mut failed: MessageReader<ConnectFailed>, mut saw: ResMut<SawFailure>) {
        if let Some(failure) = failed.read().last() {
            saw.0 = Some(failure.reason.clone());
        }
    }

    #[test]
    fn a_handshake_with_no_client_at_all_is_still_reported() {
        // The earliest failures — socket bind, transport creation — never get
        // as far as inserting a `RenetClient`. Without this, a `None` client
        // would have no way to report anything, and `Connecting` would hang
        // exactly as silently as before this feature existed.
        let mut app = App::new();
        app.add_message::<ConnectFailed>()
            .init_resource::<SawFailure>()
            .add_systems(Update, (report_join_failure, watch_failure).chain());
        app.update();

        assert_eq!(
            app.world().resource::<SawFailure>().0.as_deref(),
            Some("could not reach the host — check the address, and that it's still open")
        );
    }

    #[test]
    fn a_netcode_timeout_reads_as_the_same_friendly_message() {
        // `renetcode`'s own ~15s connect timeout ends with the transport
        // calling `disconnect_due_to_transport`, which renet reports as
        // `DisconnectReason::Transport` — not anything reagent-specific. This
        // is the ordinary case a real timed-out join actually hits, so it
        // gets the plain-English message rather than the raw variant name.
        let mut client = RenetClient::new(ConnectionConfig::default());
        client.disconnect_due_to_transport();

        let mut app = App::new();
        app.insert_resource(client)
            .add_message::<ConnectFailed>()
            .init_resource::<SawFailure>()
            .add_systems(Update, (report_join_failure, watch_failure).chain());
        app.update();

        assert_eq!(
            app.world().resource::<SawFailure>().0.as_deref(),
            Some("could not reach the host — check the address, and that it's still open")
        );
    }

    #[test]
    fn a_connecting_client_only_reaches_playing_once_actually_connected() {
        // The bug this pins: entering `Playing` — and with it building the lab
        // and running the full simulation — before a connection exists, which
        // is what made a stuck or slow handshake look exactly like a freeze.
        // See `AppState::Connecting`'s doc comment.
        let mut server = App::new();
        let mut client = App::new();
        for app in [&mut server, &mut client] {
            app.add_plugins((
                MinimalPlugins,
                StatesPlugin,
                RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
            ));
            register_replication(app);
            app.finish();
        }
        client.init_state::<AppState>().add_systems(
            OnEnter(ClientState::Connected),
            finish_joining.run_if(in_state(AppState::Connecting)),
        );
        client
            .world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Connecting);
        client.update();
        assert_eq!(
            *client.world().resource::<State<AppState>>().get(),
            AppState::Connecting,
            "must not jump to Playing before the handshake finishes"
        );

        server.connect_client(&mut client);
        client.update();

        assert_eq!(
            *client.world().resource::<State<AppState>>().get(),
            AppState::Playing,
            "must reach Playing once ClientState::Connected actually fires"
        );
    }

    #[test]
    fn chemistry_schema_and_four_person_capacity_are_pinned() {
        assert_eq!(
            PROTOCOL_ID & 0xff,
            5,
            "positional sound messages need revision 5"
        );
        assert_eq!(MAX_REMOTE_CLIENTS, 3);
        assert_eq!(steam::LOBBY_CAPACITY, 4);
        assert_eq!(steam::MAX_REMOTE_CLIENTS, MAX_REMOTE_CLIENTS);
    }
}
