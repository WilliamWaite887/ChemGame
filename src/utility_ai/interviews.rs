//! Witness interviews: the *pull* half of how knowledge moves.
//!
//! [`super::reports`] is the push half — a witness who saw something urgent
//! walks over and volunteers it. That path deliberately refuses to carry
//! `SuspiciousHandling`, because an ordinary worker who half-saw something odd
//! is not a witness with a theory, and a station where every glimpse becomes a
//! broadcast accusation has no room for a covert antagonist.
//!
//! So the quiet knowledge has to be *asked for*. An investigator opens a case
//! about an incident, walks to each plausible witness in turn, and asks. That
//! is the only route by which a `SuspiciousHandling` memory ever leaves the
//! head that formed it.
//!
//! Three properties this is built to preserve:
//!
//! - **An interview can come back empty.** Most will. A witness who saw
//!   nothing says so, and that is a real outcome the investigator pays travel
//!   time for, not a failure to be optimised away.
//! - **Testimony carries its modality.** "I saw Vale handling it" and "someone
//!   told me about it" arrive as different-strength evidence, because
//!   [`Modality`] survives into the case. An investigation built only on
//!   hearsay is visibly weaker than one with an eyewitness.
//! - **Nobody is compelled and nobody is omniscient.** The investigator learns
//!   only what that witness actually holds, at the confidence they hold it.
//!   There is no global query anywhere in this module.

use bevy::prelude::*;

use super::perception::{Modality, NpcMemory, StimulusKind};
use super::{
    stable_text_key, ActionResult, ActionTarget, IncidentId, IncidentKind, IncidentLedger,
    JobBoard, JobCapability, JobDomain, JobTicket, JobTicketId, JobTicketState, Normalized,
    ReservationKey, UtilityActionResolved, UtilityAgent, UtilityBucket,
};
use crate::crew::CrewMember;

/// The work qualification an investigator needs.
pub(super) const INTERVIEW_CAPABILITY: &str = "security.interview";

/// How long one interview takes, in seconds.
const INTERVIEW_SECONDS: f32 = 4.0;

/// How close the investigator must be for the witness to actually answer.
/// Conversational distance, matching a spoken report.
const INTERVIEW_RANGE: f32 = 3.0;

/// A witness below this confidence has nothing usable to say. They are still
/// interviewed — the investigator does not know in advance — but the answer is
/// recorded as "saw nothing useful".
const USABLE_TESTIMONY: f32 = 0.2;

/// How many witnesses one case will canvass before closing. Bounded so an
/// investigation cannot consume a whole shift on a crowded station.
const MAX_INTERVIEWS: usize = 6;

/// Which incidents are worth investigating.
///
/// Deliberately the *covert* kinds. A burn or a fire is answered by Medical and
/// Engineering responding to it; nobody needs to be asked who saw a fire. These
/// are the ones where the question "what did anyone notice?" is the whole
/// problem.
pub(super) fn worth_investigating(kind: IncidentKind) -> bool {
    matches!(
        kind,
        IncidentKind::Tampering
            | IncidentKind::Theft
            | IncidentKind::Contamination
            // Poisoning was missing, which quietly made the food-contamination
            // thread unfalsifiable: the act emitted `SuspiciousHandling`
            // memories, and no investigation was ever opened to collect them,
            // so the one route by which such a memory leaves a witness's head
            // was never taken. A poisoning is exactly the shape this list
            // describes — the question "what did anyone notice?" is the whole
            // problem.
            //
            // Note this admits *ordinary* poisonings too: a Botany accident, a
            // bad batch. That is correct and deliberate. An investigation that
            // can only be opened when there is a culprit to find would be
            // consulting private truth to decide whether to look.
            | IncidentKind::Poisoning
    )
}

