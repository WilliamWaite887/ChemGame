//! Cargo keeping the lab in glassware.
//!
//! Every hand-off over the counter takes the container with it — the crew walk
//! away holding your beaker, which is true in the original too — and nothing
//! else in the game makes one. The ChemMaster mints pills and bottles from its
//! buffer, but getting anything *into* the buffer needs a beaker, so without a
//! supply line the lab is bricked after five deliveries.
//!
//! Supply arrives the way everything else in this lab does: someone walks in on
//! a timer, puts it on the counter, and goes. The courier is lifted almost
//! wholesale from [`crate::produce`], which already solved this shape.

use bevy::prelude::*;

use crate::containers::{spawn_container, Container, ContainerKind};
use crate::crew::{spawn_crew_member, CrewMember, CrewPhase, CrewRoute};
use crate::lab::{COUNTER_DROP_Z, COUNTER_SPOT, COUNTER_TOP};
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

/// A crate that has been decided on but not yet handed to a courier.
///
/// Ordering and dispatching are separate because the courier may be
/// unavailable the moment a deficit is found — he is on the ordinary crew
/// roster too, so he can still be walking out from an earlier delivery.
/// Ordering happens on its own periodic check; dispatch retries until it
/// takes. Collapsed into one system, a courier who happened to be in the room
/// would cancel the lab's entire resupply, and any requisition paid for it
/// with it.
#[derive(Resource, Default)]
struct PendingRestock(Option<usize>);

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
    let due = match timer.as_mut() {
        Some(t) => t.tick(time.delta()).just_finished(),
        // Nothing scheduled yet: this is the first frame, and the lab should
        // not sit unchecked for a full interval before the first delivery.
        None => true,
    };
    if !due {
        return;
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
    // ChemMaster without limit, so counting them would let a player who spammed
    // packaging starve themselves of the beakers that make packaging possible.
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
    pending.0 = Some(needed);
}

/// Sends cargo in as soon as there is a courier free to come.
fn dispatch_glassware(
    mut commands: Commands,
    mut pending: ResMut<PendingRestock>,
    station: Option<Res<StationData>>,
    present: Query<&CrewMember, crate::crew::NotResident>,
) {
    let Some(needed) = pending.0 else {
        return;
    };
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
        pending.0 = None;
        return;
    };

    let (beakers, large) = crate_contents(needed, supply.large_every);
    // His own lane at the counter, clear of whoever is queuing for an order.
    let courier = spawn_crew_member(&mut commands, def, -1.1);
    commands
        .entity(courier)
        .insert(GlasswareDelivery { beakers, large });
    pending.0 = None;
}

/// Puts the crate down once he reaches the counter, then sends him out.
fn unload_glassware(
    mut commands: Commands,
    mut radio: ResMut<RadioLog>,
    mut couriers: Query<(Entity, &CrewMember, &GlasswareDelivery, &mut CrewRoute)>,
) {
    for (entity, member, delivery, mut route) in &mut couriers {
        if route.phase != CrewPhase::Waiting {
            continue;
        }

        let kinds = std::iter::repeat_n(ContainerKind::Beaker, delivery.beakers).chain(
            std::iter::repeat_n(ContainerKind::LargeBeaker, delivery.large),
        );
        let span = (delivery.total() as f32 - 1.0) * ITEM_SPACING;
        for (index, kind) in kinds.enumerate() {
            let x = COUNTER_SPOT.x + CRATE_X_OFFSET - span * 0.5 + index as f32 * ITEM_SPACING;
            let (_, height) = kind.dimensions();
            spawn_container(
                &mut commands,
                kind,
                Vec3::new(x, COUNTER_TOP + height * 0.5, COUNTER_DROP_Z),
            );
        }

        radio.push(RadioEntry {
            channel: channel_for(&member.role),
            text: format!(
                "{}: dropped {} off at the window. Try to hang on to them this time.",
                member.name,
                describe(delivery)
            ),
            good: true,
        });
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

    fn delivery(beakers: usize, large: usize) -> GlasswareDelivery {
        GlasswareDelivery { beakers, large }
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
