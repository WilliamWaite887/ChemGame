//! Medical response built from real incident subjects and shared job tickets.
//!
//! The first slice deliberately keeps diagnosis simple: a responder reaches
//! the casualty, reserves a real bed, escorts the same entity to Medical, and
//! provides bounded standard burn care. More complex cases can branch into a
//! linked Chemistry request without changing custody or target identity.

use std::collections::HashMap;

use bevy::prelude::*;

use super::department::{control_for_resident, DepartmentRoster};
use super::jobs::{
    JobBoard, JobCapability, JobDomain, JobTicket, JobTicketId, JobTicketState, NpcJobProfile,
    UtilitySpots,
};
use super::{
    interrupt_utility_action, stable_text_key, try_handoff, ActionResult, ActionTarget,
    ControlOwner, CurrentAction, IncidentCreated, IncidentKind, IncidentLedger, IncidentResolved,
    LocomotionOwner, Normalized, NpcActivity, NpcPosture, ReservationBook, ReservationKey,
    ReservationOwner, SuspendedUtilityControl, UtilityActionId, UtilityActionResolved,
    UtilityAgent, UtilityBucket, UtilityControlBundle,
};
use crate::crew::{
    recall_resident_for_order, send_on_errand_with_reach, Ambient, AvailableResidents, CrewMember,
    CrewRoute, Errand, ErrandGoal, ErrandOutcome, ErrandResolved, StationResident,
};
use crate::interaction::Interactable;
use crate::order_intake::{
    AcceptedOrder, AwaitingConversation, Intake, PendingOrder, RequestContext, RequestSource,
};
use crate::orders::{
    abandon_carried_fulfillment, CarryingFulfillment, CounterOrder, CrisisOrder, DevelopmentOrder,
    FulfillmentApplicationResult, FulfillmentApplied, HostileOrder, IllicitOrder, Order, OrderUse,
};
use chem_sim::{Route, Units};

