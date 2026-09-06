//! Spoken reports: how knowledge travels between crew members.
//!
//! Before this module, memory was a dead end. A witness who saw a collapse and
//! could not reach it kept the only copy of that fact and walked away with it,
//! because [`Modality::Told`] was modelled but nothing ever published one. The
//! station therefore had two failure modes at once: an emergency nobody could
//! answer, and a witness with no way to raise the alarm.
//!
//! A report is a *physical, spoken act*, not a state transfer. The reporter
//! walks to someone, says a line the player can hear, and only then does the
//! listener learn anything. That constraint is deliberate and load-bearing:
//!
//! - It cannot leak. A report reaches exactly the listener that was walked to,
//!   plus whoever else genuinely overhears it through [`witness_stimuli`]. It
//!   is never a broadcast.
//! - It is observable. A player watching the room sees the alarm being raised
//!   and can act on the same information, rather than crew silently converging.
//! - It is interruptible. A reporter that is knocked out mid-errand never
//!   delivers, and the fact stays where it was.
//!
//! Hearsay is deliberately weaker than sight. A told fact carries reduced
//! confidence and keeps its teller, so a later Security interview can separate
//! "I saw it" from "Vale told me" — and so a chain of retellings decays instead
//! of laundering a rumour into certainty.

use bevy::prelude::*;

use super::perception::{MemoryFact, Modality, NpcMemory, StimulusKind};
use super::{
    stable_text_key, ActionResult, ActionTarget, Normalized, ReservationKey, UtilityActionId,
    UtilityActionResolved, UtilityAgent, UtilityBucket, UtilityOpportunity,
    UtilityOpportunityBuffer,
};
use crate::crew::CrewMember;

/// How long delivering a report takes, in seconds.
const REPORT_SECONDS: f32 = 3.0;

/// A witness only bothers to report something it is reasonably sure of. Below
/// this, a half-glimpsed thing is not worth crossing the station for.
const WORTH_REPORTING: f32 = 0.35;

/// A listener already this certain learns nothing from being told.
///
/// Without this, two witnesses to the same collapse would report it to each
/// other forever. The gap must exceed the confidence penalty below, or a told
/// fact could immediately justify a return report.
const ALREADY_KNOWS: f32 = 0.5;

/// What a told fact retains of the teller's own certainty.
///
/// Strictly less than one so a chain of retellings decays. Hearsay about
/// hearsay is worth progressively less, which is what stops a rumour from
/// hardening into fact by circulating.
const HEARSAY_RETENTION: f32 = 0.6;

/// How far a reporter will walk to tell someone, in metres. Beyond this it is
/// not worth abandoning a post for.
const REPORT_RANGE: f32 = 20.0;

/// Which kinds of knowledge a crew member will cross a room to pass on.
///
/// Deliberately narrow. Ordinary crew raise alarms about danger; they do not
/// file reports about a meal being served. `SuspiciousHandling` is excluded on
/// purpose — an ordinary worker who half-saw something odd is not a witness
/// with a theory, and turning every glimpse into a station-wide accusation
/// would make the antagonist unplayable. Security interviews are the intended
/// route for that, and they pull rather than push.
fn worth_reporting(kind: StimulusKind) -> bool {
    matches!(
        kind,
        StimulusKind::Casualty | StimulusKind::Hazard | StimulusKind::CallForHelp
    )
}

