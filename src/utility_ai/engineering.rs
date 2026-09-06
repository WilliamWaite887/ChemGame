//! Engineering routine work on the shared utility, job, and incident contracts.
//!
//! Each ticket comes from one named authority-side asset fact. The adapter only
//! changes that fact after the shared action lifecycle reports the exact ticket
//! owner completed it. Equipment failures remain real downtime until a repair
//! job resolves the incident attached to that same asset.

use bevy::prelude::*;

use super::department::{control_for_resident, DepartmentRoster};
use super::jobs::{
    JobBoard, JobCapability, JobDomain, JobTicket, JobTicketId, JobTicketState, NarrativeTier,
    NpcJobProfile, UtilitySpots,
};
use super::{
    stable_text_key, ActionResult, ActionTarget, ControlOwner, DepartmentProblemCandidate,
    DepartmentProblemDirector, DepartmentProblemPolicy, IncidentCreated, IncidentId, IncidentKind,
    IncidentLedger, IncidentResolved, IncidentStatus, Normalized, ProblemPermitDecision,
    ProblemStabilitySnapshot, ReservationKey, UtilityActionId, UtilityActionResolved, UtilityAgent,
    UtilityBucket,
};
use crate::crew::{Ambient, CrewMember, StationResident};

pub const ENGINEERING_SECOND_CORE: &str = "Chief Engineer Morrow";
pub const ENGINEERING_SUPPORT: [&str; 2] = crate::crew::fluff::ENGINEERING_SUPPORT_NAMES;
const ENGINEERING_CORE: [&str; 2] = ["Tech Lindqvist", ENGINEERING_SECOND_CORE];

const INSPECT_CAPABILITY: &str = "engineering.inspect";
const MAINTAIN_CAPABILITY: &str = "engineering.maintain";
const COOLANT_CAPABILITY: &str = "engineering.coolant";
const POWER_CAPABILITY: &str = "engineering.power";
const REPAIR_CAPABILITY: &str = "engineering.repair";
const ENGINEERING_CAPABILITIES: [&str; 6] = [
    INSPECT_CAPABILITY,
    MAINTAIN_CAPABILITY,
    COOLANT_CAPABILITY,
    POWER_CAPABILITY,
    REPAIR_CAPABILITY,
    super::aid::assessment_capability(JobDomain::Engineering),
];
pub(super) const ENGINEERING_ROSTER: DepartmentRoster = DepartmentRoster {
    domain: JobDomain::Engineering,
    core: &ENGINEERING_CORE,
    support: &ENGINEERING_SUPPORT,
    expected_support: 2,
    capabilities: &ENGINEERING_CAPABILITIES,
};

const GENERATOR_INSPECTION_SPOT: &str = "engineering.generator.inspect";
const BREAKER_MAINTENANCE_SPOT: &str = "engineering.breaker.maintenance";
const COOLANT_MANIFOLD_SPOT: &str = "engineering.coolant.manifold";
const POWER_MONITOR_SPOT: &str = "engineering.power.monitor";
const ENGINEERING_SPOTS: [&str; 4] = [
    GENERATOR_INSPECTION_SPOT,
    BREAKER_MAINTENANCE_SPOT,
    COOLANT_MANIFOLD_SPOT,
    POWER_MONITOR_SPOT,
];

const ENGINEERING_EQUIPMENT_FAULT: &str = "engineering.equipment_fault";
const ENGINEERING_PROBLEM_POLICY: DepartmentProblemPolicy = DepartmentProblemPolicy {
    domain: JobDomain::Engineering,
    opening_grace: 2,
    domain_cooldown: 4,
    actor_cooldown: 4,
    unresolved_cap: 1,
    shift_cap: 2,
};

/// The bounded world facts that can create Engineering work.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineeringAssetState {
    InspectionDue,
    MaintenanceDue,
    CoolantCheckDue,
    PowerCheckDue,
    Operational,
    Faulted {
        incident: IncidentId,
        severity: Normalized,
    },
}

/// One stable Engineering target and the exact map affordance used to service it.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineeringAsset {
    pub id: String,
    pub spot: &'static str,
    pub state: EngineeringAssetState,
}

/// A finite Engineering shift. No timer creates routine tickets in this slice;
/// each authored fact can be completed once unless a bounded fault reopens it.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct EngineeringWorkState {
    pub assets: Vec<EngineeringAsset>,
    pub completed_jobs: u32,
    pub resolved_faults: u32,
}

impl Default for EngineeringWorkState {
    fn default() -> Self {
        Self {
            assets: vec![
                EngineeringAsset {
                    id: "engineering.asset.generator".into(),
                    spot: GENERATOR_INSPECTION_SPOT,
                    state: EngineeringAssetState::InspectionDue,
                },
                EngineeringAsset {
                    id: "engineering.asset.breaker".into(),
                    spot: BREAKER_MAINTENANCE_SPOT,
                    state: EngineeringAssetState::MaintenanceDue,
                },
                EngineeringAsset {
                    id: "engineering.asset.coolant".into(),
                    spot: COOLANT_MANIFOLD_SPOT,
                    state: EngineeringAssetState::CoolantCheckDue,
                },
                EngineeringAsset {
                    id: "engineering.asset.power".into(),
                    spot: POWER_MONITOR_SPOT,
                    state: EngineeringAssetState::PowerCheckDue,
                },
            ],
            completed_jobs: 0,
            resolved_faults: 0,
        }
    }
}

