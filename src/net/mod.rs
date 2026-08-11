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

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
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
use crate::produce::Produce;

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
    /// Reads `--host`, `--join [addr]` from the command line.
    pub fn from_args() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--host" => return LaunchMode::Host,
                "--join" => {
                    let address = iter
                        .next()
                        .and_then(|value| parse_address(value))
                        .unwrap_or_else(local_address);
                    return LaunchMode::Join(address);
                }
                _ => {}
            }
        }
        LaunchMode::Singleplayer
    }
}

fn parse_address(value: &str) -> Option<SocketAddr> {
    value
        .parse::<SocketAddr>()
        .ok()
        // Bare host or IP: fall back to the default port rather than refusing.
        .or_else(|| value.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, DEFAULT_PORT)))
}

fn local_address() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)
}

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        let mode = LaunchMode::from_args();
        app.insert_resource(mode)
            .add_plugins((RepliconPlugins, RepliconRenetPlugins));

        register_replication(app);

        match mode {
            LaunchMode::Singleplayer => {}
            LaunchMode::Host => {
                app.add_systems(Startup, start_hosting);
            }
            LaunchMode::Join(_) => {
                app.add_systems(Startup, start_joining);
            }
        }
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
    let address = local_address();
    let Ok(socket) = UdpSocket::bind(address) else {
        error!("could not bind {address}; staying singleplayer");
        return;
    };
    let Ok(public) = socket.local_addr() else {
        return;
    };
    let Ok(since_epoch) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        return;
    };

    let server = RenetServer::new(ConnectionConfig {
        server_channels_config: channels.server_configs(),
        client_channels_config: channels.client_configs(),
        ..Default::default()
    });
    let config = ServerConfig {
        current_time: since_epoch,
        max_clients: 4,
        protocol_id: PROTOCOL_ID,
        public_addresses: vec![public],
        authentication: ServerAuthentication::Unsecure,
    };
    let Ok(transport) = NetcodeServerTransport::new(config, socket) else {
        error!("could not start the netcode transport");
        return;
    };

    commands.insert_resource(server);
    commands.insert_resource(transport);
    info!("hosting the lab on {public}");
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
    fn connected_pair() -> (App, App) {
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
        server.connect_client(&mut client);
        (server, client)
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
}