/// Offers each holder of an urgent fact the chance to go and tell someone.
///
/// The offer is `Important`, not `Emergency`: raising the alarm matters more
/// than routine work but must never outrank actually treating the casualty. A
/// responder who can reach the patient should go to the patient.
#[allow(clippy::type_complexity)]
fn offer_reports(
    time: Res<Time>,
    mut buffer: ResMut<UtilityOpportunityBuffer>,
    crew: Query<(Entity, &CrewMember, &Transform, &NpcMemory), With<UtilityAgent>>,
) {
    let now = time.elapsed_secs();

    // Collected first so a reporter can be matched against listeners without
    // holding two borrows of the same query.
    let everyone: Vec<(Entity, &CrewMember, Vec3, &NpcMemory)> = crew
        .iter()
        .map(|(entity, member, transform, memory)| (entity, member, transform.translation, memory))
        .collect();

    for (reporter, _, at, memory) in &everyone {
        // Sorted by confidence so the most certain fact is the one carried,
        // and so a fixed world produces a stable offer order.
        let mut worth: Vec<&MemoryFact> = memory
            .recall(now)
            .filter(|fact| worth_reporting(fact.kind))
            .filter(|fact| fact.confidence_at(now) >= WORTH_REPORTING)
            .collect();
        worth.sort_by(|a, b| {
            b.confidence_at(now)
                .total_cmp(&a.confidence_at(now))
                .then_with(|| a.kind.cmp(&b.kind))
        });

        for fact in worth {
            let Some(subject) = fact.subject else {
                continue;
            };
            let certainty = fact.confidence_at(now);

            // Find the nearest crew member who does not already know this, is
            // not the subject of it, and is not the reporter itself. Ties break
            // on name so the choice is reproducible.
            let mut listeners: Vec<(Entity, &CrewMember, Vec3)> = everyone
                .iter()
                .filter(|(other, _, _, _)| other != reporter && *other != subject)
                .filter(|(_, _, _, their_memory)| {
                    their_memory
                        .best_about(subject, fact.kind, now)
                        .is_none_or(|held| held.confidence_at(now) < ALREADY_KNOWS)
                })
                .filter(|(_, _, their_at, _)| their_at.distance(*at) <= REPORT_RANGE)
                .map(|(entity, other, their_at, _)| (*entity, *other, *their_at))
                .collect();
            if listeners.is_empty() {
                continue;
            }
            listeners.sort_by(|(_, a, a_at), (_, b, b_at)| {
                a_at.distance(*at)
                    .total_cmp(&b_at.distance(*at))
                    .then_with(|| a.name.cmp(&b.name))
            });
            let (listener, _, listener_at) = listeners[0];

            // Urgency is the reporter's own certainty. Someone who barely heard
            // a bang is less driven to raise the alarm than an eyewitness.
            let appeal = Normalized::new(certainty.clamp(0.0, 1.0))
                .expect("decayed confidence is already within the normalized range");
            buffer.offer(
                UtilityOpportunity::new(
                    *reporter,
                    UtilityActionId::ReportIncident,
                    UtilityBucket::Important,
                    stable_text_key(&format!(
                        "report.{:016x}.{:016x}",
                        listener.to_bits(),
                        subject.to_bits()
                    )),
                    appeal,
                )
                // Walking to the listener's live position, not a fixed spot: a
                // report is delivered to a person, who moves.
                .with_target(ActionTarget::Point(listener_at))
                .with_reservation(
                    ReservationKey(format!("report.listener.{:016x}", listener.to_bits())),
                    1,
                )
                .with_timing(REPORT_SECONDS, REPORT_SECONDS + 30.0),
            );
            // One report at a time. Carrying the single most urgent fact keeps
            // a witness from queueing up a monologue.
            break;
        }
    }
}

