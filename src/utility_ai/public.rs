//! Safe public projections of authority-only utility state.
//!
//! Everything in this module answers one question: what may a client be told?
//! The utility kernel's scores, needs, reservations, private intent, and the
//! covert layer's custody are all authority-only. The Crew menu renders from
//! *this* module alone, so a guest can draw the whole screen from replicated
//! components without ever holding [`super::jobs::JobBoard`].
//!
//! The projection is deliberately lossy in one direction only. It narrows real
//! counts into a qualitative label; it never invents a fact the player could
//! not otherwise go and look at. Every [`DepartmentCondition`] worse than
//! `OnSchedule` carries the [`ConditionReason`] that produced it, and each
//! reason names a thing standing in a room: queued work, an unresolved fault,
//! a body in a bed. That is the rule the plan states as "a status cannot say
//! `Backlogged` when there is no world fact the player can inspect", and
//! `reason_is_backed_by_a_countable_world_fact` is what holds it.

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use super::incidents::IncidentLedger;
use super::jobs::{JobBoard, JobDomain, JobTicketState, NpcJobProfile};
use super::NpcActivity;

/// How much work one available worker is expected to absorb before the
/// department reads as under pressure. Tuned against the authored rosters:
/// departments run two core plus a small support crew, so a backlog of two
/// tickets per free worker is genuinely visible as a queue in the room.
///
/// There is no `BUSY` rung: reaching the ratio at all means at least one
/// ticket is waiting for at least one free worker, which is already `Busy`.
const TICKETS_PER_WORKER_STRAINED: f32 = 2.0;
const TICKETS_PER_WORKER_BACKLOGGED: f32 = 3.0;

/// A department with nobody able to work is not merely slow. This is the one
/// rung that does not come from a ratio, because the ratio is undefined.
const NO_WORKERS_IS_BACKLOGGED: DepartmentCondition = DepartmentCondition::Backlogged;

/// Qualitative department pressure. This is the *only* pressure vocabulary the
/// player ever sees; the underlying counts stay on the authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DepartmentCondition {
    #[default]
    OnSchedule,
    Busy,
    Strained,
    Backlogged,
    Emergency,
}

impl DepartmentCondition {
    pub fn label(self) -> &'static str {
        match self {
            DepartmentCondition::OnSchedule => "On schedule",
            DepartmentCondition::Busy => "Busy",
            DepartmentCondition::Strained => "Strained",
            DepartmentCondition::Backlogged => "Backlogged",
            DepartmentCondition::Emergency => "Emergency",
        }
    }
}

/// Why a department reads the way it does, in terms of something the player can
/// walk over and look at.
///
/// This is an enum rather than a `String` so the projection cannot smuggle a
/// private number out inside prose, and so the UI can localize later. `count`
/// is the inspectable quantity — the number of things physically there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionReason {
    /// An active incident in this department. Outranks every workload reason.
    ActiveIncident { count: usize },
    /// Work is published and nobody has picked it up.
    QueuedWork { count: usize },
    /// Every worker is busy and more work is waiting.
    AllWorkersEngaged { count: usize },
    /// The department has published work but nobody available to do it.
    NoAvailableWorkers,
    /// Work exists and is being handled at a normal pace.
    WorkInProgress { count: usize },
}

impl ConditionReason {
    /// The number of physical things a player could count in the room. Zero
    /// means "no countable backing", which is only legal for a department that
    /// is `OnSchedule`.
    pub fn evidence_count(self) -> usize {
        match self {
            ConditionReason::ActiveIncident { count }
            | ConditionReason::QueuedWork { count }
            | ConditionReason::AllWorkersEngaged { count }
            | ConditionReason::WorkInProgress { count } => count,
            // Backed by the *absence* of workers, which is equally visible: the
            // room is empty. Counted separately so the invariant below can say
            // so out loud instead of special-casing a zero.
            ConditionReason::NoAvailableWorkers => 0,
        }
    }

