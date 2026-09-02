//! Opt-in rendered regression scenario. Use an isolated LOCALAPPDATA folder.
//! Compiled out of release builds; ordinary play never installs these systems.
use super::*;
use crate::{
    crew::{AtCounter, ReturnsToDuty},
    interaction::InteractionMode,
    lab::{DeliveryLane, DeliveryStations},
    net::LaunchMode,
    player::{LocalPlayer, PlayerCamera},
};
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

#[derive(Resource, Default)]
struct Scenario {
    started: Option<std::time::Instant>,
    populated: bool,
    seeded: bool,
    stage: u8,
    shots: HashSet<u8>,
}

pub(super) fn install(app: &mut App) {
    app.init_resource::<Scenario>()
        .add_systems(
            Update,
            drive
                .run_if(in_state(AppState::Playing))
                .run_if(resource_exists::<crate::lab::MapReady>),
        )
        .add_systems(
            PostUpdate,
            view.after(crate::player::follow_chemist)
                .run_if(in_state(AppState::Playing)),
        );
}

fn output(world: &World) -> std::path::PathBuf {
    let role = match world.resource::<LaunchMode>() {
        LaunchMode::Singleplayer => "solo",
        LaunchMode::Join(_) => "client",
        _ => "host",
    };
    std::path::PathBuf::from("target/order-playtest").join(role)
}

fn shoot(world: &mut World, id: u8, label: &str) {
    if !world.resource_mut::<Scenario>().shots.insert(id) {
        return;
    }
    let root = output(world);
    let _ = std::fs::create_dir_all(&root);
    world
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(root.join(format!("{id}-{label}.png"))));
}

fn drive(world: &mut World) {
    let Some(player) = world
        .query_filtered::<Entity, With<LocalPlayer>>()
        .iter(world)
        .next()
    else {
        return;
    };
    let now = std::time::Instant::now();
    let started = *world.resource_mut::<Scenario>().started.get_or_insert(now);
    let elapsed = now.duration_since(started).as_secs_f32();
    let authority = !matches!(world.resource::<LaunchMode>(), LaunchMode::Join(_));
    if authority && elapsed > 1.0 && !world.resource::<Scenario>().populated {
        populate_visitors(world);
        world.resource_mut::<Scenario>().populated = true;
    }
    if authority && elapsed > 3.0 && !world.resource::<Scenario>().seeded {
        seed(world);
        world.resource_mut::<Scenario>().seeded = true;
    }
    if authority && elapsed < 24.0 {
        for mut at in world
            .query_filtered::<&mut Transform, With<Chemist>>()
            .iter_mut(world)
        {
            at.translation = Vec3::new(4.0, 0.93, 3.9);
        }
    }
    let speaker = world
        .query::<(Entity, &AwaitingConversation)>()
        .iter(world)
        .find(|(_, p)| p.id == 900_001 && p.arrived)
        .map(|(e, p)| (e, p.id));
    let local_takes_order = matches!(
        world.resource::<LaunchMode>(),
        LaunchMode::Singleplayer | LaunchMode::Join(_)
    );
    if elapsed > 8.0 && elapsed < 14.0 && local_takes_order {
        if let Some((target, id)) = speaker {
            if world
                .get::<InteractionMode>(player)
                .is_some_and(|m| m.is_roaming())
            {
                world.write_message(OpenOrderConversation { target, id });
            }
        }
    }
    if elapsed > 12.0
        && world
            .get::<InteractionMode>(player)
            .is_some_and(|m| matches!(m, InteractionMode::OrderConversation(..)))
    {
        shoot(world, 1, "conversation");
    }
    if elapsed > 15.0 && elapsed < 24.0 && local_takes_order {
        if let Some((target, id)) = speaker {
            if matches!(
                world.get::<InteractionMode>(player),
                Some(InteractionMode::OrderConversation(..))
            ) {
                world.write_message(AcceptOrder { target, id });
                *world.get_mut::<InteractionMode>(player).unwrap() = InteractionMode::Roaming;
            }
        }
    }
    if elapsed > 26.0 && world.resource::<Scenario>().stage < 1 {
        *world.get_mut::<InteractionMode>(player).unwrap() = InteractionMode::OrderDirectory {
            machine: None,
            return_to_book: false,
        };
        world.resource_mut::<Scenario>().stage = 1;
    }
    if elapsed > 29.0 {
        shoot(world, 2, "all-orders");
    }
    if elapsed > 35.0 && world.resource::<Scenario>().stage < 2 {
        *world.get_mut::<InteractionMode>(player).unwrap() = InteractionMode::Roaming;
        world.resource_mut::<Scenario>().stage = 2;
    }
    if elapsed > 59.0 {
        shoot(world, 3, "pickup-line");
    }
    if elapsed > 65.0 && world.resource::<Scenario>().stage < 3 {
        let board = world
            .query::<(Entity, &crate::machines::Machine)>()
            .iter(world)
            .find(|(_, m)| m.kind == crate::machines::MachineKind::StandingBoard)
            .map(|(e, _)| e);
        if let Some(board) = board {
            *world.get_mut::<InteractionMode>(player).unwrap() =
                InteractionMode::UsingMachine(board);
        }
        world.resource_mut::<Scenario>().stage = 3;
    }
    if elapsed > 68.0 {
        shoot(world, 4, "standing-board");
    }
    if elapsed > 75.0 && world.resource::<Scenario>().stage < 4 {
        let accepted = world.query::<&Order>().iter(world).count();
        let waiting = world.query::<&AwaitingConversation>().iter(world).count();
        let reached = world
            .query::<&queue::QueuePosition>()
            .iter(world)
            .filter(|p| p.pickup && p.reached)
            .count();
        let positions: Vec<_> = world
            .query_filtered::<&Transform, With<CrewMember>>()
            .iter(world)
            .map(|t| t.translation)
            .collect();
        let mut closest = f32::INFINITY;
        for (i, a) in positions.iter().enumerate() {
            for b in positions.iter().skip(i + 1) {
                if (a.y - b.y).abs() < 1.2 {
                    closest = closest.min(crate::nav::flat_distance(*a, *b));
                }
            }
        }
        let report=format!("Accepted requests: {accepted}\nWaiting to speak: {waiting}\nPickup positions reached (authority only): {reached}\nClosest NPC bodies on same floor: {closest:.3} m\n");
        let root = output(world);
        let _ = std::fs::create_dir_all(&root);
        let _ = std::fs::write(root.join("result.txt"), &report);
        info!("Order playtest: {report}");
        world.resource_mut::<Scenario>().stage = 4;
    }
    if elapsed > 80.0 {
        world.write_message(bevy::app::AppExit::Success);
    }
}

