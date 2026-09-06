//! The Bridge: navigation readiness, communications, and station reporting.
//!
//! The last of seven department adapters, and the only one whose department did
//! not previously exist. Until Odera and Sissel were promoted, `Bridge` was not
//! an `orders::Department` at all — the six residents were `crew::fluff`
//! support, invisible to standing, grading, and the favor system. The room was
//! the most heavily furnished on the station and the least connected to it.
//!
//! Bridge is also the only six-person department: two core characters and
//! **four** support, where every other department has two. The role split the
//! design fixes is deliberate and the work below follows it:
//!
//! - **Odera** owns physical readiness — the helm, and whether the station is
//!   actually in a fit state to be somewhere.
//! - **Sissel** owns communications, reports, and briefings — what the rest of
//!   the station is *told*.
//!
//! That split is why the two capabilities are distinct rather than one
//! "bridge.operate": a department whose two halves are interchangeable is two
//! portraits, not two characters.

use bevy::prelude::*;

use super::department::{control_for_resident, DepartmentRoster};
use super::jobs::UtilitySpots;
use super::{
    stable_text_key, ActionTarget, JobBoard, JobCapability, JobDomain, JobTicket, JobTicketId,
    JobTicketState, Normalized, ReservationKey, UtilityBucket,
};
use crate::crew::{CrewMember, CrewRoute, StationResident};

pub const BRIDGE_SUPPORT: [&str; 4] = crate::crew::fluff::BRIDGE_SUPPORT_NAMES;
const BRIDGE_CORE: [&str; 2] = [crate::social::ODERA, crate::social::SISSEL];

const HELM_CAPABILITY: &str = "bridge.helm";
const COMMS_CAPABILITY: &str = "bridge.comms";
const MONITOR_CAPABILITY: &str = "bridge.monitor";

/// Shared by every Bridge worker. Monitoring is the department's baseline duty,
/// so support officers can hold a console while the core pair do their own work.
const BRIDGE_CAPABILITIES: [&str; 4] = [
    HELM_CAPABILITY,
    COMMS_CAPABILITY,
    MONITOR_CAPABILITY,
    super::aid::assessment_capability(JobDomain::Bridge),
];

pub(super) const BRIDGE_ROSTER: DepartmentRoster = DepartmentRoster {
    domain: JobDomain::Bridge,
    core: &BRIDGE_CORE,
    support: &BRIDGE_SUPPORT,
    expected_support: 4,
    capabilities: &BRIDGE_CAPABILITIES,
};

const HELM_SPOT: &str = "bridge.helm";
const COMMS_SPOT: &str = "bridge.comms";
const MONITOR_SPOT: &str = "bridge.station.monitor";
const BRIEFING_SPOT: &str = "bridge.briefing";

const BRIDGE_SPOTS: [&str; 4] = [HELM_SPOT, COMMS_SPOT, MONITOR_SPOT, BRIEFING_SPOT];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BridgeJobKind {
    /// Odera's post: holding the helm and station attitude.
    StandHelm,
    /// Sissel's post: traffic, logs, outgoing reports.
    WorkComms,
    /// Anyone's: watching the station boards.
    MonitorStation,
    /// A shared briefing, which is why its spot holds two.
    Brief,
}

impl BridgeJobKind {
    fn id(self) -> &'static str {
        match self {
            Self::StandHelm => "bridge.helm",
            Self::WorkComms => "bridge.comms",
            Self::MonitorStation => "bridge.monitor",
            Self::Brief => "bridge.brief",
        }
    }

    fn spot(self) -> &'static str {
        match self {
            Self::StandHelm => HELM_SPOT,
            Self::WorkComms => COMMS_SPOT,
            Self::MonitorStation => MONITOR_SPOT,
            Self::Brief => BRIEFING_SPOT,
        }
    }

    fn capability(self) -> &'static str {
        match self {
            Self::StandHelm => HELM_CAPABILITY,
            Self::WorkComms => COMMS_CAPABILITY,
            Self::MonitorStation | Self::Brief => MONITOR_CAPABILITY,
        }
    }

    /// All routine. The Bridge notices problems and reports them; it does not
    /// respond to them, so nothing here should ever compete with an emergency
    /// or with a Security interview.
    fn urgency(self) -> f32 {
        match self {
            Self::StandHelm => 0.5,
            Self::WorkComms => 0.45,
            Self::MonitorStation => 0.35,
            Self::Brief => 0.3,
        }
    }

    fn seconds(self) -> f32 {
        match self {
            Self::StandHelm => 14.0,
            Self::WorkComms => 12.0,
            Self::MonitorStation => 10.0,
            Self::Brief => 8.0,
        }
    }
}