impl EngineeringWorkState {
    pub fn asset(&self, id: &str) -> Option<&EngineeringAsset> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    pub fn asset_mut(&mut self, id: &str) -> Option<&mut EngineeringAsset> {
        self.assets.iter_mut().find(|asset| asset.id == id)
    }

    fn work_for(&self, asset: &EngineeringAsset) -> Option<EngineeringJobKind> {
        match asset.state {
            EngineeringAssetState::InspectionDue => Some(EngineeringJobKind::InspectMachinery),
            EngineeringAssetState::MaintenanceDue => Some(EngineeringJobKind::MaintainBreaker),
            EngineeringAssetState::CoolantCheckDue => Some(EngineeringJobKind::CheckCoolant),
            EngineeringAssetState::PowerCheckDue => Some(EngineeringJobKind::CheckPower),
            EngineeringAssetState::Faulted { .. } => Some(EngineeringJobKind::RepairFault),
            EngineeringAssetState::Operational => None,
        }
    }

    fn can_complete(&self, asset_index: usize, kind: EngineeringJobKind) -> bool {
        self.assets
            .get(asset_index)
            .is_some_and(|asset| self.work_for(asset) == Some(kind))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineeringJobKind {
    InspectMachinery,
    MaintainBreaker,
    CheckCoolant,
    CheckPower,
    RepairFault,
}

impl EngineeringJobKind {
    const ALL: [Self; 5] = [
        Self::InspectMachinery,
        Self::MaintainBreaker,
        Self::CheckCoolant,
        Self::CheckPower,
        Self::RepairFault,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::InspectMachinery => "engineering.inspect",
            Self::MaintainBreaker => "engineering.maintain",
            Self::CheckCoolant => "engineering.coolant",
            Self::CheckPower => "engineering.power",
            Self::RepairFault => "engineering.repair",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    fn capability(self) -> &'static str {
        match self {
            Self::InspectMachinery => INSPECT_CAPABILITY,
            Self::MaintainBreaker => MAINTAIN_CAPABILITY,
            Self::CheckCoolant => COOLANT_CAPABILITY,
            Self::CheckPower => POWER_CAPABILITY,
            Self::RepairFault => REPAIR_CAPABILITY,
        }
    }

    fn urgency(self) -> f32 {
        match self {
            Self::RepairFault => 1.0,
            Self::MaintainBreaker => 0.98,
            Self::CheckPower => 0.96,
            Self::CheckCoolant => 0.95,
            Self::InspectMachinery => 0.93,
        }
    }

    fn perform_seconds(self) -> f32 {
        match self {
            Self::InspectMachinery => 4.0,
            Self::CheckPower => 4.5,
            Self::CheckCoolant => 5.0,
            Self::MaintainBreaker => 6.0,
            Self::RepairFault => 7.0,
        }
    }

    fn risk(self) -> Normalized {
        let risk = match self {
            Self::InspectMachinery => 0.04,
            Self::MaintainBreaker => 0.11,
            Self::CheckCoolant => 0.12,
            Self::CheckPower => 0.10,
            Self::RepairFault => 0.0,
        };
        Normalized::new(risk).expect("Engineering risk is normalized")
    }
}

/// Exact completion detail consumed by Engineering's bounded fault director.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct EngineeringJobCompleted {
    pub worker: Entity,
    pub asset_id: String,
    pub kind: String,
    pub risk: Normalized,
}

impl DepartmentProblemDirector {
    pub fn force_next_engineering_fault(&mut self) {
        self.force_next(JobDomain::Engineering, ENGINEERING_EQUIPMENT_FAULT);
    }
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<EngineeringWorkState>()
        .add_message::<EngineeringJobCompleted>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_engineering_work
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            PreUpdate,
            activate_engineering_workers
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            publish_engineering_tickets
                .in_set(super::UtilityAiSet::BuildContext)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (
                apply_engineering_job_results,
                apply_engineering_workplace_risk,
            )
                .chain()
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

fn reset_engineering_work(
    mut state: ResMut<EngineeringWorkState>,
    mut problems: ResMut<DepartmentProblemDirector>,
    mut board: ResMut<JobBoard>,
    mut reservations: ResMut<super::ReservationBook>,
    mut incidents: ResMut<IncidentLedger>,
) {
    ENGINEERING_ROSTER
        .validate()
        .expect("the Engineering utility roster must be valid");
    *state = EngineeringWorkState::default();
    problems.reset_domain(ENGINEERING_PROBLEM_POLICY);
    board.remove_domain(JobDomain::Engineering);
    incidents.remove_domain(JobDomain::Engineering);
    for spot in ENGINEERING_SPOTS {
        reservations.release_key(&ReservationKey(format!("utility.spot.{spot}")));
    }
}

fn engineering_profile(name: &str) -> Option<NpcJobProfile> {
    let tier = ENGINEERING_ROSTER.profile_for(name)?.narrative_tier;
    let capabilities: &[&str] = match name {
        "Tech Lindqvist" => &[INSPECT_CAPABILITY, REPAIR_CAPABILITY],
        ENGINEERING_SECOND_CORE => &[POWER_CAPABILITY],
        "Mechanic Torres" => &[MAINTAIN_CAPABILITY, REPAIR_CAPABILITY],
        "Systems Tech Adeyemi" => &[COOLANT_CAPABILITY],
        _ => return None,
    };
    Some(NpcJobProfile::new(
        JobDomain::Engineering,
        tier,
        capabilities
            .iter()
            .map(|capability| JobCapability::new(*capability)),
    ))
}

fn activate_engineering_workers(
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
        if social.as_deref().is_some_and(|social| {
            social
                .active_favor
                .as_ref()
                .is_some_and(|favor| favor.owner == member.name)
        }) {
            continue;
        }
        let Some((control, _)) =
            control_for_resident(&member.name, route, owner, ENGINEERING_ROSTER)
        else {
            continue;
        };
        let Some(profile) = engineering_profile(&member.name) else {
            continue;
        };
        commands
            .entity(entity)
            .insert((control, profile, StationResident));
    }
}

fn asset_ticket_id(asset_id: &str) -> JobTicketId {
    JobTicketId(stable_text_key(&format!("engineering.ticket.{asset_id}")))
}

fn publish_engineering_tickets(
    time: Res<Time>,
    state: Res<EngineeringWorkState>,
    spots: Res<UtilitySpots>,
    mut board: ResMut<JobBoard>,
) {
    for asset in &state.assets {
        let Some(kind) = state.work_for(asset) else {
            continue;
        };
        let id = asset_ticket_id(&asset.id);
        if board.ticket(id).is_some() {
            continue;
        }
        let Some(spot) = spots.get(asset.spot) else {
            continue;
        };
        board
            .publish(JobTicket {
                id,
                domain: JobDomain::Engineering,
                kind: kind.id().into(),
                target: ActionTarget::Point(spot.at),
                subject: None,
                reservation: ReservationKey(format!("utility.spot.{}", asset.spot)),
                reservation_capacity: spot.capacity,
                bucket: if kind == EngineeringJobKind::RepairFault {
                    UtilityBucket::Important
                } else {
                    UtilityBucket::Routine
                },
                urgency: Normalized::new(kind.urgency())
                    .expect("Engineering urgency is normalized"),
                required_capability: JobCapability::new(kind.capability()),
                created_at: time.elapsed_secs(),
                deadline: None,
                risk: kind.risk(),
                perform_seconds: kind.perform_seconds(),
                state: JobTicketState::Available,
            })
            .expect("Engineering checked the stable asset ticket before publishing");
    }
}

fn apply_engineering_job_results(
    mut results: MessageReader<UtilityActionResolved>,
    mut completed: MessageWriter<EngineeringJobCompleted>,
    mut incident_resolved: MessageWriter<IncidentResolved>,
    mut state: ResMut<EngineeringWorkState>,
    mut board: ResMut<JobBoard>,
    mut incidents: ResMut<IncidentLedger>,
) {
    for result in results.read() {
        if result.key.action != UtilityActionId::PerformJob
            || result.result != ActionResult::Completed
        {
            continue;
        }
        let id = JobTicketId(result.key.target_key);
        let Some(ticket) = board.ticket(id).cloned() else {
            continue;
        };
        if ticket.domain != JobDomain::Engineering
            || ticket.state != JobTicketState::Completed(result.claim)
        {
            continue;
        }
        let Some(kind) = EngineeringJobKind::from_id(&ticket.kind) else {
            board.cancel(id);
            continue;
        };
        let Some(asset_index) = state
            .assets
            .iter()
            .position(|asset| asset_ticket_id(&asset.id) == id)
        else {
            board.cancel(id);
            continue;
        };
        if !state.can_complete(asset_index, kind) {
            board.cancel(id);
            continue;
        }

        let repair_incident = match (kind, state.assets[asset_index].state) {
            (EngineeringJobKind::RepairFault, EngineeringAssetState::Faulted { incident, .. }) => {
                let Some(record) = incidents.get(incident) else {
                    board.cancel(id);
                    continue;
                };
                if record.status != IncidentStatus::Active
                    || record.kind != IncidentKind::EquipmentFailure
                    || record.department != JobDomain::Engineering
                {
                    board.cancel(id);
                    continue;
                }
                Some(incident)
            }
            (EngineeringJobKind::RepairFault, _) => {
                board.cancel(id);
                continue;
            }
            _ => None,
        };

        if board.take_completed(id, result.claim).is_err() {
            continue;
        }
        if let Some(incident) = repair_incident {
            incidents
                .resolve(incident)
                .expect("the exact active Engineering incident was checked before commit");
            state.assets[asset_index].state = EngineeringAssetState::Operational;
            state.resolved_faults = state.resolved_faults.saturating_add(1);
            incident_resolved.write(IncidentResolved { id: incident });
        } else {
            state.assets[asset_index].state = EngineeringAssetState::Operational;
        }
        state.completed_jobs = state.completed_jobs.saturating_add(1);
        completed.write(EngineeringJobCompleted {
            worker: result.agent,
            asset_id: state.assets[asset_index].id.clone(),
            kind: kind.id().into(),
            risk: ticket.risk,
        });
    }
}

fn apply_engineering_workplace_risk(
    time: Res<Time>,
    stability: Res<crate::instability::StationStability>,
    mut completions: MessageReader<EngineeringJobCompleted>,
    mut created: MessageWriter<IncidentCreated>,
    mut witnessed: MessageWriter<super::Stimulus>,
    mut state: ResMut<EngineeringWorkState>,
    spots: Res<UtilitySpots>,
    mut problems: ResMut<DepartmentProblemDirector>,
    mut incidents: ResMut<IncidentLedger>,
    workers: Query<&CrewMember>,
    // Disjoint from `workers` above, which reads only `CrewMember`, so these
    // are two non-conflicting borrows rather than a `ParamSet`.
    mut bodies: Query<(
        &mut crate::body::Body,
        Option<&mut crate::body::Bloodstream>,
    )>,
) {
    problems.ensure_policy(ENGINEERING_PROBLEM_POLICY);

    struct PreparedEngineeringProblem {
        candidate: DepartmentProblemCandidate,
        asset_id: String,
        location: Vec3,
    }

    let mut candidates = Vec::new();
    for completion in completions.read() {
        let Some(kind) = EngineeringJobKind::from_id(&completion.kind) else {
            continue;
        };
        if kind == EngineeringJobKind::RepairFault {
            continue;
        }
        let Some(asset) = state.asset(&completion.asset_id) else {
            continue;
        };
        if asset.state != EngineeringAssetState::Operational {
            continue;
        }
        let Some(spot) = spots.get(asset.spot) else {
            continue;
        };
        let Ok(member) = workers.get(completion.worker) else {
            continue;
        };
        candidates.push(PreparedEngineeringProblem {
            candidate: DepartmentProblemCandidate::new(
                ENGINEERING_EQUIPMENT_FAULT,
                JobDomain::Engineering,
                IncidentKind::EquipmentFailure,
                completion.worker,
                completion.risk,
                stable_text_key(&format!(
                    "{}:{}:{}",
                    member.name, completion.asset_id, completion.kind
                )),
            ),
            asset_id: completion.asset_id.clone(),
            location: spot.at,
        });
    }
    candidates.sort_by(|left, right| left.candidate.deterministic_cmp(&right.candidate));

    for prepared in candidates {
        if !state
            .asset(&prepared.asset_id)
            .is_some_and(|asset| asset.state == EngineeringAssetState::Operational)
        {
            continue;
        }
        let subject = prepared.candidate.subject;
        let ProblemPermitDecision::Permit(permit) =
            problems.request_permit(prepared.candidate, &stability, &incidents)
        else {
            continue;
        };
        let severity = engineering_fault_severity_from_snapshot(permit.stability());
        let Ok(id) = incidents.create(
            IncidentKind::EquipmentFailure,
            JobDomain::Engineering,
            subject,
            None,
            prepared.location,
            severity,
            time.elapsed_secs(),
        ) else {
            problems.abort(permit);
            continue;
        };
        let Some(asset) = state.asset_mut(&prepared.asset_id) else {
            incidents
                .resolve(id)
                .expect("the just-created Engineering incident is active");
            problems.abort(permit);
            continue;
        };
        if asset.state != EngineeringAssetState::Operational {
            incidents
                .resolve(id)
                .expect("the just-created Engineering incident is active");
            problems.abort(permit);
            continue;
        }
        asset.state = EngineeringAssetState::Faulted {
            incident: id,
            severity,
        };
        problems.commit(permit);
        // A fault is loud and visible where it happens, so whoever is nearby
        // remembers it and whoever is not does not. This is what stops the
        // whole shift converging on a breaker nobody witnessed trip.
        witnessed.write(
            super::Stimulus::new(super::StimulusKind::Hazard, prepared.location)
                .with_strength(severity.get().clamp(0.3, 1.0)),
        );
        created.write(IncidentCreated {
            id,
            kind: IncidentKind::EquipmentFailure,
            department: JobDomain::Engineering,
            subject,
            location: prepared.location,
            severity,
        });

        // A bad enough fault hurts whoever was working it.
        //
        // Raised as a *separate* `Burn` incident rather than folding damage
        // into the `EquipmentFailure` above, because the two need different
        // responses and resolve independently: the asset needs a technician
        // with repair capability, the person needs Medical and a bed. Merging
        // them would mean repairing the pump discharged the patient.
        if severity.get() < INJURING_FAULT_SEVERITY {
            continue;
        }
        let Ok((mut body, blood)) = bodies.get_mut(subject) else {
            continue;
        };
        let Ok(injury) = incidents.create(
            IncidentKind::Burn,
            JobDomain::Engineering,
            subject,
            None,
            prepared.location,
            severity,
            time.elapsed_secs(),
        ) else {
            // The ledger is full or already tracking this body. The fault still
            // stands — an asset breaking does not depend on Medical having room.
            continue;
        };
        let previously_collapsed = body.0.collapsed;
        body.0.apply(chem_sim::Damage::of(
            chem_sim::DamageKind::Burn,
            engineering_fault_burn(severity),
        ));
        if let Some(blood) = blood {
            blood
                .0
                .reconcile_collapse(&mut body.0, previously_collapsed);
        }
        // A hurt person is a louder, different event from a broken machine, and
        // it is the one that brings Medical rather than a technician.
        witnessed.write(
            super::Stimulus::new(super::StimulusKind::Casualty, prepared.location).about(subject),
        );
        created.write(IncidentCreated {
            id: injury,
            kind: IncidentKind::Burn,
            department: JobDomain::Engineering,
            subject,
            location: prepared.location,
            severity,
        });
    }
}

fn engineering_fault_severity_from_snapshot(snapshot: ProblemStabilitySnapshot) -> Normalized {
    engineering_fault_severity_from_health(snapshot.health())
}

fn engineering_fault_severity_from_health(health: f32) -> Normalized {
    let deterioration = 1.0 - health.clamp(0.0, 1.0);
    Normalized::new(0.25 + deterioration * 0.40)
        .expect("the clamped Engineering fault severity is normalized")
}

/// Above this fault severity the technician working the asset is hurt by it.
///
/// A gate rather than a scaled chance: most faults are a breaker tripping or a
/// pump seizing, which is a *repair job*, not an injury. Only a bad one puts
/// the person holding the panel in Medical. Keeping it a threshold on the same
/// severity the fault already has means a deteriorating station produces more
/// casualties without a second random roll deciding it.
const INJURING_FAULT_SEVERITY: f32 = 0.45;

/// How much burn damage an injuring fault does, scaled by its own severity.
///
/// Burn rather than brute: these are powered assets — a breaker panel, a pump
/// motor — so an arc flash is the honest injury, and it routes into the exact
/// standard burn care Medical already implements for the Cargo reference case
/// rather than needing a new treatment branch.
fn engineering_fault_burn(severity: Normalized) -> chem_sim::Units {
    // At the threshold this is a real but survivable injury; at full severity
    // it is enough to collapse an unlucky technician. Whole units, matching
    // `cargo_burn_damage_from_health`, so the two departments' injuries are
    // comparable rather than each inventing their own scale.
    chem_sim::Units::whole(12 + (severity.get() * 18.0).round() as i32)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::body::{Bloodstream, Body};
    use crate::crew::{CrewPosts, CrewRoute, ErrandResolved};
    use crate::utility_ai::{
        begin_reference_actions, consume_utility_arrivals, perform_reference_actions,
        resolve_reference_actions, select_reference_actions, tick_current_actions, ActionKey,
        LocomotionOwner, NpcActivity, ReservationBook, ReservationOwner, UtilityControlBundle,
        UtilityDecisionLog,
    };

    #[derive(Resource, Default)]
    struct CompletionLog(Vec<EngineeringJobCompleted>);

    fn record_completions(
        mut messages: MessageReader<EngineeringJobCompleted>,
        mut log: ResMut<CompletionLog>,
    ) {
        log.0.extend(messages.read().cloned());
    }

    fn insert_test_spots(app: &mut App) {
        for (index, id) in ENGINEERING_SPOTS.into_iter().enumerate() {
            assert!(app.world_mut().resource_mut::<UtilitySpots>().insert(
                id,
                Vec3::new(-4.0 + index as f32 * 2.0, 0.0, 0.0),
                1,
            ));
        }
    }

    #[test]
    fn authored_engineering_cast_has_distinct_tiers_and_qualifications() {
        ENGINEERING_ROSTER.validate().unwrap();
        let lindqvist = engineering_profile("Tech Lindqvist").unwrap();
        let morrow = engineering_profile(ENGINEERING_SECOND_CORE).unwrap();
        let torres = engineering_profile(ENGINEERING_SUPPORT[0]).unwrap();
        let adeyemi = engineering_profile(ENGINEERING_SUPPORT[1]).unwrap();

        assert_eq!(lindqvist.narrative_tier, NarrativeTier::Core);
        assert_eq!(morrow.narrative_tier, NarrativeTier::Core);
        assert_eq!(torres.narrative_tier, NarrativeTier::Support);
        assert_eq!(adeyemi.narrative_tier, NarrativeTier::Support);
        assert!(lindqvist.can_do(&JobCapability::new(INSPECT_CAPABILITY)));
        assert!(lindqvist.can_do(&JobCapability::new(REPAIR_CAPABILITY)));
        assert!(morrow.can_do(&JobCapability::new(POWER_CAPABILITY)));
        assert!(!morrow.can_do(&JobCapability::new(REPAIR_CAPABILITY)));
        assert!(torres.can_do(&JobCapability::new(MAINTAIN_CAPABILITY)));
        assert!(torres.can_do(&JobCapability::new(REPAIR_CAPABILITY)));
        assert!(adeyemi.can_do(&JobCapability::new(COOLANT_CAPABILITY)));
        assert_eq!(
            [&lindqvist, &morrow, &torres, &adeyemi]
                .into_iter()
                .map(|profile| profile.capabilities.clone())
                .collect::<HashSet<_>>()
                .len(),
            4
        );
    }

    #[test]
    fn engineering_support_stays_off_the_random_chemistry_customer_roster() {
        let roster: Vec<crate::crew::CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        for name in ENGINEERING_SUPPORT {
            assert!(
                !roster.iter().any(|member| member.name == name),
                "{name} must remain off the Chemistry customer roster",
            );
        }
    }

    #[test]
    fn only_the_four_authored_engineering_workers_migrate() {
        let mut app = App::new();
        app.add_systems(
            Update,
            (activate_engineering_workers, ApplyDeferred).chain(),
        );
        for (name, role) in [
            ("Tech Lindqvist", "Engineering"),
            (ENGINEERING_SECOND_CORE, "Engineering"),
            (ENGINEERING_SUPPORT[0], "Engineering"),
            (ENGINEERING_SUPPORT[1], "Engineering"),
            ("Miner Sato", "Cargo"),
        ] {
            app.world_mut().spawn((
                CrewMember {
                    name: name.into(),
                    role: role.into(),
                },
                Ambient::new(5.0),
                CrewRoute::standing(),
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
        assert!(!names.contains("Miner Sato"));
        assert!(ENGINEERING_CORE
            .into_iter()
            .all(|name| names.contains(name)));
        assert!(ENGINEERING_SUPPORT
            .into_iter()
            .all(|name| names.contains(name)));
    }

    #[test]
    fn ticket_generation_is_bounded_and_reserves_each_exact_asset_target() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<EngineeringWorkState>()
            .init_resource::<UtilitySpots>()
            .init_resource::<JobBoard>()
            .add_systems(Update, publish_engineering_tickets);
        insert_test_spots(&mut app);

        for _ in 0..20 {
            app.update();
        }

        let state = app.world().resource::<EngineeringWorkState>();
        let board = app.world().resource::<JobBoard>();
        let spots = app.world().resource::<UtilitySpots>();
        assert_eq!(board.len(), state.assets.len());
        let ids: HashSet<_> = board.iter().map(|ticket| ticket.id).collect();
        assert_eq!(ids.len(), state.assets.len());
        for asset in &state.assets {
            let ticket = board.ticket(asset_ticket_id(&asset.id)).unwrap();
            let spot = spots.get(asset.spot).unwrap();
            assert_eq!(ticket.domain, JobDomain::Engineering);
            assert_eq!(ticket.target, ActionTarget::Point(spot.at));
            assert_eq!(
                ticket.reservation,
                ReservationKey(format!("utility.spot.{}", asset.spot))
            );
            assert_eq!(ticket.reservation_capacity, spot.capacity);
        }
    }

    #[test]
    fn exact_completion_changes_only_its_named_engineering_fact() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<EngineeringWorkState>()
            .init_resource::<UtilitySpots>()
            .init_resource::<JobBoard>()
            .init_resource::<IncidentLedger>()
            .add_message::<UtilityActionResolved>()
            .add_message::<EngineeringJobCompleted>()
            .add_message::<IncidentResolved>()
            .add_systems(
                Update,
                (publish_engineering_tickets, apply_engineering_job_results).chain(),
            );
        insert_test_spots(&mut app);
        app.update();

        let target_id = "engineering.asset.generator";
        let ticket_id = asset_ticket_id(target_id);
        let correct = ReservationOwner {
            agent: Entity::from_raw_u32(7).unwrap(),
            action_instance: 11,
        };
        let stale = ReservationOwner {
            agent: correct.agent,
            action_instance: 10,
        };
        app.world_mut()
            .resource_mut::<JobBoard>()
            .claim(ticket_id, correct)
            .unwrap();
        app.world_mut()
            .resource_mut::<JobBoard>()
            .resolve(ticket_id, correct, ActionResult::Completed)
            .unwrap();
        let before = app.world().resource::<EngineeringWorkState>().clone();

        app.world_mut().write_message(UtilityActionResolved {
            agent: correct.agent,
            key: ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: ticket_id.0,
            },
            claim: stale,
            result: ActionResult::Completed,
        });
        app.update();
        assert_eq!(*app.world().resource::<EngineeringWorkState>(), before);
        assert_eq!(
            app.world()
                .resource::<JobBoard>()
                .ticket(ticket_id)
                .unwrap()
                .state,
            JobTicketState::Completed(correct)
        );

        app.world_mut().write_message(UtilityActionResolved {
            agent: correct.agent,
            key: ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: ticket_id.0,
            },
            claim: correct,
            result: ActionResult::Completed,
        });
        app.update();

        let after = app.world().resource::<EngineeringWorkState>();
        assert_eq!(
            after.asset(target_id).unwrap().state,
            EngineeringAssetState::Operational
        );
        for untouched in before.assets.iter().filter(|asset| asset.id != target_id) {
            assert_eq!(after.asset(&untouched.id), Some(untouched));
        }
        assert_eq!(after.completed_jobs, 1);
        assert!(app
            .world()
            .resource::<JobBoard>()
            .ticket(ticket_id)
            .is_none());
    }