const MEDICAL_CORE: [&str; 2] = ["Dr. Vance", crate::social::OKONKWO];
const MEDICAL_SUPPORT: [&str; 2] = crate::crew::fluff::MEDICAL_SUPPORT_NAMES;
const MEDICAL_RESPONSE_CAPABILITY: &str = "medical.response";
const MEDICAL_REQUEST_CAPABILITY: &str = "medical.request_treatment";
const MEDICAL_CAPABILITIES: [&str; 3] = [
    MEDICAL_RESPONSE_CAPABILITY,
    MEDICAL_REQUEST_CAPABILITY,
    super::aid::assessment_capability(JobDomain::Medical),
];
pub(super) const MEDICAL_ROSTER: DepartmentRoster = DepartmentRoster {
    domain: JobDomain::Medical,
    core: &MEDICAL_CORE,
    support: &MEDICAL_SUPPORT,
    expected_support: 2,
    capabilities: &MEDICAL_CAPABILITIES,
};
const MEDICAL_BEDS: [&str; 2] = ["medical.bed.1", "medical.bed.2"];
const RESPONSE_SECONDS: f32 = 2.0;
const STANDARD_BURN_CARE_SECONDS: f32 = 12.0;
const STANDARD_BURN_CARE_UNITS: i32 = 18;
const STANDARD_CARE_MAX_SEVERITY: f32 = 0.45;
const TREATMENT_REQUEST_RETRY_SECONDS: f32 = 5.0;
const TREATMENT_OBSERVATION_SECONDS: f32 = 45.0;
const TREATMENT_ORDER_AMOUNT: i32 = 10;
const TREATMENT_DOSE: i32 = 5;
const MAX_TREATMENT_ATTEMPTS: u8 = 3;
const TRANSPORT_REACH: f32 = 0.3;
const TRANSPORT_INSTANCE_PREFIX: u64 = 0x4d45_4400_0000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MedicalCaseId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MedicalCaseStatus {
    AwaitingResponder,
    Transporting { responder: Entity, bed: String },
    Admitted { bed: String },
    NeedsTreatment,
    TreatmentRequested { requester: Entity },
    RecoveringFromTreatment { requester: Entity },
    Escalated,
    Resolved,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MedicalCase {
    pub id: MedicalCaseId,
    pub incident: super::IncidentId,
    /// Additional reports about the same embodied casualty. They remain part
    /// of one case and resolve with it, so a second damage kind cannot reopen a
    /// stale case after the patient has already recovered.
    related_incidents: Vec<super::IncidentId>,
    pub patient: Entity,
    pub kind: IncidentKind,
    pub severity: Normalized,
    pub opened_at: f32,
    pub response_ticket: JobTicketId,
    pub request_ticket: JobTicketId,
    pub status: MedicalCaseStatus,
    source_entity: Entity,
    bed_claim: Option<ReservationOwner>,
    next_request_at: f32,
    treatment_attempts: u8,
    treatment_observation_elapsed: f32,
    treatment_baseline_damage: Option<Units>,
}

#[derive(Resource, Default)]
pub struct MedicalCaseLedger {
    cases: HashMap<MedicalCaseId, MedicalCase>,
}

impl MedicalCaseLedger {
    pub fn get(&self, id: MedicalCaseId) -> Option<&MedicalCase> {
        self.cases.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &MedicalCase> {
        self.cases.values()
    }

    pub fn active(&self) -> impl Iterator<Item = &MedicalCase> {
        self.cases
            .values()
            .filter(|case| case.status != MedicalCaseStatus::Resolved)
    }

    fn contains_incident(&self, incident: super::IncidentId) -> bool {
        self.cases
            .values()
            .any(|case| case.incident == incident || case.related_incidents.contains(&incident))
    }

    fn case_for_ticket(&self, ticket: JobTicketId) -> Option<MedicalCaseId> {
        self.cases
            .values()
            .find(|case| case.response_ticket == ticket)
            .map(|case| case.id)
    }

    fn case_for_request_ticket(&self, ticket: JobTicketId) -> Option<MedicalCaseId> {
        self.cases
            .values()
            .find(|case| case.request_ticket == ticket)
            .map(|case| case.id)
    }
}

fn medical_kind_priority(kind: IncidentKind) -> u8 {
    match kind {
        IncidentKind::Poisoning => 3,
        IncidentKind::BruteInjury => 2,
        IncidentKind::Burn => 1,
        _ => 0,
    }
}

fn resolve_case_incidents(
    case: &MedicalCase,
    incidents: &mut IncidentLedger,
    resolved: &mut MessageWriter<IncidentResolved>,
) {
    for id in std::iter::once(case.incident).chain(case.related_incidents.iter().copied()) {
        if incidents.resolve(id).is_ok() {
            resolved.write(IncidentResolved { id });
        }
    }
}

/// Marks a real crew body as owned by an active Medical case. It is also the
/// one-controller exemption that lets an incapacitated passenger remain under
/// Medical transport rather than being reclaimed by generic incapacity logic.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MedicalPatient {
    pub(super) case: MedicalCaseId,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TransportedPatient {
    pub(super) responder: Entity,
}

#[derive(Component, Clone, Debug)]
pub(super) struct MedicalTransportTask {
    pub(super) case: MedicalCaseId,
    pub(super) patient: Entity,
    pub(super) bed: String,
    pub(super) bed_at: Vec3,
    pub(super) bed_claim: ReservationOwner,
}

#[derive(Component, Clone, Copy, Debug)]
struct InpatientCare {
    case: MedicalCaseId,
    elapsed: f32,
    bed_claim: ReservationOwner,
}

/// Stable authority-side identity carried through the order pipeline so a
/// delivery outcome can be matched to one Medical case without guessing from
/// the requester's role or the patient's current location.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
struct MedicalCaseSource(MedicalCaseId);

/// Marks the resident temporarily representing a case at Chemistry. The order
/// components own the visit itself; this marker only lets Medical detect an
/// expired or otherwise abandoned request and retry it deliberately.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
struct MedicalTreatmentRequester {
    case: MedicalCaseId,
}

/// One-frame bridge between generic action cleanup and recalling that exact
/// completed utility actor into the existing order queue.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
struct PendingMedicalRequestStart {
    case: MedicalCaseId,
    ticket: JobTicketId,
    claim: ReservationOwner,
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<MedicalCaseLedger>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_medical_response
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            PreUpdate,
            activate_medical_response
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (
                open_cases_from_reports,
                start_pending_medical_requests,
                tick_inpatient_care,
                observe_treatment_fulfillments,
                recover_linked_treatments,
                repair_interrupted_medical_cases,
            )
                .chain()
                .in_set(super::UtilityAiSet::MaintainNeeds)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (
                open_cases_from_incidents,
                publish_medical_response_jobs,
                publish_treatment_request_jobs,
            )
                .chain()
                .in_set(super::UtilityAiSet::BuildContext)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (follow_medical_transport, finish_medical_transport)
                .chain()
                .after(crate::crew::run_errands)
                .in_set(super::UtilityAiSet::Attach)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (begin_medical_transport, capture_completed_treatment_request)
                .chain()
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

fn reset_medical_response(
    mut commands: Commands,
    mut cases: ResMut<MedicalCaseLedger>,
    mut board: ResMut<JobBoard>,
    mut reservations: ResMut<ReservationBook>,
    sources: Query<Entity, With<MedicalCaseSource>>,
    requesters: Query<Entity, With<MedicalTreatmentRequester>>,
    pending_starts: Query<Entity, With<PendingMedicalRequestStart>>,
) {
    MEDICAL_ROSTER
        .validate()
        .expect("the Medical utility roster must be valid");
    for entity in &sources {
        commands.entity(entity).despawn();
    }
    for entity in &requesters {
        commands
            .entity(entity)
            .remove::<MedicalTreatmentRequester>();
    }
    for entity in &pending_starts {
        commands
            .entity(entity)
            .remove::<PendingMedicalRequestStart>();
    }
    *cases = MedicalCaseLedger::default();
    board.remove_domain(JobDomain::Medical);
    for bed in MEDICAL_BEDS {
        reservations.release_key(&ReservationKey(format!("utility.spot.{bed}")));
    }
}

fn medical_profile(name: &str) -> Option<NpcJobProfile> {
    MEDICAL_ROSTER.profile_for(name)
}

fn activate_medical_response(
    mut commands: Commands,
    social: Option<Res<crate::social::SocialState>>,
    residents: Query<
        (Entity, &CrewMember, &CrewRoute, Option<&ControlOwner>),
        (
            With<Ambient>,
            Without<UtilityAgent>,
            Without<crate::social::NpcCommitment>,
        ),
    >,
) {
    for (entity, member, route, owner) in &residents {
        if social.as_deref().is_some_and(|social| {
            social
                .active_favor
                .as_ref()
                .is_some_and(|favor| favor.owner == member.name)
        }) {
            continue;
        }
        let Some((control, profile)) =
            control_for_resident(&member.name, route, owner, MEDICAL_ROSTER)
        else {
            continue;
        };
        commands
            .entity(entity)
            .insert((control, profile, StationResident));
    }
}

fn medical_incident(kind: IncidentKind) -> bool {
    matches!(
        kind,
        IncidentKind::Burn | IncidentKind::BruteInjury | IncidentKind::Poisoning
    )
}

/// How sure a reporter must still be, on arrival, for their report to open a
/// case.
///
/// Above `reports::WORTH_REPORTING` (0.35) on purpose: a witness may cross the
/// room to mention something they half-saw, and that is worth other crew
/// hearing, but it should not by itself commit Medical to a patient.
const CONFIDENT_ENOUGH_TO_FILE: f32 = 0.5;

/// Opens an incident from a witness who walked over and said so.
///
/// The explicit incident-report path. Every other route into the ledger is a
/// system that *already knew* — a department adapter watching its own worker,
/// or `handle_crew_collapse` observing a body directly. This one requires a
/// person to have seen it, remembered it, crossed the station, and said it out
/// loud, which is the difference between a station that notices casualties and
/// one that is simply told about them.
///
/// The kind is diagnosed from the reported body's *actual* damage rather than
/// from what the reporter claimed, because a witness says "someone's down", not
/// "brute trauma, severity 0.6". Reporting is how the case is opened; the
/// diagnosis is still Medical's.
fn open_cases_from_reports(
    time: Res<Time>,
    mut reported: MessageReader<super::reports::CasualtyReported>,
    mut incidents: ResMut<IncidentLedger>,
    mut created: MessageWriter<IncidentCreated>,
    bodies: Query<(
        &crate::body::Body,
        Option<&crate::body::Bloodstream>,
        Option<&super::NpcJobProfile>,
    )>,
) {
    for report in reported.read() {
        // Nobody files a casualty report about themselves. A body that is down
        // is not walking across the station to say so, and treating one as a
        // report would let a collapse open its own case through this path —
        // exactly the omniscience the report route exists to remove.
        if report.reporter == report.subject {
            continue;
        }
        // A vague report is worth passing on but not worth opening a case on.
        // The threshold sits above `reports::WORTH_REPORTING`, so a witness may
        // still cross the room to mention something they are unsure of without
        // that alone committing Medical to a patient.
        if report.confidence < CONFIDENT_ENOUGH_TO_FILE {
            continue;
        }
        // A body a department adapter is already tracking needs no second case
        // — the same guard `handle_crew_collapse` uses, for the same reason.
        if incidents
            .active_for(report.subject)
            .any(|incident| medical_incident(incident.kind))
        {
            continue;
        }
        let Ok((body, blood, profile)) = bodies.get(report.subject) else {
            continue;
        };
        // A report about someone who has since got up is stale, not a case.
        // The walk takes time, and a station that opened a case for every
        // recovered stumble would fill Medical with people who are fine.
        if !body.0.collapsed && !blood.is_some_and(|blood| blood.0.incapacitated()) {
            continue;
        }
        let kind = crate::crew::utility_casualty_kind(body, blood);
        let severity = crate::crew::utility_casualty_severity(body);
        let department = profile.map_or(JobDomain::Medical, |profile| profile.primary);
        let Ok(id) = incidents.create(
            kind,
            department,
            report.subject,
            // The reporter is a witness, not a cause. Recording them as the
            // incident's source would make raising the alarm look like guilt.
            None,
            report.at,
            severity,
            time.elapsed_secs(),
        ) else {
            continue;
        };
        created.write(IncidentCreated {
            id,
            kind,
            department,
            subject: report.subject,
            location: report.at,
            severity,
        });
    }
}

#[allow(clippy::type_complexity)]
pub(super) fn open_cases_from_incidents(
    mut commands: Commands,
    mut witnessed: MessageWriter<super::Stimulus>,
    incidents: Res<IncidentLedger>,
    mut cases: ResMut<MedicalCaseLedger>,
    mut reservations: ResMut<ReservationBook>,
    mut social: Option<ResMut<crate::social::SocialState>>,
    mut patients: Query<
        (
            Option<&CrewMember>,
            Option<&CurrentAction>,
            &mut ControlOwner,
            &mut LocomotionOwner,
            &mut NpcActivity,
            Option<&MedicalTransportTask>,
            Option<&CarryingFulfillment>,
            Option<&Transform>,
        ),
        With<UtilityAgent>,
    >,
) {
    let mut new_incidents: Vec<_> = incidents
        .active()
        .filter(|incident| medical_incident(incident.kind))
        .filter(|incident| !cases.contains_incident(incident.id))
        .cloned()
        .collect();
    new_incidents.sort_by_key(|incident| incident.id);

    for incident in new_incidents {
        if let Some(active_case) = cases.cases.values_mut().find(|case| {
            case.patient == incident.subject && case.status != MedicalCaseStatus::Resolved
        }) {
            if !active_case.related_incidents.contains(&incident.id)
                && active_case.incident != incident.id
            {
                active_case.related_incidents.push(incident.id);
            }
            if medical_kind_priority(incident.kind) > medical_kind_priority(active_case.kind)
                || (incident.kind == active_case.kind
                    && incident.severity.get() > active_case.severity.get())
            {
                active_case.kind = incident.kind;
            }
            if incident.severity.get() > active_case.severity.get() {
                active_case.severity = incident.severity;
            }
            continue;
        }
        let id = MedicalCaseId(incident.id.0);
        let response_ticket =
            JobTicketId(stable_text_key("medical.respond") ^ incident.id.0.rotate_left(23));
        let request_ticket =
            JobTicketId(stable_text_key("medical.request") ^ incident.id.0.rotate_left(31));
        let Ok((
            member,
            action,
            mut owner,
            mut locomotion,
            mut activity,
            transport,
            carrying,
            transform,
        )) = patients.get_mut(incident.subject)
        else {
            continue;
        };
        let accepted = match *owner {
            ControlOwner::UtilityAction => interrupt_utility_action(
                &mut commands,
                incident.subject,
                action,
                &mut owner,
                &mut locomotion,
                ControlOwner::MedicalTransport,
                LocomotionOwner::None,
            )
            .is_ok(),
            ControlOwner::Incapacitated => {
                let result = try_handoff(
                    &mut owner,
                    ControlOwner::Incapacitated,
                    ControlOwner::MedicalTransport,
                );
                if result.is_ok() {
                    *locomotion = LocomotionOwner::None;
                }
                result.is_ok()
            }
            ControlOwner::MedicalTransport => true,
            _ => false,
        };
        if !accepted {
            continue;
        }

        // A responder can itself become the next casualty. Abort the older
        // transport before admitting that body so one entity is never both a
        // Medical patient and an active transporter. The original patient
        // remains case-owned and is immediately eligible for a new responder.
        if let Some(transport) = transport.cloned() {
            reservations.release_owner(transport.bed_claim);
            if let Some(previous_case) = cases.cases.get_mut(&transport.case) {
                previous_case.status = MedicalCaseStatus::AwaitingResponder;
                previous_case.bed_claim = None;
            }
            commands
                .entity(transport.patient)
                .remove::<TransportedPatient>()
                .insert((NpcActivity::Helping, NpcPosture::Standing));
            commands
                .entity(incident.subject)
                .remove::<MedicalTransportTask>();
            // The same body is now the patient in its new case. It remains
            // Medical-owned, but it no longer owns the aborted responder's
            // movement lease.
            *locomotion = LocomotionOwner::None;
        }
        if let Some(carrying) = carrying {
            abandon_carried_fulfillment(
                &mut commands,
                incident.subject,
                carrying,
                transform.map_or(incident.location, |transform| transform.translation),
            );
        }
        // Private legacy visit payloads each own their own cancellation
        // semantics. These adapters make Medical admission the single revoke
        // point without leaking their components into the utility kernel.
        crate::antagonist::cancel_illicit_offer(&mut commands, incident.subject);
        crate::shift::cancel_glassware_delivery(&mut commands, incident.subject);
        crate::smuggler::cancel_smuggler_loitering(&mut commands, incident.subject);
        if let Some(social) = social.as_deref_mut() {
            if member.is_some_and(|member| {
                social
                    .active_favor
                    .as_ref()
                    .is_some_and(|favor| favor.owner == member.name)
            }) {
                social.active_favor = None;
            }
        }
        let source_entity = commands.spawn(MedicalCaseSource(id)).id();
        cases.cases.insert(
            id,
            MedicalCase {
                id,
                incident: incident.id,
                related_incidents: Vec::new(),
                patient: incident.subject,
                kind: incident.kind,
                severity: incident.severity,
                opened_at: incident.created_at,
                response_ticket,
                request_ticket,
                status: MedicalCaseStatus::AwaitingResponder,
                source_entity,
                bed_claim: None,
                next_request_at: incident.created_at,
                treatment_attempts: 0,
                treatment_observation_elapsed: 0.0,
                treatment_baseline_damage: None,
            },
        );
        // Announce the casualty so nearby crew can actually learn about it.
        // Whether anyone does is up to perception: an unseen collapse in an
        // empty room is remembered by nobody.
        if let Some(at) = transform.map(|transform| transform.translation) {
            witnessed.write(
                super::Stimulus::new(super::StimulusKind::Casualty, at)
                    .about(incident.subject)
                    .with_strength(incident.severity.get().clamp(0.2, 1.0)),
            );
        }
        *activity = NpcActivity::Helping;
        commands
            .entity(incident.subject)
            .remove::<CrewRoute>()
            .remove::<Errand>()
            .remove::<super::SuspendedUtilityControl>()
            .remove::<Order>()
            .remove::<OrderUse>()
            .remove::<PendingOrder>()
            .remove::<AcceptedOrder>()
            .remove::<AwaitingConversation>()
            .remove::<RequestContext>()
            .remove::<IllicitOrder>()
            .remove::<CrisisOrder>()
            .remove::<CounterOrder>()
            .remove::<HostileOrder>()
            .remove::<DevelopmentOrder>()
            .remove::<crate::security_case::OrderHold>()
            .remove::<crate::social::NpcCommitment>()
            .remove::<crate::social::PersonalFavor>()
            .remove::<crate::crew::ReturnsToDuty>()
            .remove::<crate::crew::AtCounter>()
            .remove::<crate::interaction::Interactable>()
            .remove::<MedicalTreatmentRequester>()
            .remove::<PendingMedicalRequestStart>()
            .insert((MedicalPatient { case: id }, NpcPosture::Standing));
    }
}

fn publish_medical_response_jobs(
    time: Res<Time>,
    cases: Res<MedicalCaseLedger>,
    mut board: ResMut<JobBoard>,
) {
    for case in cases
        .active()
        .filter(|case| case.status == MedicalCaseStatus::AwaitingResponder)
    {
        if board.ticket(case.response_ticket).is_some() {
            continue;
        }
        let urgency = 0.85 + case.severity.get() * 0.15;
        board
            .publish(JobTicket {
                id: case.response_ticket,
                domain: JobDomain::Medical,
                kind: format!("medical.respond.{}", case.id.0),
                target: ActionTarget::Entity(case.patient),
                // Naming the patient is what makes this ticket answerable only
                // by someone who saw or heard the collapse.
                subject: Some(case.patient),
                reservation: ReservationKey(format!("medical.patient.{}", case.id.0)),
                reservation_capacity: 1,
                bucket: UtilityBucket::Emergency,
                urgency: Normalized::new(urgency).expect("medical urgency is normalized"),
                required_capability: JobCapability::new(MEDICAL_RESPONSE_CAPABILITY),
                created_at: time.elapsed_secs(),
                deadline: Some(time.elapsed_secs() + 60.0),
                risk: Normalized::ZERO,
                perform_seconds: RESPONSE_SECONDS,
                state: JobTicketState::Available,
            })
            .expect("a case publishes at most one response ticket");
    }
}

#[allow(clippy::too_many_arguments)]
fn begin_medical_transport(
    mut commands: Commands,
    mut completed: MessageReader<UtilityActionResolved>,
    spots: Res<UtilitySpots>,
    mut reservations: ResMut<ReservationBook>,
    mut board: ResMut<JobBoard>,
    mut cases: ResMut<MedicalCaseLedger>,
    patients: Query<&MedicalPatient>,
    mut responders: Query<
        (
            &Transform,
            &mut ControlOwner,
            &mut LocomotionOwner,
            &mut NpcActivity,
        ),
        (With<UtilityAgent>, Without<MedicalPatient>),
    >,
) {
    for result in completed.read() {
        if result.key.action != UtilityActionId::PerformJob
            || result.result != ActionResult::Completed
        {
            continue;
        }
        let ticket_id = JobTicketId(result.key.target_key);
        let Some(ticket) = board.ticket(ticket_id) else {
            continue;
        };
        if ticket.domain != JobDomain::Medical
            || ticket.state != JobTicketState::Completed(result.claim)
        {
            continue;
        }
        let Some(case_id) = cases.case_for_ticket(ticket_id) else {
            continue;
        };
        let Some(case) = cases.get(case_id).cloned() else {
            continue;
        };
        if board.take_completed(ticket_id, result.claim).is_err() {
            continue;
        }
        if case.status != MedicalCaseStatus::AwaitingResponder
            || patients.get(case.patient).is_err()
        {
            continue;
        }

        let bed_claim = ReservationOwner {
            agent: result.agent,
            action_instance: TRANSPORT_INSTANCE_PREFIX ^ case_id.0,
        };
        let bed = MEDICAL_BEDS.into_iter().find_map(|bed| {
            let spot = spots.get(bed)?;
            let key = ReservationKey(format!("utility.spot.{bed}"));
            reservations
                .reserve(key, spot.capacity, bed_claim)
                .ok()
                .map(|_| (bed.to_string(), spot.at))
        });
        let Some((bed, bed_at)) = bed else {
            continue;
        };

        let Ok((responder_at, mut owner, mut locomotion, mut activity)) =
            responders.get_mut(result.agent)
        else {
            reservations.release_owner(bed_claim);
            continue;
        };
        if try_handoff(
            &mut owner,
            ControlOwner::UtilityAction,
            ControlOwner::MedicalTransport,
        )
        .is_err()
        {
            reservations.release_owner(bed_claim);
            continue;
        }
        *locomotion = LocomotionOwner::MedicalTransport;
        *activity = NpcActivity::Traveling;
        let bed_at = bed_at.with_y(responder_at.translation.y);
        let active_case = cases
            .cases
            .get_mut(&case_id)
            .expect("the response case still exists");
        active_case.status = MedicalCaseStatus::Transporting {
            responder: result.agent,
            bed: bed.clone(),
        };
        active_case.bed_claim = Some(bed_claim);

        commands.entity(case.patient).insert(TransportedPatient {
            responder: result.agent,
        });
        commands.entity(result.agent).insert(MedicalTransportTask {
            case: case_id,
            patient: case.patient,
            bed,
            bed_at,
            bed_claim,
        });
        send_on_errand_with_reach(
            &mut commands,
            result.agent,
            ErrandGoal::Point(bed_at),
            TRANSPORT_REACH,
        );
    }
}

fn follow_medical_transport(
    transports: Query<
        (
            Entity,
            &MedicalTransportTask,
            &ControlOwner,
            &LocomotionOwner,
            &crate::body::Body,
            &crate::body::Bloodstream,
        ),
        Without<MedicalPatient>,
    >,
    passengers: Query<&TransportedPatient>,
    mut transforms: Query<&mut Transform>,
) {
    for (responder, transport, owner, locomotion, body, blood) in &transports {
        if *owner != ControlOwner::MedicalTransport
            || *locomotion != LocomotionOwner::MedicalTransport
            || body.0.collapsed
            || blood.0.incapacitated()
        {
            continue;
        }
        let Ok(passenger) = passengers.get(transport.patient) else {
            continue;
        };
        if passenger.responder != responder {
            continue;
        }
        let Ok([responder_at, mut patient_at]) =
            transforms.get_many_mut([responder, transport.patient])
        else {
            continue;
        };
        let y = patient_at.translation.y;
        let offset = responder_at.rotation * Vec3::new(-0.6, 0.0, -0.2);
        patient_at.translation = responder_at.translation + offset;
        patient_at.translation.y = y;
        patient_at.rotation = responder_at.rotation;
    }
}

#[allow(clippy::type_complexity)]
fn finish_medical_transport(
    mut commands: Commands,
    mut arrivals: MessageReader<ErrandResolved>,
    mut cases: ResMut<MedicalCaseLedger>,
    mut reservations: ResMut<ReservationBook>,
    mut responders: Query<
        (
            &MedicalTransportTask,
            &mut ControlOwner,
            &mut LocomotionOwner,
            &mut NpcActivity,
        ),
        Without<MedicalPatient>,
    >,
    mut patients: Query<
        (
            &MedicalPatient,
            &mut Transform,
            &mut ControlOwner,
            &mut LocomotionOwner,
            &mut NpcActivity,
            &mut NpcPosture,
            Option<&TransportedPatient>,
        ),
        Without<MedicalTransportTask>,
    >,
) {
    for arrival in arrivals.read() {
        let Ok((transport, mut owner, mut locomotion, mut activity)) =
            responders.get_mut(arrival.walker)
        else {
            continue;
        };
        let transport = transport.clone();
        if *owner != ControlOwner::MedicalTransport
            || *locomotion != LocomotionOwner::MedicalTransport
        {
            continue;
        }

        let Ok((
            medical_patient,
            mut at,
            patient_owner,
            mut patient_locomotion,
            mut patient_activity,
            mut posture,
            transported,
        )) = patients.get_mut(transport.patient)
        else {
            abort_medical_transport(
                &mut commands,
                &mut cases,
                &mut reservations,
                arrival.walker,
                &transport,
                &mut owner,
                &mut locomotion,
                &mut activity,
                false,
            );
            continue;
        };
        let attached_to_this_responder =
            transported.is_some_and(|transported| transported.responder == arrival.walker);
        if medical_patient.case != transport.case
            || *patient_owner != ControlOwner::MedicalTransport
            || !attached_to_this_responder
        {
            abort_medical_transport(
                &mut commands,
                &mut cases,
                &mut reservations,
                arrival.walker,
                &transport,
                &mut owner,
                &mut locomotion,
                &mut activity,
                attached_to_this_responder,
            );
            continue;
        }

        match arrival.outcome {
            ErrandOutcome::Arrived => {
                let inpatient_claim = ReservationOwner {
                    agent: transport.patient,
                    action_instance: TRANSPORT_INSTANCE_PREFIX ^ transport.case.0,
                };
                let bed_key = ReservationKey(format!("utility.spot.{}", transport.bed));
                if reservations
                    .transfer(&bed_key, transport.bed_claim, inpatient_claim)
                    .is_err()
                {
                    *patient_activity = NpcActivity::Helping;
                    *posture = NpcPosture::Standing;
                    abort_medical_transport(
                        &mut commands,
                        &mut cases,
                        &mut reservations,
                        arrival.walker,
                        &transport,
                        &mut owner,
                        &mut locomotion,
                        &mut activity,
                        true,
                    );
                    continue;
                }
                *patient_locomotion = LocomotionOwner::None;
                commands
                    .entity(transport.patient)
                    .remove::<TransportedPatient>()
                    .remove::<Errand>()
                    .remove::<CrewRoute>();
                at.translation = transport.bed_at.with_y(at.translation.y);
                *patient_activity = NpcActivity::Resting;
                *posture = NpcPosture::Lying;
                let case = cases
                    .cases
                    .get_mut(&transport.case)
                    .expect("the transported case still exists");
                case.status = MedicalCaseStatus::Admitted { bed: transport.bed };
                case.bed_claim = Some(inpatient_claim);
                commands.entity(transport.patient).insert(InpatientCare {
                    case: transport.case,
                    elapsed: 0.0,
                    bed_claim: inpatient_claim,
                });
                release_medical_responder(
                    &mut commands,
                    arrival.walker,
                    &mut owner,
                    &mut locomotion,
                    &mut activity,
                );
            }
            ErrandOutcome::Unreachable => {
                *patient_activity = NpcActivity::Helping;
                *posture = NpcPosture::Standing;
                abort_medical_transport(
                    &mut commands,
                    &mut cases,
                    &mut reservations,
                    arrival.walker,
                    &transport,
                    &mut owner,
                    &mut locomotion,
                    &mut activity,
                    true,
                );
            }
        }
    }
}

fn release_medical_responder(
    commands: &mut Commands,
    responder: Entity,
    owner: &mut ControlOwner,
    locomotion: &mut LocomotionOwner,
    activity: &mut NpcActivity,
) {
    if try_handoff(
        owner,
        ControlOwner::MedicalTransport,
        ControlOwner::UtilityAction,
    )
    .is_err()
    {
        return;
    }
    *locomotion = LocomotionOwner::None;
    *activity = NpcActivity::Idle;
    commands
        .entity(responder)
        .remove::<MedicalTransportTask>()
        .remove::<Errand>()
        .insert(CrewRoute::standing());
}

#[allow(clippy::too_many_arguments)]
fn abort_medical_transport(
    commands: &mut Commands,
    cases: &mut MedicalCaseLedger,
    reservations: &mut ReservationBook,
    responder: Entity,
    transport: &MedicalTransportTask,
    owner: &mut ControlOwner,
    locomotion: &mut LocomotionOwner,
    activity: &mut NpcActivity,
    detach_patient: bool,
) {
    reservations.release_owner(transport.bed_claim);
    if let Some(case) = cases.cases.get_mut(&transport.case) {
        case.status = MedicalCaseStatus::AwaitingResponder;
        case.bed_claim = None;
    }
    if detach_patient {
        commands
            .entity(transport.patient)
            .remove::<TransportedPatient>()
            .insert((NpcActivity::Helping, NpcPosture::Standing));
    }
    release_medical_responder(commands, responder, owner, locomotion, activity);
}

#[allow(clippy::type_complexity)]
fn tick_inpatient_care(
    mut commands: Commands,
    time: Res<Time>,
    mut cases: ResMut<MedicalCaseLedger>,
    mut incidents: ResMut<IncidentLedger>,
    mut reservations: ResMut<ReservationBook>,
    mut resolved: MessageWriter<IncidentResolved>,
    mut patients: Query<(
        Entity,
        Option<&CrewMember>,
        &MedicalPatient,
        &mut InpatientCare,
        &mut crate::body::Body,
        Option<&mut crate::body::Bloodstream>,
        &mut ControlOwner,
        &mut LocomotionOwner,
        &mut NpcActivity,
        &mut NpcPosture,
    )>,
) {
    for (
        entity,
        member,
        patient,
        mut care,
        mut body,
        mut blood,
        mut owner,
        mut locomotion,
        mut activity,
        mut posture,
    ) in &mut patients
    {
        care.elapsed += time.delta_secs();
        if care.elapsed < STANDARD_BURN_CARE_SECONDS {
            continue;
        }
        if care.case != patient.case {
            continue;
        }
        let Some(case) = cases.get(care.case).cloned() else {
            continue;
        };
        if !matches!(case.status, MedicalCaseStatus::Admitted { .. }) {
            continue;
        }

        let needs_linked_treatment =
            case.kind != IncidentKind::Burn || case.severity.get() > STANDARD_CARE_MAX_SEVERITY;
        if needs_linked_treatment {
            let active_case = cases
                .cases
                .get_mut(&care.case)
                .expect("the inpatient case still exists");
            active_case.status = MedicalCaseStatus::NeedsTreatment;
            active_case.next_request_at = time.elapsed_secs();
            care.elapsed = 0.0;
            continue;
        }

        let previously_collapsed = body.0.collapsed;
        body.0.heal(chem_sim::Damage::of(
            chem_sim::DamageKind::Burn,
            chem_sim::Units::whole(STANDARD_BURN_CARE_UNITS),
        ));
        if let Some(blood) = blood.as_deref_mut() {
            blood
                .0
                .reconcile_collapse(&mut body.0, previously_collapsed);
        }
        if body.0.collapsed
            || blood
                .as_deref()
                .is_some_and(|blood| blood.0.incapacitated())
        {
            let active_case = cases
                .cases
                .get_mut(&care.case)
                .expect("the inpatient case still exists");
            active_case.status = MedicalCaseStatus::NeedsTreatment;
            active_case.next_request_at = time.elapsed_secs();
            care.elapsed = 0.0;
            continue;
        }
        resolve_case_incidents(&case, &mut incidents, &mut resolved);
        cases
            .cases
            .get_mut(&care.case)
            .expect("the inpatient case still exists")
            .status = MedicalCaseStatus::Resolved;
        reservations.release_owner(care.bed_claim);
        commands.entity(case.source_entity).despawn();

        if *owner == ControlOwner::MedicalTransport {
            let _ = try_handoff(
                &mut owner,
                ControlOwner::MedicalTransport,
                ControlOwner::UtilityAction,
            );
        }
        *locomotion = LocomotionOwner::None;
        *activity = NpcActivity::Idle;
        *posture = NpcPosture::Standing;
        commands
            .entity(entity)
            .remove::<MedicalPatient>()
            .remove::<InpatientCare>()
            .remove::<TransportedPatient>()
            .insert(CrewRoute::standing());
        if let Some(member) = member {
            commands.entity(entity).insert(Interactable::new(format!(
                "{} — {}",
                member.name, member.role
            )));
        }
    }
}

fn treatment_reagent(kind: IncidentKind) -> (&'static str, Route) {
    match kind {
        IncidentKind::Burn => ("kelotane", Route::Patched),
        IncidentKind::BruteInjury => ("bicaridine", Route::Patched),
        IncidentKind::Poisoning => ("dylovene", Route::Injected),
        _ => ("inaprovaline", Route::Injected),
    }
}

fn treatment_damage(kind: IncidentKind, body: &crate::body::Body) -> Units {
    match kind {
        IncidentKind::Burn => body.0.damage.burn,
        IncidentKind::BruteInjury => body.0.damage.brute,
        IncidentKind::Poisoning => body.0.damage.toxin,
        _ => body.0.damage.total(),
    }
}

fn publish_treatment_request_jobs(
    time: Res<Time>,
    cases: Res<MedicalCaseLedger>,
    mut board: ResMut<JobBoard>,
) {
    let now = time.elapsed_secs();
    for case in cases.active().filter(|case| {
        case.status == MedicalCaseStatus::NeedsTreatment && now >= case.next_request_at
    }) {
        if board.ticket(case.request_ticket).is_some() {
            continue;
        }
        board
            .publish(JobTicket {
                id: case.request_ticket,
                domain: JobDomain::Medical,
                kind: format!("medical.request_treatment.{}", case.id.0),
                target: ActionTarget::Entity(case.patient),
                // The patient walks itself to the clinic; it needs no witness.
                subject: None,
                reservation: ReservationKey(format!("medical.request_treatment.{}", case.id.0)),
                reservation_capacity: 1,
                bucket: UtilityBucket::Important,
                urgency: Normalized::new(0.75 + case.severity.get() * 0.2)
                    .expect("treatment-request urgency is normalized"),
                required_capability: JobCapability::new(MEDICAL_REQUEST_CAPABILITY),
                created_at: now,
                deadline: Some(now + 90.0),
                risk: Normalized::ZERO,
                perform_seconds: 1.5,
                state: JobTicketState::Available,
            })
            .expect("a Medical case publishes at most one treatment-request ticket");
    }
}

/// Captures the exact actor and action instance chosen by utility selection.
/// The actual order recall starts next frame, after generic action cleanup has
/// removed the actor's finished `CurrentAction` and travel state.
fn capture_completed_treatment_request(
    mut commands: Commands,
    mut completed: MessageReader<UtilityActionResolved>,
    cases: Res<MedicalCaseLedger>,
    board: Res<JobBoard>,
) {
    for result in completed.read() {
        if result.key.action != UtilityActionId::PerformJob
            || result.result != ActionResult::Completed
        {
            continue;
        }
        let ticket_id = JobTicketId(result.key.target_key);
        let Some(ticket) = board.ticket(ticket_id) else {
            continue;
        };
        if ticket.domain != JobDomain::Medical
            || ticket.required_capability != JobCapability::new(MEDICAL_REQUEST_CAPABILITY)
            || ticket.state != JobTicketState::Completed(result.claim)
        {
            continue;
        }
        let Some(case_id) = cases.case_for_request_ticket(ticket_id) else {
            continue;
        };
        if cases
            .get(case_id)
            .is_none_or(|case| case.status != MedicalCaseStatus::NeedsTreatment)
        {
            continue;
        }
        commands
            .entity(result.agent)
            .insert(PendingMedicalRequestStart {
                case: case_id,
                ticket: ticket_id,
                claim: result.claim,
            });
    }
}

/// Complex cases enter the same conversation and physical handoff pipeline as
/// every other Chemistry request. The requester is the actor selected by the
/// shared utility/job system, not a hardcoded name chosen by the case adapter.
#[allow(clippy::too_many_arguments)]
fn start_pending_medical_requests(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<crate::chem_data::ChemDb>,
    mut cases: ResMut<MedicalCaseLedger>,
    mut board: ResMut<JobBoard>,
    mut intake: Intake,
    mut residents: AvailableResidents,
    starts: Query<(Entity, &PendingMedicalRequestStart), Without<CurrentAction>>,
    patients: Query<(&Transform, &CrewMember), With<MedicalPatient>>,
) {
    for (selected_requester, start) in &starts {
        let Some(case) = cases.get(start.case).cloned() else {
            board.cancel(start.ticket);
            commands
                .entity(selected_requester)
                .remove::<PendingMedicalRequestStart>();
            continue;
        };
        if case.status != MedicalCaseStatus::NeedsTreatment {
            board.cancel(start.ticket);
            commands
                .entity(selected_requester)
                .remove::<PendingMedicalRequestStart>();
            continue;
        }
        let Ok((patient_at, patient_member)) = patients.get(case.patient) else {
            continue;
        };
        let Ok((_, requester_member, ..)) = residents.get_mut(selected_requester) else {
            let _ = board.reopen_completed(start.ticket, start.claim);
            commands
                .entity(selected_requester)
                .remove::<PendingMedicalRequestStart>();
            continue;
        };
        let requester_name = requester_member.name.clone();
        let requester_role = requester_member.role.clone();

        let mut retry = Timer::from_seconds(0.0, TimerMode::Once);
        let Some(context) = intake.admit(
            RequestSource::MedicalCase,
            &requester_name,
            &mut retry,
            false,
        ) else {
            let _ = board.reopen_completed(start.ticket, start.claim);
            let active_case = cases
                .cases
                .get_mut(&start.case)
                .expect("the treatment case still exists");
            active_case.next_request_at = time.elapsed_secs() + 0.5;
            commands
                .entity(selected_requester)
                .remove::<PendingMedicalRequestStart>();
            continue;
        };
        let recalled = recall_resident_for_order(
            &mut commands,
            &mut residents,
            &requester_name,
            &requester_role,
            0.0,
        );
        if recalled != Some(selected_requester) {
            intake.cancel_admission(&requester_name);
            let _ = board.reopen_completed(start.ticket, start.claim);
            commands
                .entity(selected_requester)
                .remove::<PendingMedicalRequestStart>();
            continue;
        }
        if board.take_completed(start.ticket, start.claim).is_err() {
            intake.cancel_admission(&requester_name);
            commands
                .entity(selected_requester)
                .remove::<PendingMedicalRequestStart>();
            continue;
        }

        let (reagent, route) = treatment_reagent(case.kind);
        let order = Order {
            reagent: db.reagent(reagent),
            specific: false,
            minimum_purity: 0.8,
            amount: Units::whole(TREATMENT_ORDER_AMOUNT),
            plea: format!(
                "{} needs treatment in Medical. I will carry it back and use it on them.",
                patient_member.name
            ),
            patience: 180.0,
            waited: 0.0,
        };
        commands.entity(selected_requester).insert((
            PendingOrder::new(order, context),
            OrderUse::medical(
                case.patient,
                Some(case.source_entity),
                patient_at.translation,
                route,
                Units::whole(TREATMENT_DOSE),
            ),
            MedicalTreatmentRequester { case: start.case },
            Interactable::new(format!("{requester_name} - treatment request")),
        ));
        commands
            .entity(selected_requester)
            .remove::<PendingMedicalRequestStart>();

        let active_case = cases
            .cases
            .get_mut(&start.case)
            .expect("the treatment case still exists");
        active_case.status = MedicalCaseStatus::TreatmentRequested {
            requester: selected_requester,
        };
        active_case.treatment_attempts = active_case.treatment_attempts.saturating_add(1);
    }
}

/// Converts arrival-side chemistry facts into Medical state. A merely accepted
/// order is not a cure: contaminated, illicit, overdosed, empty, or missing-
/// target applications keep the incident open and can exhaust retries.
fn observe_treatment_fulfillments(
    mut commands: Commands,
    time: Res<Time>,
    mut applied: MessageReader<FulfillmentApplied>,
    mut cases: ResMut<MedicalCaseLedger>,
    patients: Query<&crate::body::Body, With<MedicalPatient>>,
) {
    for report in applied.read() {
        let Some(source) = report.source else {
            continue;
        };
        let Some(case_id) = cases
            .cases
            .values()
            .find(|case| case.source_entity == source)
            .map(|case| case.id)
        else {
            continue;
        };
        let Some(snapshot) = cases.get(case_id).cloned() else {
            continue;
        };
        if snapshot.patient != report.beneficiary
            || !matches!(
                snapshot.status,
                MedicalCaseStatus::TreatmentRequested { requester }
                    if requester == report.carrier
            )
        {
            continue;
        }

        commands
            .entity(report.carrier)
            .remove::<MedicalTreatmentRequester>();
        let clean_helpful_application = report.result == FulfillmentApplicationResult::Applied
            && report.helpful
            && !report.harmful
            && !report.illicit
            && !report.overdose;
        let baseline = patients
            .get(snapshot.patient)
            .ok()
            .map(|body| treatment_damage(snapshot.kind, body));
        let active_case = cases
            .cases
            .get_mut(&case_id)
            .expect("the treatment case still exists");
        if clean_helpful_application {
            active_case.status = MedicalCaseStatus::RecoveringFromTreatment {
                requester: report.carrier,
            };
            active_case.treatment_baseline_damage = baseline;
            active_case.treatment_observation_elapsed = 0.0;
        } else if active_case.treatment_attempts >= MAX_TREATMENT_ATTEMPTS {
            active_case.status = MedicalCaseStatus::Escalated;
        } else {
            active_case.status = MedicalCaseStatus::NeedsTreatment;
            active_case.next_request_at = time.elapsed_secs() + TREATMENT_REQUEST_RETRY_SECONDS;
        }
    }
}

#[allow(clippy::type_complexity)]
fn recover_linked_treatments(
    mut commands: Commands,
    time: Res<Time>,
    mut cases: ResMut<MedicalCaseLedger>,
    mut incidents: ResMut<IncidentLedger>,
    mut reservations: ResMut<ReservationBook>,
    mut resolved: MessageWriter<IncidentResolved>,
    mut patients: Query<(
        Entity,
        Option<&CrewMember>,
        &MedicalPatient,
        &InpatientCare,
        &crate::body::Body,
        Option<&crate::body::Bloodstream>,
        &mut ControlOwner,
        &mut LocomotionOwner,
        &mut NpcActivity,
        &mut NpcPosture,
    )>,
) {
    for (
        entity,
        member,
        patient,
        care,
        body,
        blood,
        mut owner,
        mut locomotion,
        mut activity,
        mut posture,
    ) in &mut patients
    {
        let Some(snapshot) = cases.get(patient.case).cloned() else {
            continue;
        };
        if !matches!(
            snapshot.status,
            MedicalCaseStatus::RecoveringFromTreatment { .. }
        ) {
            continue;
        }
        let current_damage = treatment_damage(snapshot.kind, body);
        let improved = snapshot
            .treatment_baseline_damage
            .is_some_and(|baseline| current_damage < baseline);
        if !improved {
            let active_case = cases
                .cases
                .get_mut(&patient.case)
                .expect("the treatment case still exists");
            active_case.treatment_observation_elapsed += time.delta_secs();
            if active_case.treatment_observation_elapsed >= TREATMENT_OBSERVATION_SECONDS {
                active_case.treatment_baseline_damage = None;
                active_case.treatment_observation_elapsed = 0.0;
                if active_case.treatment_attempts >= MAX_TREATMENT_ATTEMPTS {
                    active_case.status = MedicalCaseStatus::Escalated;
                } else {
                    active_case.status = MedicalCaseStatus::NeedsTreatment;
                    active_case.next_request_at =
                        time.elapsed_secs() + TREATMENT_REQUEST_RETRY_SECONDS;
                }
            }
            continue;
        }

        if body.0.collapsed || blood.is_some_and(|blood| blood.0.incapacitated()) {
            let active_case = cases
                .cases
                .get_mut(&patient.case)
                .expect("the treatment case still exists");
            active_case.treatment_baseline_damage = None;
            active_case.treatment_observation_elapsed = 0.0;
            if active_case.treatment_attempts >= MAX_TREATMENT_ATTEMPTS {
                active_case.status = MedicalCaseStatus::Escalated;
            } else {
                active_case.status = MedicalCaseStatus::NeedsTreatment;
                active_case.next_request_at = time.elapsed_secs() + TREATMENT_REQUEST_RETRY_SECONDS;
            }
            continue;
        }

        resolve_case_incidents(&snapshot, &mut incidents, &mut resolved);
        let active_case = cases
            .cases
            .get_mut(&patient.case)
            .expect("the treatment case still exists");
        active_case.status = MedicalCaseStatus::Resolved;
        active_case.bed_claim = None;
        reservations.release_owner(care.bed_claim);
        commands.entity(snapshot.source_entity).despawn();
        if *owner == ControlOwner::MedicalTransport {
            let _ = try_handoff(
                &mut owner,
                ControlOwner::MedicalTransport,
                ControlOwner::UtilityAction,
            );
        }
        *locomotion = LocomotionOwner::None;
        *activity = NpcActivity::Idle;
        *posture = NpcPosture::Standing;
        commands
            .entity(entity)
            .remove::<MedicalPatient>()
            .remove::<InpatientCare>()
            .remove::<TransportedPatient>()
            .insert(CrewRoute::standing());
        if let Some(member) = member {
            commands.entity(entity).insert(Interactable::new(format!(
                "{} — {}",
                member.name, member.role
            )));
        }
    }
}

/// Reopens a case when its conversation/order vanished before producing an
/// arrival-side outcome. This closes the gap between order expiry and the
/// Medical state machine without introducing a second request queue.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn repair_interrupted_medical_cases(
    mut commands: Commands,
    time: Res<Time>,
    mut cases: ResMut<MedicalCaseLedger>,
    mut incidents: ResMut<IncidentLedger>,
    mut board: ResMut<JobBoard>,
    mut reservations: ResMut<ReservationBook>,
    mut resolved: MessageWriter<IncidentResolved>,
    living: Query<()>,
    mut responders: Query<
        (
            Option<&MedicalTransportTask>,
            &mut ControlOwner,
            &mut LocomotionOwner,
            &mut NpcActivity,
            Option<&crate::body::Body>,
            Option<&crate::body::Bloodstream>,
            Option<&mut SuspendedUtilityControl>,
        ),
        (With<UtilityAgent>, Without<MedicalPatient>),
    >,
    requesters: Query<
        (Has<PendingOrder>, Has<Order>, Has<CarryingFulfillment>),
        With<MedicalTreatmentRequester>,
    >,
    mut patients: Query<(
        &MedicalPatient,
        Option<&TransportedPatient>,
        &mut NpcActivity,
        &mut NpcPosture,
    )>,
) {
    let case_ids: Vec<_> = cases.active().map(|case| case.id).collect();
    for case_id in case_ids {
        let Some(snapshot) = cases.get(case_id).cloned() else {
            continue;
        };

        if living.get(snapshot.patient).is_err() {
            if let Some(claim) = snapshot.bed_claim {
                reservations.release_owner(claim);
            }
            board.cancel(snapshot.response_ticket);
            board.cancel(snapshot.request_ticket);
            resolve_case_incidents(&snapshot, &mut incidents, &mut resolved);
            if let Ok(mut source) = commands.get_entity(snapshot.source_entity) {
                source.despawn();
            }
            if let MedicalCaseStatus::TreatmentRequested { requester } = snapshot.status {
                if let Ok(mut requester) = commands.get_entity(requester) {
                    requester.remove::<MedicalTreatmentRequester>();
                }
            }
            let case = cases
                .cases
                .get_mut(&case_id)
                .expect("the missing-patient case still exists");
            case.status = MedicalCaseStatus::Resolved;
            case.bed_claim = None;
            continue;
        }

        if let MedicalCaseStatus::Transporting { responder, .. } = snapshot.status {
            let mut transport_is_live = false;
            let mut remove_medical_errand = false;
            if let Ok((
                transport,
                mut owner,
                mut locomotion,
                mut activity,
                body,
                blood,
                suspended,
            )) = responders.get_mut(responder)
            {
                transport_is_live = transport.is_some_and(|transport| transport.case == case_id)
                    && *owner == ControlOwner::MedicalTransport
                    && *locomotion == LocomotionOwner::MedicalTransport
                    && !body.is_some_and(|body| body.0.collapsed)
                    && !blood.is_some_and(|blood| blood.0.incapacitated());
                if !transport_is_live {
                    let suspended_medical = suspended
                        .as_deref()
                        .is_some_and(|suspended| suspended.owner == ControlOwner::MedicalTransport);
                    remove_medical_errand = *owner == ControlOwner::MedicalTransport
                        || *locomotion == LocomotionOwner::MedicalTransport
                        || suspended_medical;
                    match *owner {
                        ControlOwner::MedicalTransport => {
                            if try_handoff(
                                &mut owner,
                                ControlOwner::MedicalTransport,
                                ControlOwner::UtilityAction,
                            )
                            .is_ok()
                            {
                                *locomotion = LocomotionOwner::None;
                                *activity = NpcActivity::Idle;
                            }
                        }
                        ControlOwner::Incapacitated => {
                            if let Some(mut suspended) = suspended {
                                if suspended.owner == ControlOwner::MedicalTransport {
                                    suspended.owner = ControlOwner::UtilityAction;
                                    suspended.locomotion = LocomotionOwner::None;
                                }
                            }
                        }
                        _ if *locomotion == LocomotionOwner::MedicalTransport => {
                            *locomotion = LocomotionOwner::None;
                        }
                        _ => {}
                    }
                }
            }
            if !transport_is_live {
                if let Ok(mut responder_commands) = commands.get_entity(responder) {
                    responder_commands.remove::<MedicalTransportTask>();
                    if remove_medical_errand {
                        responder_commands.remove::<Errand>();
                    }
                }
                if let Some(claim) = snapshot.bed_claim {
                    reservations.release_owner(claim);
                }
                if let Ok((patient, passenger, mut activity, mut posture)) =
                    patients.get_mut(snapshot.patient)
                {
                    if patient.case == case_id
                        && passenger.is_some_and(|passenger| passenger.responder == responder)
                    {
                        *activity = NpcActivity::Helping;
                        *posture = NpcPosture::Standing;
                        commands
                            .entity(snapshot.patient)
                            .remove::<TransportedPatient>();
                    }
                }
                let case = cases
                    .cases
                    .get_mut(&case_id)
                    .expect("the interrupted transport case still exists");
                case.status = MedicalCaseStatus::AwaitingResponder;
                case.bed_claim = None;
            }
            continue;
        }

        if let MedicalCaseStatus::TreatmentRequested { requester } = snapshot.status {
            let request_is_live = requesters
                .get(requester)
                .is_ok_and(|(pending, accepted, carrying)| pending || accepted || carrying);
            if request_is_live {
                continue;
            }
            if let Ok(mut entity) = commands.get_entity(requester) {
                entity.remove::<MedicalTreatmentRequester>();
            }
            let case = cases
                .cases
                .get_mut(&case_id)
                .expect("the interrupted request case still exists");
            if case.treatment_attempts >= MAX_TREATMENT_ATTEMPTS {
                case.status = MedicalCaseStatus::Escalated;
            } else {
                case.status = MedicalCaseStatus::NeedsTreatment;
                case.next_request_at = time.elapsed_secs() + TREATMENT_REQUEST_RETRY_SECONDS;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::{Bloodstream, Body};
    use crate::crew::{CrewPosts, ErrandResolved};
    use crate::utility_ai::{
        begin_reference_actions, consume_utility_arrivals, perform_reference_actions,
        resolve_reference_actions, select_reference_actions, sync_utility_incapacity,
        tick_current_actions, UtilityDecisionLog,
    };

    /// Builds an app with only the report-to-incident system.
    fn report_filing_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<IncidentLedger>()
            .add_message::<super::super::reports::CasualtyReported>()
            .add_message::<IncidentCreated>()
            .add_systems(Update, open_cases_from_reports);
        app
    }

    fn file_report(app: &mut App, reporter: Entity, subject: Entity) {
        app.world_mut()
            .write_message(super::super::reports::CasualtyReported {
                reporter,
                subject,
                at: Vec3::ZERO,
                confidence: 0.9,
            });
        app.update();
    }

    fn spawn_collapsed(app: &mut App) -> Entity {
        let mut body = Body::default();
        body.0.collapsed = true;
        app.world_mut().spawn((body, Bloodstream::default())).id()
    }

    /// A report about a body that is genuinely down opens a case; one about
    /// somebody who has since got up does not.
    ///
    /// The walk takes real time, so a stale report is the ordinary case rather
    /// than an edge case. Without the second half a station would accumulate
    /// Medical cases for everyone who ever stumbled and recovered — and
    /// falsification confirmed nothing else pins it.
    #[test]
    fn a_report_opens_a_case_only_while_the_subject_is_actually_down() {
        let mut app = report_filing_app();
        let reporter = app.world_mut().spawn_empty().id();
        let victim = spawn_collapsed(&mut app);
        file_report(&mut app, reporter, victim);
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .active_for(victim)
                .count(),
            1,
            "a witness reporting a real casualty must open a case"
        );

        let mut app = report_filing_app();
        let reporter = app.world_mut().spawn_empty().id();
        let fine = app
            .world_mut()
            .spawn((Body::default(), Bloodstream::default()))
            .id();
        file_report(&mut app, reporter, fine);
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .active_for(fine)
                .count(),
            0,
            "a stale report about someone who got up must not open a case"
        );
    }

    /// A half-glimpsed report is worth saying but not worth a case file.
    ///
    /// Confidence decays during the walk, so the number that matters is what
    /// the witness still believed on arrival — not what sent them.
    #[test]
    fn an_unsure_report_is_passed_on_without_opening_a_case() {
        let mut app = report_filing_app();
        let reporter = app.world_mut().spawn_empty().id();
        let victim = spawn_collapsed(&mut app);
        app.world_mut()
            .write_message(super::super::reports::CasualtyReported {
                reporter,
                subject: victim,
                at: Vec3::ZERO,
                // Above `WORTH_REPORTING`, below `CONFIDENT_ENOUGH_TO_FILE`:
                // worth crossing the room for, not worth a patient.
                confidence: 0.4,
            });
        app.update();

        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .active_for(victim)
                .count(),
            0,
            "an unsure witness must not commit Medical to a case on their own"
        );
    }

    /// A collapsed body does not file its own report.
    #[test]
    fn a_casualty_cannot_be_its_own_reporter() {
        let mut app = report_filing_app();
        let victim = spawn_collapsed(&mut app);
        file_report(&mut app, victim, victim);

        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .active_for(victim)
                .count(),
            0,
            "someone on the floor is not walking over to report themselves"
        );
    }

    /// The reporter is a witness, not a cause.
    ///
    /// Recording them as the incident's source would make raising the alarm
    /// look like guilt — and `interviews` reads exactly that field when
    /// deciding who to name.
    #[test]
    fn filing_a_report_never_attributes_the_incident_to_the_reporter() {
        let mut app = report_filing_app();
        let reporter = app.world_mut().spawn_empty().id();
        let victim = spawn_collapsed(&mut app);
        file_report(&mut app, reporter, victim);

        let ledger = app.world().resource::<IncidentLedger>();
        let incident = ledger
            .active_for(victim)
            .next()
            .expect("the case was opened");
        assert_eq!(
            incident.source, None,
            "the person who raised the alarm must not be recorded as its cause"
        );
        assert_eq!(incident.subject, victim);
    }

    #[test]
    fn medical_responder_escorts_the_exact_casualty_to_a_reserved_bed() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<IncidentLedger>()
            .init_resource::<MedicalCaseLedger>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_message::<IncidentResolved>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_systems(
                Update,
                (
                    sync_utility_incapacity,
                    tick_current_actions,
                    tick_inpatient_care,
                    open_cases_from_incidents,
                    // A response ticket names its patient, so a responder must
                    // actually perceive the casualty before it may answer. Dr.
                    // Vance stands two metres away and does.
                    crate::utility_ai::perception::witness_stimuli,
                    publish_medical_response_jobs,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    follow_medical_transport,
                    finish_medical_transport,
                    perform_reference_actions,
                    resolve_reference_actions,
                    begin_medical_transport,
                    ApplyDeferred,
                )
                    .chain(),
            );
        app.world_mut().resource_mut::<UtilitySpots>().insert(
            MEDICAL_BEDS[0],
            Vec3::new(3.0, 0.0, 0.0),
            1,
        );
        app.world_mut().resource_mut::<UtilitySpots>().insert(
            MEDICAL_BEDS[1],
            Vec3::new(3.0, 0.0, 2.0),
            1,
        );

        let patient = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Miner Sato".into(),
                    role: "Cargo".into(),
                },
                Transform::from_xyz(-2.0, crate::crew::BODY_OFFSET, 0.0),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(7, 0)),
            ))
            .id();
        app.world_mut()
            .get_mut::<Body>(patient)
            .unwrap()
            .0
            .apply(chem_sim::Damage::of(
                chem_sim::DamageKind::Burn,
                chem_sim::Units::whole(STANDARD_BURN_CARE_UNITS),
            ));
        let responder = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Dr. Vance".into(),
                    role: "Medical".into(),
                },
                Transform::from_xyz(-4.0, crate::crew::BODY_OFFSET, 0.0),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(11, 0)),
                medical_profile("Dr. Vance").unwrap(),
            ))
            .id();
        let incident = app
            .world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                patient,
                None,
                Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
                Normalized::new(0.35).unwrap(),
                0.0,
            )
            .unwrap();

        for _ in 0..500 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            if app.world().get::<MedicalTransportTask>(responder).is_some() {
                break;
            }
        }
        assert!(
            app.world().get::<MedicalTransportTask>(responder).is_some(),
            "the responder never began the transport leg",
        );
        app.world_mut()
            .get_mut::<Body>(responder)
            .unwrap()
            .0
            .collapsed = true;
        let paused_at = app.world().get::<Transform>(responder).unwrap().translation;
        for _ in 0..10 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        }
        assert_eq!(
            app.world().get::<Transform>(responder).unwrap().translation,
            paused_at,
            "a collapsed responder must not keep dragging the patient",
        );
        assert!(app.world().get::<MedicalTransportTask>(responder).is_some());
        assert!(app.world().get::<Errand>(responder).is_some());
        assert_eq!(
            app.world().get::<ControlOwner>(responder),
            Some(&ControlOwner::Incapacitated),
        );
        app.world_mut()
            .get_mut::<Body>(responder)
            .unwrap()
            .0
            .collapsed = false;

        let mut observed_laying_in_bed = false;
        let mut observed_patient_owned_bed = false;
        for _ in 0..2_000 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            observed_laying_in_bed |= app.world().get::<NpcPosture>(patient)
                == Some(&NpcPosture::Lying)
                && app.world().get::<InpatientCare>(patient).is_some();
            observed_patient_owned_bed |= app.world().resource::<ReservationBook>().is_reserved_by(
                &ReservationKey(format!("utility.spot.{}", MEDICAL_BEDS[0])),
                ReservationOwner {
                    agent: patient,
                    action_instance: TRANSPORT_INSTANCE_PREFIX ^ incident.0,
                },
            );
            if app
                .world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(incident.0))
                .is_some_and(|case| case.status == MedicalCaseStatus::Resolved)
            {
                break;
            }
        }

        let case_debug = app
            .world()
            .resource::<MedicalCaseLedger>()
            .get(MedicalCaseId(incident.0))
            .cloned();
        let tickets_debug: Vec<_> = app.world().resource::<JobBoard>().iter().cloned().collect();
        assert!(
            observed_laying_in_bed,
            "the patient never occupied a bed; case={case_debug:?}, tickets={tickets_debug:?}, patient_at={:?}, patient_owner={:?}, responder_at={:?}, responder_owner={:?}, responder_errand={}, transport_task={}",
            app.world().get::<Transform>(patient),
            app.world().get::<ControlOwner>(patient),
            app.world().get::<Transform>(responder),
            app.world().get::<ControlOwner>(responder),
            app.world().get::<Errand>(responder).is_some(),
            app.world().get::<MedicalTransportTask>(responder).is_some(),
        );
        assert!(
            observed_patient_owned_bed,
            "the admitted patient must own the bed after responder handoff",
        );
        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(incident.0))
                .unwrap()
                .status,
            MedicalCaseStatus::Resolved,
        );
        assert_eq!(
            app.world().get::<Body>(patient).unwrap().0.damage.burn,
            chem_sim::Units::ZERO,
        );
        assert_eq!(
            app.world().get::<ControlOwner>(patient),
            Some(&ControlOwner::UtilityAction),
        );
        assert_eq!(
            app.world().get::<NpcPosture>(patient),
            Some(&NpcPosture::Standing),
        );
        assert_eq!(
            app.world().get::<ControlOwner>(responder),
            Some(&ControlOwner::UtilityAction),
        );
        assert_eq!(
            app.world()
                .resource::<ReservationBook>()
                .active_claim_count(),
            0,
        );
        assert!(app.world().resource::<JobBoard>().is_empty());
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .get(incident)
                .unwrap()
                .status,
            super::super::IncidentStatus::Resolved,
        );
    }

    #[test]
    fn severe_case_recalls_a_real_cargo_coworker_into_the_shared_order_queue() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<MedicalCaseLedger>()
            .init_resource::<JobBoard>()
            .add_message::<UtilityActionResolved>()
            .insert_resource(crate::chem_data::ChemDb(
                chem_sim::ChemData::from_ron(
                    include_str!("../../assets/data/chem.reagents.ron"),
                    include_str!("../../assets/data/chem.reactions.ron"),
                )
                .unwrap(),
            ))
            .add_systems(
                Update,
                (
                    publish_treatment_request_jobs,
                    capture_completed_treatment_request,
                    ApplyDeferred,
                    start_pending_medical_requests,
                    ApplyDeferred,
                )
                    .chain(),
            );

        let patient = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Miner Sato".into(),
                    role: "Cargo".into(),
                },
                Transform::from_xyz(4.0, crate::crew::BODY_OFFSET, 2.0),
                MedicalPatient {
                    case: MedicalCaseId(41),
                },
            ))
            .id();
        let source = app
            .world_mut()
            .spawn(MedicalCaseSource(MedicalCaseId(41)))
            .id();
        let requester = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Loader Bell".into(),
                    role: "Cargo".into(),
                },
                Body::default(),
                Bloodstream::default(),
                Ambient::new(30.0),
                StationResident,
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(99, 0)),
                NpcJobProfile::new(
                    JobDomain::Cargo,
                    super::super::NarrativeTier::Support,
                    [
                        JobCapability::new("cargo.operations"),
                        JobCapability::new(MEDICAL_REQUEST_CAPABILITY),
                    ],
                )
                .with_cross_training([JobDomain::Medical]),
            ))
            .id();
        app.world_mut()
            .resource_mut::<MedicalCaseLedger>()
            .cases
            .insert(
                MedicalCaseId(41),
                MedicalCase {
                    id: MedicalCaseId(41),
                    incident: super::super::IncidentId(41),
                    related_incidents: Vec::new(),
                    patient,
                    kind: IncidentKind::Burn,
                    severity: Normalized::new(0.65).unwrap(),
                    opened_at: 0.0,
                    response_ticket: JobTicketId(401),
                    request_ticket: JobTicketId(402),
                    status: MedicalCaseStatus::NeedsTreatment,
                    source_entity: source,
                    bed_claim: None,
                    next_request_at: 0.0,
                    treatment_attempts: 0,
                    treatment_observation_elapsed: 0.0,
                    treatment_baseline_damage: None,
                },
            );

        app.update();

        let claim = ReservationOwner {
            agent: requester,
            action_instance: 55,
        };
        let ticket = app
            .world()
            .resource::<JobBoard>()
            .ticket(JobTicketId(402))
            .unwrap();
        assert!(ticket.available_to(app.world().get::<NpcJobProfile>(requester).unwrap()));
        app.world_mut()
            .resource_mut::<JobBoard>()
            .claim(JobTicketId(402), claim)
            .unwrap();
        app.world_mut()
            .resource_mut::<JobBoard>()
            .resolve(JobTicketId(402), claim, ActionResult::Completed)
            .unwrap();
        app.world_mut().write_message(UtilityActionResolved {
            agent: requester,
            key: super::super::ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: 402,
            },
            claim,
            result: ActionResult::Completed,
        });
        app.update();

        let pending = app
            .world()
            .get::<PendingOrder>(requester)
            .expect("the Cargo coworker should enter the normal conversation queue");
        assert_eq!(pending.context.source, RequestSource::MedicalCase);
        assert!(pending.order.plea.contains("Miner Sato"));
        let use_context = app.world().get::<OrderUse>(requester).unwrap();
        assert_eq!(use_context.beneficiary, patient);
        assert_eq!(use_context.source, Some(source));
        assert_eq!(use_context.use_destination.x, 4.0);
        assert_eq!(
            app.world().get::<ControlOwner>(requester),
            Some(&ControlOwner::OrderVisit),
        );
        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(41))
                .unwrap()
                .status,
            MedicalCaseStatus::TreatmentRequested { requester },
        );
    }

    #[test]
    fn simultaneous_injury_kinds_open_only_one_active_case_for_a_patient() {
        let mut app = App::new();
        app.init_resource::<IncidentLedger>()
            .init_resource::<MedicalCaseLedger>()
            .init_resource::<ReservationBook>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_systems(Update, (open_cases_from_incidents, ApplyDeferred).chain());
        let patient = app
            .world_mut()
            .spawn(UtilityControlBundle::new(UtilityAgent::new(44, 0)))
            .id();
        let mut incident_ids = Vec::new();
        for kind in [IncidentKind::Burn, IncidentKind::Poisoning] {
            incident_ids.push(
                app.world_mut()
                    .resource_mut::<IncidentLedger>()
                    .create(
                        kind,
                        JobDomain::Cargo,
                        patient,
                        None,
                        Vec3::ZERO,
                        Normalized::new(0.5).unwrap(),
                        0.0,
                    )
                    .unwrap(),
            );
        }

        app.update();

        let cases = app.world().resource::<MedicalCaseLedger>();
        assert_eq!(cases.active().count(), 1);
        let case = cases.active().next().expect("one aggregate case");
        assert_eq!(case.kind, IncidentKind::Poisoning);
        assert_eq!(case.related_incidents.len(), 1);
        assert!(incident_ids
            .iter()
            .all(|incident| cases.contains_incident(*incident)));
        assert_eq!(
            app.world()
                .get::<MedicalPatient>(patient)
                .map(|patient| patient.case),
            Some(case.id),
        );
    }

    #[test]
    fn a_responder_who_becomes_a_casualty_requeues_the_old_patient_and_bed() {
        let mut app = App::new();
        app.init_resource::<IncidentLedger>()
            .init_resource::<MedicalCaseLedger>()
            .init_resource::<ReservationBook>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_systems(Update, (open_cases_from_incidents, ApplyDeferred).chain());

        let old_case = MedicalCaseId(61);
        let mut responder_control = UtilityControlBundle::new(UtilityAgent::new(61, 0));
        responder_control.control = ControlOwner::MedicalTransport;
        responder_control.locomotion = LocomotionOwner::MedicalTransport;
        responder_control.activity = NpcActivity::Traveling;
        let responder = app
            .world_mut()
            .spawn((
                responder_control,
                Transform::from_translation(Vec3::X),
                Body::default(),
                Bloodstream::default(),
            ))
            .id();
        let old_claim = ReservationOwner {
            agent: responder,
            action_instance: 61,
        };
        let mut patient_control = UtilityControlBundle::new(UtilityAgent::new(62, 0));
        patient_control.control = ControlOwner::MedicalTransport;
        patient_control.activity = NpcActivity::Helping;
        let old_patient = app
            .world_mut()
            .spawn((
                patient_control,
                MedicalPatient { case: old_case },
                TransportedPatient { responder },
            ))
            .id();
        app.world_mut()
            .entity_mut(responder)
            .insert(MedicalTransportTask {
                case: old_case,
                patient: old_patient,
                bed: "medical.bed.1".into(),
                bed_at: Vec3::new(4.0, 0.0, 0.0),
                bed_claim: old_claim,
            });
        {
            let mut commands = app.world_mut().commands();
            crate::crew::send_on_errand(
                &mut commands,
                responder,
                crate::crew::ErrandGoal::Point(Vec3::new(4.0, 0.0, 0.0)),
            );
        }
        app.world_mut().flush();
        app.world_mut()
            .resource_mut::<ReservationBook>()
            .reserve(
                ReservationKey("utility.spot.medical.bed.1".into()),
                1,
                old_claim,
            )
            .unwrap();

        let old_incident = app
            .world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                old_patient,
                None,
                Vec3::ZERO,
                Normalized::new(0.5).unwrap(),
                0.0,
            )
            .unwrap();
        let source = app.world_mut().spawn(MedicalCaseSource(old_case)).id();
        app.world_mut()
            .resource_mut::<MedicalCaseLedger>()
            .cases
            .insert(
                old_case,
                MedicalCase {
                    id: old_case,
                    incident: old_incident,
                    related_incidents: Vec::new(),
                    patient: old_patient,
                    kind: IncidentKind::Burn,
                    severity: Normalized::new(0.5).unwrap(),
                    opened_at: 0.0,
                    response_ticket: JobTicketId(610),
                    request_ticket: JobTicketId(611),
                    status: MedicalCaseStatus::Transporting {
                        responder,
                        bed: "medical.bed.1".into(),
                    },
                    source_entity: source,
                    bed_claim: Some(old_claim),
                    next_request_at: 0.0,
                    treatment_attempts: 0,
                    treatment_observation_elapsed: 0.0,
                    treatment_baseline_damage: None,
                },
            );
        let new_incident = app
            .world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::BruteInjury,
                JobDomain::Medical,
                responder,
                None,
                Vec3::X,
                Normalized::new(0.6).unwrap(),
                1.0,
            )
            .unwrap();

        app.update();

        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(old_case)
                .unwrap()
                .status,
            MedicalCaseStatus::AwaitingResponder,
        );
        assert!(app.world().get::<TransportedPatient>(old_patient).is_none());
        assert!(app.world().get::<MedicalTransportTask>(responder).is_none());
        assert!(app.world().get::<crate::crew::Errand>(responder).is_none());
        assert_eq!(
            app.world().get::<LocomotionOwner>(responder),
            Some(&LocomotionOwner::None),
        );
        assert_eq!(
            app.world()
                .get::<MedicalPatient>(responder)
                .map(|patient| patient.case),
            Some(MedicalCaseId(new_incident.0)),
        );
        assert!(!app.world().resource::<ReservationBook>().is_reserved_by(
            &ReservationKey("utility.spot.medical.bed.1".into()),
            old_claim,
        ));
    }

    #[test]
    fn interrupted_collapsed_responder_recovers_to_utility_not_orphaned_transport() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<IncidentLedger>()
            .init_resource::<MedicalCaseLedger>()
            .init_resource::<ReservationBook>()
            .init_resource::<JobBoard>()
            .add_message::<IncidentResolved>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_systems(
                Update,
                (
                    sync_utility_incapacity,
                    repair_interrupted_medical_cases,
                    ApplyDeferred,
                )
                    .chain(),
            );

        let case_id = MedicalCaseId(63);
        let mut responder_control = UtilityControlBundle::new(UtilityAgent::new(63, 0));
        responder_control.control = ControlOwner::Incapacitated;
        responder_control.activity = NpcActivity::Down;
        let mut collapsed = Body::default();
        collapsed.0.collapsed = true;
        let responder = app
            .world_mut()
            .spawn((
                responder_control,
                collapsed,
                Bloodstream::default(),
                super::super::SuspendedUtilityControl {
                    owner: ControlOwner::MedicalTransport,
                    locomotion: LocomotionOwner::MedicalTransport,
                },
            ))
            .id();
        let bed_claim = ReservationOwner {
            agent: responder,
            action_instance: 63,
        };
        let mut patient_control = UtilityControlBundle::new(UtilityAgent::new(64, 0));
        patient_control.control = ControlOwner::MedicalTransport;
        patient_control.activity = NpcActivity::Helping;
        let patient = app
            .world_mut()
            .spawn((
                patient_control,
                MedicalPatient { case: case_id },
                TransportedPatient { responder },
            ))
            .id();
        app.world_mut()
            .entity_mut(responder)
            .insert(MedicalTransportTask {
                case: case_id,
                patient,
                bed: "medical.bed.1".into(),
                bed_at: Vec3::new(4.0, 0.0, 0.0),
                bed_claim,
            });
        {
            let mut commands = app.world_mut().commands();
            crate::crew::send_on_errand(
                &mut commands,
                responder,
                crate::crew::ErrandGoal::Point(Vec3::new(4.0, 0.0, 0.0)),
            );
        }
        app.world_mut().flush();
        app.world_mut()
            .resource_mut::<ReservationBook>()
            .reserve(
                ReservationKey("utility.spot.medical.bed.1".into()),
                1,
                bed_claim,
            )
            .unwrap();
        let incident = app
            .world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                patient,
                None,
                Vec3::ZERO,
                Normalized::new(0.5).unwrap(),
                0.0,
            )
            .unwrap();
        let source = app.world_mut().spawn(MedicalCaseSource(case_id)).id();
        app.world_mut()
            .resource_mut::<MedicalCaseLedger>()
            .cases
            .insert(
                case_id,
                MedicalCase {
                    id: case_id,
                    incident,
                    related_incidents: Vec::new(),
                    patient,
                    kind: IncidentKind::Burn,
                    severity: Normalized::new(0.5).unwrap(),
                    opened_at: 0.0,
                    response_ticket: JobTicketId(630),
                    request_ticket: JobTicketId(631),
                    status: MedicalCaseStatus::Transporting {
                        responder,
                        bed: "medical.bed.1".into(),
                    },
                    source_entity: source,
                    bed_claim: Some(bed_claim),
                    next_request_at: 0.0,
                    treatment_attempts: 0,
                    treatment_observation_elapsed: 0.0,
                    treatment_baseline_damage: None,
                },
            );

        app.update();

        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(case_id)
                .unwrap()
                .status,
            MedicalCaseStatus::AwaitingResponder,
        );
        assert!(app.world().get::<MedicalTransportTask>(responder).is_none());
        assert!(app.world().get::<crate::crew::Errand>(responder).is_none());
        assert!(app.world().get::<TransportedPatient>(patient).is_none());
        let suspended = app
            .world()
            .get::<super::super::SuspendedUtilityControl>(responder)
            .unwrap();
        assert_eq!(suspended.owner, ControlOwner::UtilityAction);
        assert_eq!(suspended.locomotion, LocomotionOwner::None);
        assert!(!app.world().resource::<ReservationBook>().is_reserved_by(
            &ReservationKey("utility.spot.medical.bed.1".into()),
            bed_claim,
        ));

        app.world_mut()
            .get_mut::<Body>(responder)
            .unwrap()
            .0
            .collapsed = false;
        app.update();

        assert_eq!(
            app.world().get::<ControlOwner>(responder),
            Some(&ControlOwner::UtilityAction),
        );
        assert_eq!(
            app.world().get::<LocomotionOwner>(responder),
            Some(&LocomotionOwner::None),
        );
        assert!(app
            .world()
            .get::<super::super::SuspendedUtilityControl>(responder)
            .is_none());
    }

    fn treatment_outcome_app(
        attempts: u8,
    ) -> (App, Entity, Entity, Entity, super::super::IncidentId) {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<MedicalCaseLedger>()
            .init_resource::<IncidentLedger>()
            .init_resource::<ReservationBook>()
            .init_resource::<JobBoard>()
            .add_message::<FulfillmentApplied>()
            .add_message::<IncidentResolved>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_systems(
                Update,
                (
                    observe_treatment_fulfillments,
                    recover_linked_treatments,
                    repair_interrupted_medical_cases,
                    ApplyDeferred,
                )
                    .chain(),
            );
        let case_id = MedicalCaseId(77);
        let patient = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Patient Rowan".into(),
                    role: "Cargo".into(),
                },
                Body::default(),
                MedicalPatient { case: case_id },
                InpatientCare {
                    case: case_id,
                    elapsed: 0.0,
                    bed_claim: ReservationOwner {
                        agent: Entity::from_bits(900),
                        action_instance: 77,
                    },
                },
                ControlOwner::MedicalTransport,
                LocomotionOwner::None,
                NpcActivity::Resting,
                NpcPosture::Lying,
            ))
            .id();
        app.world_mut()
            .get_mut::<Body>(patient)
            .unwrap()
            .0
            .damage
            .burn = Units::whole(30);
        let requester = app
            .world_mut()
            .spawn(MedicalTreatmentRequester { case: case_id })
            .id();
        let source = app.world_mut().spawn(MedicalCaseSource(case_id)).id();
        let incident = app
            .world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                patient,
                None,
                Vec3::ZERO,
                Normalized::new(0.65).unwrap(),
                0.0,
            )
            .unwrap();
        let bed_claim = ReservationOwner {
            agent: Entity::from_bits(900),
            action_instance: 77,
        };
        app.world_mut()
            .resource_mut::<ReservationBook>()
            .reserve(
                ReservationKey("utility.spot.medical.bed.1".into()),
                1,
                bed_claim,
            )
            .unwrap();
        app.world_mut()
            .resource_mut::<MedicalCaseLedger>()
            .cases
            .insert(
                case_id,
                MedicalCase {
                    id: case_id,
                    incident,
                    related_incidents: Vec::new(),
                    patient,
                    kind: IncidentKind::Burn,
                    severity: Normalized::new(0.65).unwrap(),
                    opened_at: 0.0,
                    response_ticket: JobTicketId(770),
                    request_ticket: JobTicketId(771),
                    status: MedicalCaseStatus::TreatmentRequested { requester },
                    source_entity: source,
                    bed_claim: Some(bed_claim),
                    next_request_at: 0.0,
                    treatment_attempts: attempts,
                    treatment_observation_elapsed: 0.0,
                    treatment_baseline_damage: None,
                },
            );
        (app, patient, requester, source, incident)
    }

    #[test]
    fn clean_linked_treatment_resolves_only_after_the_patient_actually_improves() {
        let (mut app, patient, requester, source, incident) = treatment_outcome_app(1);
        app.world_mut().write_message(FulfillmentApplied {
            carrier: requester,
            beneficiary: patient,
            source: Some(source),
            container: Entity::from_bits(501),
            result: FulfillmentApplicationResult::Applied,
            helpful: true,
            harmful: false,
            illicit: false,
            overdose: false,
        });

        app.update();
        assert!(matches!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(77))
                .unwrap()
                .status,
            MedicalCaseStatus::RecoveringFromTreatment { requester: observed }
                if observed == requester
        ));
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .get(incident)
                .unwrap()
                .status,
            super::super::IncidentStatus::Active,
            "delivery alone must not close the case",
        );

        app.world_mut()
            .get_mut::<Body>(patient)
            .unwrap()
            .0
            .damage
            .burn = Units::whole(29);
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.1));
        app.update();

        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(77))
                .unwrap()
                .status,
            MedicalCaseStatus::Resolved,
        );
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .get(incident)
                .unwrap()
                .status,
            super::super::IncidentStatus::Resolved,
        );
        assert!(app.world().get::<MedicalPatient>(patient).is_none());
        assert!(app.world().get::<Interactable>(patient).is_some());
        assert_eq!(
            app.world().get::<ControlOwner>(patient),
            Some(&ControlOwner::UtilityAction),
        );
        assert_eq!(
            app.world()
                .resource::<ReservationBook>()
                .active_claim_count(),
            0,
        );
        assert!(app.world().get_entity(source).is_err());
    }

    #[test]
    fn partial_improvement_never_discharges_a_still_collapsed_patient() {
        let (mut app, patient, requester, source, incident) = treatment_outcome_app(1);
        app.world_mut().write_message(FulfillmentApplied {
            carrier: requester,
            beneficiary: patient,
            source: Some(source),
            container: Entity::from_bits(502),
            result: FulfillmentApplicationResult::Applied,
            helpful: true,
            harmful: false,
            illicit: false,
            overdose: false,
        });
        app.update();

        {
            let mut body = app.world_mut().get_mut::<Body>(patient).unwrap();
            body.0.damage.burn = Units::whole(29);
            body.0.collapsed = true;
        }
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.1));
        app.update();

        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(77))
                .unwrap()
                .status,
            MedicalCaseStatus::NeedsTreatment,
        );
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .get(incident)
                .unwrap()
                .status,
            super::super::IncidentStatus::Active,
        );
        assert!(app.world().get::<MedicalPatient>(patient).is_some());
        assert!(app.world().get::<InpatientCare>(patient).is_some());
        assert!(app.world().get::<Interactable>(patient).is_none());
        assert_eq!(
            app.world().get::<NpcPosture>(patient),
            Some(&NpcPosture::Lying),
        );
        assert_eq!(
            app.world()
                .resource::<ReservationBook>()
                .active_claim_count(),
            1,
        );
    }

    #[test]
    fn harmful_linked_treatment_keeps_the_patient_admitted_and_reopens_the_request() {
        let (mut app, patient, requester, source, incident) = treatment_outcome_app(1);
        app.world_mut().write_message(FulfillmentApplied {
            carrier: requester,
            beneficiary: patient,
            source: Some(source),
            container: Entity::from_bits(502),
            result: FulfillmentApplicationResult::Applied,
            helpful: true,
            harmful: true,
            illicit: false,
            overdose: false,
        });

        app.update();

        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(77))
                .unwrap()
                .status,
            MedicalCaseStatus::NeedsTreatment,
        );
        assert!(app.world().get::<MedicalPatient>(patient).is_some());
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .get(incident)
                .unwrap()
                .status,
            super::super::IncidentStatus::Active,
        );
        assert_eq!(
            app.world()
                .resource::<ReservationBook>()
                .active_claim_count(),
            1,
        );
    }

    #[test]
    fn vanished_treatment_request_reopens_without_resolving_the_case() {
        let (mut app, patient, _, _, incident) = treatment_outcome_app(1);

        app.update();

        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(77))
                .unwrap()
                .status,
            MedicalCaseStatus::NeedsTreatment,
        );
        assert!(app.world().get::<MedicalPatient>(patient).is_some());
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .get(incident)
                .unwrap()
                .status,
            super::super::IncidentStatus::Active,
        );
    }

    #[test]
    fn missing_patient_closes_the_case_and_releases_its_bed_claim() {
        let (mut app, patient, _, source, incident) = treatment_outcome_app(1);
        app.world_mut().despawn(patient);

        app.update();

        assert_eq!(
            app.world()
                .resource::<MedicalCaseLedger>()
                .get(MedicalCaseId(77))
                .unwrap()
                .status,
            MedicalCaseStatus::Resolved,
        );
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .get(incident)
                .unwrap()
                .status,
            super::super::IncidentStatus::Resolved,
        );
        assert_eq!(
            app.world()
                .resource::<ReservationBook>()
                .active_claim_count(),
            0,
        );
        assert!(app.world().get_entity(source).is_err());
    }
}
