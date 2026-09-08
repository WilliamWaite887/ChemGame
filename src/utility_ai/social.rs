//! Personal recovery and pair social actions on the shared opportunity seam.
//!
//! Neither action is department work, so neither is published to the
//! [`JobBoard`]: a resident rests or talks because of their own accumulated
//! pressure, not because a qualified worker claimed a ticket. Both are offered
//! through [`UtilityOpportunityBuffer`] and compete against that resident's own
//! job in one scoring pass.
//!
//! This module never selects an actor, moves one, or writes a transform. It
//! publishes offers in `BuildContext` and applies outcomes in `Resolve`.

use bevy::prelude::*;

use super::jobs::UtilitySpots;
use super::{
    stable_text_key, ActionResult, ActionTarget, Normalized, ReservationKey, UtilityActionId,
    UtilityActionResolved, UtilityAgent, UtilityBucket, UtilityOpportunity,
    UtilityOpportunityBuffer,
};
use crate::crew::CrewMember;

/// Authored break and social affordances. These are ordinary map-validated
/// `utility_spot` markers, so they are reservable and route-checked like any
/// workstation.
/// Somewhere to sit in Service, in the order a resident should prefer.
///
/// The two lounge seats used to be the whole list, which meant the station's
/// only social room could seat *two* of its thirty residents — and the seats are
/// capacity 1, so a third arrival was filtered out by occupancy and fell back to
/// standing at their post. The twelve dining tables are capacity 4 apiece, so a
/// meal or a break now reads as people sharing a table rather than queueing for
/// a bench. Tables first: a resident with a choice should sit *with* somebody.
///
/// Forty-eight table places for thirty residents is deliberate headroom, not an
/// estimate of demand. Occupancy filtering is per spot, so a room sized exactly
/// to the crew puts the last arrivals back to standing at their posts the moment
/// two tables happen to fill.
const LOUNGE_SEATS: [&str; 14] = [
    "service.table.a",
    "service.table.b",
    "service.table.c",
    "service.table.d",
    "service.table.e",
    "service.table.f",
    "service.table.g",
    "service.table.h",
    "service.table.i",
    "service.table.j",
    "service.table.k",
    "service.table.l",
    "service.lounge.seat.1",
    "service.lounge.seat.2",
];
const GATHER_SPOT: &str = "service.lounge.gather";

// Thresholds, durations, recovery amounts, standing gain, the pair cooldown and
// the room-appeal weights now live in `assets/data/station.needs.ron` and reach
// these systems as `NeedsTuning`. Only the map-geometry names stay here, above:
// which `utility_spot` is a seat is a fact about the map, not a balance knob.

/// Authority-only record of who recently spoke with whom.
///
/// Keyed by the ordered pair so `(a, b)` and `(b, a)` are the same entry, which
/// is what makes the cooldown symmetric regardless of who initiated.
#[derive(Resource, Default, Debug)]
pub struct SocialCooldowns {
    recent: Vec<(u64, f32)>,
}

impl SocialCooldowns {
    fn key(a: &str, b: &str) -> u64 {
        let (first, second) = if a <= b { (a, b) } else { (b, a) };
        stable_text_key(&format!("{first}\u{1}{second}"))
    }

    /// `window` is the authored `pair_cooldown_seconds`. Passed in rather than
    /// read from a resource here so the cooldown stays a pure function of its
    /// inputs and can be tested without an `App`.
    fn ready(&self, a: &str, b: &str, now: f32, window: f32) -> bool {
        let key = Self::key(a, b);
        !self
            .recent
            .iter()
            .any(|(entry, at)| *entry == key && now - at < window)
    }

    fn note(&mut self, a: &str, b: &str, now: f32, window: f32) {
        let key = Self::key(a, b);
        self.recent
            .retain(|(entry, at)| *entry != key && now - at < window);
        self.recent.push((key, now));
    }