    pub fn text(self) -> String {
        match self {
            ConditionReason::ActiveIncident { count: 1 } => "an active incident".into(),
            ConditionReason::ActiveIncident { count } => format!("{count} active incidents"),
            ConditionReason::QueuedWork { count: 1 } => "one job waiting".into(),
            ConditionReason::QueuedWork { count } => format!("{count} jobs waiting"),
            ConditionReason::AllWorkersEngaged { count: 1 } => {
                "one job waiting, everyone busy".into()
            }
            ConditionReason::AllWorkersEngaged { count } => {
                format!("{count} jobs waiting, everyone busy")
            }
            ConditionReason::NoAvailableWorkers => "nobody available".into(),
            ConditionReason::WorkInProgress { count: 1 } => "one job underway".into(),
            ConditionReason::WorkInProgress { count } => format!("{count} jobs underway"),
        }
    }
}

/// Authority-side workload facts for one department, recomputed each pass.
///
/// These are raw counts, not scores. Nothing here is a utility value, so the
/// derivation into a public condition below stays auditable: a reader can check
/// the label against numbers they could have counted themselves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DepartmentWorkState {
    pub domain_tickets_available: usize,
    pub domain_tickets_claimed: usize,
    pub workers_total: usize,
    pub workers_available: usize,
    pub active_incidents: usize,
}

impl DepartmentWorkState {
    pub fn backlog(&self) -> usize {
        self.domain_tickets_available
    }

    /// Narrows the counts into what the player is allowed to see.
    ///
    /// Ordering matters and is deliberate: an active incident outranks any
    /// workload reading, because a department with a casualty in it is not
    /// merely "busy" no matter how short its queue is.
    pub fn condition(&self) -> (DepartmentCondition, ConditionReason) {
        if self.active_incidents > 0 {
            return (
                DepartmentCondition::Emergency,
                ConditionReason::ActiveIncident {
                    count: self.active_incidents,
                },
            );
        }

        let waiting = self.domain_tickets_available;
        if waiting == 0 {
            return if self.domain_tickets_claimed > 0 {
                (
                    DepartmentCondition::Busy,
                    ConditionReason::WorkInProgress {
                        count: self.domain_tickets_claimed,
                    },
                )
            } else {
                (
                    DepartmentCondition::OnSchedule,
                    ConditionReason::WorkInProgress { count: 0 },
                )
            };
        }

        if self.workers_available == 0 {
            return (
                NO_WORKERS_IS_BACKLOGGED,
                ConditionReason::NoAvailableWorkers,
            );
        }

        // `waiting` is non-zero and `workers_available` is non-zero here, so
        // the ratio is always at least one ticket per worker and there is no
        // rung below `Busy` to fall through to.
        let per_worker = waiting as f32 / self.workers_available as f32;
        let condition = if per_worker >= TICKETS_PER_WORKER_BACKLOGGED {
            DepartmentCondition::Backlogged
        } else if per_worker >= TICKETS_PER_WORKER_STRAINED {
            DepartmentCondition::Strained
        } else {
            DepartmentCondition::Busy
        };

        let reason = if self.domain_tickets_claimed >= self.workers_available {
            ConditionReason::AllWorkersEngaged { count: waiting }
        } else {
            ConditionReason::QueuedWork { count: waiting }
        };
        (condition, reason)
    }
}

/// The replicated per-department snapshot the Crew menu renders.
///
/// One entity per department carries this. It is a component rather than a
/// resource because `bevy_replicon` replicates components, and because it lets
/// a department page be spawned and despawned with its own row.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicDepartmentStatus {
    pub department: crate::orders::Department,
    pub condition: DepartmentCondition,
    pub reason: ConditionReason,
    /// Headcount the player could take by standing in the room and looking.
    pub workers_present: usize,
    pub workers_total: usize,
}

