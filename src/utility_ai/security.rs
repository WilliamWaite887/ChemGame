//! Security as a utility department: patrol, dispatch, evidence, and the
//! investigator role that `interviews.rs` has been publishing work for.
//!
//! **This is not `src/security_case`.** That module is the player-facing case
//! system — Security holding *the player's* batch, with a conversation UI,
//! replicated messages, and appeal actions. This adapter is the NPC-side
//! department: who staffs Security, what routine work they do, and who is
//! qualified to canvass witnesses. The two share a subject and nothing else,
//! and folding them together would put a replicated player conversation on the
//! same path as authority-only witness knowledge.
//!
//! Security's routine work is deliberately thin. A department whose job is
//! *noticing* should spend most of its time available rather than occupied:
//! patrol and dispatch exist so officers are somewhere plausible and moving,
//! and so that an interview ticket has someone free to claim it. Making the
//! routine dense would starve the thing the department exists for.

use bevy::prelude::*;

use super::department::{control_for_resident, DepartmentRoster};
use super::jobs::UtilitySpots;
use super::{
    stable_text_key, ActionTarget, JobBoard, JobCapability, JobDomain, JobTicket, JobTicketId,
    JobTicketState, Normalized, ReservationKey, UtilityBucket,
};
use crate::crew::{CrewMember, CrewRoute, StationResident};

pub const SECURITY_SUPPORT: [&str; 2] = crate::crew::fluff::SECURITY_SUPPORT_NAMES;
const SECURITY_CORE: [&str; 2] = [crate::social::REYES, "Warden Bex"];

const PATROL_CAPABILITY: &str = "security.patrol";
const DISPATCH_CAPABILITY: &str = "security.dispatch";
const EVIDENCE_CAPABILITY: &str = "security.evidence";

/// Every Security worker can interview. Investigation is the department's
/// defining act, not a specialisation within it — a station where only one
/// officer could ask questions would stall whenever that officer was busy.
const SECURITY_CAPABILITIES: [&str; 5] = [
    PATROL_CAPABILITY,
    DISPATCH_CAPABILITY,
    EVIDENCE_CAPABILITY,
    super::interviews::INTERVIEW_CAPABILITY,
    super::aid::assessment_capability(JobDomain::Security),
];

pub(super) const SECURITY_ROSTER: DepartmentRoster = DepartmentRoster {
    domain: JobDomain::Security,
    core: &SECURITY_CORE,
    support: &SECURITY_SUPPORT,
    expected_support: 2,
    capabilities: &SECURITY_CAPABILITIES,
};

const DISPATCH_SPOT: &str = "security.dispatch";
const DESK_SPOT: &str = "security.desk";
const EVIDENCE_SPOT: &str = "security.evidence";

/// The authored spots this department reserves, released on shift reset.
const SECURITY_SPOTS: [&str; 4] = [
    DISPATCH_SPOT,
    DESK_SPOT,
    EVIDENCE_SPOT,
    "security.interview.room",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SecurityJobKind {
    /// Staffing the dispatch console at the public checkpoint.
    ManDispatch,
    /// Working the officer desk bank: paperwork, log review.
    WorkDesk,
    /// Auditing the evidence locker in the secure back room.
    AuditEvidence,
}

impl SecurityJobKind {
    fn id(self) -> &'static str {
        match self {
            Self::ManDispatch => "security.dispatch",
            Self::WorkDesk => "security.desk",
            Self::AuditEvidence => "security.evidence",
        }
    }

    fn spot(self) -> &'static str {
        match self {
            Self::ManDispatch => DISPATCH_SPOT,
            Self::WorkDesk => DESK_SPOT,
            Self::AuditEvidence => EVIDENCE_SPOT,
        }
    }

    fn capability(self) -> &'static str {
        match self {
            Self::ManDispatch => DISPATCH_CAPABILITY,
            Self::WorkDesk => PATROL_CAPABILITY,
            Self::AuditEvidence => EVIDENCE_CAPABILITY,
        }
    }

    /// All routine, all modest. Nothing here should ever outrank answering a
    /// question about an incident.
    fn urgency(self) -> f32 {
        match self {
            Self::ManDispatch => 0.45,
            Self::WorkDesk => 0.3,
            Self::AuditEvidence => 0.35,
        }
    }

    fn seconds(self) -> f32 {
        match self {
            Self::ManDispatch => 12.0,
            Self::WorkDesk => 8.0,
            Self::AuditEvidence => 10.0,
        }
    }
}

const SECURITY_JOBS: [SecurityJobKind; 3] = [
    SecurityJobKind::ManDispatch,
    SecurityJobKind::WorkDesk,
    SecurityJobKind::AuditEvidence,
];

fn security_ticket_id(kind: SecurityJobKind) -> JobTicketId {
    JobTicketId(stable_text_key(&format!("security.ticket.{}", kind.id())))
}