    pub fn clear(&mut self) {
        self.recent.clear();
    }
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<SocialCooldowns>()
        .init_resource::<Conversing>()
        .init_resource::<RoomAppeal>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_social_state
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            Update,
            derive_room_appeal
                .in_set(super::UtilityAiSet::MaintainNeeds)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (offer_rest, offer_conversation)
                .in_set(super::OpportunityProviders)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (track_conversation_attendance, apply_social_outcomes)
                .chain()
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

fn reset_social_state(
    mut cooldowns: ResMut<SocialCooldowns>,
    mut present: ResMut<Conversing>,
    mut appeal: ResMut<RoomAppeal>,
) {
    cooldowns.clear();
    present.present.clear();
    *appeal = RoomAppeal::default();
}

/// How attractive the Service room currently is as somewhere to spend time.
///
/// This is a *modifier*, never a command. Nothing here tells a resident to go
/// to Service; it scales how appealing a break there looks against that
/// resident's own work. Every input is a real, inspectable world fact, so a
/// player can see why the room emptied out.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct RoomAppeal {
    /// Multiplier on the appeal of a Service break. `1.0` is neutral.
    pub service: f32,
    /// The dominant reason, for debug traces and a future crew menu. This is
    /// qualitative on purpose — it never exposes a score.
    pub service_reason: RoomAppealReason,
}

impl Default for RoomAppeal {
    fn default() -> Self {
        Self {
            service: 1.0,
            service_reason: RoomAppealReason::Ordinary,
        }
    }
}

/// Why a room is currently more or less attractive. Ordered so the most
/// player-relevant explanation wins when several apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoomAppealReason {
    #[default]
    Ordinary,
    /// Food is out and seats are free.
    Welcoming,
    /// Nothing to eat.
    NoFood,
    /// Spent plates left on the tables.
    Dirty,
    /// Every seat and the gathering spot are taken.
    Crowded,
}

/// Derives Service appeal from what is actually in the room.
///
/// Runs in `MaintainNeeds`, before offers are published, so providers in the
/// same frame read a current value.
fn derive_room_appeal(
    mut appeal: ResMut<RoomAppeal>,
    tuning: Res<super::NeedsTuning>,
    spots: Res<UtilitySpots>,
    reservations: Res<super::ReservationBook>,
    meals: Query<&super::MealBatch>,
) {
    let served = meals
        .iter()
        .filter(|batch| batch.stage == super::MealStage::Served && batch.servings_remaining > 0)
        .count();
    let spent = meals
        .iter()
        .filter(|batch| batch.stage == super::MealStage::Empty)
        .count();

    // "Full" means every authored break affordance is claimed, which is a fact
    // the reservation book already owns rather than a guess about crowding.
    let break_spots = LOUNGE_SEATS.len() + 1;
    let taken = LOUNGE_SEATS
        .iter()
        .chain(std::iter::once(&GATHER_SPOT))
        .filter(|id| {
            spots.get(id).is_some_and(|spot| {
                reservations.claims_on(&ReservationKey(format!("utility.spot.{id}")))
                    >= spot.capacity
            })
        })
        .count();

    let mut value = 1.0_f32;
    let mut reason;
    if served > 0 {
        value += tuning.appeal.food_bonus;
        reason = RoomAppealReason::Welcoming;
    } else {
        value -= tuning.appeal.no_food_penalty;
        reason = RoomAppealReason::NoFood;
    }
    // Spent plates on the tables read as neglect. Cleanup is a real Service
    // job, so this recovers on its own once someone clears them.
    if spent > 0 {
        value -= tuning.appeal.dirty_penalty;
        reason = RoomAppealReason::Dirty;
    }
    if taken >= break_spots {
        value -= tuning.appeal.crowded_penalty;
        reason = RoomAppealReason::Crowded;
    }

    appeal.service = value.clamp(0.1, 1.75);
    appeal.service_reason = reason;
}

