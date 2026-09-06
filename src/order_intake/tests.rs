use super::*;
use crate::{
    chem_data::ChemDb,
    containers::{Container, ContainerKind, HeldBy},
};
use bevy::ecs::system::RunSystemOnce;

pub(crate) fn sample_order() -> Order {
    Order {
        reagent: chem_sim::ReagentId(0),
        specific: true,
        minimum_purity: 0.7,
        amount: chem_sim::Units::whole(20),
        plea: "The scrubbers failed. We need to get the ward breathing again.".into(),
        patience: 150.0,
        waited: 0.0,
    }
}

fn app() -> App {
    let mut app = App::new();
    app.init_resource::<Time>()
        .init_resource::<IntakeState>()
        .init_resource::<Shift>()
        .init_resource::<crate::radio::RadioLog>()
        .insert_resource(crate::threat::Authored(
            ron::from_str::<GreetingScript>(include_str!(
                "../../assets/data/station.greetings.ron"
            ))
            .unwrap(),
        ))
        .insert_resource(ChemDb(
            chem_sim::ChemData::from_ron(
                include_str!("../../assets/data/chem.reagents.ron"),
                include_str!("../../assets/data/chem.reactions.ron"),
            )
            .unwrap(),
        ))
        .add_message::<FromClient<OpenOrderConversation>>()
        .add_message::<FromClient<AcceptOrder>>()
        .add_message::<ToClients<OrderConversationOpened>>()
        .add_systems(
            Update,
            (update_pending, open_conversations, accept_orders).chain(),
        );
    app
}

fn tick(app: &mut App, seconds: f32) {
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(std::time::Duration::from_secs_f32(seconds));
    app.update();
}

fn visitor(app: &mut App, id: u64) -> Entity {
    app.world_mut()
        .spawn((
            CrewMember {
                name: format!("Visitor {id}"),
                role: "Medical".into(),
            },
            Transform::from_xyz(1.0, 0.93, 0.0),
            CrewRoute::standing(),
            Bloodstream::default(),
            PendingOrder::new(
                sample_order(),
                RequestContext {
                    id,
                    source: RequestSource::Ordinary,
                    campaign: None,
                    greeting: GreetingKind::Ordinary,
                    step: None,
                },
            ),
            queue::QueuePosition {
                target: Vec3::new(1.0, 0.0, 0.0),
                reached: true,
                pickup: false,
            },
        ))
        .id()
}

fn actor(app: &mut App, client: ClientId) -> Entity {
    app.world_mut()
        .spawn((
            Chemist { client },
            Transform::from_xyz(0.0, 0.93, 0.0),
            Body::default(),
            Bloodstream::default(),
        ))
        .id()
}

fn talk(app: &mut App, client: ClientId, target: Entity, id: u64) {
    app.world_mut().write_message(FromClient {
        client_id: client,
        message: OpenOrderConversation { target, id },
    });
    tick(app, 0.01);
}
fn accept(app: &mut App, client: ClientId, target: Entity, id: u64) {
    app.world_mut().write_message(FromClient {
        client_id: client,
        message: AcceptOrder { target, id },
    });
    tick(app, 0.01);
}

#[test]
fn talking_preserves_held_beaker_and_does_not_accept_or_reset_time() {
    let mut app = app();
    let player = actor(&mut app, ClientId::Server);
    let npc = visitor(&mut app, 7);
    let beaker = app
        .world_mut()
        .spawn((Container::new(ContainerKind::LargeBeaker), HeldBy(player)))
        .id();
    tick(&mut app, 100.0);
    talk(&mut app, ClientId::Server, npc, 7);
    talk(&mut app, ClientId::Server, npc, 7);
    assert!(app.world().get::<Order>(npc).is_none());
    assert!(app.world().get::<PendingOrder>(npc).unwrap().waited >= 100.0);
    assert_eq!(app.world().get::<HeldBy>(beaker).unwrap().0, player);
    accept(&mut app, ClientId::Server, npc, 7);
    assert_eq!(app.world().get::<Order>(npc).unwrap().remaining(), 150.0);
    assert!(app.world().get::<AwaitingConversation>(npc).is_none());
    assert_eq!(app.world().get::<HeldBy>(beaker).unwrap().0, player);
}