fn populate_visitors(world: &mut World) {
    // Fresh saves initially contain eight ordinary residents. Include the
    // authored occasional visitors too, without cloning an existing identity.
    for (name, role) in [
        ("Deckhand Prewitt", "Cargo"),
        ("Tech Boyle", "Engineering"),
        ("Adair Voss", "Service"),
        ("Corwin Ashe", "Cargo"),
        ("Sergeant Voss", "Security"),
    ] {
        if world
            .query::<&CrewMember>()
            .iter(world)
            .any(|m| m.name == name)
        {
            continue;
        }
        world.spawn((
            CrewMember {
                name: name.into(),
                role: role.into(),
            },
            Ambient::new(600.0),
            CrewRoute::arrival(0.0),
            Transform::from_xyz(11.0, 0.93, 12.0),
            Body::default(),
            Bloodstream::default(),
            bevy_replicon::prelude::Replicated,
            crate::until_we_leave_the_lab(),
        ));
    }
}

fn seed(world: &mut World) {
    world.resource_mut::<Shift>().accepting_orders = false;
    world.resource_mut::<IntakeState>().next_acceptance = 10;
    let mut residents: Vec<_> = world
        .query_filtered::<(Entity, &CrewMember), With<Ambient>>()
        .iter(world)
        .filter(|(_, m)| m.role != "Medical")
        .map(|(e, m)| (e, m.name.clone()))
        .collect();
    residents.sort_by(|a, b| a.1.cmp(&b.1));
    let reagent = world
        .resource::<crate::chem_data::ChemDb>()
        .reagents
        .id_of("dylovene")
        .unwrap();
    for (index, (entity, _)) in residents.iter().take(11).enumerate() {
        let order=Order{reagent,specific:true,minimum_purity:0.0,amount:chem_sim::Units::whole(20),
            plea:"The department's emergency cupboard needs a fresh supply. Please prepare this batch for us.".into(),patience:600.0,waited:index as f32};
        let pending = index == 10;
        let at = if pending {
            world
                .resource::<DeliveryStations>()
                .station(DeliveryLane::Public)
                .queue_position(0.0)
                + Vec3::Y * 0.93
        } else {
            Vec3::new(11.0 - index as f32 * 1.1, 0.93, 12.0)
        };
        let mut npc = world.entity_mut(*entity);
        npc.remove::<(
            Ambient,
            AtCounter,
            Order,
            PendingOrder,
            AwaitingConversation,
            AcceptedOrder,
            queue::QueuePosition,
        )>();
        npc.insert((
            ReturnsToDuty,
            crate::social::NpcCommitment,
            Transform::from_translation(at),
            CrewRoute::arrival(0.0),
        ));
        if pending {
            npc.insert((
                PendingOrder::new(
                    order,
                    RequestContext {
                        id: 900_001,
                        source: RequestSource::Ordinary,
                        campaign: None,
                        greeting: GreetingKind::Ordinary,
                        step: None,
                    },
                ),
                Interactable::new("Waiting to speak"),
            ));
        } else {
            npc.insert((
                order,
                AcceptedOrder {
                    sequence: index as u64,
                },
                Interactable::new("Collecting an accepted order"),
            ));
        }
    }
    let players: Vec<_> = world
        .query_filtered::<Entity, With<Chemist>>()
        .iter(world)
        .collect();
    for player in players {
        let held = world
            .query::<&crate::containers::HeldBy>()
            .iter(world)
            .any(|h| h.0 == player);
        if !held {
            world.spawn((
                crate::containers::Container::new(crate::containers::ContainerKind::LargeBeaker),
                crate::containers::HeldBy(player),
                bevy_replicon::prelude::Replicated,
                crate::until_we_leave_the_lab(),
            ));
        }
    }
}

fn view(state: Res<Scenario>, mut cameras: Query<&mut Transform, With<PlayerCamera>>) {
    if state.started.is_none() {
        return;
    }
    for mut camera in &mut cameras {
        *camera = if state.stage == 2 {
            Transform::from_xyz(10.5, 2.4, 14.0).looking_at(Vec3::new(-4.5, 0.95, 13.2), Vec3::Y)
        } else {
            Transform::from_xyz(3.6, 2.4, 13.9).looking_at(Vec3::new(4.2, 0.95, 6.0), Vec3::Y)
        };
    }
}
