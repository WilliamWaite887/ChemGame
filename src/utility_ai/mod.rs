//! Shared utility selection and action-lifecycle contracts for station NPCs.
//!
//! This module deliberately starts as an inert kernel. A resident joins it only
//! when an authority-side migration system inserts [`UtilityAgent`]. Until the
//! Cargo pilot does that, existing residents keep their legacy behavior. The
//! legacy ambient query explicitly excludes this marker, which is the first
//! enforceable half of the one-controller migration rule.

use std::collections::{HashMap, VecDeque};

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::body::{Bloodstream, Body};
use crate::crew::{
    send_on_errand_with_reach, CrewMember, CrewPosts, CrewRoute, Errand, ErrandGoal, ErrandOutcome,
    ErrandResolved,
};
use crate::nav::NavGraph;

mod aid;
mod botany;
mod bridge;
mod capacity;
mod cargo_pilot;
mod covert;
mod deals;
mod decision_log;
mod department;
mod engineering;
mod incidents;
mod interviews;
#[cfg(any(debug_assertions, test))]
mod invariants;
mod jobs;
mod medical;
mod perception;
pub(crate) mod public;
pub(crate) mod reports;
mod security;
mod service;
mod social;
mod tuning;

pub use aid::{
    AidBatch, AidBatchId, AidError, AidIntakes, AidProvenance, AidRecord, AidRefusal, AidState,
    PublicAidStatus,
};
pub use botany::{
    BotanyJobCompleted, BotanyPlot, BotanyPlotState, BotanyProduceProvenance, BotanyWorkState,
    BOTANY_SECOND_CORE,
};
pub use capacity::{work_capacity, work_risk};
pub use cargo_pilot::{CargoJobCompleted, CargoWorkState, ForcedWorkplaceOutcome};
pub use covert::{
    CovertContaminant, CovertGoal, CustodyRecord, CustodyState, IllicitCustody, IllicitStock,
    PrivateGoal, TamperedMeals,
};
pub use deals::{
    grant as grant_deal, recover as recover_batch, AnswerApproachRequested, DealOutcome,
    DealResponse, DeceptionMethod, FavorKind, IllicitRequest, LiveApproach, LiveApproaches,
    PlayerBenefit, PublicApproach, RequestError,
};
pub use engineering::{
    EngineeringAsset, EngineeringAssetState, EngineeringJobCompleted, EngineeringWorkState,
    ENGINEERING_SECOND_CORE,
};
pub use incidents::{
    DepartmentProblemCandidate, DepartmentProblemDirector, DepartmentProblemPermit,
    DepartmentProblemPolicy, IncidentCreated, IncidentError, IncidentId, IncidentKind,
    IncidentLedger, IncidentRecord, IncidentResolved, IncidentStatus, ProblemPermitDecision,
    ProblemStabilitySnapshot, ProblemSuppression, WorkplaceRiskPressure,
};
pub use jobs::{
    JobBoard, JobCapability, JobDomain, JobTicket, JobTicketId, JobTicketState, NarrativeTier,
    NpcJobProfile, UtilitySpotDef, UtilitySpots,
};
pub use medical::{MedicalCase, MedicalCaseId, MedicalCaseLedger, MedicalCaseStatus};
pub use perception::{
    can_see, MemoryFact, Modality, NpcMemory, Stimulus, StimulusKind, EARSHOT, EYE_HEIGHT,
    SIGHT_RANGE,
};
pub use public::{
    ConditionReason, DepartmentCondition, DepartmentWorkState, PublicActivity, PublicCrewStatus,
    PublicDepartmentStatus,
};
pub use service::{
    consume_meal_serving, MealBatch, MealChemistry, MealConsumption, MealConsumptionError,
    MealIngredientProvenance, MealStage, ServiceJobCompleted, ServiceRecipe, ServiceWorkState,
    SERVICE_SECOND_CORE,
};
pub use social::{RoomAppeal, RoomAppealReason, SocialCooldowns};
pub use tuning::{
    ActionTuning, AppealWeights, BreakThresholds, NeedRates, NeedsTuning, StartingNeeds,
    TuningError,
};

/// Registers the utility kernel, its ordered schedule, and its one public
/// presentation component.
pub struct UtilityAiPlugin;

impl Plugin for UtilityAiPlugin {
    fn build(&self, app: &mut App) {
        // Authored tuning first: `NeedsTuning::authored` validates the file and
        // panics on a bad one, so a broken station fails to start here rather
        // than behaving strangely later.
        app.insert_resource(NeedsTuning::authored().clone())
            // Selection routes the communal duty fallback through the nav
            // graph, so this plugin now depends on one existing. `NavPlugin`
            // owns it in a real session and rebuilds it from the floor plan;
            // this is only the floor, for a schedule that is built without it.
            // Idempotent — `init_resource` leaves an already-built graph alone.
            .init_resource::<NavGraph>()
            // Same reasoning: selection anchors the duty fallback on the
            // resident's own department point, so this plugin depends on the
            // map having been read. `CrewPlugin` fills it in a real session.
            .init_resource::<crate::crew::Departments>()
            .init_resource::<ReservationBook>()
            .init_resource::<jobs::StandingPostCooldowns>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<IncidentLedger>()
            .init_resource::<DepartmentProblemDirector>()
            .init_resource::<UtilityOpportunityBuffer>()
            .add_message::<UtilityActionResolved>()
            .add_message::<IncidentCreated>()
            .add_message::<IncidentResolved>()
            .replicate::<NpcActivity>()
            .replicate::<NpcPosture>()
            .configure_sets(
                Update,
                (
                    UtilityAiSet::Observe,
                    UtilityAiSet::MaintainNeeds,
                    UtilityAiSet::BuildContext,
                    UtilityAiSet::Score,
                    UtilityAiSet::Select,
                    UtilityAiSet::BeginAction,
                    UtilityAiSet::Navigate,
                    UtilityAiSet::Attach,
                    UtilityAiSet::Perform,
                    UtilityAiSet::Resolve,
                    UtilityAiSet::Publish,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    prune_orphaned_utility_claims,
                    repair_owner_mismatched_actions,
                    sync_utility_incapacity,
                    tick_current_actions,
                )
                    .chain()
                    .in_set(UtilityAiSet::Observe)
                    .run_if(crate::net::is_authority),
            )
            .add_systems(
                Update,
                advance_npc_needs
                    .in_set(UtilityAiSet::MaintainNeeds)
                    .run_if(crate::net::is_authority),
            )
            // Ordered ahead of every provider in the same set, so this frame's
            // offers are never mixed with last frame's.
            .add_systems(
                Update,
                clear_opportunity_buffer
                    .in_set(UtilityAiSet::BuildContext)
                    .before(OpportunityProviders)
                    .run_if(crate::net::is_authority),
            )
            .configure_sets(
                Update,
                OpportunityProviders.in_set(UtilityAiSet::BuildContext),
            )
            .add_systems(
                Update,
                (
                    preempt_for_emergency_jobs,
                    ApplyDeferred,
                    select_reference_actions,
                    ApplyDeferred,
                )
                    .chain()
                    .in_set(UtilityAiSet::Select)
                    .run_if(crate::net::is_authority),
            )
            .add_systems(
                Update,
                begin_reference_actions
                    .in_set(UtilityAiSet::BeginAction)
                    .run_if(crate::net::is_authority),
            )
            .add_systems(
                Update,
                consume_utility_arrivals
                    .after(crate::crew::run_errands)
                    .in_set(UtilityAiSet::Navigate)
                    .run_if(crate::net::is_authority),
            )
            .add_systems(
                Update,
                perform_reference_actions
                    .in_set(UtilityAiSet::Perform)
                    .run_if(crate::net::is_authority),
            )
            .add_systems(
                Update,
                (release_interrupted_actions, resolve_reference_actions)
                    .chain()
                    .in_set(UtilityAiSet::Resolve)
                    .run_if(crate::net::is_authority),
            );
        botany::register(app);
        cargo_pilot::register(app);
        engineering::register(app);
        medical::register(app);
        service::register(app);
        perception::register(app);
        interviews::register(app);
        reports::register(app);
        bridge::register(app);
        covert::register(app);
        deals::register(app);
        security::register(app);
        social::register(app);
        public::register(app);
        decision_log::register(app);
        aid::register(app);
        #[cfg(any(debug_assertions, test))]
        invariants::register(app);
    }
}

fn prune_orphaned_utility_claims(
    living: Query<()>,
    mut reservations: ResMut<ReservationBook>,
    mut jobs: ResMut<JobBoard>,
) {
    reservations.prune_agents(|entity| living.get(entity).is_ok());
    jobs.reopen_orphaned_claims(|entity| living.get(entity).is_ok());
}

/// Repairs stale or raced action state without stealing locomotion from the
/// controller that now owns the actor. Claim cleanup remains centralized in
/// `release_interrupted_actions` and uses the exact action instance.
fn repair_owner_mismatched_actions(
    mut commands: Commands,
    agents: Query<
        (Entity, &ControlOwner, &CurrentAction),
        (With<UtilityAgent>, Without<PendingUtilityInterruption>),
    >,
) {
    for (entity, owner, action) in &agents {
        if *owner == ControlOwner::UtilityAction {
            continue;
        }
        commands
            .entity(entity)
            .insert(PendingUtilityInterruption {
                claim: ReservationOwner {
                    agent: entity,
                    action_instance: action.instance,
                },
                key: action.key,
            })
            .remove::<CurrentAction>()
            .remove::<Errand>();
    }
}

/// The only legal high-level order for utility systems.
///
/// Department modules add systems to these sets instead of inventing their own
/// update chains. Selection never moves an actor and resolution owns world
/// consequences.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UtilityAiSet {
    Observe,
    MaintainNeeds,
    BuildContext,
    Score,
    Select,
    BeginAction,
    Navigate,
    Attach,
    Perform,
    Resolve,
    Publish,
}

/// Every `OnEnter(Playing)` system that clears authority-only utility state
/// back to a fresh-shift baseline.
///
/// These exist so a new game does not inherit the previous one's tickets,
/// custody, or intakes. Anything that *restores* saved state — `shift`'s
/// `load_progress`, above all — must run strictly after them, or the reset
/// wipes the very thing that was just loaded.
///
/// Today plugin registration order happens to put these first. That is not a
/// guarantee: reordering `main.rs` would silently start discarding saved
/// custody and donations with no error anywhere. `ProgressPlugin` therefore
/// declares `.after(UtilityResetSet)` explicitly, and
/// `shift::tests::restored_state_survives_the_utility_reset` fails if the
/// ordering is ever dropped.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UtilityResetSet;

/// Every system that publishes into the [`UtilityOpportunityBuffer`].
///
/// Providers join this set so the buffer's per-frame clear is guaranteed to run
/// before all of them. Providers are unordered with respect to one another:
/// each publishes an independent offer and the selector scores them together,
/// so no provider may depend on seeing another's entries.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpportunityProviders;

/// Opts one existing station resident into utility decisions.
///
/// The marker is authority-only. `decision_serial` is incremented after every
/// selection and combines with `seed` to make near-best variety repeatable in
/// tests and saves without relying on the process-wide RNG.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtilityAgent {
    pub seed: u64,
    pub decision_serial: u64,
    pub phase_offset_millis: u16,
}

impl UtilityAgent {
    pub fn new(seed: u64, phase_offset_millis: u16) -> Self {
        Self {
            seed,
            decision_serial: 0,
            phase_offset_millis,
        }
    }

    pub fn next_entropy(&mut self) -> u64 {
        let entropy = mix64(self.seed ^ self.decision_serial);
        self.decision_serial = self.decision_serial.wrapping_add(1);
        entropy
    }
}

/// The components every migrated resident receives together. Keeping this a
/// bundle makes activation reviewable and prevents callers from forgetting the
/// explicit controller or locomotion owner.
#[derive(Bundle)]
pub struct UtilityControlBundle {
    pub agent: UtilityAgent,
    pub control: ControlOwner,
    pub locomotion: LocomotionOwner,
    pub activity: NpcActivity,
    pub posture: NpcPosture,
    pub needs: NpcNeeds,
    pub memory: perception::NpcMemory,
}

impl UtilityControlBundle {
    pub fn new(agent: UtilityAgent) -> Self {
        Self {
            agent,
            control: ControlOwner::UtilityAction,
            locomotion: LocomotionOwner::None,
            activity: NpcActivity::Idle,
            posture: NpcPosture::Standing,
            needs: NpcNeeds::default(),
            memory: perception::NpcMemory::default(),
        }
    }
}

/// Public, replicated presentation. It intentionally carries no scores,
/// knowledge, allegiance, or private target identity.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NpcActivity {
    Traveling,
    Working,
    Helping,
    Eating,
    Socializing,
    Resting,
    Treating,
    Down,
    #[default]
    Idle,
}

/// Public presentation posture kept separate from physiological collapse.
/// Medical can therefore place a conscious patient on a bed without lying to
/// body simulation or reusing `Body.collapsed` as an animation switch.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NpcPosture {
    #[default]
    Standing,
    Sitting,
    Lying,
}

/// Slowly changing personal pressure, authority-only and normalized.
///
/// These are *pressures to act*, not physiology. Damage, toxins, sedation, and
/// nutrition already live in `Body`/`Bloodstream` and are never duplicated
/// here: eating relieves `hunger` and separately puts a real dose into the
/// bloodstream through the ordinary ingestion path.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct NpcNeeds {
    pub hunger: f32,
    pub fatigue: f32,
    pub social: f32,
}

impl Default for NpcNeeds {
    /// The authored opening values.
    ///
    /// Reads `station.needs.ron` rather than repeating the numbers, so there is
    /// exactly one place a resident's starting state is decided. A second copy
    /// here would be a silent fallback that could drift from the file and mask
    /// a bad edit — the same reason `NeedsTuning::default` defers to the file.
    /// `validate` guarantees each of these sits below its own break threshold,
    /// so a shift cannot open with everyone already walking off the job.
    fn default() -> Self {
        Self::from_starting(NeedsTuning::authored().starting)
    }
}

impl NpcNeeds {
    /// Advances every pressure by the authored per-second rates.
    ///
    /// Rates arrive as an argument rather than being read from a resource here
    /// so this stays a pure function that tests can drive directly. They are
    /// authored in `assets/data/station.needs.ron` — see [`NeedRates`].
    pub fn advance(&mut self, seconds: f32, rates: NeedRates) {
        self.hunger = (self.hunger + rates.hunger_per_second * seconds).clamp(0.0, 1.0);
        self.fatigue = (self.fatigue + rates.fatigue_per_second * seconds).clamp(0.0, 1.0);
        self.social = (self.social + rates.social_per_second * seconds).clamp(0.0, 1.0);
    }

    /// The authored opening values for a shift.
    pub fn from_starting(starting: StartingNeeds) -> Self {
        Self {
            hunger: starting.hunger,
            fatigue: starting.fatigue,
            social: starting.social,
        }
    }

    pub fn relieve_hunger(&mut self, amount: f32) {
        self.hunger = (self.hunger - amount).clamp(0.0, 1.0);
    }

    pub fn relieve_fatigue(&mut self, amount: f32) {
        self.fatigue = (self.fatigue - amount).clamp(0.0, 1.0);
    }

    pub fn relieve_social(&mut self, amount: f32) {
        self.social = (self.social - amount).clamp(0.0, 1.0);
    }
}

/// How willing a body currently is to linger socially, on 0..1.
///
/// This is the one place chemistry statuses are turned into a social
/// disposition, so `EatFood`, `Rest`, and `Socialize` cannot each invent their
/// own rule for what "cheerful" or "withdrawn" means. It reads only existing
/// `Bloodstream` statuses — there is no second mood simulation.
///
/// `1.0` is neutral. Above it a resident lingers, below it they withdraw.
pub fn social_disposition(blood: &crate::body::Bloodstream) -> f32 {
    use chem_sim::StatusKind;
    let intensity = |kind: StatusKind| blood.0.status(kind).intensity.max(0.0);

    // Sustained good mood and euphoria make crew linger; the authored effect
    // docs call for exactly this rather than treating euphoria as drunkenness.
    let lift = (intensity(StatusKind::Happiness) * 0.30 + intensity(StatusKind::Euphoric) * 0.20)
        .min(0.75);
    // Withdrawal. Paranoia is a flight response, so it outweighs plain sadness.
    let withdraw = (intensity(StatusKind::Sadness) * 0.30
        + intensity(StatusKind::Paranoid) * 0.45
        + intensity(StatusKind::Hallucinating) * 0.20)
        .min(0.9);

    (1.0 + lift - withdraw).clamp(0.1, 1.75)
}

/// Whether a body is chemically capable of choosing a voluntary personal
/// action at all.
///
/// Sedation and heavy impairment should suppress leisure rather than merely
/// ranking it lower: someone that far under has no business deciding to go
/// socialize. Emergency and survival actions are unaffected because they never
/// route through this check.
pub fn fit_for_leisure(blood: &crate::body::Bloodstream) -> bool {
    use chem_sim::StatusKind;
    !blood.0.incapacitated()
        && blood.0.status(StatusKind::Sedated).intensity < 2.0
        && blood.0.status(StatusKind::Burning).intensity <= 0.0
        && blood.0.status(StatusKind::Choking).intensity <= 0.0
}

/// Advances personal pressure for every migrated resident.
///
/// Runs in `MaintainNeeds`, before context is built, so the same frame's
/// scoring sees current values.
pub(crate) fn advance_npc_needs(
    time: Res<Time>,
    tuning: Res<NeedsTuning>,
    mut needs: Query<&mut NpcNeeds, With<UtilityAgent>>,
) {
    let delta = time.delta_secs();
    for mut need in &mut needs {
        need.advance(delta, tuning.rates);
    }
}

/// The authority-side subsystem currently allowed to choose intent for an NPC.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ControlOwner {
    #[default]
    LegacyAmbient,
    UtilityAction,
    OrderVisit,
    ScriptedErrand,
    Pursuit,
    MedicalTransport,
    Incapacitated,
}

/// The subsystem currently allowed to change an NPC's position.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocomotionOwner {
    #[default]
    None,
    CrewRoute,
    Errand,
    Pursuit,
    MedicalTransport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffError {
    OwnerChanged {
        expected: ControlOwner,
        actual: ControlOwner,
    },
    IllegalTransition {
        from: ControlOwner,
        to: ControlOwner,
    },
}

/// Atomically changes intent ownership when the caller still owns the actor.
///
/// ECS adapters must release action reservations and incompatible locomotion
/// before calling this. The compare-with-expected step prevents two systems in
/// one frame from both believing they won control.
pub fn try_handoff(
    owner: &mut ControlOwner,
    expected: ControlOwner,
    next: ControlOwner,
) -> Result<(), HandoffError> {
    if *owner != expected {
        return Err(HandoffError::OwnerChanged {
            expected,
            actual: *owner,
        });
    }
    if !legal_handoff(expected, next) {
        return Err(HandoffError::IllegalTransition {
            from: expected,
            to: next,
        });
    }
    *owner = next;
    Ok(())
}

/// Ends a utility action and transfers control to an existing non-utility
/// controller. The action-scoped reservation is released by
/// [`release_interrupted_actions`] after deferred commands become visible.
pub(crate) fn interrupt_utility_action(
    commands: &mut Commands,
    entity: Entity,
    action: Option<&CurrentAction>,
    owner: &mut ControlOwner,
    locomotion: &mut LocomotionOwner,
    next_owner: ControlOwner,
    next_locomotion: LocomotionOwner,
) -> Result<(), HandoffError> {
    try_handoff(owner, ControlOwner::UtilityAction, next_owner)?;
    if let Some(action) = action {
        commands.entity(entity).insert(PendingUtilityInterruption {
            claim: ReservationOwner {
                agent: entity,
                action_instance: action.instance,
            },
            key: action.key,
        });
    }
    *locomotion = next_locomotion;
    commands
        .entity(entity)
        .remove::<CurrentAction>()
        .remove::<Errand>();
    Ok(())
}