#[test]
fn ignored_greeting_penalizes_once_without_botching_an_order() {
    let mut app = app();
    let npc = visitor(&mut app, 1);
    tick(&mut app, 119.0);
    assert!(!app.world().get::<PendingOrder>(npc).unwrap().reminded);
    tick(&mut app, 1.0);
    assert!(app.world().get::<PendingOrder>(npc).unwrap().reminded);
    tick(&mut app, 60.0);
    tick(&mut app, 180.0);
    assert!(app.world().get::<PendingOrder>(npc).is_none());
    assert_eq!(
        app.world().resource::<Shift>().npc_standing("Visitor 1"),
        -1
    );
    assert_eq!(app.world().resource::<Shift>().botched, 0);
}

#[test]
fn travel_reserves_intake_but_does_not_spend_greeting_time() {
    let mut app = app();
    let npc = visitor(&mut app, 1);
    app.world_mut()
        .get_mut::<queue::QueuePosition>(npc)
        .unwrap()
        .reached = false;
    tick(&mut app, 400.0);
    assert_eq!(app.world().get::<PendingOrder>(npc).unwrap().waited, 0.0);
}

#[test]
fn a_body_collapsed_before_acceptance_is_withdrawn_from_intake() {
    let mut app = app();
    let npc = visitor(&mut app, 12);
    let mut body = Body::default();
    body.0.collapsed = true;
    app.world_mut().entity_mut(npc).insert(body);

    tick(&mut app, 0.01);

    assert!(app.world().get::<PendingOrder>(npc).is_none());
    assert!(app.world().get::<AwaitingConversation>(npc).is_none());
    assert_eq!(
        app.world().get::<CrewRoute>(npc).unwrap().phase,
        crate::crew::CrewPhase::Leaving,
    );
}

#[test]
fn acceptance_requires_hearing_current_request_in_reach_and_clear_sight() {
    let mut app = app();
    let player = actor(&mut app, ClientId::Server);
    let npc = visitor(&mut app, 1);
    accept(&mut app, ClientId::Server, npc, 1);
    assert!(app.world().get::<Order>(npc).is_none());
    talk(&mut app, ClientId::Server, npc, 99);
    accept(&mut app, ClientId::Server, npc, 1);
    assert!(app.world().get::<Order>(npc).is_none());
    let wall = app
        .world_mut()
        .spawn((
            Transform::from_xyz(0.5, 1.0, 0.0),
            crate::lab::Solid {
                half_extents: Vec3::new(0.1, 2.0, 2.0),
            },
        ))
        .id();
    talk(&mut app, ClientId::Server, npc, 1);
    assert!(app.world().get::<HeardRequests>(player).is_none());
    app.world_mut().despawn(wall);
    talk(&mut app, ClientId::Server, npc, 1);
    app.world_mut()
        .get_mut::<Transform>(player)
        .unwrap()
        .translation
        .x = 20.0;
    accept(&mut app, ClientId::Server, npc, 1);
    assert!(app.world().get::<Order>(npc).is_none());
}

#[test]
fn two_players_accept_once_and_acceptance_order_controls_pickup() {
    let mut app = app();
    let remote = ClientId::Client(Entity::from_bits(100));
    actor(&mut app, ClientId::Server);
    actor(&mut app, remote);
    let a = visitor(&mut app, 20);
    let b = visitor(&mut app, 10);
    talk(&mut app, ClientId::Server, a, 20);
    talk(&mut app, remote, a, 20);
    for client in [remote, ClientId::Server] {
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: AcceptOrder { target: a, id: 20 },
        });
    }
    tick(&mut app, 0.01);
    assert_eq!(app.world().resource::<IntakeState>().next_acceptance, 1);
    talk(&mut app, remote, b, 10);
    accept(&mut app, remote, b, 10);
    assert!(
        app.world().get::<AcceptedOrder>(a).unwrap().sequence
            < app.world().get::<AcceptedOrder>(b).unwrap().sequence
    );
    accept(&mut app, ClientId::Server, a, 20);
    assert_eq!(app.world().resource::<IntakeState>().next_acceptance, 2);
}

