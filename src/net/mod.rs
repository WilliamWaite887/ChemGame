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
use bevy_replicon_renet::renet::ConnectionConfig;
// `RenetServer`/`RenetClient` come from bevy_renet's re-export, not raw renet:
// only the bevy wrappers are resources.
use bevy_replicon_renet::{RenetChannelsExt, RenetClient, RenetServer, RepliconRenetPlugins};

use crate::body::{Bloodstream, Body};
use crate::containers::{Container, HeldBy, InSlot};
use crate::crew::CrewMember;
use crate::hazards::{ActiveHazard, SmokeCloud, SmokePayload};
use crate::machines::{Buffer, DispenseAmount, Hopper, Machine, Thermostat};
use crate::player::Player;
use crate::produce::Produce;
use crate::AppState;

/// Arbitrary; both ends must agree.
const PROTOCOL_ID: u64 = 0x43_48_45_4d_00_00_00_01;
const DEFAULT_PORT: u16 = 5327;

/// How this process was launched.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LaunchMode {
    /// One chemist, no sockets. Server logic runs locally.
    #[default]
    Singleplayer,
    /// Listen server: plays the game and accepts a second chemist.
    Host,
    /// Joins a lab hosted elsewhere.
    Join(SocketAddr),
}

impl LaunchMode {
    /// Reads `--solo`, `--host` or `--join [addr]` from the command line.
    ///
    /// `None` means the command line said nothing, which is the signal to show
    /// the menu instead of guessing.
    pub fn from_args() -> Option<Self> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--solo" => return Some(LaunchMode::Singleplayer),
                "--host" => return Some(LaunchMode::Host),
                "--join" => {
                    let address = iter
                        .next()
                        .and_then(|value| parse_address(value))
                        .unwrap_or_else(loopback_address);
                    return Some(LaunchMode::Join(address));
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
/// gets no slot: the guest reads the host's notebook and career.
pub fn apply_command_line(app: &mut App) {
    let Some(mode) = LaunchMode::from_args() else {
        return;
    };
    app.insert_resource(mode).insert_resource(LaunchedFromArgs);
    if !matches!(mode, LaunchMode::Join(_)) {
        crate::saves::migrate_legacy_saves();
        app.insert_resource(crate::saves::SaveSlot::default_slot());
    }
}

/// Parses what someone reads out loud: an IP, an IP and port, or a hostname.
pub fn parse_address(value: &str) -> Option<SocketAddr> {
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
    !matches!(mode.as_deref(), Some(LaunchMode::Join(_)))
}

/// Run condition: this process is opening its lab to others.
fn hosting(mode: Option<Res<LaunchMode>>) -> bool {
    matches!(mode.as_deref(), Some(LaunchMode::Host))
}

/// Run condition: this process is dialling someone else's lab.
fn joining(mode: Option<Res<LaunchMode>>) -> bool {
    matches!(mode.as_deref(), Some(LaunchMode::Join(_)))
}

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        // Only if the command line has not already inserted one: the mode is
        // now a menu decision, and singleplayer is the right thing to assume
        // for anything that never reaches the menu, such as a test.
        app.init_resource::<LaunchMode>()
            .add_plugins((RepliconPlugins, RepliconRenetPlugins));

        register_replication(app);
        app.add_systems(Update, resync_on_join.run_if(is_authority))
            // Sockets open on the way into the lab rather than at startup,
            // because until the menu is done with, this process does not yet
            // know whether it is hosting, joining or neither.
            .add_systems(
                OnEnter(AppState::Playing),
                (
                    start_hosting.run_if(hosting),
                    start_joining.run_if(joining),
                ),
            );
    }
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
        .replicate::<Machine>()
        .replicate::<Buffer>()
        .replicate::<Hopper>()
        .replicate::<DispenseAmount>()
        .replicate::<Produce>()
        .replicate::<CrewMember>()
        // The marker itself, so a client can tell a chemist from any other
        // replicated entity and give them a body to look at.
        .replicate::<Player>()
        // What the chamber is set to. Without this the second chemist to walk
        // up cannot see it is running and cooks the batch.
        .replicate::<Thermostat>()
        // Clouds are entities, so replication carries them and no snapshot
        // message is needed — which is exactly why they are entities.
        .replicate::<SmokeCloud>()
        .replicate::<SmokePayload>()
        .replicate::<ActiveHazard>()
        // A chemist's condition is shared lab state too: the whole point of
        // the second pair of hands is being able to see that the first pair is
        // in trouble.
        .replicate::<Body>()
        .replicate::<Bloodstream>();
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
        max_clients: 4,
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

fn start_joining(mut commands: Commands, channels: Res<RepliconChannels>, mode: Res<LaunchMode>) {
    let LaunchMode::Join(address) = *mode else {
        return;
    };
    let Ok(since_epoch) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        return;
    };

    let client = RenetClient::new(ConnectionConfig {
        server_channels_config: channels.server_configs(),
        client_channels_config: channels.client_configs(),
        ..Default::default()
    });
    // Any local port will do; the id only needs to be unique per connection.
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
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
                Machine::new(crate::machines::MachineKind::Dispenser),
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
        let mut machines = client.world_mut().query::<(&Machine, &Mesh3d)>();
        assert_eq!(
            machines.iter(client.world()).count(),
            1,
            "the dispenser must arrive and be built: {machine} on the server"
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
                kind: crate::machines::MachineKind::Dispenser,
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
        assert!(!authority_for(Some(LaunchMode::Join(loopback_address()))));
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
}