/// Delivers a completed report: the reporter speaks, and whoever it was walked
/// to learns the fact as hearsay.
#[allow(clippy::type_complexity)]
fn deliver_reports(
    mut commands: Commands,
    time: Res<Time>,
    mut results: MessageReader<UtilityActionResolved>,
    mut witnessed: MessageWriter<super::Stimulus>,
    mut filed: MessageWriter<CasualtyReported>,
    // One query, used twice in sequence: reading the reporter's fact and then
    // writing to listeners are disjoint in time but not in the component set,
    // so two overlapping queries would be a `B0001` access conflict.
    mut crew: Query<(Entity, &Transform, &mut NpcMemory), With<UtilityAgent>>,
) {
    let now = time.elapsed_secs();

    for result in results.read() {
        if result.key.action != UtilityActionId::ReportIncident
            || result.result != ActionResult::Completed
        {
            continue;
        }

        // Re-read the fact at delivery rather than trusting the one that
        // justified the errand. The walk takes time, and confidence decays
        // during it; a reporter states what it still believes on arrival. The
        // borrow ends here so listeners can be written below.
        let Some((here, fact)) = crew.get(result.agent).ok().and_then(|(_, at, memory)| {
            let fact = memory
                .recall(now)
                .filter(|fact| worth_reporting(fact.kind) && fact.subject.is_some())
                .max_by(|a, b| a.confidence_at(now).total_cmp(&b.confidence_at(now)))
                .copied()?;
            Some((at.translation, fact))
        }) else {
            continue;
        };

        // The report is spoken aloud, so the player hears the alarm being
        // raised rather than watching crew converge in silence.
        crate::speech::say(
            &mut commands,
            result.agent,
            spoken_line(fact.kind),
            crate::speech::SpeechTone::Urgent,
        );

        // Whoever the reporter actually reached learns it. Proximity at the
        // moment of delivery is the test, so a listener who wandered off is
        // simply not told — the errand was spent and the fact stays put.
        let mut told_anyone = false;
        for (listener, listener_at, mut their_memory) in &mut crew {
            if listener == result.agent {
                continue;
            }
            if listener_at.translation.distance(here) > CONVERSATION_RANGE {
                continue;
            }
            if their_memory
                .best_about(fact.subject.expect("filtered above"), fact.kind, now)
                .is_some_and(|held| held.confidence_at(now) >= ALREADY_KNOWS)
            {
                continue;
            }
            their_memory.remember(MemoryFact {
                kind: fact.kind,
                subject: fact.subject,
                // Hearsay does not carry the actor. Being told a body went down
                // is not being told who put it there.
                actor: None,
                at: fact.at,
                room_key: fact.room_key,
                modality: Modality::Told,
                source: Some(result.agent),
                confidence: (fact.confidence_at(now) * HEARSAY_RETENTION).clamp(0.0, 1.0),
                learned_at: now,
            });
            told_anyone = true;
        }

        // A spoken alarm is itself perceivable, so anyone in earshot who was
        // not the intended listener may still overhear it. This runs through
        // the ordinary perception path rather than a second delivery rule.
        if told_anyone {
            witnessed.write(
                super::Stimulus::new(StimulusKind::CallForHelp, here)
                    .by(result.agent)
                    .with_strength(0.5),
            );
        }

        // A casualty told to someone who can act on it becomes a formal record.
        //
        // This is the explicit incident-report action: a witness crossing the
        // station and saying "someone's down" is what puts the casualty on the
        // books, rather than the ledger knowing on its own. `filed` never
        // overwrites — `IncidentLedger::create` is the same call every
        // department adapter makes, and `already_tracked` below stops a second
        // report of the same body opening a duplicate case.
        if fact.kind == StimulusKind::Casualty {
            filed.write(CasualtyReported {
                reporter: result.agent,
                subject: fact.subject.expect("filtered above"),
                at: fact.at,
                confidence: fact.confidence_at(now),
            });
        }
    }
}

/// A witness formally reported a casualty they actually saw.
///
/// Separate from the memory exchange above because the two answer different
/// questions: telling a colleague spreads knowledge, filing opens a case. A
/// station where the second happened automatically would not need witnesses at
/// all, which is the thing P6 exists to stop.
#[derive(Message, Clone, Copy, Debug)]
pub struct CasualtyReported {
    pub reporter: Entity,
    pub subject: Entity,
    pub at: Vec3,
    /// How sure the reporter still was on arrival. Carried so a later
    /// investigation can weigh a confident report against a vague one.
    pub confidence: f32,
}