const BRIDGE_JOBS: [BridgeJobKind; 4] = [
    BridgeJobKind::StandHelm,
    BridgeJobKind::WorkComms,
    BridgeJobKind::MonitorStation,
    BridgeJobKind::Brief,
];

fn bridge_ticket_id(kind: BridgeJobKind) -> JobTicketId {
    JobTicketId(stable_text_key(&format!("bridge.ticket.{}", kind.id())))
}

/// Publishes the Bridge's standing watch.
///
/// Like Security's, these are always available: a console does not stop wanting
/// to be staffed. Six residents against four posts is deliberate — the Bridge
/// should look staffed without every officer being pinned to a station.
fn publish_bridge_tickets(time: Res<Time>, spots: Res<UtilitySpots>, mut board: ResMut<JobBoard>) {
    for kind in BRIDGE_JOBS {
        let id = bridge_ticket_id(kind);
        if board.ticket(id).is_some() {
            continue;
        }
        let Some(spot) = spots.get(kind.spot()) else {
            continue;
        };
        let _ = board.publish(JobTicket {
            id,
            domain: JobDomain::Bridge,
            kind: kind.id().into(),
            target: ActionTarget::Point(spot.at),
            // Watch duty at a known console: common knowledge, never gated on
            // whether the officer witnessed anything.
            subject: None,
            reservation: ReservationKey(format!("utility.spot.{}", kind.spot())),
            reservation_capacity: spot.capacity,
            bucket: UtilityBucket::Routine,
            urgency: Normalized::new(kind.urgency()).expect("Bridge urgency is normalized"),
            required_capability: JobCapability::new(kind.capability()),
            created_at: time.elapsed_secs(),
            deadline: None,
            risk: Normalized::ZERO,
            perform_seconds: kind.seconds(),
            state: JobTicketState::Available,
        });
    }
}

/// Moves Bridge residents off the legacy ambient controller.
#[allow(clippy::type_complexity)]
fn activate_bridge_workers(
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
        if social.as_deref().is_some_and(|social| {
            social
                .active_favor
                .as_ref()
                .is_some_and(|favor| favor.owner == member.name)
        }) {
            continue;
        }
        let Some((control, profile)) =
            control_for_resident(&member.name, route, owner, BRIDGE_ROSTER)
        else {
            continue;
        };
        commands
            .entity(entity)
            .insert((control, profile, StationResident));
    }
}

fn reset_bridge_work(
    mut board: ResMut<JobBoard>,
    mut reservations: ResMut<super::ReservationBook>,
) {
    BRIDGE_ROSTER
        .validate()
        .expect("the Bridge utility roster must be valid");
    board.remove_domain(JobDomain::Bridge);
    for spot in BRIDGE_SPOTS {
        reservations.release_key(&ReservationKey(format!("utility.spot.{spot}")));
    }
}

