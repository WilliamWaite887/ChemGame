//! Cargo keeping the lab in glassware.
//!
//! Every hand-off over the counter takes the container with it — the crew walk
//! away holding your beaker, which is true in the original too — and nothing
//! else in the game makes one. The Mixing Chamber mints pills and bottles from
//! its buffer, but getting anything *into* the buffer needs a beaker, so
//! without a supply line the lab is bricked after five deliveries.
//!
//! Supply arrives the way everything else in this lab does: someone walks in on
//! a timer, puts it on the counter, and goes. The courier is lifted almost
//! wholesale from [`crate::produce`], which already solved this shape.

use bevy::prelude::*;

use crate::containers::{spawn_container, Container, ContainerKind};
use crate::crew::{
    recall_or_spawn_crew_member, AvailableResidents, CrewMember, CrewPhase, CrewRoute,
};
use crate::lab::{DeliveryLane, DeliveryStations, COUNTER_TOP};
use crate::net::is_authority;
use crate::orders::{Shift, StationData};
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::{crate_contents, restock_order};
use crate::AppState;

/// Gap between pieces laid out on the counter.
const ITEM_SPACING: f32 = 0.3;
/// Kept clear of the sample-vial drop at x = `COUNTER_SPOT.x`, so a crate never
/// lands inside a vial.
const CRATE_X_OFFSET: f32 = -1.05;

/// How often the lab's glassware deficit is rechecked.
///
/// There is no prep window to hang this off any more, so it runs on its own
/// clock instead — the first check fires immediately (a `None` timer reads as
/// due), so a fresh session is not left short for twenty seconds.
const GLASSWARE_CHECK_SECONDS: f32 = 20.0;

pub struct RestockPlugin;

impl Plugin for RestockPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingRestock>().add_systems(
            Update,
            (
                order_glassware,
                // Split from ordering so a crate that cannot go out this frame
                // is retried rather than lost — see [`PendingRestock`].
                dispatch_glassware,
                // Not gated on the accepting-orders sign: the courier may
                // still be walking mid-stride when the player flips it, and
                // one frozen holding a crate never leaves.
                unload_glassware,
            )
                .chain()
                .run_if(in_state(AppState::Playing))
                .run_if(crate::session::career_session)
                .run_if(is_authority),
        );
    }
}

/// A courier walking in with an armful of glassware.
#[derive(Component)]
struct GlasswareDelivery {
    beakers: usize,
    large: usize,
}

impl GlasswareDelivery {
    fn total(&self) -> usize {
        self.beakers + self.large
    }
}

/// Revokes a courier payload before another controller takes the NPC.
///
/// Kept as a queued world command so callers such as Medical can invoke it
/// without naming the private [`GlasswareDelivery`] component. The delivered
/// composition is banked into the exact-count purchase fields rather than the
/// rounded deficit field, so cancellation and retry cannot change the crate.
/// Calling it for an entity without a delivery is a no-op.
pub(crate) fn cancel_glassware_delivery(commands: &mut Commands, entity: Entity) {
    commands.queue(move |world: &mut World| {
        let Some(delivery) = world.get::<GlasswareDelivery>(entity) else {
            return;
        };
        let (beakers, large) = (delivery.beakers, delivery.large);
        if let Some(mut pending) = world.get_resource_mut::<PendingRestock>() {
            pending.purchased_beakers += beakers;
            pending.purchased_large += large;
        } else {
            warn!("could not requeue canceled glassware delivery: PendingRestock is unavailable");
        }
        if let Ok(mut courier) = world.get_entity_mut(entity) {
            courier.remove::<GlasswareDelivery>();
        }
    });
}