/// How close a listener must be to actually be told. Conversational distance,
/// not earshot: a report is delivered to someone, and overhearing is handled
/// separately by the perception layer.
const CONVERSATION_RANGE: f32 = 3.0;

/// What a reporter says. Deliberately vague about specifics — an NPC raising an
/// alarm states what it saw, not a case file.
fn spoken_line(kind: StimulusKind) -> &'static str {
    match kind {
        StimulusKind::Casualty => "Someone's down — get medical over there!",
        StimulusKind::Hazard => "We've got a hazard back there, it needs sealing.",
        StimulusKind::CallForHelp => "Someone was shouting for help.",
        // Unreachable via `worth_reporting`, but stated rather than panicking:
        // widening that filter should change a line, never crash a shift.
        StimulusKind::SuspiciousHandling => "I saw something I didn't like.",
        StimulusKind::Food => "The food's been sitting out.",
    }
}

pub(super) fn register(app: &mut App) {
    app.add_message::<CasualtyReported>()
        .add_systems(
            Update,
            offer_reports
                .in_set(super::OpportunityProviders)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            deliver_reports
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utility_ai::{ActionKey, UtilityControlBundle};

    fn seen(subject: Entity, confidence: f32, at: f32) -> MemoryFact {
        MemoryFact {
            kind: StimulusKind::Casualty,
            subject: Some(subject),
            actor: None,
            at: Vec3::ZERO,
            room_key: None,
            modality: Modality::Seen,
            source: None,
            confidence,
            learned_at: at,
        }
    }

    /// Builds an app running only the delivery half, which is where knowledge
    /// actually moves. The offer half is exercised separately.
    fn delivery_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .add_message::<UtilityActionResolved>()
            .add_message::<super::super::Stimulus>()
            .add_message::<CasualtyReported>()
            .add_systems(Update, deliver_reports);
        app
    }

    fn crew_at(app: &mut App, seed: u64, x: f32) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: format!("Crew {seed}"),
                    role: "Service".into(),
                },
                Transform::from_xyz(x, 0.0, 0.0),
                UtilityControlBundle::new(UtilityAgent::new(seed, 0)),
            ))
            .id()
    }

    fn finish_report(app: &mut App, agent: Entity) {
        app.world_mut().write_message(UtilityActionResolved {
            agent,
            key: ActionKey {
                action: UtilityActionId::ReportIncident,
                target_key: 0,
            },
            claim: super::super::ReservationOwner {
                agent,
                action_instance: 1,
            },
            result: ActionResult::Completed,
        });
        app.update();
    }

    /// The explicit incident-report action: telling someone puts the casualty
    /// on the books.
    ///
    /// Every other route into the incident ledger is a system that already
    /// knew — a department adapter watching its own worker, or
    /// `handle_crew_collapse` observing a body directly. This is the one that
    /// requires a person to have seen it and said so, which is what makes
    /// perception load-bearing rather than decorative.
    #[test]
    fn a_delivered_casualty_report_is_filed_for_medical() {
        let mut app = delivery_app();
        let victim = app.world_mut().spawn(Transform::default()).id();
        let reporter = crew_at(&mut app, 1, 0.0);
        let _listener = crew_at(&mut app, 2, 1.0);

        app.world_mut()
            .get_mut::<NpcMemory>(reporter)
            .unwrap()
            .remember(seen(victim, 1.0, 0.0));

        finish_report(&mut app, reporter);

        let filed: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<CasualtyReported>>()
            .drain()
            .collect();
        assert_eq!(filed.len(), 1, "delivering the report must file it");
        assert_eq!(filed[0].subject, victim, "the report is about the casualty");
        assert_eq!(
            filed[0].reporter, reporter,
            "the witness who walked over is the reporter"
        );
        assert!(
            filed[0].confidence > 0.0,
            "a filed report carries how sure the witness still was"
        );
    }

    /// A hazard is worth telling a colleague about, but it is not a casualty
    /// and must not open a Medical case.
    #[test]
    fn a_hazard_report_is_not_filed_as_a_casualty() {
        let mut app = delivery_app();
        let subject = app.world_mut().spawn(Transform::default()).id();
        let reporter = crew_at(&mut app, 1, 0.0);
        let _listener = crew_at(&mut app, 2, 1.0);

        let mut fact = seen(subject, 1.0, 0.0);
        fact.kind = StimulusKind::Hazard;
        app.world_mut()
            .get_mut::<NpcMemory>(reporter)
            .unwrap()
            .remember(fact);

        finish_report(&mut app, reporter);

        assert!(
            app.world_mut()
                .resource_mut::<Messages<CasualtyReported>>()
                .drain()
                .next()
                .is_none(),
            "a broken machine is not a patient"
        );
    }

    /// The whole point of the module: a fact that lived in one head reaches
    /// another, and does so as weaker, attributed hearsay rather than as a
    /// clone of the original sighting.
    #[test]
    fn a_delivered_report_moves_knowledge_as_attributed_hearsay() {
        let mut app = delivery_app();
        let victim = app.world_mut().spawn(Transform::default()).id();
        let reporter = crew_at(&mut app, 1, 0.0);
        let listener = crew_at(&mut app, 2, 1.0);

        app.world_mut()
            .get_mut::<NpcMemory>(reporter)
            .unwrap()
            .remember(seen(victim, 1.0, 0.0));

        assert!(
            !app.world()
                .get::<NpcMemory>(listener)
                .unwrap()
                .knows_about(victim, 0.0),
            "the listener must start out ignorant, or this test proves nothing"
        );

        finish_report(&mut app, reporter);

        let told = app
            .world()
            .get::<NpcMemory>(listener)
            .unwrap()
            .best_about(victim, StimulusKind::Casualty, 0.0)
            .copied()
            .expect("the listener must have been told");
        assert_eq!(told.modality, Modality::Told);
        assert_eq!(
            told.source,
            Some(reporter),
            "hearsay must keep its teller so an interview can trace it"
        );
        assert!(
            told.confidence < 1.0,
            "hearsay must be weaker than the sighting it came from"
        );
        assert_eq!(
            told.actor, None,
            "being told a body went down is not being told who did it"
        );
    }

    /// A report is a spoken act delivered to whoever was reached, never a
    /// broadcast. This is the property that keeps knowledge physical.
    #[test]
    fn a_report_does_not_reach_someone_across_the_station() {
        let mut app = delivery_app();
        let victim = app.world_mut().spawn(Transform::default()).id();
        let reporter = crew_at(&mut app, 1, 0.0);
        let nearby = crew_at(&mut app, 2, 1.0);
        let far = crew_at(&mut app, 3, 40.0);

        app.world_mut()
            .get_mut::<NpcMemory>(reporter)
            .unwrap()
            .remember(seen(victim, 1.0, 0.0));
        finish_report(&mut app, reporter);

        // Positive control: without this, the negative assertion below would
        // pass just as well on a delivery system that did nothing at all.
        assert!(
            app.world()
                .get::<NpcMemory>(nearby)
                .unwrap()
                .knows_about(victim, 0.0),
            "someone standing next to the reporter must be told"
        );
        assert!(
            !app.world()
                .get::<NpcMemory>(far)
                .unwrap()
                .knows_about(victim, 0.0),
            "a report must not teleport across the station"
        );
    }

    /// Retelling must decay. Otherwise a rumour launders itself into certainty
    /// by circulating, and hearsay becomes as good as having been there.
    #[test]
    fn retelling_decays_rather_than_hardening_into_fact() {
        let mut app = delivery_app();
        let victim = app.world_mut().spawn(Transform::default()).id();
        let first = crew_at(&mut app, 1, 0.0);
        let second = crew_at(&mut app, 2, 1.0);

        app.world_mut()
            .get_mut::<NpcMemory>(first)
            .unwrap()
            .remember(seen(victim, 1.0, 0.0));
        finish_report(&mut app, first);

        let after_one = app
            .world()
            .get::<NpcMemory>(second)
            .unwrap()
            .best_about(victim, StimulusKind::Casualty, 0.0)
            .unwrap()
            .confidence;

        // The second crew member now retells it to a third.
        let third = crew_at(&mut app, 3, 1.5);
        finish_report(&mut app, second);

        let after_two = app
            .world()
            .get::<NpcMemory>(third)
            .unwrap()
            .best_about(victim, StimulusKind::Casualty, 0.0)
            .expect("the third crew member must have been told")
            .confidence;

        assert!(
            after_two < after_one,
            "each retelling must lose confidence: {after_two} should be below {after_one}"
        );
    }

    /// Someone who already knows is not told again.
    ///
    /// The failure this pins is erosion, not demotion: `NpcMemory::remember`
    /// already keeps the stronger fact, so re-telling an eyewitness cannot
    /// downgrade them. What the gate prevents is a *sufficiently informed*
    /// listener being re-told repeatedly, each retelling refreshing
    /// `learned_at` and resetting the decay clock on secondhand knowledge — so
    /// a rumour two people keep repeating would never fade.
    #[test]
    fn someone_who_already_knows_is_not_told_again() {
        let mut app = delivery_app();
        let victim = app.world_mut().spawn(Transform::default()).id();
        let reporter = crew_at(&mut app, 1, 0.0);
        let listener = crew_at(&mut app, 2, 1.0);

        app.world_mut()
            .get_mut::<NpcMemory>(reporter)
            .unwrap()
            .remember(seen(victim, 1.0, 0.0));
        // The listener already holds a confident secondhand account, above the
        // threshold at which another telling would add anything.
        app.world_mut()
            .get_mut::<NpcMemory>(listener)
            .unwrap()
            .remember(MemoryFact {
                modality: Modality::Told,
                source: Some(reporter),
                confidence: 0.9,
                ..seen(victim, 0.9, 0.0)
            });

        // Time passes, so a fresh telling would visibly reset the decay clock.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(30));
        finish_report(&mut app, reporter);

        let held = app
            .world()
            .get::<NpcMemory>(listener)
            .unwrap()
            .best_about(victim, StimulusKind::Casualty, 30.0)
            .copied()
            .unwrap();
        assert_eq!(
            held.learned_at, 0.0,
            "a listener who already knows must not have their decay clock reset"
        );
    }

    /// Positive control for the gate above: a listener who is *not* already
    /// sure does get told, so the assertion there cannot pass merely because
    /// delivery failed for some unrelated reason.
    #[test]
    fn a_listener_who_is_unsure_is_still_told() {
        let mut app = delivery_app();
        let victim = app.world_mut().spawn(Transform::default()).id();
        let reporter = crew_at(&mut app, 1, 0.0);
        let listener = crew_at(&mut app, 2, 1.0);

        app.world_mut()
            .get_mut::<NpcMemory>(reporter)
            .unwrap()
            .remember(seen(victim, 1.0, 0.0));
        // Faint secondhand knowledge, below `ALREADY_KNOWS`.
        app.world_mut()
            .get_mut::<NpcMemory>(listener)
            .unwrap()
            .remember(MemoryFact {
                modality: Modality::Told,
                source: Some(reporter),
                confidence: 0.2,
                ..seen(victim, 0.2, 0.0)
            });

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(30));
        finish_report(&mut app, reporter);

        let held = app
            .world()
            .get::<NpcMemory>(listener)
            .unwrap()
            .best_about(victim, StimulusKind::Casualty, 30.0)
            .copied()
            .unwrap();
        assert_eq!(
            held.learned_at, 30.0,
            "an unsure listener must be brought up to date by a fresh report"
        );
    }

    /// The offer half: a witness who knows something urgent is given the
    /// chance to go and tell someone, and a witness with nothing to say is not.
    #[test]
    fn only_a_witness_with_urgent_news_is_offered_a_report() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<UtilityOpportunityBuffer>()
            .add_systems(Update, offer_reports);

        let victim = app.world_mut().spawn(Transform::default()).id();
        let witness = crew_at(&mut app, 1, 0.0);
        let uninformed = crew_at(&mut app, 2, 1.0);

        app.world_mut()
            .get_mut::<NpcMemory>(witness)
            .unwrap()
            .remember(seen(victim, 1.0, 0.0));
        app.update();

        let buffer = app.world().resource::<UtilityOpportunityBuffer>();
        assert!(
            buffer
                .for_agent(witness)
                .any(|offer| offer.action == UtilityActionId::ReportIncident),
            "a witness holding urgent news must be offered the chance to report it"
        );
        assert!(
            buffer.for_agent(uninformed).next().is_none(),
            "a crew member who knows nothing has nothing to report"
        );
    }

    /// A half-glimpsed thing is not worth abandoning a post for. Without this
    /// floor, the faintest trace of a memory would send someone across the
    /// station to report a maybe.
    #[test]
    fn a_barely_perceived_thing_is_not_worth_reporting() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<UtilityOpportunityBuffer>()
            .add_systems(Update, offer_reports);

        let victim = app.world_mut().spawn(Transform::default()).id();
        let unsure = crew_at(&mut app, 1, 0.0);
        let confident = crew_at(&mut app, 2, 1.0);
        crew_at(&mut app, 3, 2.0);

        app.world_mut()
            .get_mut::<NpcMemory>(unsure)
            .unwrap()
            .remember(seen(victim, WORTH_REPORTING - 0.1, 0.0));
        app.world_mut()
            .get_mut::<NpcMemory>(confident)
            .unwrap()
            .remember(seen(victim, WORTH_REPORTING + 0.1, 0.0));
        app.update();

        let buffer = app.world().resource::<UtilityOpportunityBuffer>();
        // Positive control: the same setup just above the floor does offer,
        // so the negative below cannot pass because offers are broken.
        assert!(
            buffer.for_agent(confident).next().is_some(),
            "a witness just above the floor must still be offered a report"
        );
        assert!(
            buffer.for_agent(unsure).next().is_none(),
            "a barely perceived thing must not send someone across the station"
        );
    }

    /// A report offer must never outrank treating the casualty itself.
    #[test]
    fn a_report_is_important_but_never_an_emergency() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<UtilityOpportunityBuffer>()
            .add_systems(Update, offer_reports);

        let victim = app.world_mut().spawn(Transform::default()).id();
        let witness = crew_at(&mut app, 1, 0.0);
        crew_at(&mut app, 2, 1.0);
        app.world_mut()
            .get_mut::<NpcMemory>(witness)
            .unwrap()
            .remember(seen(victim, 1.0, 0.0));
        app.update();

        let bucket = app
            .world()
            .resource::<UtilityOpportunityBuffer>()
            .for_agent(witness)
            .next()
            .expect("the witness must have been offered a report")
            .bucket;
        assert_eq!(
            bucket,
            UtilityBucket::Important,
            "raising the alarm must lose to actually treating the patient"
        );
    }

    /// Only urgent knowledge is worth crossing a room for. In particular
    /// `SuspiciousHandling` is excluded, so an ordinary worker's half-glimpse
    /// never becomes a station-wide accusation.
    #[test]
    fn only_urgent_kinds_are_worth_reporting() {
        assert!(worth_reporting(StimulusKind::Casualty));
        assert!(worth_reporting(StimulusKind::Hazard));
        assert!(worth_reporting(StimulusKind::CallForHelp));
        assert!(!worth_reporting(StimulusKind::SuspiciousHandling));
        assert!(!worth_reporting(StimulusKind::Food));
    }
}
