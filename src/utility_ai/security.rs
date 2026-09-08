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
use super::jobs::{take_completed_standing_posts, StandingPostCooldowns, UtilitySpots};
use super::{
    stable_text_key, ActionResult, ActionTarget, JobBoard, JobCapability, JobDomain, JobTicket,
    JobTicketId, JobTicketState, Normalized, ReservationKey, UtilityActionId,
    UtilityActionResolved, UtilityBucket,
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
    cooldowns: Res<StandingPostCooldowns>,
    mut board: ResMut<JobBoard>,
) {
    let now = time.elapsed_secs();
    for kind in SECURITY_JOBS {
        let id = security_ticket_id(kind);
        if board.ticket(id).is_some() {
            continue;
        }
        // After the `is_some` check deliberately, so
        // `security_publishes_its_routine_work_once_per_spot` keeps meaning
        // what it did: this gates *re*-advertising a worked post, not the
        // first publication.
        if !cooldowns.ready(id, now) {
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

/// Releases a worked post so it can be staffed again.
///
/// See [`take_completed_standing_posts`] for why this had to exist: without it
/// a completed Security ticket stayed terminal on the board forever and the
/// department published its three posts exactly once per shift.
fn apply_security_post_results(
    time: Res<Time>,
    mut results: MessageReader<UtilityActionResolved>,
    mut board: ResMut<JobBoard>,
    mut cooldowns: ResMut<StandingPostCooldowns>,
) {
    let completed: Vec<_> = results
        .read()
        .filter(|result| {
            result.key.action == UtilityActionId::PerformJob
                && result.result == ActionResult::Completed
        })
        .map(|result| (JobTicketId(result.key.target_key), result.claim))
        .collect();
    take_completed_standing_posts(
        JobDomain::Security,
        &completed,
        &mut board,
        &mut cooldowns,
        time.elapsed_secs(),
    );
}

fn reset_security_work(
    mut board: ResMut<JobBoard>,
    mut cooldowns: ResMut<StandingPostCooldowns>,
    mut reservations: ResMut<super::ReservationBook>,
) {
    cooldowns.clear();
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
    )
    .add_systems(
        Update,
        apply_security_post_results
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
            .init_resource::<StandingPostCooldowns>()
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

    /// A worked post must go back on the board.
    ///
    /// Security had no `apply_*_job_results` system at all, so a completed
    /// ticket stayed `Completed(owner)` forever, the publisher's
    /// `board.ticket(id).is_some()` guard stayed true, and the department
    /// advertised its three posts exactly once per shift. Live, that was
    /// `Security 3/0` in all twenty-five snapshots of a 500-second run:
    /// available work that no officer ever claimed twice.
    ///
    /// Falsifies the results system: drop `apply_security_post_results` from
    /// the schedule and the ticket never returns.
    #[test]
    fn security_republishes_a_post_after_it_is_worked() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<StandingPostCooldowns>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (apply_security_post_results, publish_security_tickets).chain(),
            );
        for kind in SECURITY_JOBS {
            app.world_mut()
                .resource_mut::<UtilitySpots>()
                .insert(kind.spot(), Vec3::ZERO, 1);
        }
        app.update();

        // Work one post through to completion, exactly as the lifecycle does.
        let worker = app.world_mut().spawn_empty().id();
        let id = security_ticket_id(SecurityJobKind::ManDispatch);
        let claim = super::super::ReservationOwner {
            agent: worker,
            action_instance: 1,
        };
        {
            let mut board = app.world_mut().resource_mut::<JobBoard>();
            board.claim(id, claim).expect("the post is available");
            board
                .resolve(id, claim, ActionResult::Completed)
                .expect("the claim is ours");
        }
        app.world_mut().write_message(UtilityActionResolved {
            agent: worker,
            key: super::super::ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: id.0,
            },
            claim,
            result: ActionResult::Completed,
        });

        // Step in small frames past the cooldown rather than jumping it: a
        // single large advance can sail over the window under test.
        let mut back = false;
        for _ in 0..200 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            if app
                .world()
                .resource::<JobBoard>()
                .ticket(id)
                .is_some_and(|ticket| ticket.state == JobTicketState::Available)
            {
                back = true;
                break;
            }
        }
        assert!(
            back,
            "a completed Security post was never taken off the board, so it \
             could never be advertised again"
        );
    }

    /// The post must not be re-advertised the instant it is vacated.
    ///
    /// Without a cooldown the console reappears on the board the same frame the
    /// officer steps away from it, and the officer standing on it re-claims it
    /// immediately — legal, but it reads as a stuck loop and means an officer
    /// never walks anywhere. A department whose job is noticing has to move
    /// through rooms to notice anything.
    ///
    /// Falsifies the cooldown: remove the `cooldowns.ready` gate and the ticket
    /// is back on the very next frame.
    #[test]
    fn a_worked_security_post_is_not_immediately_re_advertised() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<StandingPostCooldowns>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (apply_security_post_results, publish_security_tickets).chain(),
            );
        for kind in SECURITY_JOBS {
            app.world_mut()
                .resource_mut::<UtilitySpots>()
                .insert(kind.spot(), Vec3::ZERO, 1);
        }
        app.update();

        let worker = app.world_mut().spawn_empty().id();
        let id = security_ticket_id(SecurityJobKind::ManDispatch);
        let claim = super::super::ReservationOwner {
            agent: worker,
            action_instance: 1,
        };
        {
            let mut board = app.world_mut().resource_mut::<JobBoard>();
            board.claim(id, claim).unwrap();
            board.resolve(id, claim, ActionResult::Completed).unwrap();
        }
        app.world_mut().write_message(UtilityActionResolved {
            agent: worker,
            key: super::super::ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: id.0,
            },
            claim,
            result: ActionResult::Completed,
        });

        // Just past the take, well inside the cooldown window.
        for _ in 0..10 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            assert!(
                app.world().resource::<JobBoard>().ticket(id).is_none(),
                "the vacated post was advertised again inside its cooldown, so \
                 the officer standing on it can simply retake it"
            );
        }
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