/// A crate that has been decided on but not yet handed to a courier.
///
/// Ordering and dispatching are separate because the courier may be
/// unavailable the moment a deficit is found — he is on the ordinary crew
/// roster too, so he can still be walking out from an earlier delivery.
/// Ordering happens on its own periodic check; dispatch retries until it
/// takes. Collapsed into one system, a courier who happened to be in the room
/// would cancel the lab's entire resupply, and any requisition paid for it
/// with it.
///
/// `deficit` (from the periodic check) and `purchased_*` (from
/// `shift::NpcRequisitionKind::SatoPack`, his own personal shop) are kept as
/// separate fields rather than one running total, deliberately: they are
/// still one delivery in the end, but a purchased pack's exact composition
/// has to survive to `dispatch_glassware` unchanged — "what the shop lists is
/// exactly what shows up," the same property Botanist Ivy's packs already
/// guarantee — while the deficit half still gets rounded through
/// `crate_contents`'s `large_every` split same as always. `order_glassware`
/// *adds to* `deficit` rather than overwriting it for the same reason: a
/// purchase queued the same tick a periodic check lands must never be
/// clobbered.
#[derive(Resource, Default)]
pub struct PendingRestock {
    deficit: usize,
    purchased_beakers: usize,
    purchased_large: usize,
}

impl PendingRestock {
    fn is_empty(&self) -> bool {
        self.deficit == 0 && self.purchased_beakers == 0 && self.purchased_large == 0
    }
}

/// Banks a personal purchase from Miner Sato's own shop onto the one pending
/// delivery — see [`PendingRestock`]'s own doc comment for why this merges
/// rather than spawning him a second time. `pub(super)`: only `shift::
/// apply_npc_requisition`'s `SatoPack` arm calls this.
pub(super) fn queue_glassware_purchase(pending: &mut PendingRestock, beakers: usize, large: usize) {
    pending.purchased_beakers += beakers;
    pending.purchased_large += large;
}

/// Works out how short the lab is, on a periodic check rather than once per
/// prep — there is no prep to hang it off any more.
fn order_glassware(
    time: Res<Time>,
    mut timer: Local<Option<Timer>>,
    mut pending: ResMut<PendingRestock>,
    mut shift: ResMut<Shift>,
    station: Option<Res<StationData>>,
    glassware: Query<&Container>,
) {
    // An `ExpeditedFreight` favor skips the wait entirely: whoever owed the
    // player a quiet word with Cargo made this run happen now instead of at
    // the next check. Spent here rather than at the delivery itself so it
    // cannot be banked against a check that was already due anyway.
    let expedited = shift.requisition.expedited_freight_favors > 0
        && timer.as_ref().is_some_and(|t| !t.just_finished());
    let due = match timer.as_mut() {
        Some(t) => t.tick(time.delta()).just_finished(),
        // Nothing scheduled yet: this is the first frame, and the lab should
        // not sit unchecked for a full interval before the first delivery.
        None => true,
    };
    if !due && !expedited {
        return;
    }
    if !due && expedited {
        shift.requisition.expedited_freight_favors -= 1;
    }
    *timer = Some(Timer::from_seconds(
        GLASSWARE_CHECK_SECONDS,
        TimerMode::Repeating,
    ));

    let Some(station) = station else {
        return;
    };
    let supply = &station.config.supply;

    // Only beaker-class glassware counts, wherever it is — bench, hand, machine
    // slot, delivery window. Pills, bottles and syringes are minted by the
    // Mixing Chamber without limit, so counting them would let a player who
    // spammed packaging starve themselves of the beakers that make packaging
    // possible.
    // That is also why cargo never resupplies a syringe: it is something you
    // make, not something you order.
    let live = glassware
        .iter()
        .filter(|container| {
            matches!(
                container.kind,
                ContainerKind::Beaker | ContainerKind::LargeBeaker
            )
        })
        .count();

    // A requisition raises the target *and* the crate it can arrive in. Raising
    // only the target would make the purchase a no-op in exactly the case it is
    // bought for: a lab already short by more than a crate is capped at
    // `crate_max` either way.
    let bonus = shift.requisition.glassware;
    let needed = restock_order(
        live,
        supply.glassware_target + bonus,
        supply.crate_max + bonus,
    );
    if needed == 0 {
        return;
    }

    // Only spent once it has bought something. Zeroed above the early return it
    // could be consumed by a check that delivered nothing at all.
    shift.requisition.glassware = 0;
    // Adds rather than overwrites — a personal purchase already queued this
    // same tick must not be clobbered. See `PendingRestock`'s doc comment.
    pending.deficit += needed;
}