    #[test]
    fn four_workers_complete_the_bounded_shift_without_claim_collisions() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<IncidentLedger>()
            .init_resource::<EngineeringWorkState>()
            .init_resource::<CompletionLog>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_message::<EngineeringJobCompleted>()
            .add_message::<IncidentResolved>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    publish_engineering_tickets,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    apply_engineering_job_results,
                    ApplyDeferred,
                    record_completions,
                )
                    .chain(),
            );
        insert_test_spots(&mut app);

        let mut worker_entities = Vec::new();
        for (index, name) in ENGINEERING_CORE
            .into_iter()
            .chain(ENGINEERING_SUPPORT)
            .enumerate()
        {
            worker_entities.push(
                app.world_mut()
                    .spawn((
                        CrewMember {
                            name: name.into(),
                            role: "Engineering".into(),
                        },
                        Transform::from_xyz(
                            -3.0 + index as f32 * 2.0,
                            crate::crew::BODY_OFFSET,
                            -2.0,
                        ),
                        Body::default(),
                        Bloodstream::default(),
                        CrewRoute::standing(),
                        UtilityControlBundle::new(UtilityAgent::new(800 + index as u64, 0)),
                        engineering_profile(name).unwrap(),
                    ))
                    .id(),
            );
        }

        let asset_count = app.world().resource::<EngineeringWorkState>().assets.len();
        let mut largest_board = 0;
        for _ in 0..3_000 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            largest_board = largest_board.max(app.world().resource::<JobBoard>().len());
            if app
                .world()
                .resource::<EngineeringWorkState>()
                .assets
                .iter()
                .all(|asset| asset.state == EngineeringAssetState::Operational)
                && app.world().resource::<JobBoard>().is_empty()
            {
                break;
            }
        }

        let state = app.world().resource::<EngineeringWorkState>();
        assert!(state
            .assets
            .iter()
            .all(|asset| asset.state == EngineeringAssetState::Operational));
        assert_eq!(state.completed_jobs, asset_count as u32);
        assert!(largest_board <= asset_count);
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
            "Engineering did not distribute one real fact to every worker: {completions:?}"
        );
        let world = app.world_mut();
        let mut workers_query = world.query::<(
            &ControlOwner,
            &LocomotionOwner,
            &NpcActivity,
            &NpcJobProfile,
        )>();
        assert!(workers_query
            .iter(world)
            .all(|(owner, locomotion, _, profile)| {
                *owner == ControlOwner::UtilityAction
                    && *locomotion == LocomotionOwner::None
                    && profile.primary == JobDomain::Engineering
            }));
        assert_eq!(worker_entities.len(), 4);
    }

    /// Builds a fault-only app whose technician has a real body.
    ///
    /// `health` drives severity through the same path the game uses, so the
    /// test tunes the *station*, not the injury — a deteriorating station
    /// hurting more people is the behaviour, and a test that set severity
    /// directly would not prove it.
    fn injury_app(health: f32) -> (App, Entity) {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DepartmentProblemDirector>()
            .init_resource::<IncidentLedger>()
            .init_resource::<EngineeringWorkState>()
            .init_resource::<UtilitySpots>()
            .init_resource::<JobBoard>()
            .insert_resource(crate::instability::StationStability {
                value: health * crate::instability::STABILITY_MAX,
                ..Default::default()
            })
            .add_message::<EngineeringJobCompleted>()
            .add_message::<UtilityActionResolved>()
            .add_message::<IncidentCreated>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_message::<IncidentResolved>()
            .add_systems(Update, apply_engineering_workplace_risk);
        insert_test_spots(&mut app);
        let power_asset = "engineering.asset.power";
        {
            let mut state = app.world_mut().resource_mut::<EngineeringWorkState>();
            state.assets.retain(|asset| asset.id == power_asset);
            state.assets[0].state = EngineeringAssetState::Operational;
        }
        let worker = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: ENGINEERING_SECOND_CORE.into(),
                    role: "Engineering".into(),
                },
                Body::default(),
                Bloodstream::default(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<DepartmentProblemDirector>()
            .force_next_engineering_fault();
        app.world_mut().write_message(EngineeringJobCompleted {
            worker,
            asset_id: power_asset.into(),
            kind: EngineeringJobKind::CheckPower.id().into(),
            risk: EngineeringJobKind::CheckPower.risk(),
        });
        app.update();
        (app, worker)
    }

    /// A severe fault burns the technician working it; a mild one does not.
    ///
    /// Both halves matter. Without the first, Engineering is the only staffed
    /// department whose accidents never produce a patient. Without the second,
    /// every routine breaker trip fills Medical, and the repair job — which is
    /// the ordinary case — stops reading as ordinary.
    #[test]
    fn a_severe_engineering_fault_burns_its_technician_and_a_mild_one_does_not() {
        // A healthy station: severity sits below the injury threshold.
        let (app, worker) = injury_app(1.0);
        let burn = app.world().get::<Body>(worker).unwrap().0.damage.burn;
        assert_eq!(
            burn,
            chem_sim::Units::ZERO,
            "a routine fault on a healthy station must not hurt anyone"
        );
        assert!(
            !app.world()
                .resource::<IncidentLedger>()
                .active_for(worker)
                .any(|incident| incident.kind == IncidentKind::Burn),
            "no casualty case should exist for a mild fault"
        );

        // A deteriorating station pushes the same fault past the threshold.
        let (app, worker) = injury_app(0.0);
        let burn = app.world().get::<Body>(worker).unwrap().0.damage.burn;
        assert!(
            burn > chem_sim::Units::ZERO,
            "a severe fault must actually injure the technician holding the panel"
        );

        // The injury is its own incident, separate from the equipment failure:
        // repairing the pump must not discharge the patient.
        let ledger = app.world().resource::<IncidentLedger>();
        let kinds: Vec<_> = ledger
            .active_for(worker)
            .map(|incident| incident.kind)
            .collect();
        assert!(
            kinds.contains(&IncidentKind::Burn),
            "the hurt technician needs a Medical case of their own: {kinds:?}"
        );
        assert!(
            kinds.contains(&IncidentKind::EquipmentFailure),
            "the broken asset still needs a repair: {kinds:?}"
        );
    }

    /// A hurt technician is a *casualty*, which is what brings Medical rather
    /// than another engineer.
    #[test]
    fn an_injuring_fault_emits_a_casualty_stimulus_about_the_hurt_worker() {
        let (mut app, worker) = injury_app(0.0);
        let casualties: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<crate::utility_ai::Stimulus>>()
            .drain()
            .filter(|stimulus| stimulus.kind == crate::utility_ai::StimulusKind::Casualty)
            .collect();
        assert_eq!(casualties.len(), 1, "exactly one body went down");
        assert_eq!(
            casualties[0].subject,
            Some(worker),
            "the casualty must name the technician, not the asset"
        );
    }

    #[test]
    fn forced_power_fault_targets_the_exact_asset_and_repair_resolves_it() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<crate::instability::StationStability>()
            .init_resource::<DepartmentProblemDirector>()
            .init_resource::<IncidentLedger>()
            .init_resource::<EngineeringWorkState>()
            .init_resource::<UtilitySpots>()
            .init_resource::<JobBoard>()
            .add_message::<EngineeringJobCompleted>()
            .add_message::<UtilityActionResolved>()
            .add_message::<IncidentCreated>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_message::<IncidentResolved>()
            .add_systems(
                Update,
                (
                    publish_engineering_tickets,
                    apply_engineering_job_results,
                    apply_engineering_workplace_risk,
                )
                    .chain(),
            );
        insert_test_spots(&mut app);
        let power_asset = "engineering.asset.power";
        {
            let mut state = app.world_mut().resource_mut::<EngineeringWorkState>();
            state.assets.retain(|asset| asset.id == power_asset);
            state.assets[0].state = EngineeringAssetState::Operational;
        }
        let worker = app
            .world_mut()
            .spawn(CrewMember {
                name: ENGINEERING_SECOND_CORE.into(),
                role: "Engineering".into(),
            })
            .id();
        let bystander = app
            .world_mut()
            .spawn(CrewMember {
                name: "Tech Lindqvist".into(),
                role: "Engineering".into(),
            })
            .id();
        app.world_mut()
            .resource_mut::<DepartmentProblemDirector>()
            .force_next_engineering_fault();
        app.world_mut().write_message(EngineeringJobCompleted {
            worker,
            asset_id: power_asset.into(),
            kind: EngineeringJobKind::CheckPower.id().into(),
            risk: EngineeringJobKind::CheckPower.risk(),
        });

        app.update();

        let (incident_id, severity) = match app
            .world()
            .resource::<EngineeringWorkState>()
            .asset(power_asset)
            .unwrap()
            .state
        {
            EngineeringAssetState::Faulted { incident, severity } => (incident, severity),
            other => panic!("forced fault did not change the exact power fact: {other:?}"),
        };
        let incident = app
            .world()
            .resource::<IncidentLedger>()
            .get(incident_id)
            .unwrap();
        assert_eq!(incident.kind, IncidentKind::EquipmentFailure);
        assert_eq!(incident.department, JobDomain::Engineering);
        assert_eq!(incident.subject, worker);
        assert_ne!(incident.subject, bystander);
        assert_eq!(
            incident.location,
            app.world()
                .resource::<UtilitySpots>()
                .get(POWER_MONITOR_SPOT)
                .unwrap()
                .at
        );
        assert_eq!(incident.severity, severity);
        let fault_location = incident.location;

        // A fault must be *perceivable*, not merely recorded. Without this the
        // hazard would exist in the ledger while no crew member ever noticed
        // it, and Engineering's stimulus emission could be deleted silently.
        let hazards: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<crate::utility_ai::Stimulus>>()
            .drain()
            .filter(|stimulus| stimulus.kind == crate::utility_ai::StimulusKind::Hazard)
            .collect();
        assert_eq!(
            hazards.len(),
            1,
            "a committed fault must emit exactly one Hazard stimulus"
        );
        assert_eq!(
            hazards[0].at, fault_location,
            "the hazard is perceived where the fault happened"
        );

        app.update();
        let ticket_id = asset_ticket_id(power_asset);
        let owner = ReservationOwner {
            agent: worker,
            action_instance: 44,
        };
        app.world_mut()
            .resource_mut::<JobBoard>()
            .claim(ticket_id, owner)
            .unwrap();
        app.world_mut()
            .resource_mut::<JobBoard>()
            .resolve(ticket_id, owner, ActionResult::Completed)
            .unwrap();
        app.world_mut().write_message(UtilityActionResolved {
            agent: worker,
            key: ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: ticket_id.0,
            },
            claim: owner,
            result: ActionResult::Completed,
        });
        app.update();

        let state = app.world().resource::<EngineeringWorkState>();
        assert_eq!(
            state.asset(power_asset).unwrap().state,
            EngineeringAssetState::Operational
        );
        assert_eq!(state.resolved_faults, 1);
        assert_eq!(
            app.world()
                .resource::<IncidentLedger>()
                .get(incident_id)
                .unwrap()
                .status,
            IncidentStatus::Resolved
        );
        assert!(app
            .world()
            .resource::<JobBoard>()
            .ticket(ticket_id)
            .is_none());
    }

    #[test]
    fn poor_stability_increases_fault_frequency_and_severity_within_bounds() {
        let base = EngineeringJobKind::CheckCoolant.risk();
        let stable = crate::instability::StationStability::default();
        let critical = crate::instability::StationStability {
            value: 0.0,
            band: crate::instability::StabilityBand::Critical,
            ..default()
        };
        let stable_pressure = super::super::WorkplaceRiskPressure::from_stability(base, &stable);
        let critical_pressure =
            super::super::WorkplaceRiskPressure::from_stability(base, &critical);

        assert!(critical_pressure.chance.get() > stable_pressure.chance.get());
        assert!(critical_pressure.grace_cost > stable_pressure.grace_cost);
        assert_eq!(engineering_fault_severity_from_health(1.0).get(), 0.25);
        assert_eq!(engineering_fault_severity_from_health(0.0).get(), 0.65);
        assert!(engineering_fault_severity_from_health(-1.0).get() <= 1.0);
        assert!(engineering_fault_severity_from_health(2.0).get() >= 0.0);
    }

    #[test]
    fn engineering_registration_shares_the_ordered_crew_schedule() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::state::app::StatesPlugin,
            bevy_replicon::prelude::RepliconSharedPlugin::default(),
        ))
        .init_state::<crate::AppState>()
        .add_plugins((crate::crew::CrewPlugin, crate::utility_ai::UtilityAiPlugin));

        app.update();
        assert!(app.world().contains_resource::<EngineeringWorkState>());
    }
}