fn legal_handoff(from: ControlOwner, to: ControlOwner) -> bool {
    if from == to {
        return false;
    }
    if to == ControlOwner::Incapacitated {
        return true;
    }
    match from {
        ControlOwner::LegacyAmbient => matches!(
            to,
            ControlOwner::UtilityAction
                | ControlOwner::OrderVisit
                | ControlOwner::ScriptedErrand
                | ControlOwner::Pursuit
        ),
        ControlOwner::UtilityAction => matches!(
            to,
            ControlOwner::LegacyAmbient
                | ControlOwner::OrderVisit
                | ControlOwner::ScriptedErrand
                | ControlOwner::Pursuit
                | ControlOwner::MedicalTransport
        ),
        ControlOwner::OrderVisit
        | ControlOwner::ScriptedErrand
        | ControlOwner::Pursuit
        | ControlOwner::MedicalTransport => {
            matches!(
                to,
                ControlOwner::LegacyAmbient | ControlOwner::UtilityAction
            )
        }
        ControlOwner::Incapacitated => to != ControlOwner::Incapacitated,
    }
}

/// Stable action identity used by selection and dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum UtilityActionId {
    IdleObserve = 0,
    MaintainPost = 1,
    PerformJob = 2,
    SeekMedicalHelp = 3,
    RespondToCasualty = 4,
    TransportPatient = 5,
    RequestTreatment = 6,
    ReturnTreatmentToCase = 7,
    Socialize = 8,
    EatFood = 9,
    Rest = 10,
    ReportIncident = 11,
    Sabotage = 12,
    PoisonFood = 13,
    ConcealEvidence = 14,
}

/// True priority classes. Only candidates in the highest non-empty bucket are
/// compared with one another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum UtilityBucket {
    #[default]
    Idle = 0,
    Routine = 1,
    Important = 2,
    Emergency = 3,
}

impl UtilityBucket {
    fn allows_variety(self, policy: SelectionPolicy) -> bool {
        self <= policy.randomize_through
    }
}