impl PublicDepartmentStatus {
    pub fn empty(department: crate::orders::Department) -> Self {
        Self {
            department,
            condition: DepartmentCondition::OnSchedule,
            reason: ConditionReason::WorkInProgress { count: 0 },
            workers_present: 0,
            workers_total: 0,
        }
    }
}

/// What one resident is publicly doing. This is a *presentation summary*, never
/// a controller: nothing in the UI may write it, and writing it would not move
/// anyone. The private action ID that produced it never crosses this boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicActivity {
    Working,
    TravelingForWork,
    Responding,
    OnBreak,
    Socializing,
    Eating,
    RecoveringInMedical,
    TreatingPatient,
    #[default]
    Unavailable,
}

impl PublicActivity {
    /// Derives the public summary from the already-replicated activity.
    ///
    /// Deliberately many-to-one: `Working` covers every job kind, so a covert
    /// actor tampering with a meal and an honest cook preparing one are the
    /// same word. That collapse is the no-spoiler guarantee, and
    /// `covert_and_honest_handling_are_indistinguishable` is what pins it.
    pub fn from_activity(activity: NpcActivity, in_medical: bool) -> Self {
        if in_medical {
            return PublicActivity::RecoveringInMedical;
        }
        match activity {
            NpcActivity::Working => PublicActivity::Working,
            NpcActivity::Traveling => PublicActivity::TravelingForWork,
            NpcActivity::Helping => PublicActivity::Responding,
            NpcActivity::Treating => PublicActivity::TreatingPatient,
            NpcActivity::Eating => PublicActivity::Eating,
            NpcActivity::Socializing => PublicActivity::Socializing,
            NpcActivity::Resting => PublicActivity::OnBreak,
            NpcActivity::Down => PublicActivity::RecoveringInMedical,
            NpcActivity::Idle => PublicActivity::Unavailable,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PublicActivity::Working => "Working",
            PublicActivity::TravelingForWork => "On the way to a job",
            PublicActivity::Responding => "Responding",
            PublicActivity::OnBreak => "On break",
            PublicActivity::Socializing => "Talking",
            PublicActivity::Eating => "Eating",
            PublicActivity::RecoveringInMedical => "In Medical",
            PublicActivity::TreatingPatient => "Treating a patient",
            PublicActivity::Unavailable => "Unavailable",
        }
    }
}

/// The replicated per-resident snapshot. Attached to the resident's own entity.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicCrewStatus {
    pub activity: PublicActivity,
    /// True only when the body is visibly down or in a bed. A private injury
    /// nobody has seen never sets this.
    pub visibly_hurt: bool,
}

/// Marks the entity carrying one department's public status row.
#[derive(Component, Clone, Copy, Debug)]
pub struct DepartmentStatusRow(pub crate::orders::Department);

fn spawn_department_rows(mut commands: Commands, existing: Query<&DepartmentStatusRow>) {
    if existing.iter().next().is_some() {
        return;
    }
    for department in crate::orders::Department::ALL {
        commands.spawn((
            DepartmentStatusRow(department),
            PublicDepartmentStatus::empty(department),
            super::aid::PublicAidStatus {
                department,
                state: None,
                claimed_label: None,
            },
            Replicated,
        ));
    }
}

/// Recomputes every department's public status from real board and ledger
/// facts. Authority-only: this is the one system allowed to read the job board
/// and write the replicated projection.
fn project_department_status(
    board: Res<JobBoard>,
    ledger: Res<IncidentLedger>,
    workers: Query<(&NpcJobProfile, &NpcActivity)>,
    mut rows: Query<(&DepartmentStatusRow, &mut PublicDepartmentStatus)>,
) {
    for (row, mut status) in &mut rows {
        let domain = JobDomain::from_department(row.0);
        let mut state = DepartmentWorkState::default();

        for ticket in board.iter().filter(|ticket| ticket.domain == domain) {
            match ticket.state {
                JobTicketState::Available => state.domain_tickets_available += 1,
                JobTicketState::Claimed(_) => state.domain_tickets_claimed += 1,
                JobTicketState::Completed(_) => {}
            }
        }

        for (profile, activity) in &workers {
            if profile.primary != domain {
                continue;
            }
            state.workers_total += 1;
            if !matches!(activity, NpcActivity::Down) {
                state.workers_available += 1;
            }
        }

        state.active_incidents = ledger.active_in_domain(domain).count();

        let (condition, reason) = state.condition();
        let next = PublicDepartmentStatus {
            department: row.0,
            condition,
            reason,
            workers_present: state.workers_available,
            workers_total: state.workers_total,
        };
        // Change detection drives replication, so only write on a real change.
        if *status != next {
            *status = next;
        }
    }
}