/// Sends cargo in as soon as there is a courier free to come.
fn dispatch_glassware(
    mut commands: Commands,
    mut pending: ResMut<PendingRestock>,
    station: Option<Res<StationData>>,
    present: Query<&CrewMember, crate::crew::NotResident>,
    mut residents: AvailableResidents,
) {
    if pending.is_empty() {
        return;
    }
    let Some(station) = station else {
        return;
    };
    let supply = &station.config.supply;

    // One of him is plenty. He is in the ordinary crew roster too, so without
    // this a restock landing while he is already at the counter with an order
    // would put two of him in the room. Held rather than dropped: he will be
    // gone in a moment, and the crate is already paid for.
    if present.iter().any(|member| member.name == supply.courier) {
        return;
    }
    let Some(def) = station
        .crew
        .iter()
        .find(|member| member.name == supply.courier)
    else {
        warn!(
            "no crew member named '{}' to bring glassware",
            supply.courier
        );
        *pending = PendingRestock::default();
        return;
    };

    let (deficit_beakers, deficit_large) = crate_contents(pending.deficit, supply.large_every);
    let beakers = deficit_beakers + pending.purchased_beakers;
    let large = deficit_large + pending.purchased_large;
    // His own lane at the counter, clear of whoever is queuing for an order.
    let Some(courier) = recall_or_spawn_crew_member(&mut commands, &mut residents, def, -1.1)
    else {
        // This identity exists, but its current owner has not released it.
        // Keep the exact crate contents queued until Sato is free instead of
        // manufacturing a second body or silently losing a paid requisition.
        return;
    };
    commands
        .entity(courier)
        .insert(GlasswareDelivery { beakers, large });
    *pending = PendingRestock::default();
}

/// Puts the crate down once he reaches the counter, then sends him out.
fn unload_glassware(
    mut commands: Commands,
    mut radio: ResMut<RadioLog>,
    stations: Option<Res<DeliveryStations>>,
    mut couriers: Query<(Entity, &CrewMember, &GlasswareDelivery, &mut CrewRoute)>,
) {
    for (entity, member, delivery, mut route) in &mut couriers {
        if route.phase != CrewPhase::Waiting {
            continue;
        }

        let kinds = std::iter::repeat_n(ContainerKind::Beaker, delivery.beakers).chain(
            std::iter::repeat_n(ContainerKind::LargeBeaker, delivery.large),
        );
        let station = stations
            .as_deref()
            .cloned()
            .unwrap_or_default()
            .station(DeliveryLane::Public);
        let across = station.transform.rotation * Vec3::X;
        let span = (delivery.total() as f32 - 1.0) * ITEM_SPACING;
        for (index, kind) in kinds.enumerate() {
            let offset = CRATE_X_OFFSET - span * 0.5 + index as f32 * ITEM_SPACING;
            let (_, height) = kind.dimensions();
            spawn_container(
                &mut commands,
                kind,
                station.drop_position(COUNTER_TOP + height * 0.5) + across * offset,
            );
        }

        radio.push(
            RadioEntry::new(
                channel_for(&member.role),
                format!(
                    "Dropped {} off at the window. Try to hang on to them this time.",
                    describe(delivery)
                ),
            )
            .speaker(&member.name)
            .positive(),
        );
        info!("{} delivered {}", member.name, describe(delivery));

        commands.entity(entity).remove::<GlasswareDelivery>();
        route.leave();
    }
}