/// Publishes Security's standing routine work.
///
/// Unlike Engineering's asset-driven tickets, these are always available: the
/// checkpoint always wants staffing. They are republished whenever the board
/// is empty of them, which is what keeps an officer who finished an interview
/// from standing idle.
fn publish_security_tickets(
    time: Res<Time>,
    spots: Res<UtilitySpots>,
    mut board: ResMut<JobBoard>,
) {
    for kind in SECURITY_JOBS {
        let id = security_ticket_id(kind);
        if board.ticket(id).is_some() {
            continue;
        }
        let Some(spot) = spots.get(kind.spot()) else {
            continue;
        };
        let _ = board.publish(JobTicket {
            id,
            domain: JobDomain::Security,
            kind: kind.id().into(),
            target: ActionTarget::Point(spot.at),
            // Routine station work at a known place: common knowledge, never
            // gated on whether the officer witnessed anything.
            subject: None,
            reservation: ReservationKey(format!("utility.spot.{}", kind.spot())),
            reservation_capacity: spot.capacity,
            bucket: UtilityBucket::Routine,
            urgency: Normalized::new(kind.urgency()).expect("Security urgency is normalized"),
            required_capability: JobCapability::new(kind.capability()),
            created_at: time.elapsed_secs(),
            deadline: None,
            risk: Normalized::ZERO,
            perform_seconds: kind.seconds(),
            state: JobTicketState::Available,
        });
    }
}

/// Moves Security residents off the legacy ambient controller.
#[allow(clippy::type_complexity)]
fn activate_security_workers(
    mut commands: Commands,
    social: Option<Res<crate::social::SocialState>>,
    residents: Query<
        (
            Entity,
            &CrewMember,
            &CrewRoute,
            Option<&super::ControlOwner>,
        ),
        (With<crate::crew::Ambient>, Without<super::UtilityAgent>),
    >,
) {
    for (entity, member, route, owner) in &residents {
        // A resident mid-favor belongs to the social system until it releases
        // them; taking control here would strand that favor.
        if social.as_deref().is_some_and(|social| {
            social
                .active_favor
                .as_ref()
                .is_some_and(|favor| favor.owner == member.name)
        }) {
            continue;
        }
        let Some((control, profile)) =
            control_for_resident(&member.name, route, owner, SECURITY_ROSTER)
        else {
            continue;
        };
        commands
            .entity(entity)
            .insert((control, profile, StationResident));
    }
}

fn reset_security_work(
    mut board: ResMut<JobBoard>,
    mut reservations: ResMut<super::ReservationBook>,
) {
    SECURITY_ROSTER
        .validate()
        .expect("the Security utility roster must be valid");
    board.remove_domain(JobDomain::Security);
    for spot in SECURITY_SPOTS {
        reservations.release_key(&ReservationKey(format!("utility.spot.{spot}")));
    }
}