/// Typed normalized facts. Department modules may extend this enum, but they
/// should not smuggle arbitrary story state into the selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UtilityFactId {
    CanAct,
    HasTarget,
    AtTarget,
    DutyPressure,
    PersonalNeed,
    Hazard,
    TreatmentUrgency,
    SocialNeed,
    Opportunity,
    Safety,
    /// How sure this agent is about the thing a candidate concerns. Sourced
    /// from [`perception::NpcMemory`], so an eyewitness outranks someone who
    /// only heard a shout.
    Awareness,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Normalized(f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizedError {
    NonFinite,
    OutOfRange,
}

impl Normalized {
    pub const ZERO: Self = Self(0.0);
    pub const ONE: Self = Self(1.0);

    pub fn new(value: f32) -> Result<Self, NormalizedError> {
        if !value.is_finite() {
            return Err(NormalizedError::NonFinite);
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(NormalizedError::OutOfRange);
        }
        Ok(Self(value))
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct UtilityFacts(HashMap<UtilityFactId, Normalized>);

impl UtilityFacts {
    pub fn set(&mut self, id: UtilityFactId, value: f32) -> Result<(), NormalizedError> {
        self.0.insert(id, Normalized::new(value)?);
        Ok(())
    }

    pub fn get(&self, id: UtilityFactId) -> Normalized {
        self.0.get(&id).copied().unwrap_or(Normalized::ZERO)
    }
}

/// Maps one normalized fact to one normalized consideration score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResponseCurve {
    Linear,
    InverseLinear,
    Step { threshold: Normalized },
    Power { exponent: f32 },
}

impl ResponseCurve {
    pub fn evaluate(self, input: Normalized) -> Normalized {
        let x = input.get();
        let value = match self {
            ResponseCurve::Linear => x,
            ResponseCurve::InverseLinear => 1.0 - x,
            ResponseCurve::Step { threshold } => (x >= threshold.get()) as u8 as f32,
            ResponseCurve::Power { exponent } => {
                if exponent.is_finite() && exponent > 0.0 {
                    x.powf(exponent)
                } else {
                    0.0
                }
            }
        };
        Normalized(value.clamp(0.0, 1.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Consideration {
    pub fact: UtilityFactId,
    pub curve: ResponseCurve,
    /// Positive exponents below one soften a consideration; values above one
    /// make it more selective. Invalid weights veto the candidate.
    pub weight: f32,
}

impl Consideration {
    fn score(self, facts: &UtilityFacts) -> Normalized {
        if !self.weight.is_finite() || self.weight <= 0.0 {
            return Normalized::ZERO;
        }
        let value = self.curve.evaluate(facts.get(self.fact)).get();
        Normalized(value.powf(self.weight).clamp(0.0, 1.0))
    }
}

/// An action plus stable target identity and the considerations that gate it.
#[derive(Clone, Debug, PartialEq)]
pub struct UtilityCandidate {
    pub action: UtilityActionId,
    pub bucket: UtilityBucket,
    /// Stable authored/job/case identity, not a transient array index.
    pub target_key: u64,
    pub base_weight: Normalized,
    pub considerations: Vec<Consideration>,
}

impl UtilityCandidate {
    pub fn key(&self) -> ActionKey {
        ActionKey {
            action: self.action,
            target_key: self.target_key,
        }
    }

    pub fn score(&self, facts: &UtilityFacts) -> Normalized {
        let mut total = self.base_weight.get();
        for consideration in &self.considerations {
            let value = consideration.score(facts).get();
            if value == 0.0 {
                return Normalized::ZERO;
            }
            total *= value;
        }
        Normalized(total.clamp(0.0, 1.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActionKey {
    pub action: UtilityActionId,
    pub target_key: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionPolicy {
    /// A candidate at this fraction of the best score may be selected for
    /// variety in low-priority buckets.
    pub near_best_ratio: Normalized,
    /// The current action is retained when its score times this factor still
    /// meets or beats the best alternative.
    pub current_action_bonus: f32,
    /// Important and Emergency remain deterministic by default.
    pub randomize_through: UtilityBucket,
}

impl Default for SelectionPolicy {
    fn default() -> Self {
        Self {
            near_best_ratio: Normalized(0.9),
            current_action_bonus: 1.08,
            randomize_through: UtilityBucket::Routine,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurrentSelection {
    pub key: ActionKey,
    /// False during minimum commitment unless a higher-level interrupt policy
    /// has already authorized the switch.
    pub may_switch: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CandidateScore {
    pub index: usize,
    pub key: ActionKey,
    pub bucket: UtilityBucket,
    pub score: Normalized,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionResult {
    pub index: usize,
    pub scores: Vec<CandidateScore>,
}

/// Scores candidates, selects the highest eligible bucket, applies hysteresis,
/// then chooses deterministically among near-best low-priority options.
pub fn select_candidate(
    candidates: &[UtilityCandidate],
    facts: &UtilityFacts,
    policy: SelectionPolicy,
    entropy: u64,
    current: Option<CurrentSelection>,
) -> Option<SelectionResult> {
    let scores: Vec<CandidateScore> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| CandidateScore {
            index,
            key: candidate.key(),
            bucket: candidate.bucket,
            score: candidate.score(facts),
        })
        .collect();

    let bucket = scores
        .iter()
        .filter(|candidate| candidate.score.get() > 0.0)
        .map(|candidate| candidate.bucket)
        .max()?;
    let in_bucket: Vec<&CandidateScore> = scores
        .iter()
        .filter(|candidate| candidate.bucket == bucket && candidate.score.get() > 0.0)
        .collect();
    let best = in_bucket
        .iter()
        .map(|candidate| candidate.score.get())
        .fold(0.0_f32, f32::max);

    if let Some(current) = current {
        if let Some(existing) = in_bucket
            .iter()
            .copied()
            .find(|candidate| candidate.key == current.key)
        {
            let forced_to_stay = !current.may_switch;
            let wins_hysteresis = policy.current_action_bonus.is_finite()
                && policy.current_action_bonus >= 1.0
                && existing.score.get() * policy.current_action_bonus >= best;
            if forced_to_stay || wins_hysteresis {
                return Some(SelectionResult {
                    index: existing.index,
                    scores,
                });
            }
        }
    }

    let threshold = best * policy.near_best_ratio.get();
    let mut near_best: Vec<&CandidateScore> = in_bucket
        .into_iter()
        .filter(|candidate| candidate.score.get() >= threshold)
        .collect();
    near_best.sort_by_key(|candidate| candidate.key);

    let selected = if bucket.allows_variety(policy) && near_best.len() > 1 {
        let offset = (mix64(entropy) % near_best.len() as u64) as usize;
        near_best[offset]
    } else {
        near_best.into_iter().max_by(|left, right| {
            left.score
                .get()
                .total_cmp(&right.score.get())
                .then_with(|| right.key.cmp(&left.key))
        })?
    };

    Some(SelectionResult {
        index: selected.index,
        scores,
    })
}

fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionPhase {
    Selected,
    Reserving,
    Traveling,
    Performing,
    Resolving,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ActionTarget {
    Point(Vec3),
    Entity(Entity),
}

impl ActionTarget {
    fn errand_goal(self) -> ErrandGoal {
        match self {
            ActionTarget::Point(point) => ErrandGoal::Point(point),
            ActionTarget::Entity(entity) => ErrandGoal::Target(entity),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptPolicy {
    Never,
    AfterCommitment,
    Emergency,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionResult {
    Completed,
    ReservationUnavailable,
    InvalidTarget,
    Unreachable,
    TimedOut,
    Interrupted,
}

/// Authority-only execution state. Route waypoints and public presentation live
/// elsewhere.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct CurrentAction {
    pub key: ActionKey,
    pub bucket: UtilityBucket,
    pub phase: ActionPhase,
    pub target: Option<ActionTarget>,
    pub reservation: Option<ReservationKey>,
    pub reservation_capacity: usize,
    pub elapsed: f32,
    pub phase_elapsed: f32,
    pub perform_for: f32,
    pub minimum_commitment: f32,
    pub timeout: f32,
    pub interrupt_policy: InterruptPolicy,
    pub instance: u64,
    pub result: Option<ActionResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionTransitionError {
    Illegal { from: ActionPhase, to: ActionPhase },
}

impl CurrentAction {
    pub fn advance(&mut self, next: ActionPhase) -> Result<(), ActionTransitionError> {
        let legal = matches!(
            (self.phase, next),
            (ActionPhase::Selected, ActionPhase::Reserving)
                | (ActionPhase::Reserving, ActionPhase::Traveling)
                | (ActionPhase::Reserving, ActionPhase::Performing)
                | (ActionPhase::Traveling, ActionPhase::Performing)
                | (ActionPhase::Performing, ActionPhase::Resolving)
        );
        if !legal {
            return Err(ActionTransitionError::Illegal {
                from: self.phase,
                to: next,
            });
        }
        self.phase = next;
        self.phase_elapsed = 0.0;
        Ok(())
    }

    pub fn finish(&mut self, result: ActionResult) {
        self.phase = ActionPhase::Resolving;
        self.phase_elapsed = 0.0;
        self.result = Some(result);
    }

    pub fn timed_out(&self) -> bool {
        self.timeout.is_finite() && self.timeout >= 0.0 && self.elapsed >= self.timeout
    }

    pub fn may_interrupt_for(&self, incoming: UtilityBucket) -> bool {
        match self.interrupt_policy {
            InterruptPolicy::Never => false,
            InterruptPolicy::AfterCommitment => self.elapsed >= self.minimum_commitment,
            InterruptPolicy::Emergency => {
                incoming == UtilityBucket::Emergency || self.elapsed >= self.minimum_commitment
            }
        }
    }
}

/// One reservation belongs to one action instance, not merely one NPC. A late
/// cleanup from an old action therefore cannot release the new action's claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReservationOwner {
    pub agent: Entity,
    pub action_instance: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReservationKey(pub String);

#[derive(Clone, Debug)]
struct ReservationSlot {
    capacity: usize,
    owners: Vec<ReservationOwner>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReservationError {
    ZeroCapacity,
    CapacityMismatch { existing: usize, requested: usize },
    Full,
    UnknownKey,
    NotOwned,
}

/// Authority-only claims for workstations, beds, seats, objects, and pairs.
#[derive(Resource, Default)]
pub struct ReservationBook {
    slots: HashMap<ReservationKey, ReservationSlot>,
}

impl ReservationBook {
    pub fn reserve(
        &mut self,
        key: ReservationKey,
        capacity: usize,
        owner: ReservationOwner,
    ) -> Result<(), ReservationError> {
        if capacity == 0 {
            return Err(ReservationError::ZeroCapacity);
        }
        let slot = self.slots.entry(key).or_insert_with(|| ReservationSlot {
            capacity,
            owners: Vec::with_capacity(capacity),
        });
        if slot.capacity != capacity {
            return Err(ReservationError::CapacityMismatch {
                existing: slot.capacity,
                requested: capacity,
            });
        }
        if slot.owners.contains(&owner) {
            return Ok(());
        }
        if slot.owners.len() >= slot.capacity {
            return Err(ReservationError::Full);
        }
        slot.owners.push(owner);
        Ok(())
    }

    pub fn is_reserved_by(&self, key: &ReservationKey, owner: ReservationOwner) -> bool {
        self.slots
            .get(key)
            .is_some_and(|slot| slot.owners.contains(&owner))
    }

    /// How many claims one slot currently holds. Read-only: scoring may inspect
    /// occupancy to judge crowding, but only selection may claim capacity.
    pub fn claims_on(&self, key: &ReservationKey) -> usize {
        self.slots.get(key).map_or(0, |slot| slot.owners.len())
    }

    /// Atomically hands a claim from a transient actor to the durable entity
    /// that now owns the resource, such as a responder transferring a bed to
    /// its admitted patient.
    pub fn transfer(
        &mut self,
        key: &ReservationKey,
        from: ReservationOwner,
        to: ReservationOwner,
    ) -> Result<(), ReservationError> {
        let slot = self
            .slots
            .get_mut(key)
            .ok_or(ReservationError::UnknownKey)?;
        let index = slot
            .owners
            .iter()
            .position(|owner| *owner == from)
            .ok_or(ReservationError::NotOwned)?;
        if from == to {
            return Ok(());
        }
        if slot.owners.contains(&to) {
            slot.owners.remove(index);
        } else {
            slot.owners[index] = to;
        }
        Ok(())
    }

    /// Atomically replaces an exact owner while also validating the capacity
    /// expected by the incoming action. This is used when an emergency and the
    /// interrupted action need the same exclusive resource.
    pub fn replace_owner(
        &mut self,
        key: &ReservationKey,
        capacity: usize,
        from: ReservationOwner,
        to: ReservationOwner,
    ) -> Result<(), ReservationError> {
        if capacity == 0 {
            return Err(ReservationError::ZeroCapacity);
        }
        let slot = self
            .slots
            .get_mut(key)
            .ok_or(ReservationError::UnknownKey)?;
        if slot.capacity != capacity {
            return Err(ReservationError::CapacityMismatch {
                existing: slot.capacity,
                requested: capacity,
            });
        }
        let index = slot
            .owners
            .iter()
            .position(|owner| *owner == from)
            .ok_or(ReservationError::NotOwned)?;
        if from == to {
            return Ok(());
        }
        if slot.owners.contains(&to) {
            slot.owners.remove(index);
        } else {
            slot.owners[index] = to;
        }
        Ok(())
    }

    pub fn release_owner(&mut self, owner: ReservationOwner) -> usize {
        let mut released = 0;
        for slot in self.slots.values_mut() {
            let before = slot.owners.len();
            slot.owners.retain(|candidate| *candidate != owner);
            released += before - slot.owners.len();
        }
        released
    }

    pub fn release_key(&mut self, key: &ReservationKey) -> usize {
        self.slots
            .remove(key)
            .map(|slot| slot.owners.len())
            .unwrap_or(0)
    }

    pub fn prune_agents(&mut self, mut alive: impl FnMut(Entity) -> bool) -> usize {
        let mut released = 0;
        for slot in self.slots.values_mut() {
            let before = slot.owners.len();
            slot.owners.retain(|owner| alive(owner.agent));
            released += before - slot.owners.len();
        }
        released
    }

    pub fn active_claim_count(&self) -> usize {
        self.slots.values().map(|slot| slot.owners.len()).sum()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecisionRecord {
    pub decision_serial: u64,
    pub selected: Option<ActionKey>,
    pub scores: Vec<CandidateScore>,
}

/// Bounded authority-only diagnostics. Nothing here is replicated or consumed
/// by player UI.
#[derive(Resource)]
pub struct UtilityDecisionLog {
    per_agent: HashMap<Entity, VecDeque<DecisionRecord>>,
    capacity_per_agent: usize,
}

impl Default for UtilityDecisionLog {
    fn default() -> Self {
        Self {
            per_agent: HashMap::new(),
            capacity_per_agent: 16,
        }
    }
}

impl UtilityDecisionLog {
    pub fn push(&mut self, agent: Entity, record: DecisionRecord) {
        let entries = self.per_agent.entry(agent).or_default();
        if entries.len() == self.capacity_per_agent {
            entries.pop_front();
        }
        entries.push_back(record);
    }

    pub fn entries(&self, agent: Entity) -> impl Iterator<Item = &DecisionRecord> {
        self.per_agent.get(&agent).into_iter().flatten()
    }
}

#[derive(Component, Clone, Copy, Debug)]
struct DecisionClock {
    remaining: f32,
}

/// One action lifecycle completed. Department systems consume this message in
/// Resolve to apply their own side effects exactly once.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct UtilityActionResolved {
    pub agent: Entity,
    pub key: ActionKey,
    pub claim: ReservationOwner,
    pub result: ActionResult,
}

#[derive(Component, Clone, Copy, Debug)]
struct PendingUtilityInterruption {
    claim: ReservationOwner,
    key: ActionKey,
}

/// A fully claimed emergency action waiting for the interrupted action's exact
/// cleanup to finish. The new claim is acquired before the old action is
/// removed, so a losing responder never has to abandon its current work.
#[derive(Component, Clone, Debug)]
struct PendingEmergencyReplacement {
    action: CurrentAction,
}

/// The exact controller suspended by incapacity. Route and errand components
/// stay in place because both shared locomotion executors already pause for an
/// incapacitated bloodstream; recovery can therefore resume the same visit or
/// scripted task without inventing a second intent.
#[derive(Component, Clone, Copy, Debug)]
struct SuspendedUtilityControl {
    owner: ControlOwner,
    locomotion: LocomotionOwner,
}

/// One non-job action a provider is offering to one exact agent this frame.
///
/// This is the seam for personal, social, reporting, and covert actions. They
/// are not department work and must never be published to the [`JobBoard`] as
/// `PerformJob`: a job ticket is a claimable unit of station work that any
/// qualified worker may take, whereas an opportunity is already addressed to a
/// specific agent and carries its own feasibility.
#[derive(Clone, Debug, PartialEq)]
pub struct UtilityOpportunity {
    /// The exact agent this opportunity is offered to. A provider that wants
    /// several agents to consider the same target publishes one entry each, so
    /// per-agent feasibility stays honest.
    pub agent: Entity,
    pub action: UtilityActionId,
    pub bucket: UtilityBucket,
    /// Stable identity for this action/target pair. It is compared against the
    /// current action's key for hysteresis, so it must not change frame to
    /// frame for the same underlying opportunity.
    pub target_key: u64,
    /// Normalized base appeal. Target-specific quality, affinity, and
    /// opportunity value belong here; shared pressure belongs in the facts.
    pub base_weight: Normalized,
    /// Multiplicative considerations. Any zero vetoes the candidate, which is
    /// how a provider expresses a hard precondition.
    pub considerations: Vec<Consideration>,
    pub target: Option<ActionTarget>,
    pub reservation: Option<ReservationKey>,
    pub reservation_capacity: usize,
    pub perform_seconds: f32,
    pub minimum_commitment: f32,
    pub timeout: f32,
    pub interrupt_policy: InterruptPolicy,
}

impl UtilityOpportunity {
    /// A bounded personal action at a point, with sensible lifecycle defaults.
    /// Callers override any field they actually care about.
    pub fn new(
        agent: Entity,
        action: UtilityActionId,
        bucket: UtilityBucket,
        target_key: u64,
        base_weight: Normalized,
    ) -> Self {
        Self {
            agent,
            action,
            bucket,
            target_key,
            base_weight,
            considerations: Vec::new(),
            target: None,
            reservation: None,
            reservation_capacity: 1,
            perform_seconds: 1.0,
            minimum_commitment: 1.0,
            timeout: 60.0,
            interrupt_policy: InterruptPolicy::Emergency,
        }
    }

    pub fn with_target(mut self, target: ActionTarget) -> Self {
        self.target = Some(target);
        self
    }

    pub fn with_reservation(mut self, key: ReservationKey, capacity: usize) -> Self {
        self.reservation = Some(key);
        self.reservation_capacity = capacity;
        self
    }

    pub fn with_timing(mut self, perform_seconds: f32, timeout: f32) -> Self {
        self.perform_seconds = perform_seconds;
        self.minimum_commitment = perform_seconds;
        self.timeout = timeout;
        self
    }

    pub fn with_consideration(mut self, consideration: Consideration) -> Self {
        self.considerations.push(consideration);
        self
    }

    pub fn key(&self) -> ActionKey {
        ActionKey {
            action: self.action,
            target_key: self.target_key,
        }
    }
}

/// Per-frame authority-only candidate seam for actions that are not department
/// jobs.
///
/// Providers publish during `BuildContext`; the same selector that reads the
/// [`JobBoard`] consumes it during `Select`. It is cleared at the start of every
/// context build, so it is a proposal surface for this frame and never a
/// persistent request queue. It holds no reservations, moves no actor, and
/// applies no outcome — selection reserves through the ordinary
/// [`ReservationBook`], and the owning provider commits its consequence when it
/// sees the ordinary [`UtilityActionResolved`].
#[derive(Resource, Default, Debug)]
pub struct UtilityOpportunityBuffer {
    offers: Vec<UtilityOpportunity>,
}

impl UtilityOpportunityBuffer {
    pub fn offer(&mut self, opportunity: UtilityOpportunity) {
        self.offers.push(opportunity);
    }

    /// Every opportunity offered to one agent this frame.
    pub fn for_agent(&self, agent: Entity) -> impl Iterator<Item = &UtilityOpportunity> {
        self.offers
            .iter()
            .filter(move |opportunity| opportunity.agent == agent)
    }

    pub fn iter(&self) -> impl Iterator<Item = &UtilityOpportunity> {
        self.offers.iter()
    }

    pub fn clear(&mut self) {
        self.offers.clear();
    }

    pub fn len(&self) -> usize {
        self.offers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offers.is_empty()
    }
}

/// Clears last frame's proposals before providers publish this frame's. Ordered
/// first in `BuildContext` so a provider in the same set always writes into an
/// empty buffer regardless of system ordering between providers.
pub(crate) fn clear_opportunity_buffer(mut buffer: ResMut<UtilityOpportunityBuffer>) {
    buffer.clear();
}

const MIN_DECISION_SECONDS: f32 = 0.35;
const DECISION_SPREAD_SECONDS: f32 = 0.4;
const AT_TARGET_DISTANCE: f32 = 0.3;
/// How far from their own department point a resident will take a communal
/// duty post as a fallback.
///
/// Deliberately generous enough to cover a large room and its immediate
/// approach, and deliberately far short of the station. The nine authored duty
/// posts all sit on the Bridge, so an unbounded search sent every postless
/// resident there — a dozen bodies packed against one wall, which is what the
/// first live run of the fallback actually produced.
const DUTY_POST_DEPARTMENT_REACH: f32 = 18.0;

/// How far from a shared department point a resident stands when that point is
/// all they have. Comfortably more than one body clearance, so a department's
/// worth of people form a loose group rather than a pile.
const DEPARTMENT_SPREAD_RADIUS: f32 = 1.6;

/// How close an action may require a body to get to *another body*, in metres.
///
/// `npc_motion` refuses any step that would bring two crew within
/// [`crate::npc_motion::CLEARANCE`], so two people physically cannot stand
/// nearer than that. Asking for [`AT_TARGET_DISTANCE`] against a spacing floor
/// more than twice as large is a walk that can never end: the worker circles
/// the body until the errand deadline and reports `Unreachable`, another worker
/// claims the same ticket, and the pair of them do it forever.
///
/// Found in play, not in tests, and the reason the tests missed it is worth
/// keeping: `NpcMotion` is an optional resource, so a harness that never
/// inserts it has no body spacing at all and every such walk arrives. A test
/// that walks someone to a person must insert the resource or it is measuring a
/// station where people can stand inside each other.
const AT_BODY_DISTANCE: f32 = crate::npc_motion::CLEARANCE + AT_TARGET_DISTANCE;
const IDLE_OBSERVE_SECONDS: f32 = 0.6;
const MAINTAIN_POST_SECONDS: f32 = 1.2;

/// Base weight for standing at a post with nothing else to do.
///
/// Deliberately below the lowest urgency any department publishes — 0.30, which
/// Security's `WorkDesk` and Bridge's `Brief` share — so that *any* real ticket
/// outranks the fallback. `MaintainPost` still multiplies this by
/// `DutyPressure`, so the score a resident actually carries is lower again.
///
/// This is what makes `MaintainPost` a floor rather than a competitor. It is
/// the only thing standing between a resident and idling when their board is
/// empty, and it must never be the reason a board *stays* empty.
const MAINTAIN_POST_WEIGHT: Normalized = Normalized(0.25);

fn sync_utility_incapacity(
    mut commands: Commands,
    mut agents: Query<
        (
            Entity,
            &Body,
            &Bloodstream,
            &mut ControlOwner,
            &mut LocomotionOwner,
            &mut NpcActivity,
            Option<&CurrentAction>,
            Option<&SuspendedUtilityControl>,
            Has<medical::MedicalPatient>,
        ),
        With<UtilityAgent>,
    >,
) {
    for (
        entity,
        body,
        blood,
        mut owner,
        mut locomotion,
        mut activity,
        action,
        suspended,
        medical_patient,
    ) in &mut agents
    {
        let down = body.0.collapsed || blood.0.incapacitated();
        if down {
            *activity = NpcActivity::Down;
            if medical_patient && *owner == ControlOwner::MedicalTransport {
                continue;
            }
            if *owner == ControlOwner::Incapacitated || suspended.is_some() {
                continue;
            }

            let previous_owner = *owner;
            let previous_locomotion = *locomotion;
            let restore_locomotion =
                if previous_owner == ControlOwner::UtilityAction && action.is_some() {
                    LocomotionOwner::None
                } else {
                    previous_locomotion
                };
            let transferred = if previous_owner == ControlOwner::UtilityAction {
                interrupt_utility_action(
                    &mut commands,
                    entity,
                    action,
                    &mut owner,
                    &mut locomotion,
                    ControlOwner::Incapacitated,
                    LocomotionOwner::None,
                )
            } else {
                let result = try_handoff(&mut owner, previous_owner, ControlOwner::Incapacitated);
                if result.is_ok() {
                    *locomotion = LocomotionOwner::None;
                }
                result
            };
            if transferred.is_ok() {
                commands.entity(entity).insert(SuspendedUtilityControl {
                    owner: previous_owner,
                    locomotion: restore_locomotion,
                });
            }
            continue;
        }

        let Some(suspended) = suspended else {
            continue;
        };
        if try_handoff(&mut owner, ControlOwner::Incapacitated, suspended.owner).is_err() {
            continue;
        }
        *locomotion = suspended.locomotion;
        *activity = if suspended.locomotion == LocomotionOwner::None {
            NpcActivity::Idle
        } else {
            NpcActivity::Traveling
        };
        commands.entity(entity).remove::<SuspendedUtilityControl>();
    }
}

fn tick_current_actions(time: Res<Time>, mut actions: Query<&mut CurrentAction>) {
    let dt = time.delta_secs();
    for mut action in &mut actions {
        if action.phase == ActionPhase::Resolving {
            continue;
        }
        action.elapsed += dt;
        action.phase_elapsed += dt;
        if action.timed_out() {
            action.finish(ActionResult::TimedOut);
        }
    }
}

#[derive(Clone, Debug)]
struct EmergencyActionProposal {
    agent: Entity,
    expected_key: ActionKey,
    expected_instance: u64,
    ticket: JobTicket,
    score: Normalized,
}

/// Emergency switching is a proposal-and-commit transaction. Every eligible
/// worker may propose for the same ticket, but proposals are sorted before any
/// actor is changed. A proposal must acquire both the exact ticket and its
/// reservation before its previous action is interrupted. Failed and losing
/// proposals therefore retain their old action, route, and claims.
fn preempt_for_emergency_jobs(world: &mut World) {
    promote_pending_emergency_replacements(world);

    let mut emergency_tickets: Vec<JobTicket> = world
        .get_resource::<JobBoard>()
        .into_iter()
        .flat_map(JobBoard::iter)
        .filter(|ticket| {
            ticket.bucket == UtilityBucket::Emergency && ticket.state == JobTicketState::Available
        })
        .cloned()
        .collect();
    emergency_tickets.sort_by_key(|ticket| ticket.id);
    if emergency_tickets.is_empty() {
        return;
    }

    let now = world
        .get_resource::<Time>()
        .map(|time| time.elapsed_secs())
        .unwrap_or_default();

    let mut proposals = Vec::new();
    let mut agents = world.query_filtered::<(
        Entity,
        &ControlOwner,
        &CurrentAction,
        &NpcJobProfile,
        &Transform,
        Option<&Body>,
        Option<&Bloodstream>,
        Option<&perception::NpcMemory>,
    ), (
        With<UtilityAgent>,
        Without<PendingUtilityInterruption>,
        Without<PendingEmergencyReplacement>,
    )>();
    for (entity, owner, action, profile, _transform, body, blood, memory) in agents.iter(world) {
        let can_act = !body.is_some_and(|body| body.0.collapsed)
            && !blood.is_some_and(|blood| blood.0.incapacitated());
        if *owner != ControlOwner::UtilityAction
            || !can_act
            || action.phase == ActionPhase::Resolving
            || action.bucket == UtilityBucket::Emergency
            || !action.may_interrupt_for(UtilityBucket::Emergency)
        {
            continue;
        }

        let mut facts = UtilityFacts::default();
        facts
            .set(UtilityFactId::CanAct, 1.0)
            .expect("boolean facts are normalized");
        for ticket in emergency_tickets.iter().filter(|ticket| {
            ticket.available_to(profile) && perception::may_respond_to(memory, ticket.subject, now)
        }) {
            // How sure this witness is scales its proposal, so among several
            // who know, the one who actually saw it outranks one who only
            // heard a shout through a wall. Un-modelled agents propose at full
            // strength, matching the un-gated behaviour above.
            let certainty = match (memory, ticket.subject) {
                (Some(memory), Some(subject)) => memory
                    .recall(now)
                    .filter(|fact| fact.subject == Some(subject))
                    .map(|fact| fact.confidence_at(now))
                    .fold(0.0f32, f32::max),
                _ => 1.0,
            };
            facts
                .set(UtilityFactId::Awareness, certainty)
                .expect("confidence is already clamped to 0..1");
            let candidate = UtilityCandidate {
                action: UtilityActionId::PerformJob,
                bucket: ticket.bucket,
                target_key: ticket.id.0,
                base_weight: ticket.urgency,
                considerations: vec![
                    Consideration {
                        fact: UtilityFactId::CanAct,
                        curve: ResponseCurve::Linear,
                        weight: 1.0,
                    },
                    Consideration {
                        fact: UtilityFactId::Awareness,
                        curve: ResponseCurve::Linear,
                        weight: 1.0,
                    },
                ],
            };
            let score = candidate.score(&facts);
            if score.get() > 0.0 {
                proposals.push(EmergencyActionProposal {
                    agent: entity,
                    expected_key: action.key,
                    expected_instance: action.instance,
                    ticket: ticket.clone(),
                    score,
                });
            }
        }
    }

    proposals.sort_by(|left, right| {
        right
            .score
            .get()
            .total_cmp(&left.score.get())
            .then_with(|| left.ticket.id.cmp(&right.ticket.id))
            .then_with(|| left.agent.to_bits().cmp(&right.agent.to_bits()))
    });
    for proposal in proposals {
        let _ = try_commit_emergency_proposal(world, &proposal);
    }
}

fn promote_pending_emergency_replacements(world: &mut World) {
    let mut query = world.query::<(
        Entity,
        &PendingEmergencyReplacement,
        Has<PendingUtilityInterruption>,
    )>();
    let pending: Vec<(Entity, CurrentAction, bool)> = query
        .iter(world)
        .map(|(entity, replacement, cleaning_old_action)| {
            (entity, replacement.action.clone(), cleaning_old_action)
        })
        .collect();

    for (entity, action, cleaning_old_action) in pending {
        if cleaning_old_action {
            continue;
        }
        let claim = ReservationOwner {
            agent: entity,
            action_instance: action.instance,
        };
        let ticket_is_held = world.get_resource::<JobBoard>().is_some_and(|jobs| {
            jobs.ticket(JobTicketId(action.key.target_key))
                .is_some_and(|ticket| ticket.state == JobTicketState::Claimed(claim))
        });
        let reservation_is_held = action.reservation.as_ref().is_some_and(|key| {
            world
                .get_resource::<ReservationBook>()
                .is_some_and(|reservations| reservations.is_reserved_by(key, claim))
        });
        let actor_may_resume = world.get::<ControlOwner>(entity)
            == Some(&ControlOwner::UtilityAction)
            && world.get::<CurrentAction>(entity).is_none()
            && world.get::<Errand>(entity).is_none()
            && !world
                .get::<Body>(entity)
                .is_some_and(|body| body.0.collapsed)
            && !world
                .get::<Bloodstream>(entity)
                .is_some_and(|blood| blood.0.incapacitated());

        if ticket_is_held && reservation_is_held && actor_may_resume {
            if let Ok(mut actor) = world.get_entity_mut(entity) {
                actor.insert(action).remove::<PendingEmergencyReplacement>();
            }
            continue;
        }

        if let Some(mut reservations) = world.get_resource_mut::<ReservationBook>() {
            reservations.release_owner(claim);
        }
        if let Some(mut jobs) = world.get_resource_mut::<JobBoard>() {
            let _ = jobs.release_claim(JobTicketId(action.key.target_key), claim);
        }
        if let Ok(mut actor) = world.get_entity_mut(entity) {
            actor.remove::<PendingEmergencyReplacement>();
        }
    }
}

fn try_commit_emergency_proposal(world: &mut World, proposal: &EmergencyActionProposal) -> bool {
    let exact_action_still_owned = world.get::<ControlOwner>(proposal.agent)
        == Some(&ControlOwner::UtilityAction)
        && world
            .get::<CurrentAction>(proposal.agent)
            .is_some_and(|action| {
                action.key == proposal.expected_key
                    && action.instance == proposal.expected_instance
                    && action.phase != ActionPhase::Resolving
                    && action.bucket != UtilityBucket::Emergency
                    && action.may_interrupt_for(UtilityBucket::Emergency)
            })
        && world
            .get::<NpcJobProfile>(proposal.agent)
            .is_some_and(|profile| proposal.ticket.available_to(profile))
        && world
            .get::<PendingUtilityInterruption>(proposal.agent)
            .is_none()
        && world
            .get::<PendingEmergencyReplacement>(proposal.agent)
            .is_none()
        && !world
            .get::<Body>(proposal.agent)
            .is_some_and(|body| body.0.collapsed)
        && !world
            .get::<Bloodstream>(proposal.agent)
            .is_some_and(|blood| blood.0.incapacitated());
    let ticket_is_still_available = world.get_resource::<JobBoard>().is_some_and(|jobs| {
        jobs.ticket(proposal.ticket.id)
            .is_some_and(|ticket| ticket == &proposal.ticket)
    });
    if !exact_action_still_owned || !ticket_is_still_available {
        return false;
    }

    let old_action = world
        .get::<CurrentAction>(proposal.agent)
        .expect("the exact action was just rechecked")
        .clone();
    let old_claim = ReservationOwner {
        agent: proposal.agent,
        action_instance: old_action.instance,
    };
    let Some(agent) = world.get::<UtilityAgent>(proposal.agent) else {
        return false;
    };
    let previous_decision_serial = agent.decision_serial;
    let mut next_instance = previous_decision_serial.wrapping_add(1);
    if next_instance == old_action.instance {
        next_instance = next_instance.wrapping_add(1);
    }
    let new_claim = ReservationOwner {
        agent: proposal.agent,
        action_instance: next_instance,
    };

    let ticket_claimed = world
        .get_resource_mut::<JobBoard>()
        .is_some_and(|mut jobs| jobs.claim(proposal.ticket.id, new_claim).is_ok());
    if !ticket_claimed {
        return false;
    }

    let reused_old_reservation = old_action.reservation.as_ref()
        == Some(&proposal.ticket.reservation)
        && world
            .get_resource::<ReservationBook>()
            .is_some_and(|reservations| {
                reservations.is_reserved_by(&proposal.ticket.reservation, old_claim)
            });
    let reservation_claimed =
        world
            .get_resource_mut::<ReservationBook>()
            .is_some_and(|mut reservations| {
                if reused_old_reservation {
                    reservations
                        .replace_owner(
                            &proposal.ticket.reservation,
                            proposal.ticket.reservation_capacity.max(1),
                            old_claim,
                            new_claim,
                        )
                        .is_ok()
                } else {
                    reservations
                        .reserve(
                            proposal.ticket.reservation.clone(),
                            proposal.ticket.reservation_capacity.max(1),
                            new_claim,
                        )
                        .is_ok()
                }
            });
    if !reservation_claimed {
        if let Some(mut jobs) = world.get_resource_mut::<JobBoard>() {
            let _ = jobs.release_claim(proposal.ticket.id, new_claim);
        }
        return false;
    }

    let target = match proposal.ticket.target {
        ActionTarget::Point(point) => {
            let y = world
                .get::<Transform>(proposal.agent)
                .map_or(point.y, |transform| transform.translation.y);
            ActionTarget::Point(point.with_y(y))
        }
        target @ ActionTarget::Entity(_) => target,
    };
    let replacement = CurrentAction {
        key: ActionKey {
            action: UtilityActionId::PerformJob,
            target_key: proposal.ticket.id.0,
        },
        bucket: proposal.ticket.bucket,
        phase: ActionPhase::Selected,
        target: Some(target),
        reservation: Some(proposal.ticket.reservation.clone()),
        reservation_capacity: proposal.ticket.reservation_capacity,
        elapsed: 0.0,
        phase_elapsed: 0.0,
        perform_for: proposal.ticket.perform_seconds,
        minimum_commitment: proposal.ticket.perform_seconds,
        timeout: proposal.ticket.perform_seconds + 60.0,
        interrupt_policy: InterruptPolicy::Emergency,
        instance: next_instance,
        result: None,
    };

    let mut actor = world
        .get_entity_mut(proposal.agent)
        .expect("an exclusively rechecked actor cannot disappear");
    actor
        .insert((
            PendingUtilityInterruption {
                claim: old_claim,
                key: old_action.key,
            },
            PendingEmergencyReplacement {
                action: replacement,
            },
        ))
        .remove::<CurrentAction>()
        .remove::<Errand>();
    actor
        .get_mut::<UtilityAgent>()
        .expect("an emergency proposal requires UtilityAgent")
        .decision_serial = next_instance;
    *actor
        .get_mut::<LocomotionOwner>()
        .expect("a utility-controlled actor has locomotion ownership") = LocomotionOwner::None;
    *actor
        .get_mut::<NpcActivity>()
        .expect("a utility-controlled actor has public activity") = NpcActivity::Idle;

    if let Some(mut log) = world.get_resource_mut::<UtilityDecisionLog>() {
        log.push(
            proposal.agent,
            DecisionRecord {
                decision_serial: previous_decision_serial,
                selected: Some(ActionKey {
                    action: UtilityActionId::PerformJob,
                    target_key: proposal.ticket.id.0,
                }),
                scores: vec![CandidateScore {
                    index: 0,
                    key: ActionKey {
                        action: UtilityActionId::PerformJob,
                        target_key: proposal.ticket.id.0,
                    },
                    bucket: proposal.ticket.bucket,
                    score: proposal.score,
                }],
            },
        );
    }
    true
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn select_reference_actions(
    mut commands: Commands,
    time: Res<Time>,
    posts: Res<CrewPosts>,
    departments: Res<crate::crew::Departments>,
    spots: Res<UtilitySpots>,
    // Required for the same reason `reservations` is. Without a graph the duty
    // fallback below silently finds nothing, every unposted resident loses
    // `MaintainPost` again, and a harness would measure exactly the bug this
    // parameter exists to fix while reporting a pass.
    nav: Res<NavGraph>,
    jobs: Option<Res<JobBoard>>,
    // Required, not optional, deliberately: a harness without it would silently
    // lose the occupancy filter below and measure a station where every
    // workstation has unlimited room. An absent resource must break the test,
    // not quietly change the physics it is testing.
    reservations: Res<ReservationBook>,
    opportunities: Option<Res<UtilityOpportunityBuffer>>,
    mut log: ResMut<UtilityDecisionLog>,
    mut agents: Query<
        (
            Entity,
            &CrewMember,
            &Transform,
            &mut UtilityAgent,
            &ControlOwner,
            Option<&Body>,
            Option<&Bloodstream>,
            Option<&CrewRoute>,
            Option<&NpcJobProfile>,
            Option<&mut DecisionClock>,
            Option<&perception::NpcMemory>,
        ),
        (
            Without<CurrentAction>,
            Without<Errand>,
            Without<PendingUtilityInterruption>,
        ),
    >,
) {
    for (entity, member, transform, mut agent, owner, body, blood, route, profile, clock, memory) in
        &mut agents
    {
        if *owner != ControlOwner::UtilityAction || route.is_some_and(CrewRoute::is_moving) {
            continue;
        }

        let mut clock = clock;
        let due = if let Some(clock) = clock.as_mut() {
            clock.remaining -= time.delta_secs();
            clock.remaining <= 0.0
        } else {
            agent.phase_offset_millis == 0
        };
        if !due {
            if clock.is_none() {
                commands.entity(entity).insert(DecisionClock {
                    remaining: agent.phase_offset_millis as f32 / 1_000.0,
                });
            }
            continue;
        }

        let can_act = !body.is_some_and(|body| body.0.collapsed)
            && !blood.is_some_and(|blood| blood.0.incapacitated());
        // `MaintainPost` carries `HasTarget` as a multiplicative consideration,
        // and a zero consideration deletes a candidate outright rather than
        // merely lowering it. So a resident the map never gave a personal post
        // to had no Routine candidate at all and idled the whole shift — which
        // was twenty-one of the thirty, five core crew among them.
        //
        // Three tiers, each answering a failure the one before it caused:
        //
        // 1. The resident's own `work` post, when the map authors one.
        // 2. A communal `duty` post near their *own* department. The pool is
        //    the station's own idea — `CrewPosts` calls a duty post a place
        //    "the station's business gets done", true for whoever stands in it
        //    — but all nine authored posts sit on the Bridge, so an unfiltered
        //    search marched Medical, Service and Botany across the station to
        //    stand in Ops. The department anchor keeps a fallback a fallback.
        // 3. Their department point, when nothing is authored near it.
        //
        // Tiers 2 and 3 both need *dispersing*, and this is the part the first
        // live run got wrong twice. Taking the single nearest candidate gives
        // every resident who shares an anchor the identical coordinate, so
        // Bridge, Engineering and Security all piled onto one duty post: eight
        // bodies inside two metres. Ranking the reachable candidates and then
        // indexing by the agent's own seed spreads them deterministically —
        // same resident, same slot, every decision, so the `target_key` stays
        // stable and hysteresis still holds.
        let home = departments.home(&member.role);
        let target = posts
            .work(&member.name)
            .or_else(|| {
                let home = home?;
                let mut reachable: Vec<(Vec3, f32)> = posts
                    .duty_points()
                    .filter(|point| {
                        crate::nav::flat_distance(*point, home) <= DUTY_POST_DEPARTMENT_REACH
                    })
                    // Never loiter on somebody's workstation. Every Bridge duty
                    // post is authored at *exactly* the coordinates of a Bridge
                    // `utility_spot` — 0.0 m apart — because both describe the
                    // same console. Six Bridge crew against four watch posts
                    // means two are always without one, and before this they
                    // fell back onto the very consoles the other four had
                    // reserved and were standing at. That is what the player
                    // saw: a row of bodies pressed together at the comms desk,
                    // growing across the run as more departments idled into it.
                    //
                    // Reservations cannot prevent this. `MaintainPost` takes no
                    // reservation — it is the thing a resident does when they
                    // could not get one — so the exclusion has to be positional.
                    .filter(|point| {
                        !spots.iter().any(|(_, spot)| {
                            crate::nav::flat_distance(spot.at, *point) < AT_TARGET_DISTANCE
                        })
                    })
                    .filter_map(|point| {
                        nav.nearest_reachable(transform.translation, [(point, point)])
                    })
                    .collect();
                if reachable.is_empty() {
                    return None;
                }
                // Nearest first, then a stable tiebreak so the order cannot
                // depend on map iteration order.
                reachable.sort_by(|a, b| {
                    a.1.total_cmp(&b.1)
                        .then_with(|| a.0.x.total_cmp(&b.0.x))
                        .then_with(|| a.0.z.total_cmp(&b.0.z))
                });
                let slot = (agent.seed % reachable.len() as u64) as usize;
                Some(reachable[slot].0)
            })
            // Nothing authored nearby: stand near the department point itself.
            // Worse than a console, still their own room, and still a Routine
            // candidate rather than a shift spent idling — but offset per
            // resident, because a department point is one coordinate and four
            // people sent to it stand inside each other.
            .or_else(|| {
                let home = home?;
                let (slot, occupants) = profile
                    .map(|profile| roster_of(profile.primary))
                    .and_then(|roster| roster.standing_slot(&member.name))
                    .unwrap_or((0, 1));
                Some(home + department_spread(slot, occupants, DEPARTMENT_SPREAD_RADIUS))
            })
            .map(|point| point.with_y(transform.translation.y));
        let at_target = target.is_some_and(|point| {
            transform.translation.distance_squared(point) <= AT_TARGET_DISTANCE * AT_TARGET_DISTANCE
        });

        let mut facts = UtilityFacts::default();
        facts
            .set(UtilityFactId::CanAct, if can_act { 1.0 } else { 0.0 })
            .expect("boolean facts are normalized");
        facts
            .set(
                UtilityFactId::HasTarget,
                if target.is_some() { 1.0 } else { 0.0 },
            )
            .expect("boolean facts are normalized");
        facts
            .set(UtilityFactId::AtTarget, if at_target { 1.0 } else { 0.0 })
            .expect("boolean facts are normalized");
        facts
            .set(UtilityFactId::DutyPressure, 0.75)
            .expect("constant is normalized");
        facts
            .set(UtilityFactId::PersonalNeed, 0.35)
            .expect("constant is normalized");

        let target_key = stable_text_key(&member.name);
        let mut candidates = vec![
            UtilityCandidate {
                action: UtilityActionId::IdleObserve,
                bucket: UtilityBucket::Idle,
                target_key: 0,
                base_weight: Normalized::ONE,
                considerations: vec![Consideration {
                    fact: UtilityFactId::CanAct,
                    curve: ResponseCurve::Linear,
                    weight: 1.0,
                }],
            },
            UtilityCandidate {
                action: UtilityActionId::MaintainPost,
                bucket: UtilityBucket::Routine,
                target_key,
                // A fallback must lose to real work, and this used to be
                // `Normalized::ONE`. At full strength it scored 0.75 after
                // `DutyPressure`, which beat every ticket Security (0.30-0.45)
                // and Bridge (0.30-0.50) publish — so the moment those
                // residents finally *had* a post to stand at, they stopped
                // doing their jobs entirely: `Security 3/0` and `Bridge 4/0`
                // in all twenty-five snapshots of a live run, tickets offered
                // as candidates and never once claimed.
                //
                // Raising those departments to match instead would be the
                // wrong repair. Their low numbers are deliberate — a
                // department whose job is *noticing* should be interruptible,
                // and `security.rs` says so — and it was standing at a post
                // that was mispriced, not the work.
                base_weight: MAINTAIN_POST_WEIGHT,
                considerations: vec![
                    Consideration {
                        fact: UtilityFactId::CanAct,
                        curve: ResponseCurve::Linear,
                        weight: 1.0,
                    },
                    Consideration {
                        fact: UtilityFactId::HasTarget,
                        curve: ResponseCurve::Linear,
                        weight: 1.0,
                    },
                    Consideration {
                        fact: UtilityFactId::DutyPressure,
                        curve: ResponseCurve::Linear,
                        weight: 1.0,
                    },
                ],
            },
        ];
        if let (Some(jobs), Some(profile)) = (jobs.as_deref(), profile) {
            let now = time.elapsed_secs();
            candidates.extend(
                jobs.available_for(profile)
                    .filter(|job| {
                        // A worker only answers a call about a body it knows about.
                        perception::may_respond_to(memory, job.subject, now)
                    })
                    .filter(|job| {
                        // Work whose workstation is already occupied is real
                        // work, but not work that can be *started*. Proposing
                        // it wins the score, fails its reservation immediately,
                        // and frees the worker to propose it again next tick —
                        // a visible thrash rather than a choice to do something
                        // else. Botany showed this plainly: it publishes one
                        // ticket per plot but keys them all to the one shared
                        // tending spot, so the board advertised four jobs when
                        // one was physically possible, and three workers spent
                        // the shift failing twice a second.
                        //
                        // A read, never a claim. Two workers can still pick the
                        // same free slot in one frame and one of them lose it;
                        // that race is fine and self-correcting. What this
                        // removes is the steady state where the loser can never
                        // win.
                        reservations.claims_on(&job.reservation) < job.reservation_capacity.max(1)
                    })
                    .map(|job| UtilityCandidate {
                        action: UtilityActionId::PerformJob,
                        bucket: job.bucket,
                        target_key: job.id.0,
                        base_weight: job.urgency,
                        considerations: vec![Consideration {
                            fact: UtilityFactId::CanAct,
                            curve: ResponseCurve::Linear,
                            weight: 1.0,
                        }],
                    }),
            );
        }

        // Personal, social, reporting, and covert offers compete in the same
        // scoring pass as department work, so a hungry worker weighs eating
        // against its own job rather than being driven by a separate
        // controller. `CanAct` is appended so an incapacitated body vetoes
        // every offer without each provider restating it.
        let offered: Vec<&UtilityOpportunity> = opportunities
            .as_deref()
            .map(|buffer| {
                buffer
                    .for_agent(entity)
                    // The same occupancy rule the job board gets, for the same
                    // reason. `social::offer_company` offers the one gather
                    // spot to *everybody* who is lonely, so once the lounge
                    // fills the rest re-propose it and fail on every decision
                    // tick — twenty crew, twenty times a minute. A full room is
                    // a reason to do something else, not a reason to keep
                    // walking into it.
                    .filter(|offer| {
                        offer.reservation.as_ref().is_none_or(|key| {
                            reservations.claims_on(key) < offer.reservation_capacity.max(1)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        candidates.extend(offered.iter().map(|offer| {
            let mut considerations = offer.considerations.clone();
            considerations.push(Consideration {
                fact: UtilityFactId::CanAct,
                curve: ResponseCurve::Linear,
                weight: 1.0,
            });
            UtilityCandidate {
                action: offer.action,
                bucket: offer.bucket,
                target_key: offer.target_key,
                base_weight: offer.base_weight,
                considerations,
            }
        }));

        let decision_serial = agent.decision_serial;
        let entropy = agent.next_entropy();
        let selection = select_candidate(
            &candidates,
            &facts,
            SelectionPolicy::default(),
            entropy,
            None,
        );
        let selected_key = selection
            .as_ref()
            .map(|selection| candidates[selection.index].key());
        log.push(
            entity,
            DecisionRecord {
                decision_serial,
                selected: selected_key,
                scores: selection
                    .as_ref()
                    .map(|selection| selection.scores.clone())
                    .unwrap_or_default(),
            },
        );

        let interval = decision_interval(agent.seed, agent.decision_serial);
        if let Some(clock) = clock.as_mut() {
            clock.remaining = interval;
        } else {
            commands.entity(entity).insert(DecisionClock {
                remaining: interval,
            });
        }

        let Some(selection) = selection else {
            continue;
        };
        let selected = &candidates[selection.index];

        // An offered action carries its own execution contract, so it is
        // resolved before the kernel's own action arms. Matching on the key
        // rather than the action id keeps two offers of the same kind for
        // different targets distinct.
        let selected_offer = offered
            .iter()
            .find(|offer| offer.key() == selected.key())
            .copied();
        if let Some(offer) = selected_offer {
            let target = offer.target.map(|target| match target {
                ActionTarget::Point(point) => {
                    ActionTarget::Point(point.with_y(transform.translation.y))
                }
                target @ ActionTarget::Entity(_) => target,
            });
            let selected_action = CurrentAction {
                key: offer.key(),
                bucket: offer.bucket,
                phase: ActionPhase::Selected,
                target,
                reservation: offer.reservation.clone(),
                reservation_capacity: offer.reservation_capacity,
                elapsed: 0.0,
                phase_elapsed: 0.0,
                perform_for: offer.perform_seconds,
                minimum_commitment: offer.minimum_commitment,
                timeout: offer.timeout,
                interrupt_policy: offer.interrupt_policy,
                instance: agent.decision_serial,
                result: None,
            };
            commands.queue(move |world: &mut World| {
                commit_selected_action(world, entity, selected_action);
            });
            continue;
        }

        let (target, reservation, reservation_capacity, perform_for, timeout) =
            match selected.action {
                UtilityActionId::MaintainPost => (
                    target.map(ActionTarget::Point),
                    None,
                    1,
                    MAINTAIN_POST_SECONDS,
                    45.0,
                ),
                UtilityActionId::IdleObserve => (None, None, 1, IDLE_OBSERVE_SECONDS, 5.0),
                UtilityActionId::PerformJob => {
                    let Some(job) = jobs
                        .as_deref()
                        .and_then(|jobs| jobs.ticket(JobTicketId(selected.target_key)))
                    else {
                        continue;
                    };
                    let target = match job.target {
                        ActionTarget::Point(point) => {
                            ActionTarget::Point(point.with_y(transform.translation.y))
                        }
                        target @ ActionTarget::Entity(_) => target,
                    };
                    (
                        Some(target),
                        Some(job.reservation.clone()),
                        job.reservation_capacity,
                        job.perform_seconds,
                        job.perform_seconds + 60.0,
                    )
                }
                _ => continue,
            };
        let selected_action = CurrentAction {
            key: selected.key(),
            bucket: selected.bucket,
            phase: ActionPhase::Selected,
            target,
            reservation,
            reservation_capacity,
            elapsed: 0.0,
            phase_elapsed: 0.0,
            perform_for,
            minimum_commitment: perform_for,
            timeout,
            interrupt_policy: InterruptPolicy::Emergency,
            instance: agent.decision_serial,
            result: None,
        };
        commands.queue(move |world: &mut World| {
            commit_selected_action(world, entity, selected_action);
        });
    }
}

fn commit_selected_action(world: &mut World, entity: Entity, action: CurrentAction) -> bool {
    let Ok(mut actor) = world.get_entity_mut(entity) else {
        return false;
    };
    let may_commit = actor.get::<ControlOwner>() == Some(&ControlOwner::UtilityAction)
        && !actor.contains::<CurrentAction>()
        && !actor.contains::<Errand>()
        && !actor.contains::<PendingUtilityInterruption>()
        && !actor.contains::<PendingEmergencyReplacement>();
    if !may_commit {
        return false;
    }
    actor.insert(action);
    true
}

fn decision_interval(seed: u64, serial: u64) -> f32 {
    let fraction = (mix64(seed ^ serial) & 0xffff) as f32 / u16::MAX as f32;
    MIN_DECISION_SECONDS + fraction * DECISION_SPREAD_SECONDS
}

/// The authored roster for one work domain.
///
/// Each department owns its own roster constant, so this is the only place the
/// seven are visible together. That makes it the only place a cross-department
/// invariant — "every department can claim its own tickets" — can be stated.
///
/// No longer test-only: selection reads it to give a resident their rank within
/// their own department, which is what spreads a department falling back to its
/// single shared point around a ring instead of into one pile.
pub(crate) fn roster_of(domain: JobDomain) -> department::DepartmentRoster {
    match domain {
        JobDomain::Medical => medical::MEDICAL_ROSTER,
        JobDomain::Security => security::SECURITY_ROSTER,
        JobDomain::Engineering => engineering::ENGINEERING_ROSTER,
        JobDomain::Cargo => cargo_pilot::CARGO_ROSTER,
        JobDomain::Service => service::SERVICE_ROSTER,
        JobDomain::Botany => botany::BOTANY_ROSTER,
        JobDomain::Bridge => bridge::BRIDGE_ROSTER,
    }
}

/// A per-resident standing place on a ring around a shared point.
///
/// A department point is a single coordinate, so everyone falling back to one is
/// asked to stand in the same place; `npc_motion` then refuses the last few
/// centimetres and they bunch against each other instead.
///
/// `slot` must be the resident's *rank* among the people who share this anchor,
/// and `occupants` how many that is — not a hash. The first version placed
/// bodies by golden angle on `seed % 360`, which reads as evenly spread and is
/// not: measured against the real roster it put Medical's four crew **0.08 m**
/// apart, because two of their seeds happened to land on nearly the same ray and
/// no radius separates points on one ray. Ranking cannot collide.
fn department_spread(slot: usize, occupants: usize, radius: f32) -> Vec3 {
    let occupants = occupants.max(1);
    let turn = slot as f32 / occupants as f32 * std::f32::consts::TAU;
    Vec3::new(turn.cos() * radius, 0.0, turn.sin() * radius)
}

fn stable_text_key(text: &str) -> u64 {
    text.as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn begin_reference_actions(
    mut commands: Commands,
    mut reservations: ResMut<ReservationBook>,
    jobs: Option<ResMut<JobBoard>>,
    targets: Query<(&Transform, Has<CrewMember>)>,
    mut agents: Query<(
        Entity,
        &Transform,
        &ControlOwner,
        &mut LocomotionOwner,
        &mut NpcActivity,
        &mut CurrentAction,
    )>,
) {
    let mut jobs = jobs;
    for (entity, transform, owner, mut locomotion, mut activity, mut action) in &mut agents {
        if *owner != ControlOwner::UtilityAction || action.phase != ActionPhase::Selected {
            continue;
        }
        action
            .advance(ActionPhase::Reserving)
            .expect("Selected always enters Reserving");

        if action.key.action == UtilityActionId::IdleObserve {
            action
                .advance(ActionPhase::Performing)
                .expect("IdleObserve needs no travel");
            *activity = NpcActivity::Idle;
            continue;
        }

        let Some(target) = action.target else {
            action.finish(ActionResult::InvalidTarget);
            continue;
        };
        let key = action.reservation.clone().unwrap_or_else(|| {
            ReservationKey(format!("utility.target.{:016x}", action.key.target_key))
        });
        let claim = ReservationOwner {
            agent: entity,
            action_instance: action.instance,
        };
        if reservations
            .reserve(key.clone(), action.reservation_capacity.max(1), claim)
            .is_err()
        {
            action.finish(ActionResult::ReservationUnavailable);
            continue;
        }
        action.reservation = Some(key);

        if action.key.action == UtilityActionId::PerformJob {
            let Some(jobs) = jobs.as_deref_mut() else {
                reservations.release_owner(claim);
                action.finish(ActionResult::InvalidTarget);
                continue;
            };
            if jobs
                .claim(JobTicketId(action.key.target_key), claim)
                .is_err()
            {
                reservations.release_owner(claim);
                action.finish(ActionResult::ReservationUnavailable);
                continue;
            }
        }

        // A spot that admits more than one worker sends all of them to the
        // *same* coordinate, and only the first can stand on it — body spacing
        // holds the second at `CLEARANCE`, which is further than
        // `AT_TARGET_DISTANCE`, so they orbit until the errand deadline and
        // report `Unreachable`. Then the next claimant does it too. Six spots
        // are authored at capacity 2, including both Bridge duty stations and
        // both Security ones, so this quietly halved several departments.
        let shared_spot = action.reservation_capacity > 1;

        // How close this action may ask its worker to get depends on what it is
        // walking to. A crate or a floor spot can be stood on; a person cannot,
        // because body spacing holds walkers apart. Decided from the target
        // itself rather than declared per ticket, so no adapter can publish
        // work at a distance the station's own movement rules forbid.
        let (destination, reach) = match target {
            ActionTarget::Point(point) => (Some(point), AT_TARGET_DISTANCE),
            ActionTarget::Entity(target) => match targets.get(target) {
                Ok((at, is_a_body)) => (
                    Some(at.translation),
                    if is_a_body {
                        AT_BODY_DISTANCE
                    } else {
                        AT_TARGET_DISTANCE
                    },
                ),
                Err(_) => (None, AT_TARGET_DISTANCE),
            },
        };
        // Whichever rule is more permissive wins: a shared spot and a body are
        // the same physical problem — something is already standing where this
        // worker was told to go.
        let reach = if shared_spot {
            reach.max(AT_BODY_DISTANCE)
        } else {
            reach
        };
        let Some(destination) = destination else {
            action.finish(ActionResult::InvalidTarget);
            continue;
        };
        // Horizontal, matching `crew::run_errands`' arrival test. Measured in
        // 3D this disagrees with the walk that follows it: a worker already
        // standing over a shelf item would be sent on an errand to where it is
        // already standing.
        let flat_gap = Vec2::new(
            transform.translation.x - destination.x,
            transform.translation.z - destination.z,
        )
        .length();
        if flat_gap <= reach && (transform.translation.y - destination.y).abs() < 1.2 {
            action
                .advance(ActionPhase::Performing)
                .expect("reserved action may perform at its target");
            *activity = activity_for(action.key.action);
            continue;
        }

        action
            .advance(ActionPhase::Traveling)
            .expect("reserved action may travel to its target");
        *locomotion = LocomotionOwner::Errand;
        *activity = NpcActivity::Traveling;
        send_on_errand_with_reach(&mut commands, entity, target.errand_goal(), reach);
    }
}

fn consume_utility_arrivals(
    mut arrivals: MessageReader<ErrandResolved>,
    mut agents: Query<(
        &ControlOwner,
        &mut LocomotionOwner,
        &mut NpcActivity,
        &mut CurrentAction,
    )>,
) {
    for arrival in arrivals.read() {
        let Ok((owner, mut locomotion, mut activity, mut action)) = agents.get_mut(arrival.walker)
        else {
            continue;
        };
        if *owner != ControlOwner::UtilityAction
            || action.phase != ActionPhase::Traveling
            || *locomotion != LocomotionOwner::Errand
        {
            continue;
        }
        *locomotion = LocomotionOwner::None;
        match arrival.outcome {
            ErrandOutcome::Arrived => {
                action
                    .advance(ActionPhase::Performing)
                    .expect("a traveling action may perform after arrival");
                *activity = activity_for(action.key.action);
            }
            ErrandOutcome::Unreachable => action.finish(ActionResult::Unreachable),
        }
    }
}

fn perform_reference_actions(mut agents: Query<(&mut CurrentAction, &mut NpcActivity)>) {
    for (mut action, mut activity) in &mut agents {
        if action.phase != ActionPhase::Performing {
            continue;
        }
        *activity = activity_for(action.key.action);
        if action.phase_elapsed >= action.perform_for {
            action.finish(ActionResult::Completed);
        }
    }
}

fn activity_for(action: UtilityActionId) -> NpcActivity {
    match action {
        UtilityActionId::MaintainPost | UtilityActionId::PerformJob => NpcActivity::Working,
        UtilityActionId::SeekMedicalHelp
        | UtilityActionId::RespondToCasualty
        | UtilityActionId::TransportPatient
        | UtilityActionId::RequestTreatment
        | UtilityActionId::ReturnTreatmentToCase => NpcActivity::Treating,
        UtilityActionId::Socialize => NpcActivity::Socializing,
        UtilityActionId::EatFood => NpcActivity::Eating,
        UtilityActionId::Rest => NpcActivity::Resting,
        UtilityActionId::IdleObserve => NpcActivity::Idle,
        UtilityActionId::ReportIncident => NpcActivity::Helping,
        UtilityActionId::Sabotage
        | UtilityActionId::PoisonFood
        | UtilityActionId::ConcealEvidence => NpcActivity::Working,
    }
}

fn release_interrupted_actions(
    mut commands: Commands,
    mut reservations: ResMut<ReservationBook>,
    jobs: Option<ResMut<JobBoard>>,
    mut resolved: MessageWriter<UtilityActionResolved>,
    interrupted: Query<(Entity, &PendingUtilityInterruption)>,
) {
    let mut jobs = jobs;
    for (entity, interruption) in &interrupted {
        reservations.release_owner(interruption.claim);
        if interruption.key.action == UtilityActionId::PerformJob {
            if let Some(jobs) = jobs.as_deref_mut() {
                let _ = jobs
                    .release_claim(JobTicketId(interruption.key.target_key), interruption.claim);
            }
        }
        resolved.write(UtilityActionResolved {
            agent: entity,
            key: interruption.key,
            claim: interruption.claim,
            result: ActionResult::Interrupted,
        });
        commands
            .entity(entity)
            .remove::<PendingUtilityInterruption>();
    }
}

fn resolve_reference_actions(
    mut commands: Commands,
    mut reservations: ResMut<ReservationBook>,
    jobs: Option<ResMut<JobBoard>>,
    mut resolved: MessageWriter<UtilityActionResolved>,
    mut agents: Query<(
        Entity,
        &ControlOwner,
        &mut LocomotionOwner,
        &mut NpcActivity,
        &CurrentAction,
    )>,
) {
    let mut jobs = jobs;
    for (entity, owner, mut locomotion, mut activity, action) in &mut agents {
        if *owner != ControlOwner::UtilityAction || action.phase != ActionPhase::Resolving {
            continue;
        }
        if action.reservation.is_some() {
            reservations.release_owner(ReservationOwner {
                agent: entity,
                action_instance: action.instance,
            });
        }
        let result = action.result.unwrap_or(ActionResult::InvalidTarget);
        if action.key.action == UtilityActionId::PerformJob {
            if let Some(jobs) = jobs.as_deref_mut() {
                let _ = jobs.resolve(
                    JobTicketId(action.key.target_key),
                    ReservationOwner {
                        agent: entity,
                        action_instance: action.instance,
                    },
                    result,
                );
            }
        }
        resolved.write(UtilityActionResolved {
            agent: entity,
            key: action.key,
            claim: ReservationOwner {
                agent: entity,
                action_instance: action.instance,
            },
            result,
        });
        *locomotion = LocomotionOwner::None;
        *activity = NpcActivity::Idle;
        commands
            .entity(entity)
            .remove::<CurrentAction>()
            .remove::<Errand>()
            .insert(CrewRoute::standing());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(value: f32) -> Normalized {
        Normalized::new(value).unwrap()
    }

    fn candidate(
        action: UtilityActionId,
        bucket: UtilityBucket,
        target_key: u64,
        fact: UtilityFactId,
    ) -> UtilityCandidate {
        UtilityCandidate {
            action,
            bucket,
            target_key,
            base_weight: Normalized::ONE,
            considerations: vec![Consideration {
                fact,
                curve: ResponseCurve::Linear,
                weight: 1.0,
            }],
        }
    }

    #[test]
    fn normalized_values_reject_invalid_inputs() {
        assert_eq!(Normalized::new(-0.1), Err(NormalizedError::OutOfRange));
        assert_eq!(Normalized::new(1.1), Err(NormalizedError::OutOfRange));
        assert_eq!(Normalized::new(f32::NAN), Err(NormalizedError::NonFinite));
        assert_eq!(
            Normalized::new(f32::INFINITY),
            Err(NormalizedError::NonFinite)
        );
    }

    #[test]
    fn response_curves_keep_their_normalized_contract() {
        assert_eq!(
            ResponseCurve::Linear.evaluate(normalized(0.25)),
            normalized(0.25)
        );
        assert_eq!(
            ResponseCurve::InverseLinear.evaluate(normalized(0.25)),
            normalized(0.75)
        );
        assert_eq!(
            ResponseCurve::Step {
                threshold: normalized(0.5)
            }
            .evaluate(normalized(0.49)),
            Normalized::ZERO
        );
        assert_eq!(
            ResponseCurve::Step {
                threshold: normalized(0.5)
            }
            .evaluate(normalized(0.5)),
            Normalized::ONE
        );
        assert_eq!(
            ResponseCurve::Power { exponent: 2.0 }.evaluate(normalized(0.5)),
            normalized(0.25)
        );
    }

    #[test]
    fn one_zero_consideration_vetoes_a_candidate() {
        let candidate = UtilityCandidate {
            action: UtilityActionId::PerformJob,
            bucket: UtilityBucket::Routine,
            target_key: 7,
            base_weight: Normalized::ONE,
            considerations: vec![
                Consideration {
                    fact: UtilityFactId::CanAct,
                    curve: ResponseCurve::Linear,
                    weight: 1.0,
                },
                Consideration {
                    fact: UtilityFactId::HasTarget,
                    curve: ResponseCurve::Linear,
                    weight: 1.0,
                },
            ],
        };
        let mut facts = UtilityFacts::default();
        facts.set(UtilityFactId::CanAct, 1.0).unwrap();
        facts.set(UtilityFactId::HasTarget, 0.0).unwrap();

        assert_eq!(candidate.score(&facts), Normalized::ZERO);
    }

    #[test]
    fn highest_nonempty_bucket_wins_before_raw_score() {
        let mut facts = UtilityFacts::default();
        facts.set(UtilityFactId::DutyPressure, 1.0).unwrap();
        facts.set(UtilityFactId::TreatmentUrgency, 0.1).unwrap();
        let candidates = vec![
            candidate(
                UtilityActionId::PerformJob,
                UtilityBucket::Routine,
                1,
                UtilityFactId::DutyPressure,
            ),
            candidate(
                UtilityActionId::RespondToCasualty,
                UtilityBucket::Emergency,
                2,
                UtilityFactId::TreatmentUrgency,
            ),
        ];

        let selected =
            select_candidate(&candidates, &facts, SelectionPolicy::default(), 0, None).unwrap();
        assert_eq!(selected.index, 1);
    }

    #[test]
    fn near_best_selection_is_stable_for_a_fixed_entropy_and_candidate_order() {
        let mut facts = UtilityFacts::default();
        facts.set(UtilityFactId::DutyPressure, 0.95).unwrap();
        facts.set(UtilityFactId::PersonalNeed, 0.9).unwrap();
        let candidates = vec![
            candidate(
                UtilityActionId::PerformJob,
                UtilityBucket::Routine,
                1,
                UtilityFactId::DutyPressure,
            ),
            candidate(
                UtilityActionId::Rest,
                UtilityBucket::Routine,
                2,
                UtilityFactId::PersonalNeed,
            ),
        ];

        let first =
            select_candidate(&candidates, &facts, SelectionPolicy::default(), 42, None).unwrap();
        let second =
            select_candidate(&candidates, &facts, SelectionPolicy::default(), 42, None).unwrap();
        assert_eq!(first.index, second.index);
    }

    #[test]
    fn hysteresis_retains_a_close_current_action() {
        let mut facts = UtilityFacts::default();
        facts.set(UtilityFactId::DutyPressure, 0.91).unwrap();
        facts.set(UtilityFactId::PersonalNeed, 0.9).unwrap();
        let candidates = vec![
            candidate(
                UtilityActionId::PerformJob,
                UtilityBucket::Routine,
                1,
                UtilityFactId::DutyPressure,
            ),
            candidate(
                UtilityActionId::Rest,
                UtilityBucket::Routine,
                2,
                UtilityFactId::PersonalNeed,
            ),
        ];

        let selected = select_candidate(
            &candidates,
            &facts,
            SelectionPolicy::default(),
            99,
            Some(CurrentSelection {
                key: candidates[1].key(),
                may_switch: true,
            }),
        )
        .unwrap();
        assert_eq!(selected.index, 1);
    }

    #[test]
    fn emergency_may_interrupt_minimum_commitment_but_routine_may_not() {
        let action = CurrentAction {
            key: ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: 1,
            },
            bucket: UtilityBucket::Routine,
            phase: ActionPhase::Performing,
            target: None,
            reservation: None,
            reservation_capacity: 1,
            elapsed: 1.0,
            phase_elapsed: 1.0,
            perform_for: 5.0,
            minimum_commitment: 5.0,
            timeout: 20.0,
            interrupt_policy: InterruptPolicy::Emergency,
            instance: 3,
            result: None,
        };
        assert!(!action.may_interrupt_for(UtilityBucket::Routine));
        assert!(action.may_interrupt_for(UtilityBucket::Emergency));
    }

    #[test]
    fn reservations_are_atomic_idempotent_and_action_scoped() {
        let key = ReservationKey("cargo.scale.1".to_string());
        let first = ReservationOwner {
            agent: Entity::from_raw_u32(1).unwrap(),
            action_instance: 4,
        };
        let second = ReservationOwner {
            agent: Entity::from_raw_u32(2).unwrap(),
            action_instance: 9,
        };
        let mut book = ReservationBook::default();

        assert_eq!(book.reserve(key.clone(), 1, first), Ok(()));
        assert_eq!(book.reserve(key.clone(), 1, first), Ok(()));
        assert_eq!(
            book.reserve(key.clone(), 1, second),
            Err(ReservationError::Full)
        );
        assert_eq!(book.release_owner(first), 1);
        assert_eq!(book.reserve(key.clone(), 1, second), Ok(()));
        assert!(book.is_reserved_by(&key, second));
    }

    #[test]
    fn reservation_transfer_survives_cleanup_of_the_transient_owner() {
        let responder = ReservationOwner {
            agent: Entity::from_bits(10),
            action_instance: 1,
        };
        let patient = ReservationOwner {
            agent: Entity::from_bits(20),
            action_instance: 2,
        };
        let key = ReservationKey("medical.bed.1".into());
        let mut book = ReservationBook::default();
        book.reserve(key.clone(), 1, responder).unwrap();
        book.transfer(&key, responder, patient).unwrap();

        assert!(!book.is_reserved_by(&key, responder));
        assert!(book.is_reserved_by(&key, patient));
        assert_eq!(book.prune_agents(|entity| entity != responder.agent), 0);
        assert!(book.is_reserved_by(&key, patient));
    }

    #[test]
    fn stale_cleanup_cannot_release_a_new_action_claim() {
        let agent = Entity::from_raw_u32(1).unwrap();
        let old = ReservationOwner {
            agent,
            action_instance: 10,
        };
        let new = ReservationOwner {
            agent,
            action_instance: 11,
        };
        let old_key = ReservationKey("cargo.dispatch".to_string());
        let new_key = ReservationKey("cargo.scale".to_string());
        let mut book = ReservationBook::default();
        book.reserve(old_key, 1, old).unwrap();
        book.reserve(new_key.clone(), 1, new).unwrap();

        assert_eq!(book.release_owner(old), 1);
        assert!(book.is_reserved_by(&new_key, new));
    }

    #[test]
    fn handoff_is_compare_and_swap_and_rejects_competing_owners() {
        let mut owner = ControlOwner::LegacyAmbient;
        assert_eq!(
            try_handoff(
                &mut owner,
                ControlOwner::LegacyAmbient,
                ControlOwner::UtilityAction
            ),
            Ok(())
        );
        assert_eq!(owner, ControlOwner::UtilityAction);
        assert_eq!(
            try_handoff(
                &mut owner,
                ControlOwner::LegacyAmbient,
                ControlOwner::OrderVisit
            ),
            Err(HandoffError::OwnerChanged {
                expected: ControlOwner::LegacyAmbient,
                actual: ControlOwner::UtilityAction,
            })
        );
        assert_eq!(owner, ControlOwner::UtilityAction);
    }

    #[test]
    fn deferred_selection_rechecks_control_ownership_before_committing() {
        let mut world = World::new();
        let actor = world.spawn(ControlOwner::OrderVisit).id();
        let action = CurrentAction {
            key: ActionKey {
                action: UtilityActionId::IdleObserve,
                target_key: 0,
            },
            bucket: UtilityBucket::Idle,
            phase: ActionPhase::Selected,
            target: None,
            reservation: None,
            reservation_capacity: 1,
            elapsed: 0.0,
            phase_elapsed: 0.0,
            perform_for: 0.5,
            minimum_commitment: 0.5,
            timeout: 5.0,
            interrupt_policy: InterruptPolicy::Emergency,
            instance: 1,
            result: None,
        };

        assert!(!commit_selected_action(&mut world, actor, action));
        assert!(world.get::<CurrentAction>(actor).is_none());
        assert_eq!(
            world.get::<ControlOwner>(actor),
            Some(&ControlOwner::OrderVisit)
        );
    }

    /// The load-bearing claim of the perception layer: an emergency ticket
    /// naming a subject is answered only by a worker who perceived it. Both
    /// responders here are identical in role, capability, and readiness — the
    /// only difference is what they know.
    #[test]
    fn only_a_witness_is_preempted_onto_a_subject_bearing_emergency() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<ReservationBook>()
            .init_resource::<JobBoard>()
            .add_systems(Update, preempt_for_emergency_jobs);

        let capability = JobCapability::new("medical.response");
        let patient = app.world_mut().spawn(Transform::default()).id();
        app.world_mut()
            .resource_mut::<JobBoard>()
            .publish(JobTicket {
                id: JobTicketId(9),
                domain: JobDomain::Medical,
                kind: "medical.respond.9".into(),
                target: ActionTarget::Entity(patient),
                subject: Some(patient),
                reservation: ReservationKey("medical.patient.9".into()),
                reservation_capacity: 1,
                bucket: UtilityBucket::Emergency,
                urgency: Normalized::ONE,
                required_capability: capability.clone(),
                created_at: 0.0,
                deadline: None,
                risk: Normalized::ZERO,
                perform_seconds: 1.0,
                state: JobTicketState::Available,
            })
            .unwrap();

        let responder = |app: &mut App, seed: u64, knows: bool| {
            let mut memory = perception::NpcMemory::default();
            if knows {
                memory.remember(perception::MemoryFact {
                    kind: perception::StimulusKind::Casualty,
                    subject: Some(patient),
                    actor: None,
                    at: Vec3::ZERO,
                    room_key: None,
                    modality: perception::Modality::Seen,
                    source: None,
                    confidence: 1.0,
                    learned_at: 0.0,
                });
            }
            let actor = app
                .world_mut()
                .spawn((
                    CrewMember {
                        name: format!("Responder {seed}"),
                        role: "Medical".into(),
                    },
                    Transform::default(),
                    Body::default(),
                    Bloodstream::default(),
                    CrewRoute::standing(),
                    UtilityControlBundle::new(UtilityAgent::new(seed, 0)),
                    NpcJobProfile::new(
                        JobDomain::Medical,
                        NarrativeTier::Support,
                        [capability.clone()],
                    ),
                ))
                .id();
            // The bundle already carries an `NpcMemory`; including a second in
            // the spawn tuple is a duplicate-component panic, so overwrite it.
            app.world_mut().entity_mut(actor).insert(memory);
            // A routine action that is willing to yield to an emergency, so
            // the only thing that can stop preemption is the awareness gate.
            app.world_mut().entity_mut(actor).insert(CurrentAction {
                key: ActionKey {
                    action: UtilityActionId::MaintainPost,
                    target_key: seed,
                },
                bucket: UtilityBucket::Routine,
                phase: ActionPhase::Performing,
                target: Some(ActionTarget::Point(Vec3::ZERO)),
                reservation: None,
                reservation_capacity: 1,
                elapsed: 0.1,
                phase_elapsed: 0.1,
                perform_for: 10.0,
                minimum_commitment: 10.0,
                timeout: 20.0,
                interrupt_policy: InterruptPolicy::Emergency,
                instance: seed,
                result: None,
            });
            actor
        };
        // Spawn order is load-bearing. On equal scores the proposal sort
        // breaks ties on `Entity::to_bits`, which Bevy encodes so that a
        // *later*-spawned entity sorts first. The ignorant responder is
        // therefore spawned last, so the tie-break actively favours it: only
        // consulting awareness can make the witness win instead.
        let witness = responder(&mut app, 92, true);
        let elsewhere = responder(&mut app, 91, false);
        assert!(
            elsewhere.to_bits() < witness.to_bits(),
            "the tie-break must favour the ignorant responder for this test to mean anything"
        );

        app.update();

        let claimant = match app
            .world()
            .resource::<JobBoard>()
            .ticket(JobTicketId(9))
            .expect("the emergency ticket is still on the board")
            .state
        {
            JobTicketState::Claimed(owner) => Some(owner.agent),
            _ => None,
        };
        assert_eq!(
            claimant,
            Some(witness),
            "only the responder that perceived the casualty may claim the call"
        );
    }

    /// The real select/begin/resolve chain with no navigation or perform
    /// phase, run at population.
    ///
    /// **What these sims are for, stated honestly.** Every individual guard
    /// they lean on — the compare-and-swap ticket claim, reservation capacity,
    /// the rollback when a claim fails — already has a unit test that fails
    /// when it is removed; each of those was checked by deleting the guard and
    /// confirming the *unit* test caught it, not this. What these add is the
    /// composite: that thirty or sixty-four agents scoring the same small
    /// queue on the same frame end up with a board where every ticket has at
    /// most one owner, no unclaimed ticket is holding a reservation, and the
    /// queue neither stalls nor oversubscribes.
    ///
    /// That is a property of the whole chain rather than of any one guard, and
    /// it is the property that would actually be visible in a running station.
    /// They are regression cover for the interaction, not a second copy of the
    /// unit tests.
    fn scale_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    preempt_for_emergency_jobs,
                    ApplyDeferred,
                    select_reference_actions,
                    ApplyDeferred,
                    // Selection only *proposes*. The ticket claim and the
                    // reservation both happen here, so a fixture that stopped
                    // at `Select` would show every worker holding whatever it
                    // fancied and prove nothing about contention.
                    begin_reference_actions,
                    ApplyDeferred,
                    // A refused claim only *marks* its action resolving;
                    // the reservation comes back here. Without this stage a
                    // leaked reservation would look identical to a clean
                    // rollback, which is precisely the bug scale is for.
                    resolve_reference_actions,
                    ApplyDeferred,
                )
                    .chain(),
            );
        app
    }

    fn scale_ticket(id: u64, domain: JobDomain, capability: &JobCapability) -> JobTicket {
        JobTicket {
            id: JobTicketId(id),
            domain,
            kind: format!("scale.{id}"),
            target: ActionTarget::Point(Vec3::X),
            subject: None,
            // A distinct key per ticket at capacity 1: the strictest case, and
            // the one where a double-claim would actually corrupt something.
            reservation: ReservationKey(format!("scale.{id}")),
            reservation_capacity: 1,
            bucket: UtilityBucket::Routine,
            urgency: Normalized::ONE,
            required_capability: capability.clone(),
            created_at: 0.0,
            deadline: None,
            risk: Normalized::ZERO,
            perform_seconds: 5.0,
            state: JobTicketState::Available,
        }
    }

    fn spawn_scale_worker(
        app: &mut App,
        index: usize,
        domain: JobDomain,
        capability: &JobCapability,
    ) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: format!("Worker {index}"),
                    role: "Scale".into(),
                },
                Transform::default(),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(index as u64, index as u16)),
                NpcJobProfile::new(domain, NarrativeTier::Support, [capability.clone()]),
            ))
            .id()
    }

    /// Runs one population against one work queue and returns which tickets
    /// each worker ended up holding, plus how many reservations are held on
    /// each ticket's key.
    ///
    /// The reservation counts are the part that only means something at scale:
    /// a single worker can never leak one, because there is nobody to lose a
    /// race to. Under contention, a claim that fails after its reservation
    /// succeeded must give the reservation back, or the ticket becomes
    /// permanently unclaimable while looking available on the board.
    /// Who holds which ticket, by board ownership.
    type ScaleHoldings = Vec<(Entity, Option<u64>)>;
    /// Unclaimed tickets that are still holding a reservation, and how many.
    type ScaleLeaks = Vec<(u64, usize)>;

    fn run_scale_sim_full(
        workers: usize,
        tickets: usize,
        frames: usize,
    ) -> (ScaleHoldings, ScaleLeaks) {
        let mut app = scale_app();
        let capability = JobCapability::new("scale.work");
        for id in 0..tickets as u64 {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(scale_ticket(id, JobDomain::Cargo, &capability))
                .expect("ids are unique");
        }
        let spawned: Vec<Entity> = (0..workers)
            .map(|index| spawn_scale_worker(&mut app, index, JobDomain::Cargo, &capability))
            .collect();
        // Decisions are deliberately staggered by `phase_offset_millis` so the
        // whole station does not re-plan on one frame. Time therefore has to
        // advance or every worker but the zero-offset one waits forever — the
        // same reason the real game does not select everybody at once.
        for _ in 0..frames {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.25));
            app.update();
        }
        // Read ownership from the *board*, not from `CurrentAction`. A worker
        // whose claim was refused keeps a selected action until the next
        // decision tick clears it; the board is what actually decides who owns
        // the work, and it is what a double-claim would corrupt.
        let held = spawned
            .into_iter()
            .map(|worker| {
                let held = app
                    .world()
                    .resource::<JobBoard>()
                    .iter()
                    .find(|ticket| match ticket.state {
                        JobTicketState::Claimed(owner) | JobTicketState::Completed(owner) => {
                            owner.agent == worker
                        }
                        JobTicketState::Available => false,
                    })
                    .map(|ticket| ticket.id.0);
                (worker, held)
            })
            .collect();

        // An *unclaimed* ticket must hold no reservations at all. One that does
        // is a leak: nobody owns the work, and nobody ever can.
        let book = app.world().resource::<ReservationBook>();
        let leaks = app
            .world()
            .resource::<JobBoard>()
            .iter()
            .filter(|ticket| ticket.state == JobTicketState::Available)
            .map(|ticket| (ticket.id.0, book.claims_on(&ticket.reservation)))
            .filter(|(_, claims)| *claims > 0)
            .collect();
        (held, leaks)
    }

    fn run_scale_sim(workers: usize, tickets: usize, frames: usize) -> ScaleHoldings {
        let (held, leaks) = run_scale_sim_full(workers, tickets, frames);
        assert!(
            leaks.is_empty(),
            "unclaimed tickets are holding reservations nobody can use: {leaks:?}",
        );
        held
    }

    /// The invariant every scale sim shares: no ticket is held twice, and the
    /// board agrees with what the workers think they hold.
    fn assert_no_contention(held: &ScaleHoldings) {
        let mut claimed = std::collections::HashMap::new();
        for (worker, ticket) in held {
            let Some(ticket) = ticket else { continue };
            if let Some(other) = claimed.insert(*ticket, *worker) {
                panic!("ticket {ticket} held by both {other} and {worker}; all: {held:?}");
            }
        }
    }

    #[test]
    fn eight_agents_share_a_small_queue_without_contention() {
        // The smallest interesting population: more work than workers, so
        // everyone should be busy and nobody should collide.
        let held = run_scale_sim(8, 16, 4);
        assert_no_contention(&held);
        // Not all eight, necessarily: a worker whose claim loses a race
        // re-decides on its next tick rather than blocking, so a given frame
        // can leave one idle. What matters is that the station is working, not
        // that every body is busy on a particular frame.
        let working = held.iter().filter(|(_, ticket)| ticket.is_some()).count();
        assert!(working >= 7, "the station is not working: {held:?}");
    }

    #[test]
    fn thirty_agents_contend_for_scarce_work_without_double_claiming() {
        // The station's real target population against a queue too small for
        // it. This is the contention case: 30 workers, 10 tickets, so 20 must
        // come away with nothing rather than 20 sharing 10 claims.
        let held = run_scale_sim(30, 10, 4);
        assert_no_contention(&held);
        // Ten tickets, thirty workers: at most ten can be holding work, and
        // the other twenty must come away with *nothing* rather than sharing.
        // Before the board was consulted instead of `CurrentAction`, this read
        // thirty — every worker convinced it owned something.
        let working = held.iter().filter(|(_, ticket)| ticket.is_some()).count();
        assert!(
            working <= 10,
            "more workers hold work than there is work: {held:?}",
        );
        assert!(working >= 9, "the queue went largely untouched: {held:?}");
    }

    #[test]
    fn sixty_four_agents_stay_consistent_under_heavy_contention() {
        // Well past the design target. What is being checked is not
        // performance but that nothing degrades into double-claiming when the
        // per-frame candidate set is large.
        let held = run_scale_sim(64, 24, 6);
        assert_no_contention(&held);
        let working = held.iter().filter(|(_, ticket)| ticket.is_some()).count();
        assert!(working <= 24, "oversubscribed at 64 agents: {held:?}");
        assert!(working >= 22, "the queue stalled at 64 agents: {held:?}");
    }

    #[test]
    fn a_crowded_station_still_hands_every_ticket_to_somebody() {
        // Starvation from the other side: with far more workers than work, no
        // ticket may be left sitting available while an idle worker who could
        // do it stands next to it.
        let mut app = scale_app();
        let capability = JobCapability::new("scale.work");
        for id in 0..5u64 {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(scale_ticket(id, JobDomain::Cargo, &capability))
                .unwrap();
        }
        for index in 0..40 {
            spawn_scale_worker(&mut app, index, JobDomain::Cargo, &capability);
        }
        for _ in 0..6 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.25));
            app.update();
        }
        let idle_work = app
            .world()
            .resource::<JobBoard>()
            .iter()
            .filter(|ticket| ticket.state == JobTicketState::Available)
            .count();
        assert_eq!(idle_work, 0, "work was left unclaimed by an idle station");
    }

    #[test]
    fn workers_never_take_work_outside_their_own_department_at_scale() {
        // Domain scoping is enforced per-candidate, so a crowded frame is
        // exactly where a missing filter would show up as Cargo staff doing
        // Medical's job.
        let mut app = scale_app();
        let cargo_capability = JobCapability::new("cargo.work");
        let medical_capability = JobCapability::new("medical.work");
        for id in 0..12u64 {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(scale_ticket(id, JobDomain::Medical, &medical_capability))
                .unwrap();
        }
        let cargo: Vec<Entity> = (0..30)
            .map(|index| spawn_scale_worker(&mut app, index, JobDomain::Cargo, &cargo_capability))
            .collect();
        for _ in 0..4 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.25));
            app.update();
        }
        for worker in cargo {
            // Idling or observing is fine — a worker with nothing to do is not
            // a bug. Taking a *job ticket* from another department is.
            let took_a_job = app
                .world()
                .entity(worker)
                .get::<CurrentAction>()
                .is_some_and(|action| action.key.action == UtilityActionId::PerformJob);
            assert!(!took_a_job, "a Cargo worker took Medical's work");
        }
    }

    #[test]
    fn emergency_job_preempts_routine_work_then_selects_after_exact_claim_cleanup() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    preempt_for_emergency_jobs,
                    ApplyDeferred,
                    select_reference_actions,
                    ApplyDeferred,
                    release_interrupted_actions,
                    ApplyDeferred,
                )
                    .chain(),
            );
        let capability = JobCapability::new("medical.response");
        for (id, bucket) in [(1, UtilityBucket::Routine), (2, UtilityBucket::Emergency)] {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(JobTicket {
                    id: JobTicketId(id),
                    domain: JobDomain::Medical,
                    kind: format!("medical.test.{id}"),
                    target: ActionTarget::Point(Vec3::X),
                    subject: None,
                    reservation: ReservationKey(format!("medical.test.{id}")),
                    reservation_capacity: 1,
                    bucket,
                    urgency: Normalized::ONE,
                    required_capability: capability.clone(),
                    created_at: 0.0,
                    deadline: None,
                    risk: Normalized::ZERO,
                    perform_seconds: 1.0,
                    state: JobTicketState::Available,
                })
                .unwrap();
        }
        let actor = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Responder".into(),
                    role: "Medical".into(),
                },
                Transform::default(),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(41, 0)),
                NpcJobProfile::new(JobDomain::Medical, NarrativeTier::Support, [capability]),
            ))
            .id();
        let old_claim = ReservationOwner {
            agent: actor,
            action_instance: 10,
        };
        app.world_mut()
            .resource_mut::<JobBoard>()
            .claim(JobTicketId(1), old_claim)
            .unwrap();
        let old_reservation = ReservationKey("medical.test.1".into());
        app.world_mut()
            .resource_mut::<ReservationBook>()
            .reserve(old_reservation.clone(), 1, old_claim)
            .unwrap();
        app.world_mut().entity_mut(actor).insert(CurrentAction {
            key: ActionKey {
                action: UtilityActionId::PerformJob,
                target_key: 1,
            },
            bucket: UtilityBucket::Routine,
            phase: ActionPhase::Performing,
            target: Some(ActionTarget::Point(Vec3::ZERO)),
            reservation: Some(old_reservation.clone()),
            reservation_capacity: 1,
            elapsed: 0.1,
            phase_elapsed: 0.1,
            perform_for: 10.0,
            minimum_commitment: 10.0,
            timeout: 20.0,
            interrupt_policy: InterruptPolicy::Emergency,
            instance: old_claim.action_instance,
            result: None,
        });

        app.update();
        assert!(app.world().get::<CurrentAction>(actor).is_none());
        assert!(!app
            .world()
            .resource::<ReservationBook>()
            .is_reserved_by(&old_reservation, old_claim));
        assert_eq!(
            app.world()
                .resource::<JobBoard>()
                .ticket(JobTicketId(1))
                .unwrap()
                .state,
            JobTicketState::Available,
        );

        app.update();
        let selected = app
            .world()
            .get::<CurrentAction>(actor)
            .expect("the emergency should be selected after cleanup");
        assert_eq!(selected.key.action, UtilityActionId::PerformJob);
        assert_eq!(selected.key.target_key, 2);
        assert_eq!(selected.bucket, UtilityBucket::Emergency);
    }

    #[test]
    fn one_emergency_preempts_one_worker_while_the_losing_proposal_keeps_its_work() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    preempt_for_emergency_jobs,
                    ApplyDeferred,
                    select_reference_actions,
                    ApplyDeferred,
                    release_interrupted_actions,
                    ApplyDeferred,
                )
                    .chain(),
            );
        let capability = JobCapability::new("medical.response");
        for (id, bucket) in [
            (1, UtilityBucket::Routine),
            (2, UtilityBucket::Routine),
            (3, UtilityBucket::Emergency),
        ] {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(JobTicket {
                    id: JobTicketId(id),
                    domain: JobDomain::Medical,
                    kind: format!("medical.transaction.{id}"),
                    target: ActionTarget::Point(Vec3::X * id as f32),
                    subject: None,
                    reservation: ReservationKey(format!("medical.transaction.{id}")),
                    reservation_capacity: 1,
                    bucket,
                    urgency: Normalized::ONE,
                    required_capability: capability.clone(),
                    created_at: 0.0,
                    deadline: None,
                    risk: Normalized::ZERO,
                    perform_seconds: 1.0,
                    state: JobTicketState::Available,
                })
                .unwrap();
        }

        let spawn_worker = |world: &mut World, name: &str, routine_ticket: u64, instance: u64| {
            let worker = world
                .spawn((
                    CrewMember {
                        name: name.into(),
                        role: "Medical".into(),
                    },
                    Transform::default(),
                    Body::default(),
                    Bloodstream::default(),
                    CrewRoute::standing(),
                    UtilityControlBundle::new(UtilityAgent::new(instance, 0)),
                    NpcJobProfile::new(
                        JobDomain::Medical,
                        NarrativeTier::Support,
                        [capability.clone()],
                    ),
                ))
                .id();
            let claim = ReservationOwner {
                agent: worker,
                action_instance: instance,
            };
            world
                .resource_mut::<JobBoard>()
                .claim(JobTicketId(routine_ticket), claim)
                .unwrap();
            let reservation = ReservationKey(format!("medical.transaction.{routine_ticket}"));
            world
                .resource_mut::<ReservationBook>()
                .reserve(reservation.clone(), 1, claim)
                .unwrap();
            world.entity_mut(worker).insert(CurrentAction {
                key: ActionKey {
                    action: UtilityActionId::PerformJob,
                    target_key: routine_ticket,
                },
                bucket: UtilityBucket::Routine,
                phase: ActionPhase::Performing,
                target: Some(ActionTarget::Point(Vec3::ZERO)),
                reservation: Some(reservation),
                reservation_capacity: 1,
                elapsed: 0.1,
                phase_elapsed: 0.1,
                perform_for: 10.0,
                minimum_commitment: 10.0,
                timeout: 20.0,
                interrupt_policy: InterruptPolicy::Emergency,
                instance,
                result: None,
            });
            (worker, claim)
        };
        let (first, first_claim) = spawn_worker(app.world_mut(), "First", 1, 10);
        let (second, second_claim) = spawn_worker(app.world_mut(), "Second", 2, 20);
        let (winner, winner_ticket, winner_old_claim, loser, loser_ticket, loser_old_claim) =
            if first.to_bits() < second.to_bits() {
                (first, 1, first_claim, second, 2, second_claim)
            } else {
                (second, 2, second_claim, first, 1, first_claim)
            };

        app.update();

        assert!(app.world().get::<CurrentAction>(winner).is_none());
        assert!(app
            .world()
            .get::<PendingEmergencyReplacement>(winner)
            .is_some());
        let losing_action = app
            .world()
            .get::<CurrentAction>(loser)
            .expect("the losing proposal must retain its routine action");
        assert_eq!(losing_action.key.target_key, loser_ticket);
        assert_eq!(losing_action.instance, loser_old_claim.action_instance);
        assert_eq!(
            app.world()
                .resource::<JobBoard>()
                .ticket(JobTicketId(winner_ticket))
                .unwrap()
                .state,
            JobTicketState::Available,
        );
        assert_eq!(
            app.world()
                .resource::<JobBoard>()
                .ticket(JobTicketId(loser_ticket))
                .unwrap()
                .state,
            JobTicketState::Claimed(loser_old_claim),
        );
        assert!(app.world().resource::<ReservationBook>().is_reserved_by(
            &ReservationKey(format!("medical.transaction.{loser_ticket}")),
            loser_old_claim,
        ));
        assert!(!app.world().resource::<ReservationBook>().is_reserved_by(
            &ReservationKey(format!("medical.transaction.{winner_ticket}")),
            winner_old_claim,
        ));

        app.update();

        let emergency = app
            .world()
            .get::<CurrentAction>(winner)
            .expect("the sole winner should begin the preclaimed emergency");
        assert_eq!(emergency.key.action, UtilityActionId::PerformJob);
        assert_eq!(emergency.key.target_key, 3);
        assert_eq!(emergency.bucket, UtilityBucket::Emergency);
        let losing_action = app
            .world()
            .get::<CurrentAction>(loser)
            .expect("the losing worker must still own the same routine action");
        assert_eq!(losing_action.key.target_key, loser_ticket);
        assert_eq!(losing_action.instance, loser_old_claim.action_instance);
        assert_eq!(
            app.world()
                .resource::<JobBoard>()
                .ticket(JobTicketId(loser_ticket))
                .unwrap()
                .state,
            JobTicketState::Claimed(loser_old_claim),
        );
    }

    #[test]
    fn incapacity_suspends_one_controller_and_restores_the_exact_owner() {
        let mut app = App::new();
        app.init_resource::<ReservationBook>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    sync_utility_incapacity,
                    ApplyDeferred,
                    release_interrupted_actions,
                    ApplyDeferred,
                )
                    .chain(),
            );

        let reservation = ReservationKey("cargo.console.1".to_string());
        let mut down = Body::default();
        down.0.collapsed = true;
        let utility_worker = app
            .world_mut()
            .spawn((
                down,
                Bloodstream::default(),
                UtilityControlBundle::new(UtilityAgent::new(5, 0)),
                CurrentAction {
                    key: ActionKey {
                        action: UtilityActionId::PerformJob,
                        target_key: 7,
                    },
                    bucket: UtilityBucket::Routine,
                    phase: ActionPhase::Performing,
                    target: None,
                    reservation: Some(reservation.clone()),
                    reservation_capacity: 1,
                    elapsed: 1.0,
                    phase_elapsed: 1.0,
                    perform_for: 4.0,
                    minimum_commitment: 4.0,
                    timeout: 20.0,
                    interrupt_policy: InterruptPolicy::Emergency,
                    instance: 12,
                    result: None,
                },
            ))
            .id();
        let utility_claim = ReservationOwner {
            agent: utility_worker,
            action_instance: 12,
        };
        app.world_mut()
            .resource_mut::<ReservationBook>()
            .reserve(reservation.clone(), 1, utility_claim)
            .unwrap();

        let mut down = Body::default();
        down.0.collapsed = true;
        let mut order_control = UtilityControlBundle::new(UtilityAgent::new(6, 0));
        order_control.control = ControlOwner::OrderVisit;
        order_control.locomotion = LocomotionOwner::CrewRoute;
        order_control.activity = NpcActivity::Traveling;
        let order_visitor = app
            .world_mut()
            .spawn((down, Bloodstream::default(), order_control))
            .id();

        app.update();
        for entity in [utility_worker, order_visitor] {
            assert_eq!(
                app.world().get::<ControlOwner>(entity),
                Some(&ControlOwner::Incapacitated)
            );
            assert_eq!(
                app.world().get::<LocomotionOwner>(entity),
                Some(&LocomotionOwner::None)
            );
            assert_eq!(
                app.world().get::<NpcActivity>(entity),
                Some(&NpcActivity::Down)
            );
        }
        assert!(app.world().get::<CurrentAction>(utility_worker).is_none());
        assert!(!app
            .world()
            .resource::<ReservationBook>()
            .is_reserved_by(&reservation, utility_claim));

        app.world_mut()
            .get_mut::<Body>(utility_worker)
            .unwrap()
            .0
            .collapsed = false;
        app.world_mut()
            .get_mut::<Body>(order_visitor)
            .unwrap()
            .0
            .collapsed = false;
        app.update();

        assert_eq!(
            app.world().get::<ControlOwner>(utility_worker),
            Some(&ControlOwner::UtilityAction)
        );
        assert_eq!(
            app.world().get::<LocomotionOwner>(utility_worker),
            Some(&LocomotionOwner::None)
        );
        assert_eq!(
            app.world().get::<ControlOwner>(order_visitor),
            Some(&ControlOwner::OrderVisit)
        );
        assert_eq!(
            app.world().get::<LocomotionOwner>(order_visitor),
            Some(&LocomotionOwner::CrewRoute)
        );
    }

    #[test]
    fn incapacity_preserves_an_inherited_route_when_no_action_was_interrupted() {
        let mut app = App::new();
        app.add_systems(Update, (sync_utility_incapacity, ApplyDeferred).chain());
        let mut down = Body::default();
        down.0.collapsed = true;
        let mut control = UtilityControlBundle::new(UtilityAgent::new(31, 0));
        control.locomotion = LocomotionOwner::CrewRoute;
        control.activity = NpcActivity::Traveling;
        let resident = app
            .world_mut()
            .spawn((
                down,
                Bloodstream::default(),
                CrewRoute::to(Vec3::X),
                control,
            ))
            .id();

        app.update();
        assert_eq!(
            app.world().get::<ControlOwner>(resident),
            Some(&ControlOwner::Incapacitated),
        );
        assert_eq!(
            app.world().get::<LocomotionOwner>(resident),
            Some(&LocomotionOwner::None),
        );

        app.world_mut()
            .get_mut::<Body>(resident)
            .unwrap()
            .0
            .collapsed = false;
        app.update();
        assert_eq!(
            app.world().get::<ControlOwner>(resident),
            Some(&ControlOwner::UtilityAction),
        );
        assert_eq!(
            app.world().get::<LocomotionOwner>(resident),
            Some(&LocomotionOwner::CrewRoute),
            "activation inherited the moving route, so recovery must return it",
        );
        assert!(app.world().get::<CrewRoute>(resident).unwrap().is_moving());
    }

    #[test]
    fn action_lifecycle_rejects_skipping_execution() {
        let mut action = CurrentAction {
            key: ActionKey {
                action: UtilityActionId::MaintainPost,
                target_key: 1,
            },
            bucket: UtilityBucket::Routine,
            phase: ActionPhase::Selected,
            target: None,
            reservation: None,
            reservation_capacity: 1,
            elapsed: 0.0,
            phase_elapsed: 0.0,
            perform_for: 1.0,
            minimum_commitment: 1.0,
            timeout: 10.0,
            interrupt_policy: InterruptPolicy::AfterCommitment,
            instance: 1,
            result: None,
        };
        assert!(action.advance(ActionPhase::Resolving).is_err());
        action.advance(ActionPhase::Reserving).unwrap();
        action.advance(ActionPhase::Traveling).unwrap();
        action.advance(ActionPhase::Performing).unwrap();
        action.advance(ActionPhase::Resolving).unwrap();
    }

    #[test]
    fn public_activity_and_posture_have_wire_representations() {
        let encoded = ron::to_string(&NpcActivity::Treating).unwrap();
        let decoded: NpcActivity = ron::from_str(&encoded).unwrap();
        assert_eq!(decoded, NpcActivity::Treating);
        let encoded = ron::to_string(&NpcPosture::Lying).unwrap();
        let decoded: NpcPosture = ron::from_str(&encoded).unwrap();
        assert_eq!(decoded, NpcPosture::Lying);
    }

    #[test]
    fn replication_registers_activity_but_none_of_the_private_decision_state() {
        use bevy_replicon::shared::replication::rules::ReplicationRules;

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::state::app::StatesPlugin,
            RepliconSharedPlugin::default(),
            UtilityAiPlugin,
        ));

        let activity = app.world_mut().register_component::<NpcActivity>();
        let posture = app.world_mut().register_component::<NpcPosture>();
        let private = [
            app.world_mut().register_component::<UtilityAgent>(),
            app.world_mut().register_component::<ControlOwner>(),
            app.world_mut().register_component::<LocomotionOwner>(),
            app.world_mut().register_component::<CurrentAction>(),
            app.world_mut().register_component::<DecisionClock>(),
        ];
        let rules = app.world().resource::<ReplicationRules>();
        let replicated = |id| {
            rules
                .iter()
                .any(|rule| rule.components.iter().any(|component| component.id == id))
        };

        assert!(replicated(activity));
        assert!(replicated(posture));
        assert!(private.into_iter().all(|id| !replicated(id)));
    }

    #[test]
    fn crew_and_utility_plugins_build_one_ordered_schedule() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::state::app::StatesPlugin,
            RepliconSharedPlugin::default(),
        ))
        .init_state::<crate::AppState>()
        .add_plugins((crate::crew::CrewPlugin, UtilityAiPlugin));

        // Schedule initialization detects ordering cycles and invalid system
        // access even though Loading correctly keeps gameplay systems inert.
        app.update();
    }

    #[derive(Resource, Default)]
    struct ResolutionLog(Vec<UtilityActionResolved>);

    fn record_resolutions(
        mut messages: MessageReader<UtilityActionResolved>,
        mut log: ResMut<ResolutionLog>,
    ) {
        log.0.extend(messages.read().copied());
    }

    #[test]
    fn order_recall_interrupts_one_utility_controller_and_returns_it_to_utility_duty() {
        use bevy::ecs::system::RunSystemOnce;

        fn recall_doctor(
            mut commands: Commands,
            mut residents: crate::crew::AvailableResidents,
        ) -> Option<Entity> {
            crate::crew::recall_resident_for_order(
                &mut commands,
                &mut residents,
                "Dr. Vance",
                "Medical",
                0.0,
            )
        }

        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<crate::lab::DeliveryStations>()
            .init_resource::<ReservationBook>()
            .init_resource::<ResolutionLog>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    crate::crew::start_crew_at_their_department,
                    crate::crew::walk_route,
                    ApplyDeferred,
                    release_interrupted_actions,
                    record_resolutions,
                )
                    .chain(),
            );

        let start = Vec3::new(-6.0, crate::crew::BODY_OFFSET, -6.0);
        let reservation = ReservationKey("medical.bed.1".to_string());
        let action_instance = 41;
        let mut control = UtilityControlBundle::new(UtilityAgent::new(17, 0));
        control.locomotion = LocomotionOwner::Errand;
        control.activity = NpcActivity::Working;
        let doctor = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Dr. Vance".into(),
                    role: "Medical".into(),
                },
                Transform::from_translation(start),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                crate::crew::Ambient::new(5.0),
                crate::crew::StationResident,
                control,
                CurrentAction {
                    key: ActionKey {
                        action: UtilityActionId::PerformJob,
                        target_key: 99,
                    },
                    bucket: UtilityBucket::Routine,
                    phase: ActionPhase::Performing,
                    target: None,
                    reservation: Some(reservation.clone()),
                    reservation_capacity: 1,
                    elapsed: 1.0,
                    phase_elapsed: 1.0,
                    perform_for: 5.0,
                    minimum_commitment: 5.0,
                    timeout: 20.0,
                    interrupt_policy: InterruptPolicy::Emergency,
                    instance: action_instance,
                    result: None,
                },
            ))
            .id();
        let claim = ReservationOwner {
            agent: doctor,
            action_instance,
        };
        app.world_mut()
            .resource_mut::<ReservationBook>()
            .reserve(reservation.clone(), 1, claim)
            .unwrap();

        let recalled = app.world_mut().run_system_once(recall_doctor).unwrap();
        assert_eq!(recalled, Some(doctor));
        assert_eq!(
            app.world().get::<ControlOwner>(doctor),
            Some(&ControlOwner::OrderVisit)
        );
        assert_eq!(
            app.world().get::<LocomotionOwner>(doctor),
            Some(&LocomotionOwner::CrewRoute)
        );
        assert!(app.world().get::<CurrentAction>(doctor).is_none());
        assert!(app.world().get::<crate::crew::Ambient>(doctor).is_none());
        let named_doctors = {
            let world = app.world_mut();
            let mut crew = world.query::<&CrewMember>();
            crew.iter(world)
                .filter(|member| member.name == "Dr. Vance")
                .count()
        };
        assert_eq!(
            named_doctors, 1,
            "recall must reuse the utility resident rather than clone the named doctor",
        );

        app.update();
        assert!(
            !app.world()
                .resource::<ReservationBook>()
                .is_reserved_by(&reservation, claim),
            "the interrupted action must release its action-scoped reservation",
        );
        assert_eq!(
            app.world().resource::<ResolutionLog>().0,
            vec![UtilityActionResolved {
                agent: doctor,
                key: ActionKey {
                    action: UtilityActionId::PerformJob,
                    target_key: 99,
                },
                claim,
                result: ActionResult::Interrupted,
            }],
        );

        for _ in 0..600 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.05));
            app.update();
            if app.world().get::<CrewRoute>(doctor).unwrap().phase
                == crate::crew::CrewPhase::Waiting
            {
                break;
            }
        }
        assert!(app.world().get::<crate::crew::AtCounter>(doctor).is_some());
        app.world_mut()
            .get_mut::<CrewRoute>(doctor)
            .unwrap()
            .leave();

        for _ in 0..600 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.05));
            app.update();
            if app.world().get::<crate::crew::Ambient>(doctor).is_some() {
                break;
            }
        }
        assert_eq!(
            app.world().get::<ControlOwner>(doctor),
            Some(&ControlOwner::UtilityAction)
        );
        assert_eq!(
            app.world().get::<LocomotionOwner>(doctor),
            Some(&LocomotionOwner::None)
        );
        assert_eq!(
            app.world().get::<NpcActivity>(doctor),
            Some(&NpcActivity::Idle)
        );
        assert!(app.world().get::<crate::crew::Ambient>(doctor).is_some());
        assert!(
            app.world().get::<crate::crew::AtCounter>(doctor).is_none(),
            "returning to a department must not leave the resident enrolled as a customer",
        );
    }

    #[test]
    fn resident_selects_travels_performs_resolves_and_replans() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<ResolutionLog>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    ApplyDeferred,
                    record_resolutions,
                )
                    .chain(),
            );

        let start = Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0);
        let post = Vec3::new(2.0, 0.0, 0.0);
        app.world_mut()
            .resource_mut::<CrewPosts>()
            .set_work("Cargo Pilot".into(), post);
        let resident = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Cargo Pilot".into(),
                    role: "Cargo".into(),
                },
                Transform::from_translation(start),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(17, 0)),
            ))
            .id();

        for _ in 0..240 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            if app.world().resource::<ResolutionLog>().0.len() >= 2 {
                break;
            }
        }

        let resolutions = &app.world().resource::<ResolutionLog>().0;
        assert!(
            resolutions.len() >= 2,
            "the resident never completed and replanned two actions: {resolutions:?}"
        );
        assert!(resolutions.iter().take(2).all(|resolution| {
            resolution.agent == resident
                && resolution.key.action == UtilityActionId::MaintainPost
                && resolution.result == ActionResult::Completed
        }));
        let at = app.world().get::<Transform>(resident).unwrap().translation;
        assert!(
            at.distance(post.with_y(crate::crew::BODY_OFFSET)) <= AT_TARGET_DISTANCE,
            "resident stopped at {at:?} instead of the authored post {post:?}"
        );
        assert!(app.world().get::<Errand>(resident).is_none());
        assert_eq!(
            app.world().get::<LocomotionOwner>(resident),
            Some(&LocomotionOwner::None)
        );
    }

    /// Most of the station has no post of its own, and used to have no work.
    ///
    /// Only nine `crew_post` markers carry an occupant, so twenty-one of the
    /// thirty residents — five core crew among them — never matched
    /// `CrewPosts::work`. That returned `None`, which set `HasTarget` to zero,
    /// which zeroed a *multiplicative* consideration, which deletes a candidate
    /// outright rather than lowering it. `MaintainPost` therefore vanished from
    /// their candidate set and `IdleObserve` was the only thing left in any
    /// bucket, for the whole shift.
    ///
    /// Falsifies the duty fallback: drop the `or_else` in
    /// `select_reference_actions` and this resident selects `IdleObserve`.
    #[test]
    fn a_resident_without_a_personal_post_still_has_routine_work() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<UtilityActionResolved>()
            .add_systems(Update, (select_reference_actions, ApplyDeferred).chain());

        // A communal duty spot, and deliberately no `set_work` for this name.
        // The department point is what makes the spot *theirs*: the fallback is
        // scoped to the resident's own department so it cannot march Medical
        // across the station to stand on the Bridge.
        let duty = Vec3::new(2.0, 0.0, 0.0);
        app.world_mut()
            .resource_mut::<CrewPosts>()
            .add_duty(duty, Quat::IDENTITY);
        app.world_mut()
            .resource_mut::<crate::crew::Departments>()
            .set("Bridge".into(), duty);

        let resident = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Ensign Park".into(),
                    role: "Bridge".into(),
                },
                Transform::from_translation(Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0)),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(23, 0)),
            ))
            .id();

        // Step in small frames: a single large advance can sail past the
        // decision clock rather than landing on it.
        let mut chosen = None;
        for _ in 0..40 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            if let Some(action) = app.world().get::<CurrentAction>(resident) {
                chosen = Some(action.key.action);
                break;
            }
        }

        assert_eq!(
            chosen,
            Some(UtilityActionId::MaintainPost),
            "a resident with no personal post fell through to idling instead of \
             taking the communal duty spot",
        );
    }

    /// Standing at a post must lose to the least urgent real job on the board.
    ///
    /// `MaintainPost` used to carry `base_weight: Normalized::ONE`, scoring
    /// 0.75 after `DutyPressure`. That beat every ticket Security (0.30-0.45)
    /// and Bridge (0.30-0.50) publish, so the moment stage 1 gave those
    /// residents a post to stand at they stopped working entirely — live,
    /// `Security 3/0` and `Bridge 4/0` in all twenty-five snapshots of a
    /// 500-second run, with the tickets visibly offered as losing candidates.
    ///
    /// 0.30 is the floor any department authors, so the fallback is pinned
    /// below it here rather than in one adapter: raising Security and Bridge to
    /// match would have destroyed the interruptibility their low numbers exist
    /// to buy.
    ///
    /// Falsifies the reweighting: restore `Normalized::ONE` and the fallback
    /// outscores the job.
    #[test]
    fn standing_at_a_post_loses_to_the_least_urgent_real_work() {
        const LOWEST_AUTHORED_URGENCY: f32 = 0.30;

        let mut facts = UtilityFacts::default();
        for (fact, value) in [
            (UtilityFactId::CanAct, 1.0),
            (UtilityFactId::HasTarget, 1.0),
            (UtilityFactId::DutyPressure, 0.75),
        ] {
            facts.set(fact, value).expect("in range");
        }

        let maintain = UtilityCandidate {
            action: UtilityActionId::MaintainPost,
            bucket: UtilityBucket::Routine,
            target_key: 1,
            base_weight: MAINTAIN_POST_WEIGHT,
            considerations: vec![
                Consideration {
                    fact: UtilityFactId::CanAct,
                    curve: ResponseCurve::Linear,
                    weight: 1.0,
                },
                Consideration {
                    fact: UtilityFactId::HasTarget,
                    curve: ResponseCurve::Linear,
                    weight: 1.0,
                },
                Consideration {
                    fact: UtilityFactId::DutyPressure,
                    curve: ResponseCurve::Linear,
                    weight: 1.0,
                },
            ],
        };
        // A real ticket, scored the way `select_reference_actions` scores one.
        let job = UtilityCandidate {
            action: UtilityActionId::PerformJob,
            bucket: UtilityBucket::Routine,
            target_key: 2,
            base_weight: Normalized::new(LOWEST_AUTHORED_URGENCY).expect("in range"),
            considerations: vec![Consideration {
                fact: UtilityFactId::CanAct,
                curve: ResponseCurve::Linear,
                weight: 1.0,
            }],
        };

        let (standing, working) = (maintain.score(&facts).get(), job.score(&facts).get());
        assert!(
            working > standing,
            "standing at a post scored {standing} against the least urgent real \
             work at {working}, so a department that authors modest urgencies \
             can never be staffed",
        );
    }

    /// Everyone falling back to one department point needs their own place.
    ///
    /// Pinned against the *real* rosters rather than an invented one, because
    /// the bug this replaces was invisible to any synthetic case. The first
    /// version spread bodies by golden angle on `seed % 360`, which looks evenly
    /// distributed and is not — two Medical seeds landed on nearly the same ray
    /// and put four crew **0.08 m** apart, inside a 0.54 m clearance, so they
    /// stood in each other and `npc_motion` refused the last step.
    ///
    /// Falsifies rank-based slotting: derive the angle from the name or seed
    /// instead of the roster rank and Medical collapses again.
    #[test]
    fn a_department_falling_back_to_its_own_point_does_not_stand_in_itself() {
        for domain in JobDomain::ALL {
            let roster = roster_of(domain);
            let places: Vec<Vec3> = roster
                .core
                .iter()
                .chain(roster.support.iter())
                .filter_map(|name| roster.standing_slot(name))
                .map(|(slot, occupants)| {
                    department_spread(slot, occupants, DEPARTMENT_SPREAD_RADIUS)
                })
                .collect();
            assert_eq!(
                places.len(),
                roster.core.len() + roster.support.len(),
                "{domain:?} lost a resident between the roster and the ring",
            );
            for (index, here) in places.iter().enumerate() {
                for there in &places[index + 1..] {
                    let gap = crate::nav::flat_distance(*here, *there);
                    assert!(
                        gap >= crate::npc_motion::CLEARANCE,
                        "{domain:?} places two residents {gap:.2}m apart, inside \
                         the {:.2}m two bodies need, so they stand in each other",
                        crate::npc_motion::CLEARANCE,
                    );
                }
            }
        }
    }

    /// The fallback must name the same spot every time it is asked.
    ///
    /// Selection hysteresis holds an action across ticks by comparing
    /// `ActionKey`, and a destination redrawn at random would give the same
    /// intention a new identity on every decision — nothing could ever be
    /// held, and the resident would re-pick their way around the room forever.
    /// This is why the fallback uses route length rather than
    /// `CrewPosts::random_duty`.
    ///
    /// Falsifies determinism: swap `nearest_reachable` for `random_duty` and
    /// the chosen point stops being stable across repeated selections.
    #[test]
    fn a_duty_fallback_target_is_stable_across_decisions() {
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        let nav = crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS);
        let mut posts = CrewPosts::default();
        // Several candidates, so a random pick would almost surely differ.
        for offset in [2.0_f32, 3.0, 4.0, 5.0, 6.0] {
            posts.add_duty(Vec3::new(offset, 0.0, 0.0), Quat::IDENTITY);
        }

        let from = Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0);
        let pick = || {
            nav.nearest_reachable(from, posts.duty_points().map(|point| (point, point)))
                .map(|(point, _)| point)
        };

        let first = pick().expect("an authored duty post is reachable");
        for _ in 0..16 {
            assert_eq!(
                pick(),
                Some(first),
                "the duty fallback moved between decisions, so hysteresis can \
                 never hold the action",
            );
        }
    }

    /// A worker sent to another person must actually get there.
    ///
    /// `npc_motion` refuses any step that would bring two crew within
    /// [`crate::npc_motion::CLEARANCE`], so a walk that only counts as arrived
    /// inside [`AT_TARGET_DISTANCE`] can never finish when the destination is a
    /// body. In play that produced a permanent loop: Medical's responder walked
    /// to the casualty, circled it for the full errand deadline, reported
    /// `Unreachable`, and the next responder claimed the same ticket and did the
    /// same thing — so the patient was never treated and the incident behind
    /// them never resolved.
    ///
    /// **This test inserts `NpcMotion` on purpose.** It is an optional resource,
    /// and every harness that walks someone to a person had been leaving it out,
    /// which models a station where people can stand inside one another. That
    /// omission is why 1,500 passing tests never saw this.
    #[test]
    fn a_worker_sent_to_another_body_arrives_despite_body_spacing() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<ResolutionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<crate::npc_motion::NpcMotion>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::npc_motion::snapshot,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    ApplyDeferred,
                    record_resolutions,
                )
                    .chain(),
            );

        let bystander = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Miner Sato".into(),
                    role: "Cargo".into(),
                },
                Transform::from_xyz(2.0, crate::crew::BODY_OFFSET, 0.0),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
            ))
            .id();
        let worker = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Dr. Vance".into(),
                    role: "Medical".into(),
                },
                Transform::from_xyz(-2.0, crate::crew::BODY_OFFSET, 0.0),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(23, 0)),
                NpcJobProfile::new(
                    JobDomain::Medical,
                    NarrativeTier::Core,
                    [JobCapability::new("medical.response")],
                ),
            ))
            .id();
        app.world_mut()
            .resource_mut::<JobBoard>()
            .publish(JobTicket {
                id: JobTicketId(77),
                domain: JobDomain::Medical,
                kind: "medical.reach_a_person".into(),
                target: ActionTarget::Entity(bystander),
                // Unsubjected on purpose: this test is about the walk, not
                // about perception, and a subject would gate the claim.
                subject: None,
                reservation: ReservationKey("medical.reach_a_person".into()),
                reservation_capacity: 1,
                bucket: UtilityBucket::Routine,
                urgency: Normalized::ONE,
                required_capability: JobCapability::new("medical.response"),
                created_at: 0.0,
                deadline: None,
                risk: Normalized::ZERO,
                perform_seconds: 1.0,
                state: JobTicketState::Available,
            })
            .unwrap();

        let mut outcome = None;
        for _ in 0..600 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            outcome = app
                .world()
                .resource::<ResolutionLog>()
                .0
                .iter()
                .find(|resolution| resolution.key.action == UtilityActionId::PerformJob)
                .map(|resolution| resolution.result);
            if outcome.is_some() {
                break;
            }
        }

        assert_eq!(
            outcome,
            Some(ActionResult::Completed),
            "a worker walking to a person must arrive; body spacing holds them \
             {} apart, so an action that only counts arrival inside {} can never \
             finish",
            crate::npc_motion::CLEARANCE,
            AT_TARGET_DISTANCE,
        );
        let apart = app
            .world()
            .get::<Transform>(worker)
            .unwrap()
            .translation
            .distance(app.world().get::<Transform>(bystander).unwrap().translation);
        assert!(
            apart >= crate::npc_motion::CLEARANCE - 0.001,
            "spacing was not actually in force — they ended {apart} apart, so \
             this test proved nothing",
        );
    }

    /// Spawns one more crew member into an already-built test app.
    fn spawn_worker(app: &mut App, name: &str, seed: u64, at: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: name.into(),
                    role: "Botany".into(),
                },
                Transform::from_translation(at),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(seed, 0)),
                NpcJobProfile::new(
                    JobDomain::Botany,
                    NarrativeTier::Support,
                    [JobCapability::new("botany.tend")],
                ),
            ))
            .id()
    }

    fn failures_by(app: &App, agent: Entity) -> usize {
        app.world()
            .resource::<ResolutionLog>()
            .0
            .iter()
            .filter(|resolution| {
                resolution.agent == agent
                    && resolution.result == ActionResult::ReservationUnavailable
            })
            .count()
    }

    /// A workstation that is already taken stops being proposed.
    ///
    /// Botany publishes one ticket per plot but keys them all to the single
    /// shared tending spot, so the board advertised four jobs when one was
    /// physically possible. The three losers re-proposed and failed on every
    /// decision tick, forever: **1,983 `ReservationUnavailable` in one
    /// 580-second trace**, roughly a third of everything the station did.
    ///
    /// The second worker is spawned only after the first is established, so
    /// this pins the steady state rather than the harmless same-frame race
    /// where two workers both see a free slot and one loses it.
    #[test]
    fn a_taken_workstation_stops_being_proposed_as_a_job() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<ResolutionLog>()
            .init_resource::<JobBoard>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    ApplyDeferred,
                    record_resolutions,
                )
                    .chain(),
            );

        // Two plots, one physical spot between them — Botany's exact shape.
        let plot = Vec3::new(2.0, 0.0, 0.0);
        for id in [101u64, 102] {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(JobTicket {
                    id: JobTicketId(id),
                    domain: JobDomain::Botany,
                    kind: format!("botany.tend.{id}"),
                    target: ActionTarget::Point(plot),
                    subject: None,
                    reservation: ReservationKey("utility.spot.botany.tend".into()),
                    reservation_capacity: 1,
                    bucket: UtilityBucket::Routine,
                    urgency: Normalized::ONE,
                    required_capability: JobCapability::new("botany.tend"),
                    created_at: 0.0,
                    deadline: None,
                    risk: Normalized::ZERO,
                    // Long enough that the first worker still holds the slot
                    // for the whole of the second worker's run below.
                    perform_seconds: 60.0,
                    state: JobTicketState::Available,
                })
                .unwrap();
        }

        let first = spawn_worker(
            &mut app,
            "Grower Chen",
            41,
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );
        for _ in 0..120 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        }
        assert!(
            app.world()
                .resource::<ReservationBook>()
                .claims_on(&ReservationKey("utility.spot.botany.tend".into()))
                == 1,
            "the first worker never took the tending spot, so the second one \
             below is not actually being denied anything"
        );

        let second = spawn_worker(
            &mut app,
            "Agronomist Vale",
            97,
            Vec3::new(-3.0, crate::crew::BODY_OFFSET, 0.0),
        );
        for _ in 0..200 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        }

        assert_eq!(
            failures_by(&app, second),
            0,
            "the second worker kept being proposed a spot that is taken",
        );
        assert!(
            app.world()
                .resource::<ResolutionLog>()
                .0
                .iter()
                .any(|resolution| resolution.agent == second),
            "the second worker resolved nothing at all, so a zero failure \
             count proves nothing about what it was offered",
        );
        assert!(failures_by(&app, first) == 0);
    }

    /// An object resting on a surface has to be reachable.
    ///
    /// A body cannot change its own height — locomotion steps horizontally and
    /// rewrites y from the floor every step — so a 3D arrival radius is
    /// unsatisfiable for anything not sitting at exactly body height, and the
    /// errand only discovers it at the 45-second deadline. Botany drops produce
    /// at y 0.08 on a floor-level shelf; a walker's origin is `BODY_OFFSET`
    /// 0.93. That 0.85 m gap against a 0.3 m radius made **every** Service
    /// ingredient pickup permanently unreachable, retried by a fresh worker
    /// forever: one live trace has Cook Navarro failing the same ticket ten
    /// times and Attendant Mensah another nine.
    ///
    /// The 0.85 m offset here is the real one, not a round number, so this test
    /// fails if produce placement or `BODY_OFFSET` drifts back into conflict.
    #[test]
    fn a_worker_can_reach_an_item_resting_on_a_surface() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<ResolutionLog>()
            .init_resource::<JobBoard>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    ApplyDeferred,
                    record_resolutions,
                )
                    .chain(),
            );

        // Exactly where `botany::output_position` leaves a harvested item.
        let shelf_item = app
            .world_mut()
            .spawn(Transform::from_xyz(2.0, 0.08, 0.0))
            .id();
        app.world_mut()
            .resource_mut::<JobBoard>()
            .publish(JobTicket {
                id: JobTicketId(303),
                domain: JobDomain::Botany,
                kind: "service.intake".into(),
                target: ActionTarget::Entity(shelf_item),
                subject: None,
                reservation: ReservationKey("service.ingredient.303".into()),
                reservation_capacity: 1,
                bucket: UtilityBucket::Routine,
                urgency: Normalized::ONE,
                required_capability: JobCapability::new("botany.tend"),
                created_at: 0.0,
                deadline: None,
                risk: Normalized::ZERO,
                perform_seconds: 2.0,
                state: JobTicketState::Available,
            })
            .unwrap();
        let worker = spawn_worker(
            &mut app,
            "Cook Navarro",
            29,
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );

        let mut outcome = None;
        for _ in 0..900 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            outcome = app
                .world()
                .resource::<ResolutionLog>()
                .0
                .iter()
                .find(|r| r.agent == worker && r.key.action == UtilityActionId::PerformJob)
                .map(|r| r.result);
            if outcome.is_some() {
                break;
            }
        }
        assert_eq!(
            outcome,
            Some(ActionResult::Completed),
            "a worker could not collect an item resting {:.2}m below its own \
             origin, which is where every harvested ingredient sits",
            crate::crew::BODY_OFFSET - 0.08,
        );
    }

    /// Both workers at a capacity-2 spot have to be able to arrive at it.
    ///
    /// A shared spot is one coordinate, and only the first claimant can stand
    /// on it — body spacing holds the second `CLEARANCE` (0.72 m) away, which
    /// is further than `AT_TARGET_DISTANCE` (0.3 m). So the second orbits until
    /// the errand deadline, reports `Unreachable`, and the next claimant
    /// repeats it.
    ///
    /// Found in a live trace as a slow version of the reservation thrash: with
    /// the occupancy filter in, `Socialize` stopped failing instantly and began
    /// failing every 21-25 seconds instead, cycling through Alvarez, Odera and
    /// Imani. Six spots are authored at capacity 2 — both Bridge duty stations,
    /// both Security ones, the Service host table and the lounge — so this was
    /// quietly costing several departments half their staff.
    #[test]
    fn both_workers_at_a_shared_spot_can_reach_it() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<ResolutionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<crate::npc_motion::NpcMotion>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::npc_motion::snapshot,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    ApplyDeferred,
                    record_resolutions,
                )
                    .chain(),
            );

        // Two tickets, one shared spot at capacity 2 — the authored shape of
        // `bridge.briefing`, `security.desk` and the lounge.
        let spot = Vec3::new(2.0, 0.0, 0.0);
        for id in [201u64, 202] {
            app.world_mut()
                .resource_mut::<JobBoard>()
                .publish(JobTicket {
                    id: JobTicketId(id),
                    domain: JobDomain::Botany,
                    kind: format!("shared.spot.{id}"),
                    target: ActionTarget::Point(spot),
                    subject: None,
                    reservation: ReservationKey("utility.spot.shared".into()),
                    reservation_capacity: 2,
                    bucket: UtilityBucket::Routine,
                    urgency: Normalized::ONE,
                    required_capability: JobCapability::new("botany.tend"),
                    created_at: 0.0,
                    deadline: None,
                    risk: Normalized::ZERO,
                    perform_seconds: 4.0,
                    state: JobTicketState::Available,
                })
                .unwrap();
        }
        let first = spawn_worker(
            &mut app,
            "Grower Chen",
            13,
            Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0),
        );
        let second = spawn_worker(
            &mut app,
            "Agronomist Vale",
            71,
            Vec3::new(-3.0, crate::crew::BODY_OFFSET, 1.0),
        );

        for _ in 0..900 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        }

        for (who, agent) in [("first", first), ("second", second)] {
            let log = app.world().resource::<ResolutionLog>();
            let jobs: Vec<_> = log
                .0
                .iter()
                .filter(|r| r.agent == agent && r.key.action == UtilityActionId::PerformJob)
                .collect();
            assert!(
                jobs.iter().any(|r| r.result == ActionResult::Completed),
                "the {who} worker never completed the shared-spot job: {:?}",
                jobs.iter().map(|r| r.result).collect::<Vec<_>>(),
            );
            assert!(
                !jobs.iter().any(|r| r.result == ActionResult::Unreachable),
                "the {who} worker could not reach a spot it holds a claim on",
            );
        }
    }

    /// The same rule for offers, which is a separate code path that grew the
    /// same defect independently.
    ///
    /// `social::offer_company` offers the one gather spot to *everybody* who is
    /// lonely, so once the lounge fills the rest re-propose it and fail every
    /// tick — 306 `ReservationUnavailable` across twenty crew in 540 seconds,
    /// which became the single largest source of failure once Botany's was
    /// fixed.
    #[test]
    fn a_full_gathering_spot_stops_being_proposed_as_an_offer() {
        let (mut app, first) = opportunity_app();
        let lounge = Vec3::new(2.0, 0.0, 0.0);
        let key = ReservationKey("utility.spot.social.gather".into());

        // A provider republishes every frame, as a real one does, offering the
        // single spot to everybody it is given — which is the behaviour under
        // test, not a simplification of it.
        let offer_to = |app: &mut App, residents: &[Entity]| {
            let mut buffer = app.world_mut().resource_mut::<UtilityOpportunityBuffer>();
            buffer.clear();
            for resident in residents {
                buffer.offer(
                    UtilityOpportunity::new(
                        *resident,
                        UtilityActionId::Socialize,
                        UtilityBucket::Routine,
                        11,
                        Normalized::ONE,
                    )
                    .with_target(ActionTarget::Point(lounge))
                    .with_reservation(key.clone(), 1)
                    .with_timing(60.0, 120.0),
                );
            }
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        };

        // The first resident settles into the lounge alone, so what follows
        // pins the steady state rather than the harmless same-frame race where
        // two residents both see a free seat and one loses it.
        for _ in 0..120 {
            offer_to(&mut app, &[first]);
        }
        assert_eq!(
            app.world().resource::<ReservationBook>().claims_on(&key),
            1,
            "nobody took the gather spot, so the second resident below is not \
             actually being denied anything"
        );

        let second = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Second Lonely Resident".into(),
                    role: "Service".into(),
                },
                Transform::from_translation(Vec3::new(-3.0, crate::crew::BODY_OFFSET, 0.0)),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(59, 0)),
            ))
            .id();
        for _ in 0..200 {
            offer_to(&mut app, &[first, second]);
        }

        assert_eq!(
            app.world().resource::<ReservationBook>().claims_on(&key),
            1,
            "the gather spot should still be occupied"
        );
        assert_eq!(
            failures_by(&app, second),
            0,
            "the second resident kept being offered a lounge seat that is taken",
        );
        assert!(
            app.world()
                .resource::<ResolutionLog>()
                .0
                .iter()
                .any(|resolution| resolution.agent == second),
            "the second resident resolved nothing at all, so a zero failure \
             count proves nothing about what it was offered",
        );
    }

    /// Builds the minimum world in which one agent can consider offers.
    fn opportunity_app() -> (App, Entity) {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<UtilitySpots>()
            .init_resource::<NavGraph>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<ResolutionLog>()
            .init_resource::<UtilityOpportunityBuffer>()
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    ApplyDeferred,
                    record_resolutions,
                )
                    .chain(),
            );
        let agent = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Opportunity Subject".into(),
                    role: "Service".into(),
                },
                Transform::from_translation(Vec3::new(-2.0, crate::crew::BODY_OFFSET, 0.0)),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(31, 0)),
            ))
            .id();
        (app, agent)
    }

    /// An offered personal action must be selectable, travel, and resolve
    /// through the ordinary lifecycle — without becoming a job ticket and
    /// without a second decision controller.
    #[test]
    fn an_offered_personal_action_runs_the_ordinary_lifecycle() {
        let (mut app, agent) = opportunity_app();
        let table = Vec3::new(2.0, 0.0, 0.0);

        for _ in 0..240 {
            // A provider republishes its offer every frame, exactly as a real
            // one in `BuildContext` would.
            {
                let mut buffer = app.world_mut().resource_mut::<UtilityOpportunityBuffer>();
                buffer.clear();
                buffer.offer(
                    UtilityOpportunity::new(
                        agent,
                        UtilityActionId::EatFood,
                        UtilityBucket::Routine,
                        7,
                        Normalized::ONE,
                    )
                    .with_target(ActionTarget::Point(table))
                    .with_timing(1.0, 60.0),
                );
            }
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            if !app.world().resource::<ResolutionLog>().0.is_empty() {
                break;
            }
        }

        let resolutions = &app.world().resource::<ResolutionLog>().0;
        let eaten = resolutions
            .iter()
            .find(|resolution| resolution.key.action == UtilityActionId::EatFood)
            .expect("the offered action never resolved: {resolutions:?}");
        assert_eq!(eaten.agent, agent);
        assert_eq!(eaten.result, ActionResult::Completed);
        assert_eq!(eaten.key.target_key, 7);

        // It physically walked to the offered target rather than teleporting or
        // resolving in place.
        let at = app.world().get::<Transform>(agent).unwrap().translation;
        assert!(
            at.distance(table.with_y(crate::crew::BODY_OFFSET)) <= AT_TARGET_DISTANCE,
            "the agent resolved the offer at {at:?} instead of walking to {table:?}"
        );
        assert_eq!(
            app.world().get::<LocomotionOwner>(agent),
            Some(&LocomotionOwner::None)
        );
    }

    /// A zero consideration is how a provider states a hard precondition. It
    /// must veto the offer outright rather than merely ranking it low.
    #[test]
    fn a_zero_consideration_vetoes_an_offer_entirely() {
        /// Runs the identical offer, optionally carrying a consideration whose
        /// fact is never set and therefore reads zero.
        fn ran_offer(vetoed: bool) -> bool {
            let (mut app, agent) = opportunity_app();
            for _ in 0..60 {
                {
                    let mut buffer = app.world_mut().resource_mut::<UtilityOpportunityBuffer>();
                    buffer.clear();
                    let mut offer = UtilityOpportunity::new(
                        agent,
                        UtilityActionId::EatFood,
                        UtilityBucket::Emergency,
                        7,
                        Normalized::ONE,
                    )
                    .with_target(ActionTarget::Point(Vec3::new(2.0, 0.0, 0.0)))
                    .with_timing(0.5, 30.0);
                    if vetoed {
                        // The provider states a hard precondition: no food
                        // exists. `Opportunity` is never set, so it reads zero.
                        offer = offer.with_consideration(Consideration {
                            fact: UtilityFactId::Opportunity,
                            curve: ResponseCurve::Linear,
                            weight: 1.0,
                        });
                    }
                    buffer.offer(offer);
                }
                app.world_mut()
                    .resource_mut::<Time>()
                    .advance_by(std::time::Duration::from_secs_f32(0.1));
                app.update();
            }
            app.world()
                .resource::<ResolutionLog>()
                .0
                .iter()
                .any(|resolution| resolution.key.action == UtilityActionId::EatFood)
        }

        // The positive control: without the veto this exact offer is taken,
        // so the negative case below cannot pass for an unrelated reason.
        assert!(
            ran_offer(false),
            "the control offer was never selected, so this test proves nothing"
        );
        assert!(
            !ran_offer(true),
            "a zero consideration must veto the offer even in the Emergency bucket"
        );
    }

    /// The buffer is a per-frame proposal surface, not a request queue: an
    /// offer that stops being republished must stop being considered.
    #[test]
    fn offers_are_addressed_to_one_agent_and_do_not_persist() {
        let (mut app, agent) = opportunity_app();
        let other = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Bystander".into(),
                    role: "Service".into(),
                },
                Transform::from_translation(Vec3::new(-4.0, crate::crew::BODY_OFFSET, 0.0)),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(99, 0)),
            ))
            .id();

        app.world_mut()
            .resource_mut::<UtilityOpportunityBuffer>()
            .offer(
                UtilityOpportunity::new(
                    agent,
                    UtilityActionId::Rest,
                    UtilityBucket::Routine,
                    11,
                    Normalized::ONE,
                )
                .with_timing(0.5, 30.0),
            );
        assert_eq!(
            app.world()
                .resource::<UtilityOpportunityBuffer>()
                .for_agent(other)
                .count(),
            0,
            "an offer addressed to one agent must not be visible to another"
        );

        // Positive control: while the offer is republished every frame it is
        // taken, so the withdrawal case below cannot pass vacuously.
        for _ in 0..60 {
            {
                let mut buffer = app.world_mut().resource_mut::<UtilityOpportunityBuffer>();
                buffer.clear();
                buffer.offer(
                    UtilityOpportunity::new(
                        agent,
                        UtilityActionId::Rest,
                        UtilityBucket::Routine,
                        11,
                        Normalized::ONE,
                    )
                    .with_timing(0.5, 30.0),
                );
            }
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        }
        let rested_while_offered = app
            .world()
            .resource::<ResolutionLog>()
            .0
            .iter()
            .filter(|resolution| resolution.key.action == UtilityActionId::Rest)
            .count();
        assert!(
            rested_while_offered > 0,
            "the control offer was never selected, so this test proves nothing"
        );

        // Now withdraw it. The buffer is cleared every frame by the plugin's
        // BuildContext system; a provider that stops publishing is exactly this
        // condition.
        app.world_mut()
            .resource_mut::<UtilityOpportunityBuffer>()
            .clear();
        for _ in 0..60 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
        }

        assert_eq!(
            app.world()
                .resource::<ResolutionLog>()
                .0
                .iter()
                .filter(|resolution| resolution.key.action == UtilityActionId::Rest)
                .count(),
            rested_while_offered,
            "a withdrawn offer was still acted on, so the buffer is behaving \
             like a persistent request queue"
        );
    }
}