/// Offers a seat to any resident carrying enough fatigue to want one.
///
/// Seats are capacity-one, so two tired residents take different chairs rather
/// than stacking on the same affordance.
fn offer_rest(
    mut buffer: ResMut<UtilityOpportunityBuffer>,
    tuning: Res<super::NeedsTuning>,
    spots: Res<UtilitySpots>,
    room: Res<RoomAppeal>,
    residents: Query<(Entity, &super::NpcNeeds, &crate::body::Bloodstream), With<UtilityAgent>>,
) {
    for (resident, needs, blood) in &residents {
        if needs.fatigue < tuning.thresholds.tired_enough_to_rest || !super::fit_for_leisure(blood)
        {
            continue;
        }
        // Fatigue sets the base appeal; the room and the body's own chemistry
        // scale it. A sedated or withdrawn resident does not go and sit in a
        // crowded, foodless lounge.
        let scaled = needs.fatigue * room.service * super::social_disposition(blood);
        let appeal = Normalized::new(scaled.clamp(0.0, 1.0))
            .expect("the scaled value is clamped to the normalized range");
        for seat in LOUNGE_SEATS {
            let Some(spot) = spots.get(seat) else {
                continue;
            };
            buffer.offer(
                UtilityOpportunity::new(
                    resident,
                    UtilityActionId::Rest,
                    // Routine, not Idle. Buckets are strict priority classes,
                    // so an Idle break could never outrank `IdleObserve`, and
                    // an exhausted resident would stand around instead of
                    // sitting down. A break is a legitimate use of a shift.
                    UtilityBucket::Routine,
                    stable_text_key(seat),
                    appeal,
                )
                .with_target(ActionTarget::Point(spot.at))
                .with_reservation(
                    ReservationKey(format!("utility.spot.{seat}")),
                    spot.capacity,
                )
                .with_timing(
                    tuning.actions.rest_seconds,
                    tuning.actions.rest_seconds + 60.0,
                ),
            );
        }
    }
}

/// Offers a conversation to each member of an eligible pair.
///
/// Both residents must independently want to talk and independently choose the
/// offer. Nothing here forces a partner to attend: each publishes its own offer
/// against the shared gathering spot, and the spot's capacity of two is what
/// makes them meet. A third resident finds it full and does something else.
fn offer_conversation(
    time: Res<Time>,
    tuning: Res<super::NeedsTuning>,
    mut buffer: ResMut<UtilityOpportunityBuffer>,
    spots: Res<UtilitySpots>,
    cooldowns: Res<SocialCooldowns>,
    room: Res<RoomAppeal>,
    residents: Query<
        (
            Entity,
            &CrewMember,
            &super::NpcNeeds,
            &crate::body::Bloodstream,
        ),
        With<UtilityAgent>,
    >,
) {
    let Some(spot) = spots.get(GATHER_SPOT) else {
        return;
    };
    let now = time.elapsed_secs();

    // Sorted by stable name so a fixed population always produces the same
    // offer order, keeping deterministic near-best selection reproducible.
    let mut lonely: Vec<_> = residents
        .iter()
        .filter(|(_, _, needs, blood)| {
            needs.social >= tuning.thresholds.lonely_enough_to_talk && super::fit_for_leisure(blood)
        })
        .collect();
    if lonely.len() < 2 {
        return;
    }
    lonely.sort_by(|(_, a, _, _), (_, b, _, _)| a.name.cmp(&b.name));

    for (resident, member, needs, blood) in &lonely {
        // Only offer when at least one other willing partner is off cooldown
        // with this resident, so nobody walks to the lounge to sit alone.
        let has_partner = lonely.iter().any(|(other, other_member, _, _)| {
            *other != *resident
                && cooldowns.ready(
                    &member.name,
                    &other_member.name,
                    now,
                    tuning.actions.pair_cooldown_seconds,
                )
        });
        if !has_partner {
            continue;
        }
        // A withdrawn resident, or an unwelcoming room, makes company less
        // attractive without ever forbidding it.
        let scaled = needs.social * room.service * super::social_disposition(blood);
        let appeal = Normalized::new(scaled.clamp(0.0, 1.0))
            .expect("the scaled value is clamped to the normalized range");
        buffer.offer(
            UtilityOpportunity::new(
                *resident,
                UtilityActionId::Socialize,
                UtilityBucket::Routine,
                stable_text_key(GATHER_SPOT),
                appeal,
            )
            .with_target(ActionTarget::Point(spot.at))
            .with_reservation(
                ReservationKey(format!("utility.spot.{GATHER_SPOT}")),
                spot.capacity,
            )
            .with_timing(
                tuning.actions.socialize_seconds,
                tuning.actions.socialize_seconds + 60.0,
            ),
        );
    }
}