#[test]
fn simultaneous_generators_share_two_reservations_and_oldest_due_goes_next() {
    let mut app = app();
    let got = app
        .world_mut()
        .run_system_once(|mut intake: Intake| {
            let mut timer = Timer::from_seconds(0.0, TimerMode::Once);
            [
                RequestSource::Ordinary,
                RequestSource::Specific,
                RequestSource::Cult,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, source)| {
                intake.admit(source, &format!("Visitor {}", i + 1), &mut timer, false)
            })
            .collect::<Vec<_>>()
        })
        .unwrap();
    assert!(got[0].is_some() && got[1].is_some() && got[2].is_none());
    visitor(&mut app, 1);
    visitor(&mut app, 2);
    app.world_mut()
        .run_system_once(clear_frame_reservations)
        .unwrap();
    let rejected = app
        .world_mut()
        .run_system_once(|mut intake: Intake| {
            intake.admit(
                RequestSource::Cult,
                "Visitor 3",
                &mut Timer::default(),
                false,
            )
        })
        .unwrap();
    assert!(rejected.is_none());
    let e = app
        .world_mut()
        .query_filtered::<Entity, With<PendingOrder>>()
        .iter(app.world())
        .next()
        .unwrap();
    app.world_mut()
        .entity_mut(e)
        .remove::<PendingOrder>()
        .insert(sample_order());
    let (newer, older) = app
        .world_mut()
        .run_system_once(|mut intake: Intake| {
            let mut timer = Timer::default();
            (
                intake.admit(RequestSource::Ordinary, "Newer", &mut timer, false),
                intake.admit(RequestSource::Cult, "Visitor 3", &mut timer, true),
            )
        })
        .unwrap();
    assert!(newer.is_none() && older.is_some());
}

#[test]
fn more_than_five_accepted_requests_do_not_use_intake_capacity() {
    let mut app = app();
    actor(&mut app, ClientId::Server);
    for id in 1..=8 {
        let npc = visitor(&mut app, id);
        talk(&mut app, ClientId::Server, npc, id);
        accept(&mut app, ClientId::Server, npc, id);
    }
    assert_eq!(
        app.world_mut().query::<&Order>().iter(app.world()).count(),
        8
    );
    assert!(app
        .world_mut()
        .run_system_once(|mut intake: Intake| intake.admit(
            RequestSource::Ordinary,
            "Next",
            &mut Timer::default(),
            false
        ))
        .unwrap()
        .is_some());
}

#[test]
fn campaign_greetings_and_reminders_are_authored_neutral_and_separate() {
    let script: GreetingScript =
        ron::from_str(include_str!("../../assets/data/station.greetings.ron")).unwrap();
    for id in 0..6 {
        for reminder in [false, true] {
            let ordinary = greeting(&script, false, reminder, id);
            let campaign = greeting(&script, true, reminder, id);
            assert_ne!(ordinary, campaign);
            for secret in [
                "cult",
                "spy",
                "syndicate",
                "underworld",
                "antagonist",
                "units",
                "purity",
            ] {
                assert!(!campaign.to_lowercase().contains(secret));
            }
        }
    }
}

#[test]
fn accepted_preparation_clock_continues_during_the_walk_to_pickup() {
    let mut app = app();
    actor(&mut app, ClientId::Server);
    let npc = visitor(&mut app, 1);
    talk(&mut app, ClientId::Server, npc, 1);
    accept(&mut app, ClientId::Server, npc, 1);
    app.add_message::<crate::orders::OrderResolved>()
        .add_systems(Update, crate::orders::expire_orders);
    app.world_mut()
        .get_mut::<CrewRoute>(npc)
        .unwrap()
        .queue_to(Vec3::X * 50.0, crate::lab::DeliveryLane::Public);
    tick(&mut app, 10.0);
    assert_eq!(app.world().get::<Order>(npc).unwrap().remaining(), 140.0);
}

#[test]
fn unheard_cult_offer_retries_without_advancing_and_obsolete_situations_withdraw() {
    let mut app = app();
    app.init_resource::<crate::cult::CultProgress>();
    let npc = visitor(&mut app, 1);
    {
        let mut pending = app.world_mut().get_mut::<PendingOrder>(npc).unwrap();
        pending.context.source = RequestSource::Cult;
        pending.context.step = Some(0);
    }
    app.world_mut()
        .resource_mut::<crate::cult::CultProgress>()
        .offered_stage = Some(0);
    tick(&mut app, 180.0);
    let progress = app.world().resource::<crate::cult::CultProgress>();
    assert_eq!(progress.next_stage, 0);
    assert_eq!(progress.offered_stage, None);
    let next = visitor(&mut app, 2);
    {
        let mut pending = app.world_mut().get_mut::<PendingOrder>(next).unwrap();
        pending.context.source = RequestSource::Cult;
        pending.context.step = Some(0);
    }
    app.world_mut()
        .resource_mut::<crate::cult::CultProgress>()
        .next_stage = 1;
    tick(&mut app, 0.1);
    assert!(app.world().get::<PendingOrder>(next).is_none());
    assert_eq!(app.world().resource::<Shift>().npc_standing("Visitor 2"), 0);
}
