//! Cargo's first vertical slice on top of the shared utility/job contracts.
//!
//! Nothing in the selector knows what a manifest or freight sorter is. Cargo
//! publishes ordinary tickets here and applies Cargo state changes only after
//! the shared lifecycle reports a completed action.

use bevy::prelude::*;

use super::department::{control_for_resident, DepartmentRoster};
use super::jobs::{
    JobBoard, JobCapability, JobDomain, JobTicket, JobTicketId, JobTicketState, NpcJobProfile,
    UtilitySpots,
};
use super::{
    stable_text_key, ActionResult, ActionTarget, ControlOwner, DepartmentProblemCandidate,
    DepartmentProblemDirector, DepartmentProblemPolicy, IncidentCreated, IncidentKind,
    IncidentLedger, Normalized, ProblemPermitDecision, ProblemStabilitySnapshot, ReservationKey,
    UtilityActionId, UtilityActionResolved, UtilityAgent, UtilityBucket,
};
use crate::crew::{Ambient, CrewMember, StationResident};

pub const CARGO_SECOND_CORE: &str = "Quartermaster Rhee";
pub const CARGO_SUPPORT: [&str; 2] = crate::crew::fluff::CARGO_SUPPORT_NAMES;
const CARGO_CORE: [&str; 2] = ["Miner Sato", CARGO_SECOND_CORE];
const CARGO_CAPABILITY: &str = "cargo.operations";
const MEDICAL_REQUEST_CAPABILITY: &str = "medical.request_treatment";
const CARGO_CAPABILITIES: [&str; 3] = [
    CARGO_CAPABILITY,
    MEDICAL_REQUEST_CAPABILITY,
    super::aid::assessment_capability(JobDomain::Cargo),
];
pub(super) const CARGO_ROSTER: DepartmentRoster = DepartmentRoster {
    domain: JobDomain::Cargo,
    core: &CARGO_CORE,
    support: &CARGO_SUPPORT,
    expected_support: 2,
    capabilities: &CARGO_CAPABILITIES,
};
const ARRIVAL_SECONDS: f32 = 90.0;
const MAX_PIPELINE_FREIGHT: u32 = 16;
const MAX_REQUISITIONS: u32 = 4;
const CARGO_BURN_PROBLEM: &str = "cargo.workplace.burn";
const CARGO_PROBLEM_POLICY: DepartmentProblemPolicy = DepartmentProblemPolicy {
    domain: JobDomain::Cargo,
    opening_grace: 6,
    domain_cooldown: 8,
    actor_cooldown: 8,
    unresolved_cap: 1,
    shift_cap: 4,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CargoJobKind {
    ReviewManifest,
    WeighFreight,
    SortFreight,
    DispatchFreight,
    ClearRequisition,
}

impl CargoJobKind {
    const ALL: [Self; 5] = [
        Self::ReviewManifest,
        Self::WeighFreight,
        Self::SortFreight,
        Self::DispatchFreight,
        Self::ClearRequisition,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::ReviewManifest => "cargo.manifest",
            Self::WeighFreight => "cargo.weigh",
            Self::SortFreight => "cargo.sort",
            Self::DispatchFreight => "cargo.dispatch",
            Self::ClearRequisition => "cargo.requisition",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    fn perform_seconds(self) -> f32 {
        match self {
            Self::ReviewManifest => 5.0,
            Self::WeighFreight => 6.0,
            Self::SortFreight => 7.0,
            Self::DispatchFreight => 6.5,
            Self::ClearRequisition => 5.5,
        }
    }

    fn urgency(self) -> f32 {
        match self {
            Self::DispatchFreight => 1.0,
            Self::ClearRequisition => 0.98,
            Self::SortFreight => 0.96,
            Self::WeighFreight => 0.94,
            Self::ReviewManifest => 0.92,
        }
    }

    fn risk(self) -> f32 {
        match self {
            Self::WeighFreight | Self::SortFreight | Self::DispatchFreight => 0.12,
            Self::ReviewManifest | Self::ClearRequisition => 0.02,
        }
    }
}

/// Real, authority-side Cargo throughput. Each freight unit advances through
/// the pipeline instead of a worker merely playing an animation at a console.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct CargoWorkState {
    pub unmanifested: u32,
    pub manifested: u32,
    pub weighed: u32,
    pub sorted: u32,
    pub processed: u32,
    pub open_requisitions: u32,
    pub cleared_requisitions: u32,
    arrival_in: f32,
}

impl Default for CargoWorkState {
    fn default() -> Self {
        Self {
            unmanifested: 3,
            manifested: 2,
            weighed: 2,
            sorted: 2,
            processed: 0,
            open_requisitions: 2,
            cleared_requisitions: 0,
            arrival_in: ARRIVAL_SECONDS,
        }
    }
}

impl CargoWorkState {
    pub fn pending_freight(&self) -> u32 {
        self.unmanifested + self.manifested + self.weighed + self.sorted
    }

    fn work_available(&self, kind: CargoJobKind) -> u32 {
        match kind {
            CargoJobKind::ReviewManifest => self.unmanifested,
            CargoJobKind::WeighFreight => self.manifested,
            CargoJobKind::SortFreight => self.weighed,
            CargoJobKind::DispatchFreight => self.sorted,
            CargoJobKind::ClearRequisition => self.open_requisitions,
        }
    }

    fn complete(&mut self, kind: CargoJobKind) -> bool {
        match kind {
            CargoJobKind::ReviewManifest if self.unmanifested > 0 => {
                self.unmanifested -= 1;
                self.manifested += 1;
            }
            CargoJobKind::WeighFreight if self.manifested > 0 => {
                self.manifested -= 1;
                self.weighed += 1;
            }
            CargoJobKind::SortFreight if self.weighed > 0 => {
                self.weighed -= 1;
                self.sorted += 1;
            }
            CargoJobKind::DispatchFreight if self.sorted > 0 => {
                self.sorted -= 1;
                self.processed += 1;
            }
            CargoJobKind::ClearRequisition if self.open_requisitions > 0 => {
                self.open_requisitions -= 1;
                self.cleared_requisitions += 1;
            }
            _ => return false,
        }
        true
    }
}

/// Public event for later Crew-menu summaries and incident risk evaluation.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct CargoJobCompleted {
    pub worker: Entity,
    pub kind: String,
    pub risk: Normalized,
}