/// Tracks who is currently performing a conversation at the shared spot.
///
/// A conversation is a shared occasion, not a simultaneous instant. Agents have
/// staggered decision clocks, so two residents who talk together almost never
/// *finish* on the same tick — pairing on co-completion would silently never
/// fire. Overlapping attendance is the honest test of "they talked."
#[derive(Resource, Default, Debug)]
struct Conversing {
    present: Vec<(Entity, String)>,
}

/// Notes who is mid-conversation, before outcomes are applied.
fn track_conversation_attendance(
    mut present: ResMut<Conversing>,
    talking: Query<(Entity, &CrewMember, &super::CurrentAction), With<UtilityAgent>>,
) {
    for (entity, member, action) in &talking {
        if action.key.action != UtilityActionId::Socialize {
            continue;
        }
        if !present.present.iter().any(|(who, _)| *who == entity) {
            present.present.push((entity, member.name.clone()));
        }
    }
}

/// Applies the relief and relationship outcome of a finished personal action.
fn apply_social_outcomes(
    time: Res<Time>,
    tuning: Res<super::NeedsTuning>,
    mut results: MessageReader<UtilityActionResolved>,
    mut cooldowns: ResMut<SocialCooldowns>,
    mut present: ResMut<Conversing>,
    mut shift: Option<ResMut<crate::orders::Shift>>,
    mut residents: Query<(&CrewMember, &mut super::NpcNeeds), With<UtilityAgent>>,
) {
    let now = time.elapsed_secs();

    for result in results.read() {
        if result.result != ActionResult::Completed {
            // An abandoned conversation leaves the roster, so an interrupted
            // attendee is not later credited as a partner.
            if result.key.action == UtilityActionId::Socialize {
                present.present.retain(|(who, _)| *who != result.agent);
            }
            continue;
        }
        match result.key.action {
            UtilityActionId::Rest => {
                if let Ok((_, mut needs)) = residents.get_mut(result.agent) {
                    needs.relieve_fatigue(tuning.actions.rest_recovery);
                }
            }
            UtilityActionId::Socialize => {
                let Ok((member, mut needs)) = residents.get_mut(result.agent) else {
                    continue;
                };
                // Relief is personal: it applies even to someone whose partner
                // has already left.
                needs.relieve_social(tuning.actions.social_recovery);
                let name = member.name.clone();

                // Standing and the cooldown need a real partner. Anyone else
                // who shared the spot during this conversation counts; a lone
                // sitter gets the relief above and nothing else, so nobody can
                // farm reputation by talking to themselves.
                let partners: Vec<String> = present
                    .present
                    .iter()
                    .filter(|(who, other)| *who != result.agent && *other != name)
                    .map(|(_, other)| other.clone())
                    .collect();
                present.present.retain(|(who, _)| *who != result.agent);
                if partners.is_empty() {
                    continue;
                }
                // Credit both sides now. Staggered decision clocks mean the
                // partner will finish on a later tick with an empty roster, so
                // waiting for their own resolution would credit only whoever
                // happened to leave first.
                if let Some(shift) = shift.as_mut() {
                    shift.adjust_npc(&name, tuning.actions.social_standing_gain);
                    for partner in &partners {
                        shift.adjust_npc(partner, tuning.actions.social_standing_gain);
                    }
                }
                for partner in partners {
                    cooldowns.note(&name, &partner, now, tuning.actions.pair_cooldown_seconds);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::{Bloodstream, Body};
    use crate::crew::{CrewPosts, CrewRoute, ErrandResolved};
    use crate::utility_ai::{
        advance_npc_needs, begin_reference_actions, clear_opportunity_buffer,
        consume_utility_arrivals, perform_reference_actions, resolve_reference_actions,
        select_reference_actions, tick_current_actions, NpcNeeds, ReservationBook,
        UtilityControlBundle, UtilityDecisionLog,
    };

    /// Mirrors the authored lounge layout without depending on the real map.
    fn insert_lounge(app: &mut App) {
        let mut spots = app.world_mut().resource_mut::<UtilitySpots>();
        spots.insert(LOUNGE_SEATS[0], Vec3::new(2.0, 0.0, 1.0), 1);
        spots.insert(LOUNGE_SEATS[1], Vec3::new(3.0, 0.0, 1.0), 1);
        spots.insert(GATHER_SPOT, Vec3::new(2.5, 0.0, -1.0), 2);
    }

    fn social_app() -> App {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<UtilitySpots>()
            .init_resource::<UtilityOpportunityBuffer>()
            .init_resource::<SocialCooldowns>()
            .insert_resource(super::super::NeedsTuning::authored().clone())
            .init_resource::<Conversing>()
            .init_resource::<RoomAppeal>()
            .init_resource::<crate::orders::Shift>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    advance_npc_needs,
                    derive_room_appeal,
                    clear_opportunity_buffer,
                    offer_rest,
                    offer_conversation,
                    tick_current_actions,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    track_conversation_attendance,
                    apply_social_outcomes,
                )
                    .chain(),
            );
        insert_lounge(&mut app);
        app
    }

    fn spawn_resident(app: &mut App, name: &str, seed: u64, needs: NpcNeeds, at: Vec3) -> Entity {
        let mut bundle = UtilityControlBundle::new(UtilityAgent::new(seed, 0));
        bundle.needs = needs;
        app.world_mut()
            .spawn((
                CrewMember {
                    name: name.into(),
                    role: "Service".into(),
                },
                Transform::from_translation(at),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                bundle,
            ))
            .id()
    }

    fn run(app: &mut App, ticks: usize) {
        for _ in 0..ticks {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        }
    }

    #[test]
    fn a_tired_resident_takes_a_seat_and_recovers() {
        let mut app = social_app();
        let tired = spawn_resident(
            &mut app,
            "Cook Navarro",
            5,
            NpcNeeds {
                fatigue: 0.95,
                ..NpcNeeds::default()
            },
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );

        run(&mut app, 400);

        let needs = app.world().get::<NpcNeeds>(tired).unwrap();
        assert!(
            needs.fatigue < 0.95,
            "the tired resident never rested: fatigue {}",
            needs.fatigue
        );
        // It physically reached one of the two authored seats.
        let at = app.world().get::<Transform>(tired).unwrap().translation;
        assert!(
            LOUNGE_SEATS.iter().any(|seat| {
                let spot = app.world().resource::<UtilitySpots>().get(seat).unwrap();
                at.distance(spot.at.with_y(crate::crew::BODY_OFFSET)) <= 1.0
            }),
            "the resident recovered at {at:?} without reaching a seat"
        );
    }

    /// The authored file is actually consulted, not merely present.
    ///
    /// This is the test that earns moving tuning into RON at all. Every other
    /// test here passes just as happily against hardcoded constants that
    /// *happen to match* the shipped file — verified by falsification: replacing
    /// `tuning.thresholds.tired_enough_to_rest` with the literal `0.45` left
    /// all nine social tests green. So this one changes the value at runtime
    /// and requires behaviour to follow.
    ///
    /// A resident at 0.5 fatigue rests under the authored 0.45 threshold and
    /// must *not* rest once the threshold is raised above them. If someone
    /// re-inlines a constant, this fails and nothing else does.
    #[test]
    fn raising_the_authored_threshold_changes_who_takes_a_break() {
        let rested_at = |threshold: f32| {
            let mut app = social_app();
            app.world_mut()
                .resource_mut::<super::super::NeedsTuning>()
                .thresholds
                .tired_enough_to_rest = threshold;
            let resident = spawn_resident(
                &mut app,
                "Cook Navarro",
                5,
                NpcNeeds {
                    fatigue: 0.5,
                    ..NpcNeeds::default()
                },
                Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
            );
            run(&mut app, 400);
            app.world().get::<NpcNeeds>(resident).unwrap().fatigue
        };

        // Below the authored threshold of 0.45, so this resident breaks.
        assert!(
            rested_at(0.45) < 0.5,
            "a resident over the threshold should have rested"
        );
        // Raise the bar above them and the same resident stays at their post.
        assert!(
            rested_at(0.8) >= 0.5,
            "raising the authored threshold must stop the break; \
             if this passes with a hardcoded constant the tuning is decorative"
        );
    }

    /// A resident below the fatigue threshold must keep working rather than
    /// drifting to the lounge whenever a seat happens to be free.
    #[test]
    fn a_rested_resident_is_never_offered_a_seat() {
        let mut app = social_app();
        spawn_resident(
            &mut app,
            "Cook Navarro",
            5,
            NpcNeeds {
                fatigue: 0.05,
                ..NpcNeeds::default()
            },
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );
        app.update();
        assert!(
            app.world()
                .resource::<UtilityOpportunityBuffer>()
                .iter()
                .all(|offer| offer.action != UtilityActionId::Rest),
            "rest was offered to a resident who is not tired"
        );
    }

    /// Two lonely residents meet, both recover, and both gain a little
    /// standing. A conversation is a pair action: one resident alone must not
    /// produce it.
    #[test]
    fn two_lonely_residents_talk_and_both_gain_standing() {
        let mut app = social_app();
        let lonely = NpcNeeds {
            social: 0.95,
            ..NpcNeeds::default()
        };
        let first = spawn_resident(
            &mut app,
            "Cook Navarro",
            5,
            lonely,
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );
        let second = spawn_resident(
            &mut app,
            "Attendant Mensah",
            9,
            lonely,
            Vec3::new(-3.0, crate::crew::BODY_OFFSET, 0.0),
        );

        run(&mut app, 400);

        for (who, label) in [(first, "Cook Navarro"), (second, "Attendant Mensah")] {
            let needs = app.world().get::<NpcNeeds>(who).unwrap();
            assert!(
                needs.social < 0.95,
                "{label} never had a conversation: social {}",
                needs.social
            );
        }
        let shift = app.world().resource::<crate::orders::Shift>();
        assert!(
            shift.npc_standing("Cook Navarro") > 0 && shift.npc_standing("Attendant Mensah") > 0,
            "both participants should gain a little standing"
        );
    }

    /// One resident cannot hold a conversation with nobody. The offer must not
    /// even be published, so a lone lonely worker keeps working.
    #[test]
    fn a_lone_resident_is_never_offered_a_conversation() {
        let mut app = social_app();
        spawn_resident(
            &mut app,
            "Cook Navarro",
            5,
            NpcNeeds {
                social: 0.95,
                ..NpcNeeds::default()
            },
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );
        app.update();
        assert!(
            app.world()
                .resource::<UtilityOpportunityBuffer>()
                .iter()
                .all(|offer| offer.action != UtilityActionId::Socialize),
            "a conversation was offered with no available partner"
        );
    }

    /// The cooldown must suppress a real offer, not merely answer `ready()`
    /// correctly. With only two residents, a pair on cooldown leaves nobody
    /// eligible, so the lounge stays empty until the window passes.
    #[test]
    fn a_pair_on_cooldown_is_not_offered_a_conversation() {
        let mut app = social_app();
        let lonely = NpcNeeds {
            social: 0.95,
            ..NpcNeeds::default()
        };
        spawn_resident(
            &mut app,
            "Cook Navarro",
            5,
            lonely,
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );
        spawn_resident(
            &mut app,
            "Attendant Mensah",
            9,
            lonely,
            Vec3::new(-3.0, crate::crew::BODY_OFFSET, 0.0),
        );
        // Control: with no cooldown recorded, the offer is published.
        app.update();
        assert!(
            app.world()
                .resource::<UtilityOpportunityBuffer>()
                .iter()
                .any(|offer| offer.action == UtilityActionId::Socialize),
            "the control offer was never published, so this test proves nothing"
        );

        let now = app.world().resource::<Time>().elapsed_secs();
        let window = app
            .world()
            .resource::<super::super::NeedsTuning>()
            .actions
            .pair_cooldown_seconds;
        app.world_mut().resource_mut::<SocialCooldowns>().note(
            "Cook Navarro",
            "Attendant Mensah",
            now,
            window,
        );
        app.update();

        assert!(
            app.world()
                .resource::<UtilityOpportunityBuffer>()
                .iter()
                .all(|offer| offer.action != UtilityActionId::Socialize),
            "a pair still on cooldown was offered another conversation"
        );
    }

    /// Appeal must follow the room's real contents, and each state must name
    /// the fact a player could go and look at.
    #[test]
    fn room_appeal_follows_what_is_actually_in_the_room() {
        fn appeal_with(batches: &[(super::super::MealStage, u8)]) -> RoomAppeal {
            let mut app = social_app();
            for (index, (stage, servings)) in batches.iter().enumerate() {
                app.world_mut().spawn((
                    super::super::MealBatch {
                        id: index as u64 + 1,
                        recipe: super::super::ServiceRecipe::GardenPlate,
                        stage: *stage,
                        servings_remaining: *servings,
                        hosted: true,
                        quality_percent: 100,
                    },
                    Transform::default(),
                ));
            }
            app.update();
            app.world().resource::<RoomAppeal>().clone()
        }

        let empty = appeal_with(&[]);
        assert_eq!(empty.service_reason, RoomAppealReason::NoFood);

        let stocked = appeal_with(&[(super::super::MealStage::Served, 4)]);
        assert_eq!(stocked.service_reason, RoomAppealReason::Welcoming);
        assert!(
            stocked.service > empty.service,
            "a room with food out must be more appealing than a bare one"
        );

        // Spent plates left on the tables read as neglect, and cleanup is a
        // real Service job, so this recovers on its own.
        let dirty = appeal_with(&[
            (super::super::MealStage::Served, 4),
            (super::super::MealStage::Empty, 0),
        ]);
        assert_eq!(dirty.service_reason, RoomAppealReason::Dirty);
        assert!(
            dirty.service < stocked.service,
            "uncleared plates must cost appeal"
        );

        // An exhausted batch is not food.
        let exhausted = appeal_with(&[(super::super::MealStage::Served, 0)]);
        assert_eq!(exhausted.service_reason, RoomAppealReason::NoFood);
    }

    /// Chemistry statuses must change whether a resident wants company, using
    /// only the existing bloodstream rather than a second mood simulation.
    #[test]
    fn mood_chemistry_changes_whether_a_resident_seeks_company() {
        fn disposition_with(kind: chem_sim::StatusKind, intensity: f32) -> f32 {
            let mut blood = Bloodstream::default();
            blood.0.add_status(kind, 60.0, intensity);
            super::super::social_disposition(&blood)
        }

        let neutral = super::super::social_disposition(&Bloodstream::default());
        assert!((neutral - 1.0).abs() < f32::EPSILON, "neutral must be 1.0");

        // Sustained good mood makes crew linger; the authored effect docs ask
        // for exactly this rather than treating euphoria as drunkenness.
        assert!(disposition_with(chem_sim::StatusKind::Happiness, 1.0) > neutral);
        assert!(disposition_with(chem_sim::StatusKind::Euphoric, 1.0) > neutral);

        // Withdrawal. Paranoia is a flight response, so it outweighs sadness.
        let sad = disposition_with(chem_sim::StatusKind::Sadness, 1.0);
        let paranoid = disposition_with(chem_sim::StatusKind::Paranoid, 1.0);
        assert!(sad < neutral, "sadness must reduce willingness to linger");
        assert!(
            paranoid < sad,
            "a flight response must withdraw harder than plain sadness"
        );

        // It stays a bounded modifier, never a veto or a runaway multiplier.
        for kind in [
            chem_sim::StatusKind::Happiness,
            chem_sim::StatusKind::Paranoid,
        ] {
            let extreme = disposition_with(kind, 50.0);
            assert!(
                (0.1..=1.75).contains(&extreme),
                "disposition escaped its bounds at extreme intensity: {extreme}"
            );
        }
    }

    /// Heavy sedation should suppress voluntary leisure outright rather than
    /// merely ranking it lower — someone that far under is not deciding to go
    /// and socialize.
    #[test]
    fn a_sedated_resident_is_not_offered_leisure() {
        let mut app = social_app();
        let tired = NpcNeeds {
            fatigue: 0.95,
            social: 0.95,
            ..NpcNeeds::default()
        };
        let first = spawn_resident(
            &mut app,
            "Cook Navarro",
            5,
            tired,
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );
        let second = spawn_resident(
            &mut app,
            "Attendant Mensah",
            9,
            tired,
            Vec3::new(-3.0, crate::crew::BODY_OFFSET, 0.0),
        );
        // Control: awake, both are offered a break and company.
        app.update();
        assert!(
            app.world()
                .resource::<UtilityOpportunityBuffer>()
                .iter()
                .any(|offer| offer.action == UtilityActionId::Rest),
            "the control offer was never published, so this test proves nothing"
        );

        for who in [first, second] {
            let mut blood = app.world_mut().get_mut::<Bloodstream>(who).unwrap();
            blood
                .0
                .add_status(chem_sim::StatusKind::Sedated, 120.0, 3.0);
        }
        app.update();

        let buffer = app.world().resource::<UtilityOpportunityBuffer>();
        assert!(
            buffer.iter().all(|offer| {
                offer.action != UtilityActionId::Rest && offer.action != UtilityActionId::Socialize
            }),
            "a heavily sedated resident was still offered leisure"
        );
    }

    /// The pair cooldown is symmetric and stops one pair looping forever.
    #[test]
    fn a_pair_that_just_talked_is_not_offered_again_immediately() {
        let mut cooldowns = SocialCooldowns::default();
        // An explicit window rather than the authored one: this test is about
        // the cooldown's *logic* — symmetry, isolation between pairs, and
        // expiry — which must hold for any window a designer picks.
        let window = 90.0;
        assert!(cooldowns.ready("Cook Navarro", "Attendant Mensah", 100.0, window));

        cooldowns.note("Cook Navarro", "Attendant Mensah", 100.0, window);
        assert!(!cooldowns.ready("Cook Navarro", "Attendant Mensah", 110.0, window));
        assert!(
            !cooldowns.ready("Attendant Mensah", "Cook Navarro", 110.0, window),
            "the cooldown must not depend on who is named first"
        );

        // A different partner is unaffected.
        assert!(cooldowns.ready("Cook Navarro", "Chef Dubois", 110.0, window));
        // And the same pair is available again once the window passes.
        assert!(cooldowns.ready(
            "Cook Navarro",
            "Attendant Mensah",
            100.0 + window + 1.0,
            window,
        ));
    }
}