pub(super) fn register(app: &mut App) {
    app.add_systems(
        OnEnter(crate::AppState::Playing),
        reset_security_work
            .in_set(super::UtilityResetSet)
            .run_if(crate::net::is_authority),
    )
    .add_systems(
        PreUpdate,
        activate_security_workers
            .run_if(crate::net::is_authority)
            .run_if(in_state(crate::AppState::Playing))
            .run_if(crate::session::career_session),
    )
    .add_systems(
        Update,
        publish_security_tickets
            .in_set(super::UtilityAiSet::BuildContext)
            .run_if(crate::net::is_authority)
            .run_if(in_state(crate::AppState::Playing))
            .run_if(crate::session::career_session),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utility_ai::{jobs::NarrativeTier, ControlOwner};

    #[test]
    fn the_security_roster_is_valid_and_matches_the_authored_cast() {
        SECURITY_ROSTER.validate().unwrap();

        // The core pair must be the two Security names on the *authored*
        // customer roster, not a pair of string constants that drifted from
        // it. Engineering shipped a migration bug exactly this way.
        let roster = std::fs::read_to_string("assets/data/station.crew.ron")
            .expect("the authored crew roster is readable");
        for name in SECURITY_CORE {
            assert!(
                roster.contains(name),
                "{name} must exist on the authored crew roster"
            );
        }
        // Support workers must NOT be on it — that roster is the customer
        // list, and anyone added there starts phoning the lab for chemistry.
        for name in SECURITY_SUPPORT {
            assert!(
                !roster.contains(name),
                "{name} is support and must stay off the customer roster"
            );
        }
    }

    #[test]
    fn every_security_worker_can_interview() {
        // The investigator role is what `interviews.rs` publishes tickets for.
        // If no Security profile carried this capability those tickets would
        // sit unclaimable forever, and every canvass would silently stall.
        for name in SECURITY_CORE.iter().chain(SECURITY_SUPPORT.iter()) {
            let profile = SECURITY_ROSTER
                .profile_for(name)
                .expect("a roster member has a profile");
            assert!(
                profile.can_do(&JobCapability::new(
                    crate::utility_ai::interviews::INTERVIEW_CAPABILITY
                )),
                "{name} must be able to canvass witnesses"
            );
            assert!(profile.works_in(JobDomain::Security));
        }
    }

    #[test]
    fn core_and_support_differ_only_in_narrative_tier() {
        let core = SECURITY_ROSTER.profile_for(SECURITY_CORE[0]).unwrap();
        let support = SECURITY_ROSTER.profile_for(SECURITY_SUPPORT[0]).unwrap();
        assert_eq!(core.narrative_tier, NarrativeTier::Core);
        assert_eq!(support.narrative_tier, NarrativeTier::Support);
        // Content eligibility differs; simulation does not. A support officer
        // interviews exactly as well as a core one.
        assert_eq!(core.capabilities, support.capabilities);
        assert!(SECURITY_ROSTER.profile_for("Dr. Vance").is_none());
    }

    #[test]
    fn routine_security_work_never_outranks_an_interview() {
        // Interviews are published at `Important`; routine work must stay
        // below it, or an officer would tidy the evidence locker while a
        // witness waited to be asked what they saw.
        for kind in SECURITY_JOBS {
            assert!(
                kind.urgency() < 0.55,
                "{} must not compete with an interview",
                kind.id()
            );
        }
    }

    #[test]
    fn security_publishes_its_routine_work_once_per_spot() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .add_systems(Update, publish_security_tickets);
        for kind in SECURITY_JOBS {
            app.world_mut()
                .resource_mut::<UtilitySpots>()
                .insert(kind.spot(), Vec3::ZERO, 1);
        }

        app.update();
        app.update();

        let board = app.world().resource::<JobBoard>();
        assert_eq!(
            board
                .iter()
                .filter(|t| t.domain == JobDomain::Security)
                .count(),
            SECURITY_JOBS.len(),
            "a second frame must not republish work already on the board"
        );
        assert!(
            board.iter().all(|ticket| ticket.subject.is_none()),
            "routine station work is common knowledge and must never be memory-gated"
        );
    }

    /// The join between this adapter and `interviews.rs`, end to end.
    ///
    /// Before Security existed, `interviews.rs` published `JobDomain::Security`
    /// tickets that *nobody on the station was qualified to claim* — every
    /// canvass would have stalled silently on frame one. This is the test that
    /// proves the department closes that loop, so it runs both modules'
    /// real systems rather than asserting on capability strings.
    #[test]
    fn a_security_officer_can_actually_claim_an_interview_ticket() {
        use crate::utility_ai::{IncidentKind, IncidentLedger, UtilityAgent, UtilityControlBundle};

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<JobBoard>()
            .init_resource::<IncidentLedger>()
            .init_resource::<crate::utility_ai::interviews::InvestigationLedger>()
            .add_systems(
                Update,
                (
                    crate::utility_ai::interviews::open_investigations,
                    crate::utility_ai::interviews::publish_interview_tickets,
                )
                    .chain(),
            );

        let culprit = app.world_mut().spawn(Transform::default()).id();
        let officer = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: SECURITY_CORE[0].into(),
                    role: "Security".into(),
                },
                Transform::default(),
                UtilityControlBundle::new(UtilityAgent::new(1, 0)),
                SECURITY_ROSTER
                    .profile_for(SECURITY_CORE[0])
                    .expect("an officer has a profile"),
            ))
            .id();

        app.world_mut()
            .resource_mut::<IncidentLedger>()
            .create(
                IncidentKind::Tampering,
                JobDomain::Botany,
                culprit,
                None,
                Vec3::ZERO,
                Normalized::new(0.5).unwrap(),
                0.0,
            )
            .expect("the ledger has room");
        app.update();

        let profile = app
            .world()
            .get::<crate::utility_ai::NpcJobProfile>(officer)
            .expect("the officer carries a job profile");
        let board = app.world().resource::<JobBoard>();
        let claimable: Vec<_> = board.available_for(profile).collect();
        assert_eq!(
            claimable.len(),
            1,
            "the officer must be able to claim the interview the investigation published"
        );
        assert_eq!(claimable[0].domain, JobDomain::Security);
        assert_eq!(
            claimable[0].bucket,
            UtilityBucket::Important,
            "an interview outranks routine Security work"
        );
    }

    #[test]
    fn migration_respects_a_competing_controller() {
        let route = CrewRoute::standing();
        assert!(
            control_for_resident(SECURITY_CORE[0], &route, None, SECURITY_ROSTER).is_some(),
            "an ambient officer migrates"
        );
        assert!(
            control_for_resident(
                SECURITY_CORE[0],
                &route,
                Some(&ControlOwner::MedicalTransport),
                SECURITY_ROSTER,
            )
            .is_none(),
            "an officer being carried to Medical must not be seized by Security"
        );
    }
}