/// Compatibility name for the shared debug control used by scenario tests.
/// Forced outcomes now live in the station-wide problem director.
pub type ForcedWorkplaceOutcome = DepartmentProblemDirector;

impl DepartmentProblemDirector {
    pub fn force_next_cargo_burn(&mut self) {
        self.force_next(JobDomain::Cargo, CARGO_BURN_PROBLEM);
    }
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<CargoWorkState>()
        .add_message::<CargoJobCompleted>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_cargo_pilot
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        // Migration happens in PreUpdate. Commands are flushed before Update,
        // so legacy ambient behavior sees UtilityAgent and excludes the worker
        // on the first frame utility control can possibly act.
        .add_systems(
            PreUpdate,
            activate_cargo_pilot
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (tick_cargo_arrivals, publish_cargo_tickets)
                .chain()
                .in_set(super::UtilityAiSet::BuildContext)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (apply_cargo_job_results, apply_cargo_workplace_risk)
                .chain()
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

fn reset_cargo_pilot(
    mut cargo: ResMut<CargoWorkState>,
    mut problems: ResMut<DepartmentProblemDirector>,
    mut board: ResMut<JobBoard>,
    mut reservations: ResMut<super::ReservationBook>,
    mut incidents: ResMut<IncidentLedger>,
) {
    CARGO_ROSTER
        .validate()
        .expect("the Cargo utility roster must be valid");
    *cargo = CargoWorkState::default();
    problems.reset_domain(CARGO_PROBLEM_POLICY);
    board.remove_domain(JobDomain::Cargo);
    incidents.remove_domain(JobDomain::Cargo);
    for kind in CargoJobKind::ALL {
        reservations.release_key(&ReservationKey(format!("utility.spot.{}", kind.id())));
    }
}

fn cargo_profile(name: &str) -> Option<NpcJobProfile> {
    CARGO_ROSTER
        .profile_for(name)
        .map(|profile| profile.with_cross_training([JobDomain::Medical]))
}

fn activate_cargo_pilot(
    mut commands: Commands,
    social: Option<Res<crate::social::SocialState>>,
    residents: Query<
        (
            Entity,
            &CrewMember,
            &crate::crew::CrewRoute,
            Option<&ControlOwner>,
        ),
        (
            With<Ambient>,
            Without<UtilityAgent>,
            Without<crate::social::NpcCommitment>,
        ),
    >,
) {
    for (entity, member, route, owner) in &residents {
        if social
            .as_ref()
            .and_then(|social| social.active_favor.as_ref())
            .is_some_and(|favor| favor.owner == member.name)
        {
            continue;
        }
        let Some((control, profile)) =
            control_for_resident(&member.name, route, owner, CARGO_ROSTER)
        else {
            continue;
        };
        commands
            .entity(entity)
            .insert((control, profile, StationResident));
    }
}

fn tick_cargo_arrivals(time: Res<Time>, mut cargo: ResMut<CargoWorkState>) {
    cargo.arrival_in -= time.delta_secs();
    if cargo.arrival_in > 0.0 {
        return;
    }
    cargo.arrival_in += ARRIVAL_SECONDS;
    if cargo.pending_freight() < MAX_PIPELINE_FREIGHT {
        cargo.unmanifested += 2;
    }
    if cargo.open_requisitions < MAX_REQUISITIONS {
        cargo.open_requisitions += 1;
    }
}

fn publish_cargo_tickets(
    time: Res<Time>,
    cargo: Res<CargoWorkState>,
    spots: Res<UtilitySpots>,
    mut board: ResMut<JobBoard>,
) {
    for kind in CargoJobKind::ALL {
        if cargo.work_available(kind) == 0 {
            continue;
        }
        let id = JobTicketId(stable_text_key(kind.id()));
        if board.ticket(id).is_some() {
            continue;
        }
        let Some(spot) = spots.get(kind.id()) else {
            continue;
        };
        let ticket = JobTicket {
            id,
            domain: JobDomain::Cargo,
            kind: kind.id().to_string(),
            target: ActionTarget::Point(spot.at),
            subject: None,
            reservation: ReservationKey(format!("utility.spot.{}", kind.id())),
            reservation_capacity: spot.capacity,
            bucket: UtilityBucket::Routine,
            urgency: Normalized::new(kind.urgency()).expect("Cargo urgency is normalized"),
            required_capability: JobCapability::new(CARGO_CAPABILITY),
            created_at: time.elapsed_secs(),
            deadline: None,
            risk: Normalized::new(kind.risk()).expect("Cargo risk is normalized"),
            perform_seconds: kind.perform_seconds(),
            state: JobTicketState::Available,
        };
        board
            .publish(ticket)
            .expect("Cargo checked the stable ticket ID before publishing");
    }
}

fn apply_cargo_job_results(
    mut results: MessageReader<UtilityActionResolved>,
    mut completed: MessageWriter<CargoJobCompleted>,
    mut cargo: ResMut<CargoWorkState>,
    mut board: ResMut<JobBoard>,
) {
    for result in results.read() {
        if result.key.action != UtilityActionId::PerformJob
            || result.result != ActionResult::Completed
        {
            continue;
        }
        let id = JobTicketId(result.key.target_key);
        let Some(ticket) = board.ticket(id) else {
            continue;
        };
        if ticket.domain != JobDomain::Cargo
            || ticket.state != JobTicketState::Completed(result.claim)
        {
            continue;
        }
        let Some(kind) = CargoJobKind::from_id(&ticket.kind) else {
            continue;
        };
        if !cargo.complete(kind) {
            continue;
        }
        let kind_id = ticket.kind.clone();
        let risk = ticket.risk;
        if board.take_completed(id, result.claim).is_err() {
            continue;
        }
        completed.write(CargoJobCompleted {
            worker: result.agent,
            kind: kind_id,
            risk,
        });
    }
}

fn apply_cargo_workplace_risk(
    time: Res<Time>,
    stability: Res<crate::instability::StationStability>,
    mut completions: MessageReader<CargoJobCompleted>,
    mut created: MessageWriter<IncidentCreated>,
    mut problems: ResMut<DepartmentProblemDirector>,
    mut incidents: ResMut<IncidentLedger>,
    mut workers: ParamSet<(
        Query<(&CrewMember, &Transform)>,
        Query<(
            &mut crate::body::Body,
            Option<&mut crate::body::Bloodstream>,
        )>,
    )>,
) {
    problems.ensure_policy(CARGO_PROBLEM_POLICY);

    struct PreparedCargoProblem {
        candidate: DepartmentProblemCandidate,
        location: Vec3,
    }

    let mut candidates = Vec::new();
    {
        let workers = workers.p0();
        for completion in completions.read() {
            let Some(kind) = CargoJobKind::from_id(&completion.kind) else {
                continue;
            };
            if !matches!(
                kind,
                CargoJobKind::WeighFreight
                    | CargoJobKind::SortFreight
                    | CargoJobKind::DispatchFreight
            ) {
                continue;
            }
            let Ok((member, transform)) = workers.get(completion.worker) else {
                // A queued forced result is intentionally untouched when the
                // exact completed-job actor no longer has an applicable body.
                continue;
            };
            candidates.push(PreparedCargoProblem {
                candidate: DepartmentProblemCandidate::new(
                    CARGO_BURN_PROBLEM,
                    JobDomain::Cargo,
                    IncidentKind::Burn,
                    completion.worker,
                    completion.risk,
                    stable_text_key(&member.name),
                ),
                location: transform.translation,
            });
        }
    }
    candidates.sort_by(|left, right| left.candidate.deterministic_cmp(&right.candidate));

    let mut workers = workers.p1();
    for prepared in candidates {
        let subject = prepared.candidate.subject;
        let Ok((mut body, blood)) = workers.get_mut(subject) else {
            continue;
        };
        let ProblemPermitDecision::Permit(permit) =
            problems.request_permit(prepared.candidate, &stability, &incidents)
        else {
            continue;
        };
        let snapshot = permit.stability();
        let severity = cargo_burn_severity_from_snapshot(snapshot);
        let Ok(id) = incidents.create(
            IncidentKind::Burn,
            JobDomain::Cargo,
            subject,
            None,
            prepared.location,
            severity,
            time.elapsed_secs(),
        ) else {
            problems.abort(permit);
            continue;
        };
        let previously_collapsed = body.0.collapsed;
        body.0.apply(chem_sim::Damage::of(
            chem_sim::DamageKind::Burn,
            cargo_burn_damage_from_snapshot(snapshot),
        ));
        if let Some(blood) = blood {
            blood
                .0
                .reconcile_collapse(&mut body.0, previously_collapsed);
        }
        problems.commit(permit);
        created.write(IncidentCreated {
            id,
            kind: IncidentKind::Burn,
            department: JobDomain::Cargo,
            subject,
            location: prepared.location,
            severity,
        });
    }
}

/// Stability affects both how often routine work goes wrong and how serious a
/// resulting injury is. The separate caps keep a struggling station dramatic
/// without letting one bad roll create an unrecoverable death spiral.
fn cargo_burn_severity(stability: &crate::instability::StationStability) -> Normalized {
    cargo_burn_severity_from_health(
        (stability.value / crate::instability::STABILITY_MAX).clamp(0.0, 1.0),
    )
}

fn cargo_burn_severity_from_snapshot(snapshot: ProblemStabilitySnapshot) -> Normalized {
    cargo_burn_severity_from_health(snapshot.health())
}

fn cargo_burn_severity_from_health(health: f32) -> Normalized {
    let deterioration = 1.0 - health;
    Normalized::new(0.35 + deterioration * 0.30)
        .expect("the clamped Cargo burn severity is normalized")
}

fn cargo_burn_damage(stability: &crate::instability::StationStability) -> chem_sim::Units {
    cargo_burn_damage_from_health(
        (stability.value / crate::instability::STABILITY_MAX).clamp(0.0, 1.0),
    )
}

fn cargo_burn_damage_from_snapshot(snapshot: ProblemStabilitySnapshot) -> chem_sim::Units {
    cargo_burn_damage_from_health(snapshot.health())
}

fn cargo_burn_damage_from_health(health: f32) -> chem_sim::Units {
    let deterioration = 1.0 - health;
    chem_sim::Units::whole(18 + (deterioration * 12.0).round() as i32)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::body::{Bloodstream, Body};
    use crate::crew::{CrewPosts, CrewRoute, ErrandResolved};
    use crate::utility_ai::{
        begin_reference_actions, consume_utility_arrivals, perform_reference_actions,
        resolve_reference_actions, select_reference_actions, tick_current_actions, LocomotionOwner,
        NpcActivity, ReservationBook, UtilityControlBundle, UtilityDecisionLog,
    };

    #[derive(Resource, Default)]
    struct CompletionLog(Vec<CargoJobCompleted>);

    fn record_completions(
        mut messages: MessageReader<CargoJobCompleted>,
        mut log: ResMut<CompletionLog>,
    ) {
        log.0.extend(messages.read().cloned());
    }

    #[test]
    fn only_the_four_authored_cargo_workers_migrate() {
        let mut app = App::new();
        app.add_systems(Update, (activate_cargo_pilot, ApplyDeferred).chain());
        for (name, role) in [
            ("Miner Sato", "Cargo"),
            (CARGO_SECOND_CORE, "Cargo"),
            (CARGO_SUPPORT[0], "Cargo"),
            (CARGO_SUPPORT[1], "Cargo"),
            ("Dr. Vance", "Medical"),
        ] {
            let route = if name == "Miner Sato" {
                CrewRoute::to(Vec3::X)
            } else {
                CrewRoute::standing()
            };
            app.world_mut().spawn((
                CrewMember {
                    name: name.into(),
                    role: role.into(),
                },
                Ambient::new(5.0),
                route,
            ));
        }
        app.update();

        let world = app.world_mut();
        let mut migrated = world.query_filtered::<&CrewMember, With<UtilityAgent>>();
        let names: HashSet<_> = migrated
            .iter(world)
            .map(|member| member.name.as_str())
            .collect();
        assert_eq!(names.len(), 4);
        assert!(!names.contains("Dr. Vance"));
        assert!(CARGO_CORE.into_iter().all(|name| names.contains(name)));
        assert!(CARGO_SUPPORT.into_iter().all(|name| names.contains(name)));
        let world = app.world_mut();
        let mut sato = world.query::<(&CrewMember, &LocomotionOwner, &NpcActivity)>();
        let (_, locomotion, activity) = sato
            .iter(world)
            .find(|(member, _, _)| member.name == "Miner Sato")
            .unwrap();
        assert_eq!(*locomotion, LocomotionOwner::CrewRoute);
        assert_eq!(*activity, NpcActivity::Traveling);
    }

    #[test]
    fn cargo_migration_preserves_live_and_restored_social_commitments() {
        let mut app = App::new();
        app.insert_resource(crate::social::SocialState {
            active_favor: Some(crate::social::ActiveFavor {
                owner: CARGO_SECOND_CORE.into(),
                stage: 1,
                remaining_seconds: 90,
            }),
            ..default()
        })
        .add_systems(Update, (activate_cargo_pilot, ApplyDeferred).chain());
        let sato = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: CARGO_CORE[0].into(),
                    role: "Cargo".into(),
                },
                Ambient::new(5.0),
                CrewRoute::standing(),
                crate::social::NpcCommitment,
            ))
            .id();
        let rhee = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: CARGO_SECOND_CORE.into(),
                    role: "Cargo".into(),
                },
                Ambient::new(5.0),
                CrewRoute::standing(),
            ))
            .id();
        for name in CARGO_SUPPORT {
            app.world_mut().spawn((
                CrewMember {
                    name: name.into(),
                    role: "Cargo".into(),
                },
                Ambient::new(5.0),
                CrewRoute::standing(),
            ));
        }

        app.update();

        assert!(app.world().get::<UtilityAgent>(sato).is_none());
        assert!(app.world().get::<UtilityAgent>(rhee).is_none());
        let world = app.world_mut();
        let mut migrated = world.query_filtered::<&CrewMember, With<UtilityAgent>>();
        let names: HashSet<_> = migrated
            .iter(world)
            .map(|member| member.name.as_str())
            .collect();
        assert_eq!(names.len(), CARGO_SUPPORT.len());
        assert!(CARGO_SUPPORT.into_iter().all(|name| names.contains(name)));
    }

    #[test]
    fn ticket_generation_is_bounded_by_one_ticket_per_authored_spot() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<CargoWorkState>()
            .init_resource::<UtilitySpots>()
            .init_resource::<JobBoard>()
            .add_systems(Update, publish_cargo_tickets);
        for (index, kind) in CargoJobKind::ALL.into_iter().enumerate() {
            app.world_mut().resource_mut::<UtilitySpots>().insert(
                kind.id(),
                Vec3::new(index as f32, 0.0, 0.0),
                1,
            );
        }
        for _ in 0..20 {
            app.update();
        }
        assert_eq!(app.world().resource::<JobBoard>().len(), 5);
    }

    #[test]
    fn forced_cargo_burn_targets_the_worker_and_respects_the_one_case_cap() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<crate::instability::StationStability>()
            .init_resource::<ForcedWorkplaceOutcome>()
            .init_resource::<IncidentLedger>()
            .add_message::<CargoJobCompleted>()
            .add_message::<IncidentCreated>()
            .add_systems(Update, apply_cargo_workplace_risk);
        let worker = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Miner Sato".into(),
                    role: "Cargo".into(),
                },
                Transform::from_xyz(2.0, crate::crew::BODY_OFFSET, 3.0),
                Body::default(),
                Bloodstream::default(),
            ))
            .id();
        let bystander = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Loader Bell".into(),
                    role: "Cargo".into(),
                },
                Transform::from_xyz(2.5, crate::crew::BODY_OFFSET, 3.0),
                Body::default(),
                Bloodstream::default(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<ForcedWorkplaceOutcome>()
            .force_next_cargo_burn();
        app.world_mut().write_message(CargoJobCompleted {
            worker,
            kind: CargoJobKind::SortFreight.id().into(),
            risk: Normalized::new(0.12).unwrap(),
        });

        app.update();

        assert_eq!(
            app.world().get::<Body>(worker).unwrap().0.damage.burn,
            chem_sim::Units::whole(18),
        );
        assert_eq!(
            app.world().get::<Body>(bystander).unwrap().0.damage.burn,
            chem_sim::Units::ZERO,
        );
        let incidents = app.world().resource::<IncidentLedger>();
        let incident = incidents.active().next().expect("the burn opens a case");
        assert_eq!(incident.subject, worker);
        assert_eq!(incident.kind, IncidentKind::Burn);
        assert_eq!(
            incident.location,
            Vec3::new(2.0, crate::crew::BODY_OFFSET, 3.0)
        );
        assert_eq!(incidents.active().count(), 1);
        let first_id = incident.id;

        app.world_mut()
            .resource_mut::<ForcedWorkplaceOutcome>()
            .force_next_cargo_burn();
        app.world_mut().write_message(CargoJobCompleted {
            worker: bystander,
            kind: CargoJobKind::DispatchFreight.id().into(),
            risk: Normalized::new(0.12).unwrap(),
        });
        app.update();
        assert_eq!(
            app.world().get::<Body>(bystander).unwrap().0.damage.burn,
            chem_sim::Units::ZERO,
            "a second Cargo casualty is suppressed while the first is active",
        );
        assert_eq!(app.world().resource::<IncidentLedger>().active().count(), 1);

        app.world_mut()
            .resource_mut::<IncidentLedger>()
            .resolve(first_id)
            .unwrap();
        app.world_mut().write_message(CargoJobCompleted {
            worker: bystander,
            kind: CargoJobKind::DispatchFreight.id().into(),
            risk: Normalized::new(0.12).unwrap(),
        });
        app.update();
        assert_eq!(
            app.world().get::<Body>(bystander).unwrap().0.damage.burn,
            chem_sim::Units::whole(18),
            "the queued forced outcome becomes eligible after the first case resolves",
        );
    }

    #[test]
    fn low_stability_makes_cargo_burns_more_severe_but_keeps_them_bounded() {
        let stable = crate::instability::StationStability::default();
        let critical = crate::instability::StationStability {
            value: 0.0,
            band: crate::instability::StabilityBand::Critical,
            ..default()
        };

        assert_eq!(cargo_burn_severity(&stable).get(), 0.35);
        assert_eq!(cargo_burn_damage(&stable), chem_sim::Units::whole(18));
        assert_eq!(cargo_burn_severity(&critical).get(), 0.65);
        assert_eq!(cargo_burn_damage(&critical), chem_sim::Units::whole(30));
        assert!(cargo_burn_severity(&critical).get() <= 1.0);
    }

    #[test]
    fn four_workers_run_the_real_cargo_pipeline_without_claim_collisions() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<CompletionLog>()
            .insert_resource(CargoWorkState {
                unmanifested: 1,
                manifested: 1,
                weighed: 1,
                sorted: 1,
                processed: 0,
                open_requisitions: 1,
                cleared_requisitions: 0,
                arrival_in: 10_000.0,
            })
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_message::<CargoJobCompleted>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    publish_cargo_tickets,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    apply_cargo_job_results,
                    ApplyDeferred,
                    record_completions,
                )
                    .chain(),
            );

        for (index, kind) in CargoJobKind::ALL.into_iter().enumerate() {
            app.world_mut().resource_mut::<UtilitySpots>().insert(
                kind.id(),
                Vec3::new(-4.0 + index as f32 * 2.0, 0.0, 0.0),
                1,
            );
        }
        let mut worker_entities = Vec::new();
        for (index, name) in CARGO_CORE.into_iter().chain(CARGO_SUPPORT).enumerate() {
            let profile = cargo_profile(name).unwrap();
            let worker = app
                .world_mut()
                .spawn((
                    CrewMember {
                        name: name.into(),
                        role: "Cargo".into(),
                    },
                    Transform::from_xyz(-3.0 + index as f32 * 2.0, crate::crew::BODY_OFFSET, -2.0),
                    Body::default(),
                    Bloodstream::default(),
                    CrewRoute::standing(),
                    UtilityControlBundle::new(UtilityAgent::new(100 + index as u64, 0)),
                    profile,
                ))
                .id();
            worker_entities.push(worker);
        }

        let mut largest_board = 0;
        for _ in 0..4_000 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            largest_board = largest_board.max(app.world().resource::<JobBoard>().len());
            let state = app.world().resource::<CargoWorkState>();
            if state.pending_freight() == 0 && state.open_requisitions == 0 {
                break;
            }
        }

        let state = app.world().resource::<CargoWorkState>();
        let action_debug: Vec<_> = worker_entities
            .iter()
            .map(|worker| {
                (
                    *worker,
                    app.world()
                        .get::<crate::utility_ai::CurrentAction>(*worker)
                        .cloned(),
                )
            })
            .collect();
        let ticket_debug: Vec<_> = app.world().resource::<JobBoard>().iter().cloned().collect();
        assert_eq!(
            state.pending_freight(),
            0,
            "pipeline stalled: {state:?}; actions: {action_debug:?}; tickets: {ticket_debug:?}"
        );
        assert_eq!(state.processed, 4);
        assert_eq!(state.cleared_requisitions, 1);
        assert!(largest_board <= CargoJobKind::ALL.len());
        assert!(app.world().resource::<JobBoard>().is_empty());
        assert_eq!(
            app.world()
                .resource::<ReservationBook>()
                .active_claim_count(),
            0
        );
        let completions = &app.world().resource::<CompletionLog>().0;
        let workers: HashSet<_> = completions.iter().map(|event| event.worker).collect();
        assert_eq!(
            workers.len(),
            4,
            "the pilot did not distribute work across all four workers: {completions:?}"
        );
        assert!(completions
            .iter()
            .any(|event| event.kind == CargoJobKind::ClearRequisition.id()));
        assert!(completions
            .iter()
            .any(|event| event.kind == CargoJobKind::DispatchFreight.id()));

        let world = app.world_mut();
        let mut workers = world.query::<(
            &ControlOwner,
            &LocomotionOwner,
            &NpcActivity,
            &NpcJobProfile,
        )>();
        assert!(workers.iter(world).all(|(owner, locomotion, _, profile)| {
            *owner == ControlOwner::UtilityAction
                && *locomotion == LocomotionOwner::None
                && profile.primary == JobDomain::Cargo
        }));
    }
}