fn project_crew_status(
    mut commands: Commands,
    residents: Query<(
        Entity,
        &NpcActivity,
        Option<&super::NpcPosture>,
        Option<&PublicCrewStatus>,
    )>,
) {
    for (entity, activity, posture, current) in &residents {
        let lying = matches!(posture, Some(super::NpcPosture::Lying));
        let down = matches!(activity, NpcActivity::Down);
        let next = PublicCrewStatus {
            activity: PublicActivity::from_activity(*activity, lying && !down),
            visibly_hurt: down || lying,
        };
        if current != Some(&next) {
            commands.entity(entity).insert(next);
        }
    }
}

pub(super) fn register(app: &mut App) {
    app.replicate::<PublicDepartmentStatus>()
        .replicate::<PublicCrewStatus>()
        .add_systems(
            Update,
            (
                spawn_department_rows,
                project_department_status,
                project_crew_status,
            )
                .chain()
                .in_set(super::UtilityAiSet::Publish)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing)),
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(available: usize, claimed: usize, workers: usize) -> DepartmentWorkState {
        DepartmentWorkState {
            domain_tickets_available: available,
            domain_tickets_claimed: claimed,
            workers_total: workers,
            workers_available: workers,
            active_incidents: 0,
        }
    }

    #[test]
    fn condition_rises_with_real_backlog_per_available_worker() {
        assert_eq!(
            state(0, 0, 2).condition().0,
            DepartmentCondition::OnSchedule
        );
        assert_eq!(state(0, 1, 2).condition().0, DepartmentCondition::Busy);
        assert_eq!(state(2, 0, 2).condition().0, DepartmentCondition::Busy);
        assert_eq!(state(4, 0, 2).condition().0, DepartmentCondition::Strained);
        assert_eq!(
            state(6, 0, 2).condition().0,
            DepartmentCondition::Backlogged
        );
    }

    #[test]
    fn the_same_backlog_reads_worse_when_fewer_people_can_work() {
        // The ratio, not the raw count, is what the player sees — four jobs is
        // a normal day for four workers and a crisis for one.
        let mut busy = state(4, 0, 4);
        assert_eq!(busy.condition().0, DepartmentCondition::Busy);
        busy.workers_available = 1;
        assert_eq!(busy.condition().0, DepartmentCondition::Backlogged);
    }

    #[test]
    fn an_active_incident_outranks_any_workload_reading() {
        let mut quiet = state(0, 0, 4);
        quiet.active_incidents = 1;
        let (condition, reason) = quiet.condition();
        assert_eq!(condition, DepartmentCondition::Emergency);
        assert_eq!(reason, ConditionReason::ActiveIncident { count: 1 });
    }

    #[test]
    fn a_department_with_work_and_nobody_to_do_it_is_backlogged() {
        let stranded = DepartmentWorkState {
            domain_tickets_available: 1,
            workers_total: 2,
            workers_available: 0,
            ..Default::default()
        };
        assert_eq!(
            stranded.condition(),
            (
                DepartmentCondition::Backlogged,
                ConditionReason::NoAvailableWorkers
            ),
        );
    }

    #[test]
    fn reason_is_backed_by_a_countable_world_fact() {
        // The invariant the plan states: no label worse than OnSchedule may be
        // shown without something the player could walk over and count.
        // `NoAvailableWorkers` is the sole exception and is backed by the
        // visible *absence* of anyone in the room.
        for available in 0..6 {
            for claimed in 0..3 {
                for workers in 0..4 {
                    let mut probe = state(available, claimed, workers);
                    for incidents in 0..2 {
                        probe.active_incidents = incidents;
                        let (condition, reason) = probe.condition();
                        if condition == DepartmentCondition::OnSchedule {
                            continue;
                        }
                        if reason == ConditionReason::NoAvailableWorkers {
                            assert_eq!(probe.workers_available, 0);
                            continue;
                        }
                        assert!(
                            reason.evidence_count() > 0,
                            "{condition:?} claimed with no countable evidence: {probe:?}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn on_schedule_is_the_only_label_allowed_with_nothing_to_show() {
        let (condition, reason) = state(0, 0, 3).condition();
        assert_eq!(condition, DepartmentCondition::OnSchedule);
        assert_eq!(reason.evidence_count(), 0);
    }

    fn projection_app() -> App {
        let mut app = App::new();
        app.init_resource::<JobBoard>()
            .init_resource::<IncidentLedger>()
            .add_systems(
                Update,
                (
                    spawn_department_rows,
                    project_department_status,
                    project_crew_status,
                )
                    .chain(),
            );
        app
    }

    fn cargo_ticket(id: u64, state: JobTicketState) -> super::super::jobs::JobTicket {
        use super::super::{jobs::JobCapability, ActionTarget, ReservationKey, UtilityBucket};
        super::super::jobs::JobTicket {
            id: super::super::jobs::JobTicketId(id),
            domain: JobDomain::Cargo,
            kind: "cargo.test".into(),
            target: ActionTarget::Point(Vec3::ZERO),
            subject: None,
            reservation: ReservationKey(format!("cargo.test.{id}")),
            reservation_capacity: 1,
            bucket: UtilityBucket::Routine,
            urgency: super::super::Normalized::ONE,
            required_capability: JobCapability::new("cargo.freight"),
            created_at: 0.0,
            deadline: None,
            risk: super::super::Normalized::ZERO,
            perform_seconds: 1.0,
            state,
        }
    }

    fn status_of(app: &mut App, department: crate::orders::Department) -> PublicDepartmentStatus {
        *app.world_mut()
            .query::<&PublicDepartmentStatus>()
            .iter(app.world())
            .find(|status| status.department == department)
            .expect("every department has a row")
    }

    #[test]
    fn every_department_gets_exactly_one_public_row_and_it_is_not_respawned() {
        let mut app = projection_app();
        app.update();
        app.update();
        let rows = app
            .world_mut()
            .query::<&DepartmentStatusRow>()
            .iter(app.world())
            .count();
        assert_eq!(rows, crate::orders::Department::ALL.len());
    }

    #[test]
    fn a_changing_real_backlog_changes_the_displayed_condition_and_reason() {
        // The plan's exit criterion: the player can tell *why* a department is
        // falling behind. Drive the real board, not the pure function.
        let mut app = projection_app();
        app.world_mut().spawn((
            NpcJobProfile::new(
                JobDomain::Cargo,
                super::super::jobs::NarrativeTier::Support,
                [super::super::jobs::JobCapability::new("cargo.freight")],
            ),
            NpcActivity::Working,
        ));
        app.update();
        assert_eq!(
            status_of(&mut app, crate::orders::Department::Cargo).condition,
            DepartmentCondition::OnSchedule,
        );

        for id in 0..4 {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(cargo_ticket(id, JobTicketState::Available))
                .unwrap();
        }
        app.update();
        let loaded = status_of(&mut app, crate::orders::Department::Cargo);
        assert_eq!(loaded.condition, DepartmentCondition::Backlogged);
        assert_eq!(loaded.reason, ConditionReason::QueuedWork { count: 4 });

        // A department that is not the one with the queue stays quiet — the
        // projection must not smear one department's pressure across the board.
        assert_eq!(
            status_of(&mut app, crate::orders::Department::Botany).condition,
            DepartmentCondition::OnSchedule,
        );
    }

    #[test]
    fn public_crew_status_never_exposes_which_job_is_being_done() {
        let mut app = projection_app();
        let worker = app.world_mut().spawn(NpcActivity::Working).id();
        app.update();
        let status = *app
            .world()
            .entity(worker)
            .get::<PublicCrewStatus>()
            .expect("a resident gets a public status");
        assert_eq!(status.activity, PublicActivity::Working);
        assert!(!status.visibly_hurt);
    }

    #[test]
    fn a_guest_can_render_the_menu_from_replicated_state_alone() {
        // The co-op guarantee, checked structurally: build a world holding
        // *only* the replicated components — no JobBoard, no IncidentLedger,
        // no AidIntakes — and confirm everything the menu reads is still there.
        // If a future field on the projection needs an authority resource to
        // interpret, this stops compiling or stops finding its row.
        let mut guest = World::new();
        guest.spawn((
            PublicDepartmentStatus {
                department: crate::orders::Department::Cargo,
                condition: DepartmentCondition::Strained,
                reason: ConditionReason::QueuedWork { count: 3 },
                workers_present: 1,
                workers_total: 4,
            },
            PublicCrewStatus {
                activity: PublicActivity::Working,
                visibly_hurt: false,
            },
        ));
        assert!(guest.get_resource::<JobBoard>().is_none());
        assert!(guest.get_resource::<IncidentLedger>().is_none());

        let mut query = guest.query::<(&PublicDepartmentStatus, &PublicCrewStatus)>();
        let (status, crew) = query.iter(&guest).next().expect("the guest has the row");
        assert_eq!(status.condition.label(), "Strained");
        assert_eq!(status.reason.text(), "3 jobs waiting");
        assert_eq!(crew.activity.label(), "Working");
    }

    #[test]
    fn a_late_join_sees_current_condition_without_any_history() {
        // A guest that arrives mid-shift receives the snapshot, not a replay.
        // The projection is a pure function of current counts, so there is no
        // accumulated state a late joiner could be missing — which is exactly
        // why it was built as a recompute rather than an event stream.
        let mut app = projection_app();
        app.world_mut().spawn((
            NpcJobProfile::new(
                JobDomain::Cargo,
                super::super::jobs::NarrativeTier::Support,
                [super::super::jobs::JobCapability::new("cargo.freight")],
            ),
            NpcActivity::Working,
        ));
        for id in 0..3 {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(cargo_ticket(id, JobTicketState::Available))
                .unwrap();
        }
        // Many frames pass before the guest arrives.
        for _ in 0..20 {
            app.update();
        }
        let seen = status_of(&mut app, crate::orders::Department::Cargo);
        assert_eq!(seen.condition, DepartmentCondition::Backlogged);
        assert_eq!(seen.reason, ConditionReason::QueuedWork { count: 3 });
    }

    #[test]
    fn covert_and_honest_handling_are_indistinguishable() {
        // A saboteur tampering with a meal and a cook preparing one are both
        // `Working`. If this ever stops holding, the menu has become a
        // detector and the whole social layer is pointless.
        let honest = PublicActivity::from_activity(NpcActivity::Working, false);
        let covert = PublicActivity::from_activity(NpcActivity::Working, false);
        assert_eq!(honest, covert);
        assert_eq!(honest, PublicActivity::Working);
    }

    #[test]
    fn a_body_in_a_bed_reads_as_recovering_whatever_it_was_doing() {
        assert_eq!(
            PublicActivity::from_activity(NpcActivity::Working, true),
            PublicActivity::RecoveringInMedical,
        );
        assert_eq!(
            PublicActivity::from_activity(NpcActivity::Down, false),
            PublicActivity::RecoveringInMedical,
        );
    }
}