/// What one witness said when asked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Testimony {
    pub witness: Entity,
    /// Who the witness named, if they could. `None` is the common case: they
    /// saw something happen without being able to say who did it.
    pub named: Option<Entity>,
    /// How they came by it. An eyewitness account is worth more than a
    /// secondhand one, and the case keeps the difference rather than
    /// flattening both into "a witness said so".
    pub modality: Modality,
    /// Their confidence at the moment of asking, already decayed.
    pub weight: f32,
    pub given_at: f32,
}

impl Testimony {
    /// Whether this testimony actually advanced the case.
    ///
    /// An empty answer is still a recorded interview — the investigator went
    /// and asked — but it is not evidence.
    pub fn is_evidence(&self) -> bool {
        self.weight >= USABLE_TESTIMONY
    }

    /// Evidential weight, discounted by how the witness came to know it.
    ///
    /// Hearsay is already stored at reduced confidence by
    /// [`super::reports`]; this discounts it again, because a *case* built on
    /// retelling should be weaker than the retelling itself is as a belief.
    pub fn evidential_weight(&self) -> f32 {
        if !self.is_evidence() {
            return 0.0;
        }
        match self.modality {
            Modality::Seen => self.weight,
            Modality::Heard => self.weight * 0.6,
            Modality::Told => self.weight * 0.4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvestigationStatus {
    /// Witnesses remain to be canvassed.
    Canvassing,
    /// Everyone reachable has been asked and nobody could name anyone. The
    /// station knows something happened and not who did it — a real, common
    /// outcome, not a failure.
    Unsolved,
    /// A witness named someone with usable confidence.
    Identified(Entity),
}

/// One open investigation into a covert incident.
#[derive(Clone, Debug)]
pub struct Investigation {
    pub incident: IncidentId,
    pub at: Vec3,
    pub status: InvestigationStatus,
    /// Everyone already asked, so no witness is interviewed twice about the
    /// same incident and the canvass terminates.
    pub interviewed: Vec<Entity>,
    pub testimony: Vec<Testimony>,
}

impl Investigation {
    /// Who the evidence points at, if anyone.
    ///
    /// Requires a witness who could actually *name* someone. A case full of
    /// "I saw something" never produces a suspect, which is the correct
    /// outcome rather than a gap to be filled with a guess.
    pub fn suspect(&self) -> Option<Entity> {
        self.testimony
            .iter()
            .filter(|account| account.is_evidence() && account.named.is_some())
            .max_by(|a, b| a.evidential_weight().total_cmp(&b.evidential_weight()))
            .and_then(|account| account.named)
    }
}

/// Bounded authority-side record of open investigations.
///
/// Authority-only, like every other knowledge structure here: replicating it
/// would tell a client which crew member Security suspects.
#[derive(Resource, Default)]
pub struct InvestigationLedger {
    cases: Vec<Investigation>,
}

/// Keeps investigations bounded regardless of how eventful a shift is.
const MAX_CASES: usize = 4;

impl InvestigationLedger {
    pub fn open(&mut self, investigation: Investigation) -> bool {
        if self.cases.len() >= MAX_CASES
            || self
                .cases
                .iter()
                .any(|case| case.incident == investigation.incident)
        {
            return false;
        }
        self.cases.push(investigation);
        true
    }

    pub fn get(&self, incident: IncidentId) -> Option<&Investigation> {
        self.cases.iter().find(|case| case.incident == incident)
    }

    fn get_mut(&mut self, incident: IncidentId) -> Option<&mut Investigation> {
        self.cases.iter_mut().find(|case| case.incident == incident)
    }

    pub fn active(&self) -> impl Iterator<Item = &Investigation> {
        self.cases
            .iter()
            .filter(|case| case.status == InvestigationStatus::Canvassing)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Investigation> {
        self.cases.iter()
    }

    pub fn clear(&mut self) {
        self.cases.clear();
    }
}

/// Opens an investigation for each new covert incident.
pub(super) fn open_investigations(
    incidents: Res<IncidentLedger>,
    mut ledger: ResMut<InvestigationLedger>,
) {
    let mut fresh: Vec<_> = incidents
        .active()
        .filter(|incident| worth_investigating(incident.kind))
        .filter(|incident| ledger.get(incident.id).is_none())
        .cloned()
        .collect();
    fresh.sort_by_key(|incident| incident.id);

    for incident in fresh {
        ledger.open(Investigation {
            incident: incident.id,
            at: incident.location,
            status: InvestigationStatus::Canvassing,
            interviewed: Vec::new(),
            testimony: Vec::new(),
        });
    }
}

/// Publishes one interview ticket per open investigation.
///
/// One at a time, deliberately: an investigator works through witnesses in
/// sequence rather than the department fanning out across the station, which
/// keeps the canvass legible to a player watching it happen.
#[allow(clippy::type_complexity)]
pub(super) fn publish_interview_tickets(
    time: Res<Time>,
    mut ledger: ResMut<InvestigationLedger>,
    mut board: ResMut<JobBoard>,
    crew: Query<(Entity, &CrewMember, &Transform), With<UtilityAgent>>,
) {
    let now = time.elapsed_secs();

    // Collected first because closing a finished canvass needs a mutable
    // borrow of the same ledger this iterates.
    let open: Vec<IncidentId> = ledger.active().map(|case| case.incident).collect();

    for incident in open {
        let Some(case) = ledger.get(incident) else {
            continue;
        };
        let ticket_id = interview_ticket_id(incident);
        if board.ticket(ticket_id).is_some() {
            continue;
        }

        // The next witness is whoever is nearest the incident and has not yet
        // been asked. Ties break on name so a canvass is reproducible.
        let mut candidates: Vec<(Entity, &CrewMember, Vec3)> = crew
            .iter()
            .filter(|(who, _, _)| !case.interviewed.contains(who))
            .map(|(who, member, transform)| (who, member, transform.translation))
            .collect();

        // Nobody left to ask, or the cap reached. The canvass is over and the
        // case stands on whatever it gathered. Closing here rather than in
        //  is what makes an investigation terminate: the
        // publisher is the system that can see the roster is exhausted.
        if candidates.is_empty() || case.interviewed.len() >= MAX_INTERVIEWS {
            if let Some(case) = ledger.get_mut(incident) {
                case.status = match case.suspect() {
                    Some(named) => InvestigationStatus::Identified(named),
                    None => InvestigationStatus::Unsolved,
                };
            }
            continue;
        }
        candidates.sort_by(|(_, a, a_at), (_, b, b_at)| {
            a_at.distance(case.at)
                .total_cmp(&b_at.distance(case.at))
                .then_with(|| a.name.cmp(&b.name))
        });
        let (witness, _, _) = candidates[0];

        let _ = board.publish(JobTicket {
            id: ticket_id,
            domain: JobDomain::Security,
            kind: format!("security.interview.{}", incident.0),
            // Walking to the witness, who moves; the interview happens
            // wherever they are.
            target: ActionTarget::Entity(witness),
            // Naming the witness gates this on the investigator having some
            // reason to talk to them, and marks whose account this is.
            subject: Some(witness),
            reservation: ReservationKey(format!("security.witness.{:016x}", witness.to_bits())),
            reservation_capacity: 1,
            // Important, not Emergency: an investigation never outranks
            // treating a casualty or sealing a hazard.
            bucket: UtilityBucket::Important,
            urgency: Normalized::new(0.55).expect("a literal in range"),
            required_capability: JobCapability::new(INTERVIEW_CAPABILITY),
            created_at: now,
            deadline: Some(now + 120.0),
            risk: Normalized::ZERO,
            perform_seconds: INTERVIEW_SECONDS,
            state: JobTicketState::Available,
        });
    }
}

/// Records what a witness said once an interview finishes.
#[allow(clippy::type_complexity)]
fn record_testimony(
    time: Res<Time>,
    mut results: MessageReader<UtilityActionResolved>,
    mut board: ResMut<JobBoard>,
    mut ledger: ResMut<InvestigationLedger>,
    tampered: Option<Res<super::TamperedMeals>>,
    crew: Query<(&Transform, &NpcMemory), With<UtilityAgent>>,
    positions: Query<&Transform, With<UtilityAgent>>,
) {
    let now = time.elapsed_secs();

    for result in results.read() {
        if result.key.action != super::UtilityActionId::PerformJob {
            continue;
        }
        let ticket_id = JobTicketId(result.key.target_key);
        let Some(ticket) = board.ticket(ticket_id).cloned() else {
            continue;
        };
        if ticket.domain != JobDomain::Security {
            continue;
        }
        let Some(incident) = incident_from_interview_ticket(&ticket.kind) else {
            continue;
        };
        let Some(witness) = ticket.subject else {
            continue;
        };

        // An abandoned interview is not a recorded one. The witness stays on
        // the list to be asked again, so being interrupted does not silently
        // consume someone's testimony. Either way the ticket leaves the board,
        // so the next frame publishes a fresh one for the next witness.
        if result.result != ActionResult::Completed {
            board.cancel(ticket_id);
            continue;
        }
        if board.take_completed(ticket_id, result.claim).is_err() {
            continue;
        }

        // What this case is *about*, resolved before the mutable borrow below.
        //
        // The meal an exposure links to the incident, when there is one. This
        // is a subject link, not an answer: it narrows which of the witness's
        // memories is responsive, and supplies nothing about who did it.
        let subject = tampered
            .as_deref()
            .and_then(|tampered| tampered.exposure_for(incident))
            .map(|(meal, _culprit)| meal);

        let Some(case) = ledger.get_mut(incident) else {
            continue;
        };
        if case.interviewed.contains(&witness) {
            continue;
        }

        // The witness must actually be there to answer. If they wandered off
        // mid-errand the interview is spent without producing testimony, and
        // they remain eligible to be asked again.
        let reachable = match (positions.get(result.agent), positions.get(witness)) {
            (Ok(investigator), Ok(subject)) => {
                investigator.translation.distance(subject.translation) <= INTERVIEW_RANGE
            }
            _ => false,
        };
        if !reachable {
            continue;
        }

        case.interviewed.push(witness);

        // Read only this witness's own memory. This is the whole point: the
        // investigator learns what one person holds, not what the world knows.
        //
        // But it has to be about *this case*. `best` returns the strongest
        // handling memory a witness holds, which on a busy station is often
        // something else entirely — somebody topping up a plot yesterday, or a
        // donation handed over an hour ago, both of which produce the same
        // ambiguous stimulus by design. Answering a poisoning case with an
        // unrelated sighting is how an innocent person gets named.
        //
        // The subject link comes from the incident record; the *content* still
        // comes only from the witness. Asking "did you see anything about this
        // meal" is legitimate investigation. Reading who did it from the
        // ledger and calling it testimony would not be.
        if let Ok((_, memory)) = crew.get(witness) {
            let fact = match subject {
                Some(subject) => memory.best_about(subject, StimulusKind::SuspiciousHandling, now),
                None => memory.best(StimulusKind::SuspiciousHandling, now),
            };
            if let Some(fact) = fact {
                case.testimony.push(Testimony {
                    witness,
                    named: fact.actor,
                    modality: fact.modality,
                    weight: fact.confidence_at(now),
                    given_at: now,
                });
            }
        }
    }
}

/// Writes a closed investigation's finding onto the incident it was about.
///
/// This is what stops the ledger being inert. `IncidentRecord::source` is the
/// station's record of *who was responsible*, and until now nothing could ever
/// fill it in for a covert incident — it was set only by systems that already
/// knew the culprit because they created them. An investigation is the one
/// path from "something happened" to "and it was them", earned by asking.
///
/// P7's antagonist work is the intended consumer: this is the seam where being
/// seen has a consequence.
fn record_findings(
    mut incidents: ResMut<IncidentLedger>,
    ledger: Res<InvestigationLedger>,
    mut recorded: Local<Vec<IncidentId>>,
) {
    for case in ledger.iter() {
        let InvestigationStatus::Identified(suspect) = case.status else {
            continue;
        };
        if recorded.contains(&case.incident) {
            continue;
        }
        if incidents.attribute(case.incident, suspect).is_ok() {
            recorded.push(case.incident);
        }
    }
}

/// Stable ticket id for one investigation's current interview.
fn interview_ticket_id(incident: IncidentId) -> JobTicketId {
    JobTicketId(stable_text_key("security.interview") ^ incident.0.rotate_left(17))
}

/// Recovers the incident from an interview ticket's kind string.
fn incident_from_interview_ticket(kind: &str) -> Option<IncidentId> {
    kind.strip_prefix("security.interview.")
        .and_then(|id| id.parse().ok())
        .map(IncidentId)
}

fn reset_investigations(mut ledger: ResMut<InvestigationLedger>) {
    ledger.clear();
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<InvestigationLedger>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_investigations
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            Update,
            (open_investigations, publish_interview_tickets)
                .chain()
                .in_set(super::UtilityAiSet::BuildContext)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (record_testimony, record_findings)
                .chain()
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
    use crate::utility_ai::{
        perception::MemoryFact, ActionKey, ReservationOwner, UtilityActionId, UtilityAgent,
        UtilityControlBundle,
    };

    fn suspicious(actor: Option<Entity>, modality: Modality, confidence: f32) -> MemoryFact {
        MemoryFact {
            kind: StimulusKind::SuspiciousHandling,
            subject: Some(Entity::from_raw_u32(500).unwrap()),
            actor,
            at: Vec3::ZERO,
            room_key: None,
            modality,
            source: None,
            confidence,
            learned_at: 0.0,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<IncidentLedger>()
            .init_resource::<InvestigationLedger>()
            .init_resource::<JobBoard>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    open_investigations,
                    publish_interview_tickets,
                    record_testimony,
                    record_findings,
                )
                    .chain(),
            );
        app
    }

    fn crew_at(app: &mut App, seed: u64, x: f32) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: format!("Crew {seed:02}"),
                    role: "Service".into(),
                },
                Transform::from_xyz(x, 0.0, 0.0),
                UtilityControlBundle::new(UtilityAgent::new(seed, 0)),
            ))
            .id()
    }

    fn tampering(app: &mut App, subject: Entity) -> IncidentId {
        app.world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::Tampering,
                JobDomain::Botany,
                subject,
                None,
                Vec3::ZERO,
                Normalized::new(0.5).unwrap(),
                0.0,
            )
            .expect("the ledger has room")
    }

    /// Finishes whatever interview ticket is currently published, as `agent`.
    fn finish_interview(app: &mut App, agent: Entity, incident: IncidentId) {
        // Let the next witness's ticket be published before claiming it. The
        // ticket id is stable per case, so claiming before the republish would
        // silently re-target whoever was just interviewed.
        app.update();
        let ticket = interview_ticket_id(incident);
        let claim = ReservationOwner {
            agent,
            action_instance: 1,
        };
        let _ = app
            .world_mut()
            .resource_mut::<JobBoard>()
            .claim(ticket, claim);
        let _ = app.world_mut().resource_mut::<JobBoard>().resolve(
            ticket,
            claim,
            ActionResult::Completed,
        );
        app.world_mut().write_message(UtilityActionResolved {
            agent,
            key: ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: ticket.0,
            },
            claim,
            result: ActionResult::Completed,
        });
        // Two updates: messages written outside the schedule are read on the
        // following run, and  then acts on the closed case.
        app.update();
        app.update();
    }

    /// Only covert incidents get investigated. Nobody needs to be asked who
    /// saw a fire — Engineering is already putting it out.
    #[test]
    fn only_covert_incidents_are_investigated() {
        assert!(worth_investigating(IncidentKind::Tampering));
        assert!(worth_investigating(IncidentKind::Theft));
        assert!(worth_investigating(IncidentKind::Contamination));
        // A poisoning is the shape this list describes — "what did anyone
        // notice?" is the whole problem. Its absence made the food thread
        // unfalsifiable: the act left `SuspiciousHandling` memories that no
        // investigation was ever opened to collect.
        assert!(worth_investigating(IncidentKind::Poisoning));

        // Answered by responding to them, not by asking who saw.
        assert!(!worth_investigating(IncidentKind::Fire));
        assert!(!worth_investigating(IncidentKind::Burn));
        assert!(!worth_investigating(IncidentKind::EquipmentFailure));
    }

    /// Eligibility must not consult private truth.
    ///
    /// An ordinary poisoning — a bad batch, a Botany accident — is opened for
    /// investigation exactly like a deliberate one, and may legitimately find
    /// nobody. Opening a case only where a culprit exists would leak the
    /// answer into the decision to ask the question.
    #[test]
    fn an_ordinary_poisoning_is_investigated_like_any_other() {
        let mut app = app();
        let victim = crew_at(&mut app, 1, 1.0);
        let incident = app
            .world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::Poisoning,
                JobDomain::Service,
                victim,
                None,
                Vec3::ZERO,
                Normalized::new(0.6).unwrap(),
                0.0,
            )
            .expect("the ledger accepts a poisoning");
        app.update();

        assert!(
            app.world()
                .resource::<InvestigationLedger>()
                .get(incident)
                .is_some(),
            "nobody poisoned them, and Security still asks around",
        );
    }

    /// The load-bearing claim: an interview reads *one witness's own memory*,
    /// so a case learns what that person saw and nothing else.
    #[test]
    fn an_interview_collects_only_the_interviewed_witness_account() {
        let mut app = app();
        let culprit = crew_at(&mut app, 1, 1.5);
        let witness = crew_at(&mut app, 2, 0.5);
        let bystander = crew_at(&mut app, 3, 1.0);
        let investigator = crew_at(&mut app, 4, 0.0);

        // Only the witness saw who did it. The bystander saw nothing at all.
        app.world_mut()
            .get_mut::<NpcMemory>(witness)
            .unwrap()
            .remember(suspicious(Some(culprit), Modality::Seen, 0.9));

        let incident = tampering(&mut app, culprit);
        app.update();
        // The canvass works outward from the incident, so it takes a few
        // interviews to reach the witness. Everyone in between says nothing.
        for _ in 0..4 {
            finish_interview(&mut app, investigator, incident);
        }

        let ledger = app.world().resource::<InvestigationLedger>();
        let case = ledger.get(incident).expect("a case was opened");
        assert_eq!(
            case.testimony.len(),
            1,
            "only the one crew member who saw anything has an account"
        );
        let account = case.testimony[0];
        assert_eq!(account.witness, witness, "the account is the witness's own");
        assert_eq!(account.named, Some(culprit));
        assert_eq!(account.modality, Modality::Seen);
        assert!(
            case.interviewed.contains(&bystander),
            "the bystander was asked and had nothing to say"
        );
    }

    /// A canvass where nobody saw anything closes unsolved. The station knows
    /// something happened and not who did it, and that is a real outcome.
    #[test]
    fn a_canvass_with_no_witnesses_closes_unsolved() {
        let mut app = app();
        let culprit = crew_at(&mut app, 1, 1.0);
        let investigator = crew_at(&mut app, 2, 0.0);
        let incident = tampering(&mut app, culprit);
        app.update();

        // Ask everyone. Nobody has any memory at all.
        for _ in 0..4 {
            finish_interview(&mut app, investigator, incident);
        }

        let ledger = app.world().resource::<InvestigationLedger>();
        let case = ledger.get(incident).expect("a case was opened");
        assert_eq!(
            case.status,
            InvestigationStatus::Unsolved,
            "no testimony must mean no suspect, not a guess"
        );
        assert!(case.testimony.is_empty());
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .active()
                .find(|record| record.id == incident)
                .and_then(|record| record.source),
            None,
            "an unsolved case must never attribute the incident to anyone"
        );
    }

    /// The finding is written back onto the incident. This is the seam that
    /// makes an investigation matter rather than being a private ledger.
    #[test]
    fn an_identified_culprit_is_recorded_on_the_incident() {
        let mut app = app();
        let culprit = crew_at(&mut app, 1, 1.5);
        let witness = crew_at(&mut app, 2, 0.5);
        let investigator = crew_at(&mut app, 3, 0.0);

        app.world_mut()
            .get_mut::<NpcMemory>(witness)
            .unwrap()
            .remember(suspicious(Some(culprit), Modality::Seen, 0.9));

        let incident = tampering(&mut app, culprit);
        app.update();
        for _ in 0..4 {
            finish_interview(&mut app, investigator, incident);
        }

        let ledger = app.world().resource::<InvestigationLedger>();
        let case = ledger.get(incident).expect("a case was opened");
        assert_eq!(case.status, InvestigationStatus::Identified(culprit));
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .active()
                .find(|record| record.id == incident)
                .and_then(|record| record.source),
            Some(culprit),
            "the incident must record who the investigation identified"
        );
    }

    /// Testimony keeps its modality, so a case built on hearsay is visibly
    /// weaker than one with an eyewitness.
    #[test]
    fn hearsay_is_weaker_evidence_than_an_eyewitness() {
        let culprit = Entity::from_raw_u32(9).unwrap();
        let witness = Entity::from_raw_u32(10).unwrap();
        let account = |modality| Testimony {
            witness,
            named: Some(culprit),
            modality,
            weight: 0.8,
            given_at: 0.0,
        };

        let seen = account(Modality::Seen).evidential_weight();
        let heard = account(Modality::Heard).evidential_weight();
        let told = account(Modality::Told).evidential_weight();
        assert!(
            seen > heard && heard > told,
            "evidence must be ordered seen > heard > told, got {seen}/{heard}/{told}"
        );

        // A witness too unsure to be useful contributes nothing at all.
        let vague = Testimony {
            weight: USABLE_TESTIMONY - 0.05,
            ..account(Modality::Seen)
        };
        assert_eq!(vague.evidential_weight(), 0.0);
        assert!(!vague.is_evidence());
    }

    /// A case with accounts but no name never invents a suspect.
    #[test]
    fn seeing_something_without_seeing_who_never_produces_a_suspect() {
        let witness = Entity::from_raw_u32(10).unwrap();
        let mut case = Investigation {
            incident: IncidentId(1),
            at: Vec3::ZERO,
            status: InvestigationStatus::Canvassing,
            interviewed: vec![witness],
            testimony: vec![Testimony {
                witness,
                named: None,
                modality: Modality::Seen,
                weight: 1.0,
                given_at: 0.0,
            }],
        };
        assert_eq!(
            case.suspect(),
            None,
            "'I saw something' must not become 'it was them'"
        );

        // Positive control: the same case with a name does identify someone.
        let culprit = Entity::from_raw_u32(11).unwrap();
        case.testimony[0].named = Some(culprit);
        assert_eq!(case.suspect(), Some(culprit));
    }

    /// An existing attribution is ground truth and an investigation must not
    /// overwrite it. A system that knew the culprit at creation time is more
    /// reliable than testimony gathered afterwards.
    #[test]
    fn an_investigation_never_overwrites_a_known_culprit() {
        let mut ledger = IncidentLedger::default();
        let subject = Entity::from_raw_u32(20).unwrap();
        let real = Entity::from_raw_u32(21).unwrap();
        let accused = Entity::from_raw_u32(22).unwrap();
        let incident = ledger
            .create(
                IncidentKind::Tampering,
                JobDomain::Botany,
                subject,
                Some(real),
                Vec3::ZERO,
                Normalized::new(0.5).unwrap(),
                0.0,
            )
            .unwrap();

        ledger.attribute(incident, accused).unwrap();
        assert_eq!(
            ledger
                .active()
                .find(|record| record.id == incident)
                .and_then(|record| record.source),
            Some(real),
            "a known culprit must survive a contradicting investigation"
        );
    }
}