pub(super) fn register(app: &mut App) {
    app.add_systems(
        OnEnter(crate::AppState::Playing),
        reset_bridge_work
            .in_set(super::UtilityResetSet)
            .run_if(crate::net::is_authority),
    )
    .add_systems(
        PreUpdate,
        activate_bridge_workers
            .run_if(crate::net::is_authority)
            .run_if(in_state(crate::AppState::Playing))
            .run_if(crate::session::career_session),
    )
    .add_systems(
        Update,
        publish_bridge_tickets
            .in_set(super::UtilityAiSet::BuildContext)
            .run_if(crate::net::is_authority)
            .run_if(in_state(crate::AppState::Playing))
            .run_if(crate::session::career_session),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::Department;
    use crate::utility_ai::{jobs::NarrativeTier, ControlOwner};

    #[test]
    fn the_bridge_roster_is_valid_and_matches_the_authored_cast() {
        BRIDGE_ROSTER.validate().unwrap();

        let roster = std::fs::read_to_string("assets/data/station.crew.ron")
            .expect("the authored crew roster is readable");
        for name in BRIDGE_CORE {
            assert!(
                roster.contains(name),
                "{name} was promoted to core and must be on the authored roster"
            );
        }
        for name in BRIDGE_SUPPORT {
            assert!(
                !roster.contains(name),
                "{name} is support and must stay off the customer roster"
            );
        }
    }

    /// Bridge is the only department with four support workers. The shared
    /// roster type takes `expected_support` precisely so this cannot drift.
    #[test]
    fn the_bridge_is_the_one_six_person_department() {
        assert_eq!(BRIDGE_CORE.len(), 2);
        assert_eq!(BRIDGE_SUPPORT.len(), 4);
        assert_eq!(
            BRIDGE_ROSTER.expected_support, 4,
            "declaring the wrong support count is exactly what validate() exists to catch"
        );
        // Declaring two, as every other department does, must be rejected.
        assert!(DepartmentRoster {
            expected_support: 2,
            ..BRIDGE_ROSTER
        }
        .validate()
        .is_err());
    }

    /// The department is now real: `Department::Bridge` exists, resolves from
    /// its role string, and knows its own members. Before the promotion,
    /// `from_role("Bridge")` returned `None` and the room had no standing.
    #[test]
    fn bridge_is_a_real_department_with_the_promoted_pair_as_members() {
        assert_eq!(
            crate::orders::Department::from_role("Bridge"),
            Some(Department::Bridge)
        );
        assert_eq!(Department::Bridge.members(), &BRIDGE_CORE);
        assert!(Department::ALL.contains(&Department::Bridge));
        for name in BRIDGE_CORE {
            assert_eq!(
                crate::social::resident_department(name),
                Some(Department::Bridge),
                "{name} must appear in the social directory under Bridge"
            );
        }
        // Support workers stay out of the core social roster, which is what
        // keeps them off standing arithmetic and the favor system.
        for name in BRIDGE_SUPPORT {
            assert_eq!(crate::social::resident_department(name), None);
        }
    }

    /// Odera owns readiness, Sissel owns communications. The capabilities are
    /// separate so the pair are complementary rather than interchangeable.
    #[test]
    fn the_core_pair_have_distinct_duties() {
        assert_ne!(
            BridgeJobKind::StandHelm.capability(),
            BridgeJobKind::WorkComms.capability(),
            "a shared 'operate' capability would make the two cores interchangeable"
        );
        // Monitoring is the shared baseline, so support can hold a console.
        assert_eq!(
            BridgeJobKind::MonitorStation.capability(),
            BridgeJobKind::Brief.capability()
        );
    }

    #[test]
    fn core_and_support_differ_only_in_narrative_tier() {
        let core = BRIDGE_ROSTER.profile_for(BRIDGE_CORE[0]).unwrap();
        let support = BRIDGE_ROSTER.profile_for(BRIDGE_SUPPORT[0]).unwrap();
        assert_eq!(core.narrative_tier, NarrativeTier::Core);
        assert_eq!(support.narrative_tier, NarrativeTier::Support);
        assert_eq!(core.capabilities, support.capabilities);
        assert!(BRIDGE_ROSTER.profile_for("Dr. Vance").is_none());
    }

    /// The Bridge reports problems; it does not respond to them. Nothing here
    /// may compete with an emergency or a Security interview.
    #[test]
    fn bridge_watch_duty_never_competes_with_a_real_response() {
        for kind in BRIDGE_JOBS {
            assert!(
                kind.urgency() < 0.55,
                "{} must stay below interview urgency",
                kind.id()
            );
        }
    }

    #[test]
    fn bridge_publishes_its_watch_once_per_console() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .add_systems(Update, publish_bridge_tickets);
        for kind in BRIDGE_JOBS {
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
                .filter(|ticket| ticket.domain == JobDomain::Bridge)
                .count(),
            BRIDGE_JOBS.len(),
            "a second frame must not republish work already on the board"
        );
        assert!(
            board.iter().all(|ticket| ticket.subject.is_none()),
            "watch duty is common knowledge and must never be memory-gated"
        );
    }

    #[test]
    fn migration_respects_a_competing_controller() {
        let route = CrewRoute::standing();
        assert!(control_for_resident(BRIDGE_CORE[0], &route, None, BRIDGE_ROSTER).is_some());
        assert!(control_for_resident(
            BRIDGE_CORE[0],
            &route,
            Some(&ControlOwner::MedicalTransport),
            BRIDGE_ROSTER,
        )
        .is_none());
    }
}