/// "3 beakers and a large one" — what he says he brought.
fn describe(delivery: &GlasswareDelivery) -> String {
    let mut parts = Vec::new();
    match delivery.beakers {
        0 => {}
        1 => parts.push("a beaker".to_string()),
        count => parts.push(format!("{count} beakers")),
    }
    match delivery.large {
        0 => {}
        1 => parts.push("a large one".to_string()),
        count => parts.push(format!("{count} large ones")),
    }
    match parts.len() {
        0 => "nothing".to_string(),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().expect("checked non-empty");
            format!("{} and {last}", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::{Bloodstream, Body};
    use crate::crew::{Ambient, StationResident};
    use crate::orders::OrderConfig;
    use bevy::ecs::system::RunSystemOnce;

    fn delivery(beakers: usize, large: usize) -> GlasswareDelivery {
        GlasswareDelivery { beakers, large }
    }

    fn config() -> OrderConfig {
        ron::from_str(include_str!("../../assets/data/station.orders.ron"))
            .expect("order data should parse")
    }

    fn station() -> StationData {
        StationData {
            crew: ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap(),
            config: config(),
        }
    }

    fn restock_app() -> App {
        let mut app = App::new();
        app.insert_resource(station())
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .init_resource::<PendingRestock>()
            .init_resource::<Time>()
            .add_systems(
                Update,
                (order_glassware, dispatch_glassware, unload_glassware).chain(),
            );
        app
    }

    #[test]
    fn a_purchase_merges_with_a_pending_deficit_instead_of_clobbering_it() {
        // Every real beaker is missing, so the periodic check will find a
        // deficit on its very first tick (armed as "due now"). Queue a
        // personal purchase in the same frame the check lands.
        let mut app = restock_app();
        queue_glassware_purchase(&mut app.world_mut().resource_mut::<PendingRestock>(), 2, 1);
        app.update();

        let mut couriers = app.world_mut().query::<&GlasswareDelivery>();
        let delivered = couriers
            .iter(app.world())
            .next()
            .expect("a courier should have been dispatched carrying both halves");
        let supply = &station().config.supply;
        let (deficit_beakers, deficit_large) = crate_contents(
            restock_order(0, supply.glassware_target, supply.crate_max),
            supply.large_every,
        );
        assert_eq!(delivered.beakers, deficit_beakers + 2);
        assert_eq!(delivered.large, deficit_large + 1);
    }

    #[test]
    fn a_purchased_composition_survives_unchanged_when_there_is_no_deficit() {
        // Enough live beakers already in the lab that the very first periodic
        // check finds no deficit at all — isolates the purchase from any
        // deficit rounding, in one single update so there's no already-
        // present courier from an earlier delivery to confuse the picture.
        let mut app = restock_app();
        let target = station().config.supply.glassware_target;
        for _ in 0..target {
            app.world_mut().spawn(Container::new(ContainerKind::Beaker));
        }
        queue_glassware_purchase(&mut app.world_mut().resource_mut::<PendingRestock>(), 3, 2);
        app.update();

        let mut couriers = app.world_mut().query::<&GlasswareDelivery>();
        let delivered = couriers
            .iter(app.world())
            .next()
            .expect("the purchase alone should still dispatch a courier");
        assert_eq!((delivered.beakers, delivered.large), (3, 2));
    }

    #[test]
    fn sato_is_refused_while_he_is_already_present() {
        let mut app = restock_app();
        let supply = station().config.supply;
        app.world_mut().spawn(CrewMember {
            name: supply.courier.clone(),
            role: "Cargo".to_string(),
        });
        queue_glassware_purchase(&mut app.world_mut().resource_mut::<PendingRestock>(), 4, 0);
        app.update();

        assert!(
            app.world_mut()
                .query::<&GlasswareDelivery>()
                .iter(app.world())
                .next()
                .is_none(),
            "no delivery should dispatch while he's already at the counter"
        );
        let pending = app.world().resource::<PendingRestock>();
        assert!(
            !pending.is_empty(),
            "the purchase must stay queued, not be lost"
        );
    }

    #[test]
    fn an_idle_resident_sato_carries_the_crate_without_a_duplicate_body() {
        let mut app = restock_app();
        let supply = station().config.supply;
        let sato = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: supply.courier.clone(),
                    role: "Cargo".to_string(),
                },
                Body::default(),
                Bloodstream::default(),
                CrewRoute::arrival(0.0),
                Ambient::new(30.0),
                StationResident,
            ))
            .id();
        queue_glassware_purchase(&mut app.world_mut().resource_mut::<PendingRestock>(), 4, 0);

        app.update();

        assert!(app.world().get::<GlasswareDelivery>(sato).is_some());
        let same_name = app
            .world_mut()
            .query::<&CrewMember>()
            .iter(app.world())
            .filter(|member| member.name == supply.courier)
            .count();
        assert_eq!(
            same_name, 1,
            "dispatch must reuse the station resident rather than clone Sato"
        );
        assert!(app.world().resource::<PendingRestock>().is_empty());
    }

    #[test]
    fn a_busy_resident_sato_keeps_the_crate_queued_without_a_duplicate_body() {
        let mut app = restock_app();
        let supply = station().config.supply;
        app.world_mut().spawn((
            CrewMember {
                name: supply.courier.clone(),
                role: "Cargo".to_string(),
            },
            Body::default(),
            Bloodstream::default(),
            StationResident,
        ));
        queue_glassware_purchase(&mut app.world_mut().resource_mut::<PendingRestock>(), 4, 0);

        app.update();

        let same_name = app
            .world_mut()
            .query::<&CrewMember>()
            .iter(app.world())
            .filter(|member| member.name == supply.courier)
            .count();
        assert_eq!(same_name, 1, "a busy resident must never be cloned");
        assert!(
            app.world_mut()
                .query::<&GlasswareDelivery>()
                .iter(app.world())
                .next()
                .is_none(),
            "the busy resident must keep its existing commitment"
        );
        assert!(
            !app.world().resource::<PendingRestock>().is_empty(),
            "the exact crate contents should retry once Sato is free"
        );
    }

    #[test]
    fn canceling_a_courier_requeues_the_exact_crate_once() {
        let mut app = restock_app();
        let courier = app.world_mut().spawn(delivery(3, 2)).id();

        app.world_mut()
            .run_system_once(move |mut commands: Commands| {
                cancel_glassware_delivery(&mut commands, courier);
                cancel_glassware_delivery(&mut commands, courier);
            })
            .unwrap();

        assert!(app.world().get::<GlasswareDelivery>(courier).is_none());
        let pending = app.world().resource::<PendingRestock>();
        assert_eq!(pending.purchased_beakers, 3);
        assert_eq!(pending.purchased_large, 2);
        assert_eq!(pending.deficit, 0, "exact counts must not be rerounded");
    }

    #[test]
    fn a_crate_is_described_in_plain_english() {
        assert_eq!(describe(&delivery(3, 1)), "3 beakers and a large one");
        assert_eq!(describe(&delivery(1, 0)), "a beaker");
        assert_eq!(describe(&delivery(0, 2)), "2 large ones");
    }

    #[test]
    fn a_requisitioned_crate_can_carry_more_than_a_plain_one() {
        // `order_glassware` raises the cap alongside the target. Guarding it
        // here as well as in `shift` because the two have to move together:
        // raising one without the other silently voids the purchase.
        let plain = restock_order(0, 6, 4);
        let requisitioned = restock_order(0, 6 + 2, 4 + 2);
        assert!(requisitioned > plain);
    }

    #[test]
    fn a_crate_never_lands_on_top_of_a_sample_vial() {
        // Vials drop at COUNTER_SPOT.x; the crate is laid out around
        // COUNTER_SPOT.x + CRATE_X_OFFSET. A crate wide enough to reach back
        // across would hide the vial inside a beaker.
        let widest = crate_contents(4, 3);
        let span = ((widest.0 + widest.1) as f32 - 1.0) * ITEM_SPACING;
        let nearest = CRATE_X_OFFSET + span * 0.5;
        assert!(
            nearest < -0.15,
            "the crate reaches to {nearest} of the vial drop"
        );
    }
}
