//! Service production on the shared utility and chemistry contracts.
//!
//! Botany output remains a physical item until a Service worker reaches that
//! exact entity. Intake converts it into one bounded pantry lot while keeping
//! its real reagent mixture and private source provenance. Preparation creates
//! a physical, replicated meal batch; serving moves that exact batch to the
//! authored dining affordance; hosting and cleanup remain ordinary jobs on the
//! shared [`JobBoard`]. This module never selects an actor or moves one.

use std::collections::VecDeque;

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

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
use crate::containers::HeldBy;
use crate::crew::{Ambient, CrewMember, StationResident};
use crate::interaction::Interactable;
use crate::produce::{Produce, ProduceCatalog, ProduceId};

pub const SERVICE_SECOND_CORE: &str = "Steward Amari";
pub const SERVICE_SUPPORT: [&str; 2] = crate::crew::fluff::SERVICE_SUPPORT_NAMES;
const SERVICE_CORE: [&str; 2] = ["Chef Dubois", SERVICE_SECOND_CORE];

const INTAKE_CAPABILITY: &str = "service.ingredient_intake";
const PREPARE_CAPABILITY: &str = "service.prepare";
const SERVE_CAPABILITY: &str = "service.serve";
const HOST_CAPABILITY: &str = "service.host";
const CLEAN_CAPABILITY: &str = "service.clean";
const SERVICE_CAPABILITIES: [&str; 6] = [
    INTAKE_CAPABILITY,
    PREPARE_CAPABILITY,
    SERVE_CAPABILITY,
    HOST_CAPABILITY,
    CLEAN_CAPABILITY,
    super::aid::assessment_capability(JobDomain::Service),
];
pub(super) const SERVICE_ROSTER: DepartmentRoster = DepartmentRoster {
    domain: JobDomain::Service,
    core: &SERVICE_CORE,
    support: &SERVICE_SUPPORT,
    expected_support: 2,
    capabilities: &SERVICE_CAPABILITIES,
};

const PREP_SPOT: &str = "service.kitchen.prep";
const SERVING_SPOT: &str = "service.meal.pass";
const HOST_SPOT: &str = "service.table.host";
const CLEANUP_SPOT: &str = "service.cleanup";
const SERVICE_SPOTS: [&str; 4] = [PREP_SPOT, SERVING_SPOT, HOST_SPOT, CLEANUP_SPOT];

const MAX_PANTRY_LOTS: usize = 4;
const MAX_ACTIVE_BATCHES: usize = 4;
const SERVING_DOSE: chem_sim::Units = chem_sim::Units::whole(5);
const MAX_SERVINGS_PER_BATCH: u8 = 12;
const SERVICE_KITCHEN_BURN: &str = "service.kitchen.burn";
const SERVICE_PROBLEM_POLICY: DepartmentProblemPolicy = DepartmentProblemPolicy {
    domain: JobDomain::Service,
    opening_grace: 6,
    domain_cooldown: 8,
    actor_cooldown: 8,
    unresolved_cap: 1,
    shift_cap: 3,
};

/// Public recipe identity. It describes what diners can see, not the batch's
/// hidden reagents or source history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceRecipe {
    GardenPlate,
}

/// The public lifecycle of one physical batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MealStage {
    Prepared,
    Served,
    Empty,
}

/// Replicated meal presentation. Chemistry and provenance deliberately live in
/// the separate authority-only [`MealChemistry`] component.
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MealBatch {
    pub id: u64,
    pub recipe: ServiceRecipe,
    pub stage: MealStage,
    pub servings_remaining: u8,
    pub hosted: bool,
    /// Coarse player-facing preparation quality. This cannot reveal a hidden
    /// contaminant because it is fixed when the legitimate ingredients cook.
    pub quality_percent: u8,
}

/// Private source record for a legitimate ingredient. Keeping the consumed
/// entity identity makes later investigation and contamination attribution
/// possible without exposing it through replication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MealIngredientProvenance {
    pub source_entity: Entity,
    pub produce: ProduceId,
    pub source_plot: String,
    pub hazardous: bool,
}

/// Authority-only contents of a physical meal. Future player-delivered
/// additions must transfer into this real `Solution` and extend provenance;
/// Service itself never creates an illicit reagent.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct MealChemistry {
    pub solution: chem_sim::Solution,
    pub ingredients: Vec<MealIngredientProvenance>,
    pub prepared_by: Entity,
    pub prepared_at: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct ServiceIngredientLot {
    id: u64,
    solution: chem_sim::Solution,
    provenance: MealIngredientProvenance,
}

/// Bounded authority-side Service facts suitable for a future crew menu.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct ServiceWorkState {
    pantry: VecDeque<ServiceIngredientLot>,
    next_lot_id: u64,
    next_batch_id: u64,
    pub active_batches: usize,
    pub ingredients_received: u32,
    pub batches_prepared: u32,
    pub batches_served: u32,
    pub batches_hosted: u32,
    pub batches_cleaned: u32,
}

impl Default for ServiceWorkState {
    fn default() -> Self {
        Self {
            pantry: VecDeque::new(),
            next_lot_id: 1,
            next_batch_id: 1,
            active_batches: 0,
            ingredients_received: 0,
            batches_prepared: 0,
            batches_served: 0,
            batches_hosted: 0,
            batches_cleaned: 0,
        }
    }
}

impl ServiceWorkState {
    pub fn pantry_lots(&self) -> usize {
        self.pantry.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceJobKind {
    IntakeIngredient,
    PrepareMeal,
    ServeMeal,
    HostTable,
    CleanBatch,
}

impl ServiceJobKind {
    const ALL: [Self; 5] = [
        Self::IntakeIngredient,
        Self::PrepareMeal,
        Self::ServeMeal,
        Self::HostTable,
        Self::CleanBatch,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::IntakeIngredient => "service.intake",
            Self::PrepareMeal => "service.prepare",
            Self::ServeMeal => "service.serve",
            Self::HostTable => "service.host",
            Self::CleanBatch => "service.clean",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    fn capability(self) -> &'static str {
        match self {
            Self::IntakeIngredient => INTAKE_CAPABILITY,
            Self::PrepareMeal => PREPARE_CAPABILITY,
            Self::ServeMeal => SERVE_CAPABILITY,
            Self::HostTable => HOST_CAPABILITY,
            Self::CleanBatch => CLEAN_CAPABILITY,
        }
    }

    fn urgency(self) -> Normalized {
        let value = match self {
            Self::IntakeIngredient => 0.98,
            Self::PrepareMeal => 0.99,
            Self::ServeMeal => 1.0,
            Self::HostTable => 0.94,
            Self::CleanBatch => 0.97,
        };
        Normalized::new(value).expect("Service urgency is normalized")
    }

    fn perform_seconds(self) -> f32 {
        match self {
            Self::IntakeIngredient => 3.0,
            Self::PrepareMeal => 7.0,
            Self::ServeMeal => 4.0,
            Self::HostTable => 4.5,
            Self::CleanBatch => 4.0,
        }
    }

    fn risk(self) -> Normalized {
        Normalized::new(if self == Self::PrepareMeal { 0.08 } else { 0.0 })
            .expect("Service risk is normalized")
    }
}

/// Resolution detail used by Service summaries and its bounded kitchen risk.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct ServiceJobCompleted {
    pub worker: Entity,
    pub kind: String,
    pub ingredient: Option<ProduceId>,
    pub batch: Option<Entity>,
    pub risk: Normalized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MealConsumptionError {
    NotServed,
    Empty,
}

/// Result of transferring one serving through the body's canonical ingestion
/// path. Autonomous hunger selection is intentionally a later shared-selector
/// seam; this function is the complete consequence boundary it will call.
#[derive(Clone, Debug, PartialEq)]
pub struct MealConsumption {
    pub offered: chem_sim::Units,
    pub exposure: chem_sim::ExposureReport,
    pub batch_empty: bool,
}

/// Consumes one serving from a physical served batch into one exact body.
///
/// No reagent is synthesized here. The dose is split from the batch's private
/// `Solution`, then passed through `Bloodstream::receive`, preserving route,
/// absorption, reactions, contact effects, and later metabolism consequences.
pub fn consume_meal_serving(
    batch: &mut MealBatch,
    chemistry: &mut MealChemistry,
    body: &mut crate::body::Body,
    blood: &mut crate::body::Bloodstream,
    data: &chem_sim::ChemData,
) -> Result<MealConsumption, MealConsumptionError> {
    if batch.stage != MealStage::Served {
        return Err(MealConsumptionError::NotServed);
    }
    if batch.servings_remaining == 0 || chemistry.solution.is_empty() {
        batch.servings_remaining = 0;
        batch.stage = MealStage::Empty;
        return Err(MealConsumptionError::Empty);
    }

    let mut dose = chemistry.solution.split(SERVING_DOSE);
    let offered = dose.total_volume();
    if !offered.is_positive() {
        batch.servings_remaining = 0;
        batch.stage = MealStage::Empty;
        return Err(MealConsumptionError::Empty);
    }
    let exposure = blood
        .0
        .receive(&mut dose, chem_sim::Route::Ingested, &mut body.0, data);
    batch.servings_remaining = batch.servings_remaining.saturating_sub(1);
    if batch.servings_remaining == 0 || chemistry.solution.is_empty() {
        batch.servings_remaining = 0;
        batch.stage = MealStage::Empty;
    }
    Ok(MealConsumption {
        offered,
        exposure,
        batch_empty: batch.stage == MealStage::Empty,
    })
}

impl DepartmentProblemDirector {
    pub fn force_next_service_burn(&mut self) {
        self.force_next(JobDomain::Service, SERVICE_KITCHEN_BURN);
    }
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<ServiceWorkState>()
        .add_message::<ServiceJobCompleted>()
        .replicate::<MealBatch>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_service_work.run_if(crate::net::is_authority),
        )
        .add_systems(
            PreUpdate,
            activate_service_work
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            publish_service_tickets
                .in_set(super::UtilityAiSet::BuildContext)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            offer_meals_to_hungry_residents
                .in_set(super::OpportunityProviders)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (
                apply_service_job_results,
                apply_service_workplace_risk,
                apply_eaten_servings,
            )
                .chain()
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

/// How much hunger one serving relieves.
const SERVING_SATIETY: f32 = 0.45;
/// Below this, a resident would rather keep working than break for a meal.
const HUNGRY_ENOUGH_TO_EAT: f32 = 0.4;
const EAT_SECONDS: f32 = 3.0;

/// Offers every served meal to every hungry resident as a personal action.
///
/// This is the seam's first real consumer. Eating is deliberately *not* a job
/// ticket: it is not department work, any resident may do it regardless of
/// qualification, and it competes against that resident's own job rather than
/// being claimed off a shared board.
fn offer_meals_to_hungry_residents(
    mut buffer: ResMut<super::UtilityOpportunityBuffer>,
    meals: Query<(Entity, &MealBatch, &Transform)>,
    diners: Query<
        (Entity, &super::NpcNeeds, &crate::body::Bloodstream),
        (With<UtilityAgent>, Without<MealBatch>),
    >,
) {
    // Sorted so a fixed set of meals always produces the same offer order, and
    // therefore the same deterministic near-best selection.
    let mut served: Vec<_> = meals
        .iter()
        .filter(|(_, batch, _)| batch.stage == MealStage::Served && batch.servings_remaining > 0)
        .collect();
    if served.is_empty() {
        return;
    }
    served.sort_by_key(|(_, batch, _)| batch.id);

    for (diner, needs, blood) in &diners {
        if needs.hunger < HUNGRY_ENOUGH_TO_EAT || !super::fit_for_leisure(blood) {
            continue;
        }
        for (meal, batch, transform) in &served {
            // Hunger drives the appeal directly, so a starving resident
            // outbids routine work while a peckish one does not.
            // Hunger drives the appeal; a withdrawn body dampens it. Room
            // appeal deliberately does not apply here — a hungry resident eats
            // because they are hungry, not because the lounge looks pleasant.
            let scaled = needs.hunger * super::social_disposition(blood);
            let appeal = Normalized::new(scaled.clamp(0.0, 1.0))
                .expect("the scaled value is clamped to the normalized range");
            buffer.offer(
                super::UtilityOpportunity::new(
                    diner,
                    UtilityActionId::EatFood,
                    UtilityBucket::Routine,
                    stable_text_key(&format!("service.meal.{}", batch.id)),
                    appeal,
                )
                .with_target(ActionTarget::Point(transform.translation))
                // One seat per serving keeps two diners off the same portion.
                .with_reservation(
                    ReservationKey(format!("service.serving.{:016x}", meal.to_bits())),
                    1,
                )
                .with_timing(EAT_SECONDS, EAT_SECONDS + 60.0),
            );
        }
    }
}

/// Applies a finished meal to the exact diner who ate it.
///
/// The dose goes through the same `consume_meal_serving` path a player-facing
/// interaction would use, so contaminated food affects an NPC exactly as it
/// affects anyone else.
fn apply_eaten_servings(
    time: Res<Time>,
    mut results: MessageReader<UtilityActionResolved>,
    chem: Option<Res<crate::chem_data::ChemDb>>,
    contaminant: Option<Res<super::CovertContaminant>>,
    mut tampered: Option<ResMut<super::TamperedMeals>>,
    mut meals: Query<(Entity, &mut MealBatch, &mut MealChemistry)>,
    mut diners: Query<(
        &mut super::NpcNeeds,
        &mut crate::body::Body,
        &mut crate::body::Bloodstream,
    )>,
) {
    let data = chem.as_ref().map(|db| &db.0);
    let now = time.elapsed_secs();

    for result in results.read() {
        if result.key.action != UtilityActionId::EatFood || result.result != ActionResult::Completed
        {
            continue;
        }
        let Some(data) = data else {
            continue;
        };
        // The offer's target key names the batch, so a stale or foreign
        // resolution cannot consume a serving from the wrong meal.
        let Some((meal, mut batch, mut chemistry)) = meals.iter_mut().find(|(_, batch, _)| {
            stable_text_key(&format!("service.meal.{}", batch.id)) == result.key.target_key
        }) else {
            continue;
        };
        let Ok((mut needs, mut body, mut blood)) = diners.get_mut(result.agent) else {
            continue;
        };
        // What the contaminant looks like, if this meal was tampered with at
        // all. Measured *before* the serving so the dose that actually left the
        // bowl can be worked out afterwards.
        let contaminant = contaminant
            .as_deref()
            .map(|c| c.reagent)
            .filter(|_| tampered.as_deref().is_some_and(|t| t.culprit(meal).is_some()));
        let before = contaminant.map(|reagent| chemistry.solution.volume_of(reagent));

        if consume_meal_serving(&mut batch, &mut chemistry, &mut body, &mut blood, data).is_ok() {
            needs.relieve_hunger(SERVING_SATIETY);
            // Carry provenance from the bowl to the body. The dose itself is
            // now indistinguishable from any other exposure, so if this is not
            // recorded here the later poisoning has no way back to the act.
            //
            // Recorded with what actually moved rather than merely that the
            // meal was tampered with: an exposure has to be able to say a
            // *later* poisoning by something else is not its doing, and it
            // cannot do that without knowing what it put in them.
            if let (Some(tampered), Some(reagent), Some(before)) =
                (tampered.as_deref_mut(), contaminant, before)
            {
                let moved = before - chemistry.solution.volume_of(reagent);
                tampered.carry_to(result.agent, meal, reagent, moved, now);
            }
        }
    }
}

fn reset_service_work(
    mut commands: Commands,
    mut state: ResMut<ServiceWorkState>,
    mut problems: ResMut<DepartmentProblemDirector>,
    mut board: ResMut<JobBoard>,
    mut reservations: ResMut<super::ReservationBook>,
    mut incidents: ResMut<IncidentLedger>,
    batches: Query<Entity, With<MealBatch>>,
) {
    SERVICE_ROSTER
        .validate()
        .expect("the Service utility roster must be valid");
    *state = ServiceWorkState::default();
    problems.reset_domain(SERVICE_PROBLEM_POLICY);
    board.remove_domain(JobDomain::Service);
    incidents.remove_domain(JobDomain::Service);
    for spot in SERVICE_SPOTS {
        reservations.release_key(&ReservationKey(format!("utility.spot.{spot}")));
    }
    for batch in &batches {
        commands.entity(batch).despawn();
    }
}

fn service_profile(name: &str) -> Option<NpcJobProfile> {
    let tier = SERVICE_ROSTER.profile_for(name)?.narrative_tier;
    let capabilities: &[&str] = match name {
        "Chef Dubois" => &[PREPARE_CAPABILITY, CLEAN_CAPABILITY],
        SERVICE_SECOND_CORE => &[SERVE_CAPABILITY, HOST_CAPABILITY, CLEAN_CAPABILITY],
        "Cook Navarro" => &[INTAKE_CAPABILITY, PREPARE_CAPABILITY],
        "Attendant Mensah" => &[
            INTAKE_CAPABILITY,
            SERVE_CAPABILITY,
            HOST_CAPABILITY,
            CLEAN_CAPABILITY,
        ],
        _ => return None,
    };
    Some(NpcJobProfile::new(
        JobDomain::Service,
        tier,
        capabilities
            .iter()
            .map(|capability| JobCapability::new(*capability)),
    ))
}

fn activate_service_work(
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
        let Some((control, _)) = control_for_resident(&member.name, route, owner, SERVICE_ROSTER)
        else {
            continue;
        };
        let Some(profile) = service_profile(&member.name) else {
            continue;
        };
        commands
            .entity(entity)
            .insert((control, profile, StationResident));
    }
}

fn entity_ticket_id(kind: ServiceJobKind, entity: Entity) -> JobTicketId {
    JobTicketId(stable_text_key(&format!(
        "service.ticket.{}.{:016x}",
        kind.id(),
        entity.to_bits()
    )))
}

fn fact_ticket_id(kind: ServiceJobKind, fact_id: u64) -> JobTicketId {
    JobTicketId(stable_text_key(&format!(
        "service.ticket.{}.{}",
        kind.id(),
        fact_id
    )))
}

fn ticket(
    id: JobTicketId,
    kind: ServiceJobKind,
    target: ActionTarget,
    reservation: ReservationKey,
    capacity: usize,
    now: f32,
) -> JobTicket {
    JobTicket {
        id,
        domain: JobDomain::Service,
        kind: kind.id().into(),
        target,
        subject: None,
        reservation,
        reservation_capacity: capacity,
        bucket: UtilityBucket::Routine,
        urgency: kind.urgency(),
        required_capability: JobCapability::new(kind.capability()),
        created_at: now,
        deadline: None,
        risk: kind.risk(),
        perform_seconds: kind.perform_seconds(),
        state: JobTicketState::Available,
    }
}

fn publish_service_tickets(
    time: Res<Time>,
    state: Res<ServiceWorkState>,
    spots: Res<UtilitySpots>,
    catalog: Option<Res<ProduceCatalog>>,
    produce: Query<(
        Entity,
        &Produce,
        &super::BotanyProduceProvenance,
        Option<&HeldBy>,
    )>,
    meals: Query<(Entity, &MealBatch)>,
    mut board: ResMut<JobBoard>,
) {
    let now = time.elapsed_secs();
    let outstanding_intake = board
        .iter()
        .filter(|work| {
            work.domain == JobDomain::Service && work.kind == ServiceJobKind::IntakeIngredient.id()
        })
        .count();
    let intake_slots = MAX_PANTRY_LOTS.saturating_sub(state.pantry.len() + outstanding_intake);
    if intake_slots > 0 && catalog.is_some() {
        let catalog = catalog.as_deref().expect("checked above");
        let mut available: Vec<_> = produce
            .iter()
            .filter(|(_, item, provenance, held)| {
                held.is_none()
                    && !provenance.hazardous
                    && catalog.iter().any(|kind| kind.id == item.0)
            })
            .collect();
        available.sort_by_key(|(entity, ..)| entity.to_bits());
        for (entity, _, _, _) in available.into_iter().take(intake_slots) {
            let id = entity_ticket_id(ServiceJobKind::IntakeIngredient, entity);
            if board.ticket(id).is_some() {
                continue;
            }
            board
                .publish(ticket(
                    id,
                    ServiceJobKind::IntakeIngredient,
                    ActionTarget::Entity(entity),
                    ReservationKey(format!("service.ingredient.{:016x}", entity.to_bits())),
                    1,
                    now,
                ))
                .expect("Service checked the ingredient ticket before publishing");
        }
    }

    let outstanding_prepare = board
        .iter()
        .filter(|work| {
            work.domain == JobDomain::Service && work.kind == ServiceJobKind::PrepareMeal.id()
        })
        .count();
    let batch_slots = MAX_ACTIVE_BATCHES.saturating_sub(state.active_batches + outstanding_prepare);
    if let Some(prep) = spots.get(PREP_SPOT) {
        for lot in state.pantry.iter().take(batch_slots) {
            let id = fact_ticket_id(ServiceJobKind::PrepareMeal, lot.id);
            if board.ticket(id).is_some() {
                continue;
            }
            board
                .publish(ticket(
                    id,
                    ServiceJobKind::PrepareMeal,
                    ActionTarget::Point(prep.at),
                    ReservationKey(format!("utility.spot.{PREP_SPOT}")),
                    prep.capacity,
                    now,
                ))
                .expect("Service checked the pantry ticket before publishing");
        }
    }

    let mut meals: Vec<_> = meals.iter().collect();
    meals.sort_by_key(|(_, batch)| batch.id);
    for (entity, batch) in meals {
        let (kind, target, reservation, capacity) = match batch.stage {
            MealStage::Prepared => {
                let Some(spot) = spots.get(SERVING_SPOT) else {
                    continue;
                };
                (
                    ServiceJobKind::ServeMeal,
                    ActionTarget::Point(spot.at),
                    ReservationKey(format!("utility.spot.{SERVING_SPOT}")),
                    spot.capacity,
                )
            }
            MealStage::Served if !batch.hosted => {
                let Some(spot) = spots.get(HOST_SPOT) else {
                    continue;
                };
                (
                    ServiceJobKind::HostTable,
                    ActionTarget::Point(spot.at),
                    ReservationKey(format!("utility.spot.{HOST_SPOT}")),
                    spot.capacity,
                )
            }
            MealStage::Empty => {
                // Walk to the floor-authored cleanup spot, not to the batch.
                // A `Point` target is lifted to the walker's own height before
                // navigation; an `Entity` target keeps the entity's live
                // transform, and a spent plate sits on a table above the floor.
                // Targeting the batch directly leaves the arrival check
                // permanently unsatisfiable, so the ticket is claimed and never
                // resolved. The per-batch reservation below is what still binds
                // one worker to one plate.
                let Some(spot) = spots.get(CLEANUP_SPOT) else {
                    continue;
                };
                (
                    ServiceJobKind::CleanBatch,
                    ActionTarget::Point(spot.at),
                    ReservationKey(format!("service.batch.{:016x}", entity.to_bits())),
                    1,
                )
            }
            MealStage::Served => continue,
        };
        let id = entity_ticket_id(kind, entity);
        if board.ticket(id).is_some() {
            continue;
        }
        board
            .publish(ticket(id, kind, target, reservation, capacity, now))
            .expect("Service checked the batch ticket before publishing");
    }
}

fn solution_from_produce(kind: &crate::produce::ProduceKind) -> chem_sim::Solution {
    let mut solution = chem_sim::Solution::new(kind.total_yield());
    for (reagent, amount) in &kind.yields {
        let overflow = solution.add(*reagent, *amount);
        debug_assert!(overflow.is_zero(), "produce yield capacity was precomputed");
    }
    solution
}

fn servings_for(solution: &chem_sim::Solution) -> u8 {
    let raw = solution.total_volume().raw();
    let serving = SERVING_DOSE.raw();
    let servings = (raw + serving - 1) / serving;
    servings.clamp(1, i32::from(MAX_SERVINGS_PER_BATCH)) as u8
}

fn batch_position(base: Vec3, batch_id: u64) -> Vec3 {
    let column = (batch_id.saturating_sub(1) % 2) as f32;
    let row = ((batch_id.saturating_sub(1) / 2) % 2) as f32;
    base + Vec3::new((column - 0.5) * 0.24, 0.08, (row - 0.5) * 0.24)
}

#[allow(clippy::too_many_arguments)]
fn apply_service_job_results(
    mut commands: Commands,
    mut results: MessageReader<UtilityActionResolved>,
    mut completed: MessageWriter<ServiceJobCompleted>,
    time: Res<Time>,
    spots: Res<UtilitySpots>,
    catalog: Option<Res<ProduceCatalog>>,
    mut state: ResMut<ServiceWorkState>,
    mut board: ResMut<JobBoard>,
    mut witnessed: MessageWriter<super::Stimulus>,
    produce: Query<(&Produce, &super::BotanyProduceProvenance, Option<&HeldBy>)>,
    mut meals: Query<(Entity, &mut MealBatch, &mut MealChemistry, &mut Transform)>,
) {
    for result in results.read() {
        if result.key.action != UtilityActionId::PerformJob
            || result.result != ActionResult::Completed
        {
            continue;
        }
        let id = JobTicketId(result.key.target_key);
        let Some(work) = board.ticket(id).cloned() else {
            continue;
        };
        if work.domain != JobDomain::Service
            || work.state != JobTicketState::Completed(result.claim)
        {
            continue;
        }
        let Some(kind) = ServiceJobKind::from_id(&work.kind) else {
            board.cancel(id);
            continue;
        };

        let mut ingredient = None;
        let mut completed_batch = None;
        let applied = match kind {
            ServiceJobKind::IntakeIngredient => {
                let ActionTarget::Entity(source) = work.target else {
                    board.cancel(id);
                    continue;
                };
                let Ok((item, provenance, held)) = produce.get(source) else {
                    board.cancel(id);
                    continue;
                };
                if held.is_some() || provenance.hazardous || state.pantry.len() >= MAX_PANTRY_LOTS {
                    board.cancel(id);
                    continue;
                }
                let Some(produce_kind) = catalog
                    .as_deref()
                    .and_then(|catalog| catalog.iter().find(|kind| kind.id == item.0))
                else {
                    board.cancel(id);
                    continue;
                };
                if board.take_completed(id, result.claim).is_err() {
                    continue;
                }
                let lot_id = state.next_lot_id;
                state.next_lot_id = state.next_lot_id.saturating_add(1);
                state.pantry.push_back(ServiceIngredientLot {
                    id: lot_id,
                    solution: solution_from_produce(produce_kind),
                    provenance: MealIngredientProvenance {
                        source_entity: source,
                        produce: item.0,
                        source_plot: provenance.source_plot.clone(),
                        hazardous: provenance.hazardous,
                    },
                });
                state.ingredients_received = state.ingredients_received.saturating_add(1);
                ingredient = Some(item.0);
                commands.entity(source).despawn();
                true
            }
            ServiceJobKind::PrepareMeal => {
                let Some(lot_index) = state
                    .pantry
                    .iter()
                    .position(|lot| fact_ticket_id(kind, lot.id) == id)
                else {
                    board.cancel(id);
                    continue;
                };
                let Some(prep) = spots.get(PREP_SPOT) else {
                    board.cancel(id);
                    continue;
                };
                if state.active_batches >= MAX_ACTIVE_BATCHES {
                    board.cancel(id);
                    continue;
                }
                if board.take_completed(id, result.claim).is_err() {
                    continue;
                }
                let mut lot = state
                    .pantry
                    .remove(lot_index)
                    .expect("the pantry lot was rechecked above");
                lot.solution.temperature = chem_sim::Kelvin(330.0);
                let batch_id = state.next_batch_id;
                state.next_batch_id = state.next_batch_id.saturating_add(1);
                let servings = servings_for(&lot.solution);
                let quality_percent = (lot.solution.average_purity() * 100.0).round() as u8;
                let batch = commands
                    .spawn((
                        MealBatch {
                            id: batch_id,
                            recipe: ServiceRecipe::GardenPlate,
                            stage: MealStage::Prepared,
                            servings_remaining: servings,
                            hosted: false,
                            quality_percent,
                        },
                        MealChemistry {
                            solution: lot.solution,
                            ingredients: vec![lot.provenance],
                            prepared_by: result.agent,
                            prepared_at: time.elapsed_secs(),
                        },
                        Transform::from_translation(batch_position(prep.at, batch_id)),
                        Interactable::new("Garden plate"),
                        Replicated,
                        crate::until_we_leave_the_lab(),
                    ))
                    .id();
                state.active_batches += 1;
                state.batches_prepared = state.batches_prepared.saturating_add(1);
                completed_batch = Some(batch);
                true
            }
            ServiceJobKind::ServeMeal => {
                let Some(spot) = spots.get(SERVING_SPOT) else {
                    board.cancel(id);
                    continue;
                };
                let Some((entity, mut batch, _, mut transform)) =
                    meals.iter_mut().find(|(entity, batch, _, _)| {
                        entity_ticket_id(kind, *entity) == id && batch.stage == MealStage::Prepared
                    })
                else {
                    board.cancel(id);
                    continue;
                };
                if board.take_completed(id, result.claim).is_err() {
                    continue;
                }
                batch.stage = MealStage::Served;
                transform.translation = batch_position(spot.at, batch.id);
                state.batches_served = state.batches_served.saturating_add(1);
                // A meal reaching the serving spot is a public, ordinary event:
                // whoever is in the room sees food arrive, and that is all this
                // says. It carries no claim about the meal's contents, so a
                // clean batch and a contaminated one produce an identical
                // stimulus — the difference only surfaces later, through
                // symptoms and a Medical case. Emitting anything richer here
                // would hand every bystander an answer nobody in the room
                // actually has.
                witnessed.write(
                    super::Stimulus::new(super::StimulusKind::Food, transform.translation)
                        .about(entity)
                        .by(result.agent),
                );
                completed_batch = Some(entity);
                true
            }
            ServiceJobKind::HostTable => {
                let Some((entity, mut batch, _, _)) =
                    meals.iter_mut().find(|(entity, batch, _, _)| {
                        entity_ticket_id(kind, *entity) == id
                            && batch.stage == MealStage::Served
                            && !batch.hosted
                    })
                else {
                    board.cancel(id);
                    continue;
                };
                if board.take_completed(id, result.claim).is_err() {
                    continue;
                }
                batch.hosted = true;
                state.batches_hosted = state.batches_hosted.saturating_add(1);
                completed_batch = Some(entity);
                true
            }
            ServiceJobKind::CleanBatch => {
                let Some((entity, _batch, _, _)) =
                    meals.iter_mut().find(|(entity, batch, _, _)| {
                        entity_ticket_id(kind, *entity) == id && batch.stage == MealStage::Empty
                    })
                else {
                    board.cancel(id);
                    continue;
                };
                if board.take_completed(id, result.claim).is_err() {
                    continue;
                }
                completed_batch = Some(entity);
                commands.entity(entity).despawn();
                state.active_batches = state.active_batches.saturating_sub(1);
                state.batches_cleaned = state.batches_cleaned.saturating_add(1);
                true
            }
        };

        if applied {
            completed.write(ServiceJobCompleted {
                worker: result.agent,
                kind: kind.id().into(),
                ingredient,
                batch: completed_batch,
                risk: work.risk,
            });
        }
    }
}

fn apply_service_workplace_risk(
    time: Res<Time>,
    stability: Res<crate::instability::StationStability>,
    mut completions: MessageReader<ServiceJobCompleted>,
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
    problems.ensure_policy(SERVICE_PROBLEM_POLICY);

    struct PreparedServiceProblem {
        candidate: DepartmentProblemCandidate,
        location: Vec3,
    }

    let mut candidates = Vec::new();
    {
        let workers = workers.p0();
        for completion in completions.read() {
            if ServiceJobKind::from_id(&completion.kind) != Some(ServiceJobKind::PrepareMeal) {
                continue;
            }
            let Ok((member, transform)) = workers.get(completion.worker) else {
                continue;
            };
            candidates.push(PreparedServiceProblem {
                candidate: DepartmentProblemCandidate::new(
                    SERVICE_KITCHEN_BURN,
                    JobDomain::Service,
                    IncidentKind::Burn,
                    completion.worker,
                    completion.risk,
                    stable_text_key(&format!(
                        "{}:{}",
                        member.name,
                        completion.batch.map(Entity::to_bits).unwrap_or_default()
                    )),
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
        let severity = service_burn_severity_from_snapshot(snapshot);
        let Ok(id) = incidents.create(
            IncidentKind::Burn,
            JobDomain::Service,
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
            service_burn_damage_from_snapshot(snapshot),
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
            department: JobDomain::Service,
            subject,
            location: prepared.location,
            severity,
        });
    }
}

fn service_burn_severity_from_snapshot(snapshot: ProblemStabilitySnapshot) -> Normalized {
    service_burn_severity_from_health(snapshot.health())
}

fn service_burn_severity_from_health(health: f32) -> Normalized {
    let deterioration = 1.0 - health;
    Normalized::new(0.24 + deterioration * 0.28)
        .expect("the clamped Service burn severity is normalized")
}

fn service_burn_damage_from_snapshot(snapshot: ProblemStabilitySnapshot) -> chem_sim::Units {
    service_burn_damage_from_health(snapshot.health())
}

fn service_burn_damage_from_health(health: f32) -> chem_sim::Units {
    let deterioration = 1.0 - health;
    chem_sim::Units::whole(12 + (deterioration * 12.0).round() as i32)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::body::{Bloodstream, Body};
    use crate::crew::{CrewPosts, CrewRoute, ErrandResolved};
    use crate::utility_ai::{
        begin_reference_actions, consume_utility_arrivals, perform_reference_actions,
        resolve_reference_actions, select_reference_actions, tick_current_actions, NarrativeTier,
        ReservationBook, UtilityControlBundle, UtilityDecisionLog,
    };

    #[derive(Resource, Default)]
    struct CompletionLog(Vec<ServiceJobCompleted>);

    fn record_completions(
        mut messages: MessageReader<ServiceJobCompleted>,
        mut log: ResMut<CompletionLog>,
    ) {
        log.0.extend(messages.read().cloned());
    }

    /// Captures what the *production* path emitted.
    ///
    /// A `Stimulus` is consumed the same frame it is written, so a test that
    /// looked at the message bus after the fact would see nothing. This keeps
    /// them without a witness needing to exist.
    #[derive(Resource, Default)]
    struct StimulusLog(Vec<super::super::Stimulus>);

    fn record_stimuli(
        mut messages: MessageReader<super::super::Stimulus>,
        mut log: ResMut<StimulusLog>,
    ) {
        log.0.extend(messages.read().copied());
    }

    fn chemistry() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should parse")
    }

    fn produce_catalog(data: &chem_sim::ChemData) -> ProduceCatalog {
        let config: crate::produce::ProduceConfig =
            ron::from_str(include_str!("../../assets/data/station.produce.ron"))
                .expect("produce data should parse");
        ProduceCatalog::from_config(&config, &data.reagents)
    }

    fn insert_test_spots(app: &mut App) {
        for (index, id) in SERVICE_SPOTS.into_iter().enumerate() {
            app.world_mut().resource_mut::<UtilitySpots>().insert(
                id,
                Vec3::new(-4.0 + index as f32 * 2.0, 0.0, 0.0),
                if id == HOST_SPOT { 2 } else { 1 },
            );
        }
    }

    fn spawn_botany_output(
        app: &mut App,
        produce: ProduceId,
        hazardous: bool,
        position: Vec3,
    ) -> Entity {
        app.world_mut()
            .spawn((
                Produce(produce),
                super::super::BotanyProduceProvenance {
                    source_plot: format!("test.plot.{}", produce.0),
                    hazardous,
                },
                Transform::from_translation(position),
            ))
            .id()
    }

    #[test]
    fn authored_service_cast_has_distinct_tiers_and_qualifications() {
        SERVICE_ROSTER.validate().unwrap();
        let dubois = service_profile("Chef Dubois").unwrap();
        let amari = service_profile(SERVICE_SECOND_CORE).unwrap();
        let navarro = service_profile(SERVICE_SUPPORT[0]).unwrap();
        let mensah = service_profile(SERVICE_SUPPORT[1]).unwrap();

        assert_eq!(dubois.narrative_tier, NarrativeTier::Core);
        assert_eq!(amari.narrative_tier, NarrativeTier::Core);
        assert_eq!(navarro.narrative_tier, NarrativeTier::Support);
        assert_eq!(mensah.narrative_tier, NarrativeTier::Support);
        assert!(dubois.can_do(&JobCapability::new(PREPARE_CAPABILITY)));
        assert!(!dubois.can_do(&JobCapability::new(HOST_CAPABILITY)));
        assert!(amari.can_do(&JobCapability::new(HOST_CAPABILITY)));
        assert!(navarro.can_do(&JobCapability::new(INTAKE_CAPABILITY)));
        assert!(mensah.can_do(&JobCapability::new(SERVE_CAPABILITY)));
    }

    #[test]
    fn only_the_four_authored_service_workers_migrate() {
        let mut app = App::new();
        app.add_systems(Update, (activate_service_work, ApplyDeferred).chain());
        for name in [
            "Chef Dubois",
            SERVICE_SECOND_CORE,
            SERVICE_SUPPORT[0],
            SERVICE_SUPPORT[1],
            "Botanist Ivy",
        ] {
            app.world_mut().spawn((
                CrewMember {
                    name: name.into(),
                    role: "Service".into(),
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
        assert!(!names.contains("Botanist Ivy"));
        assert!(SERVICE_CORE.into_iter().all(|name| names.contains(name)));
        assert!(SERVICE_SUPPORT.into_iter().all(|name| names.contains(name)));
    }

    #[test]
    fn intake_is_bounded_and_rejects_held_or_hazardous_botany_output() {
        let mut app = App::new();
        let data = chemistry();
        app.init_resource::<Time>()
            .init_resource::<ServiceWorkState>()
            .init_resource::<UtilitySpots>()
            .init_resource::<JobBoard>()
            .insert_resource(produce_catalog(&data))
            .add_systems(Update, publish_service_tickets);
        insert_test_spots(&mut app);

        let mut safe = Vec::new();
        for index in 0..6 {
            safe.push(spawn_botany_output(
                &mut app,
                ProduceId(index % 4),
                false,
                Vec3::new(index as f32, 0.0, 0.0),
            ));
        }
        let hazardous = spawn_botany_output(&mut app, ProduceId(12), true, Vec3::X * 8.0);
        let held = spawn_botany_output(&mut app, ProduceId(3), false, Vec3::X * 9.0);
        let holder = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(held).insert(HeldBy(holder));

        for _ in 0..10 {
            app.update();
        }

        let board = app.world().resource::<JobBoard>();
        let intakes: Vec<_> = board
            .iter()
            .filter(|work| work.kind == ServiceJobKind::IntakeIngredient.id())
            .collect();
        assert_eq!(intakes.len(), MAX_PANTRY_LOTS);
        assert!(intakes.iter().all(|work| {
            matches!(work.target, ActionTarget::Entity(entity) if safe.contains(&entity))
        }));
        assert!(intakes.iter().all(|work| {
            !matches!(work.target, ActionTarget::Entity(entity) if entity == hazardous || entity == held)
        }));
    }

    #[test]
    fn four_workers_run_the_bounded_physical_ingredient_to_cleanup_pipeline() {
        let mut app = App::new();
        let data = chemistry();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<CompletionLog>()
            .init_resource::<StimulusLog>()
            .init_resource::<ServiceWorkState>()
            .insert_resource(produce_catalog(&data))
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_message::<ServiceJobCompleted>()
            .add_message::<super::super::Stimulus>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    publish_service_tickets,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    apply_service_job_results,
                    ApplyDeferred,
                    record_completions,
                    record_stimuli,
                )
                    .chain(),
            );
        insert_test_spots(&mut app);

        for (index, produce) in [ProduceId(0), ProduceId(1), ProduceId(2), ProduceId(3)]
            .into_iter()
            .enumerate()
        {
            spawn_botany_output(
                &mut app,
                produce,
                false,
                Vec3::new(-3.5 + index as f32 * 2.0, crate::crew::BODY_OFFSET, -2.0),
            );
        }
        for (index, name) in SERVICE_CORE.into_iter().chain(SERVICE_SUPPORT).enumerate() {
            app.world_mut().spawn((
                CrewMember {
                    name: name.into(),
                    role: "Service".into(),
                },
                Transform::from_xyz(-3.0 + index as f32 * 2.0, crate::crew::BODY_OFFSET, -3.0),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                UtilityControlBundle::new(UtilityAgent::new(900 + index as u64, 0)),
                service_profile(name).unwrap(),
            ));
        }

        let mut largest_board = 0;
        for _ in 0..7_000 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            largest_board = largest_board.max(app.world().resource::<JobBoard>().len());
            let state = app.world().resource::<ServiceWorkState>();
            if state.batches_hosted == 4 && app.world().resource::<JobBoard>().is_empty() {
                break;
            }
        }

        // Serving a meal is perceivable. Asserted inside the real pipeline
        // rather than by hand-writing a `Stimulus`: an earlier wave shipped a
        // hazard-emission test that wrote its own stimulus and therefore proved
        // only that the message bus worked. This reads what the serve path
        // actually emitted.
        {
            let served_food = app
                .world()
                .resource::<StimulusLog>()
                .0
                .iter()
                .filter(|stimulus| stimulus.kind == super::super::StimulusKind::Food)
                .count();
            assert_eq!(
                served_food, 4,
                "each of the four meals reaching the serving spot is a perceivable event"
            );

            // And it says nothing about the contents. These meals are clean;
            // `covert.rs` contaminates a meal without touching this path, so a
            // poisoned batch produces a byte-identical `Food` stimulus. If this
            // ever carried a purity or ingredient field, every bystander would
            // silently gain an answer nobody in the room actually has.
            let log = app.world().resource::<StimulusLog>();
            assert!(
                log.0
                    .iter()
                    .filter(|stimulus| stimulus.kind == super::super::StimulusKind::Food)
                    .all(|stimulus| stimulus.strength == 1.0 && stimulus.subject.is_some()),
                "serving tells a witness that food arrived and which batch, and nothing more"
            );
        }

        {
            let state = app.world().resource::<ServiceWorkState>();
            assert_eq!(state.ingredients_received, 4);
            assert_eq!(state.batches_prepared, 4);
            assert_eq!(state.batches_served, 4);
            assert_eq!(state.batches_hosted, 4);
            assert_eq!(state.pantry_lots(), 0);
            assert_eq!(state.active_batches, MAX_ACTIVE_BATCHES);
            assert!(largest_board <= 4);
        }
        {
            let world = app.world_mut();
            let mut meals = world.query::<(&MealBatch, &MealChemistry, &Transform)>();
            let physical: Vec<_> = meals
                .iter(world)
                .map(|(batch, chemistry, transform)| {
                    (
                        batch.stage,
                        batch.hosted,
                        chemistry.solution.total_volume(),
                        chemistry.ingredients.len(),
                        transform.translation,
                    )
                })
                .collect();
            assert_eq!(physical.len(), MAX_ACTIVE_BATCHES);
            assert!(physical.iter().all(|(stage, hosted, volume, sources, at)| {
                *stage == MealStage::Served
                    && *hosted
                    && volume.is_positive()
                    && *sources == 1
                    && at.is_finite()
            }));
        }

        // The future hunger candidate calls the same public consequence seam.
        // Consume every physical batch, then let the existing job lifecycle
        // send workers to those exact empty entities for cleanup.
        {
            let world = app.world_mut();
            let mut diner_body = Body::default();
            let mut diner_blood = Bloodstream::default();
            let mut meals = world.query::<(&mut MealBatch, &mut MealChemistry)>();
            for (mut batch, mut contents) in meals.iter_mut(world) {
                while batch.stage == MealStage::Served {
                    consume_meal_serving(
                        &mut batch,
                        &mut contents,
                        &mut diner_body,
                        &mut diner_blood,
                        &data,
                    )
                    .unwrap();
                }
            }
        }
        for _ in 0..3_000 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            let state = app.world().resource::<ServiceWorkState>();
            if state.batches_cleaned == 4 && app.world().resource::<JobBoard>().is_empty() {
                break;
            }
        }

        let state = app.world().resource::<ServiceWorkState>();
        assert_eq!(
            state.batches_cleaned,
            4,
            "cleanup stalled; tickets={:?}; completions={:?}",
            app.world()
                .resource::<JobBoard>()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            app.world().resource::<CompletionLog>().0,
        );
        assert_eq!(state.active_batches, 0);
        assert!(app.world().resource::<JobBoard>().is_empty());
        assert_eq!(
            app.world()
                .resource::<ReservationBook>()
                .active_claim_count(),
            0
        );
        let workers: HashSet<_> = app
            .world()
            .resource::<CompletionLog>()
            .0
            .iter()
            .map(|event| event.worker)
            .collect();
        assert_eq!(
            workers.len(),
            4,
            "Service did not distribute work across all four residents: {:?}",
            app.world().resource::<CompletionLog>().0
        );
    }

    /// Cleanup used to target the batch entity itself. A spent plate sits on a
    /// table, and only `ActionTarget::Point` is lifted to the walker's own
    /// height before navigation, so the arrival check could never be satisfied:
    /// the ticket was claimed and then held forever, stalling the whole shift.
    ///
    /// This asserts the publisher's output directly, so it fails the moment the
    /// target reverts to the batch entity — the end-to-end pipeline test proves
    /// the same bug, but only after a 3000-tick run.
    #[test]
    fn cleanup_walks_to_the_floor_spot_and_still_reserves_the_exact_batch() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<ServiceWorkState>()
            .add_systems(Update, publish_service_tickets);
        insert_test_spots(&mut app);

        // A spent plate, sitting on the table exactly where `batch_position`
        // puts it — above the floor the walker is pinned to.
        let plate_at = Vec3::new(-4.0, crate::crew::BODY_OFFSET + 0.08, 0.5);
        let batch = app
            .world_mut()
            .spawn((
                MealBatch {
                    id: 1,
                    recipe: ServiceRecipe::GardenPlate,
                    stage: MealStage::Empty,
                    servings_remaining: 0,
                    hosted: true,
                    quality_percent: 100,
                },
                Transform::from_translation(plate_at),
            ))
            .id();
        app.update();

        let board = app.world().resource::<JobBoard>();
        let cleanup = board
            .iter()
            .find(|ticket| ticket.kind == ServiceJobKind::CleanBatch.id())
            .expect("a spent batch publishes exactly one cleanup ticket");

        // The walk target must be the floor-authored spot. Targeting the plate
        // entity leaves the arrival check unsatisfiable and strands the ticket.
        let cleanup_spot = app
            .world()
            .resource::<UtilitySpots>()
            .get(CLEANUP_SPOT)
            .expect("the cleanup spot is authored")
            .at;
        assert_eq!(
            cleanup.target,
            ActionTarget::Point(cleanup_spot),
            "cleanup must walk to the floor-authored spot, never to the raised plate"
        );
        assert_ne!(cleanup.target, ActionTarget::Entity(batch));

        // Routing every plate to one shared spot must not collapse the
        // per-batch claim: the reservation still names this exact entity at
        // capacity one, so one worker takes one plate.
        assert_eq!(
            cleanup.reservation,
            ReservationKey(format!("service.batch.{:016x}", batch.to_bits()))
        );
        assert_eq!(cleanup.reservation_capacity, 1);
    }

    /// The opportunity seam's first end-to-end proof: hunger, not a job ticket,
    /// makes a resident walk to a real meal and ingest a real dose.
    #[test]
    fn a_hungry_resident_chooses_to_eat_and_the_dose_reaches_their_bloodstream() {
        let data = chemistry();
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .insert_resource(super::super::NeedsTuning::authored().clone())
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<crate::utility_ai::UtilityOpportunityBuffer>()
            .insert_resource(crate::chem_data::ChemDb(chemistry()))
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    crate::utility_ai::advance_npc_needs,
                    crate::utility_ai::clear_opportunity_buffer,
                    offer_meals_to_hungry_residents,
                    tick_current_actions,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    apply_eaten_servings,
                )
                    .chain(),
            );

        let mut solution = chem_sim::Solution::new(chem_sim::Units::whole(40));
        assert!(solution
            .add(data.reagent("plant_fibre"), chem_sim::Units::whole(40))
            .is_zero());
        let meal_at = Vec3::new(2.0, 0.0, 0.0);
        let meal = app
            .world_mut()
            .spawn((
                MealBatch {
                    id: 3,
                    recipe: ServiceRecipe::GardenPlate,
                    stage: MealStage::Served,
                    servings_remaining: 4,
                    hosted: true,
                    quality_percent: 100,
                },
                MealChemistry {
                    solution,
                    ingredients: Vec::new(),
                    prepared_by: Entity::from_bits(1),
                    prepared_at: 0.0,
                },
                Transform::from_translation(meal_at),
            ))
            .id();

        let mut bundle = UtilityControlBundle::new(UtilityAgent::new(77, 0));
        bundle.needs.hunger = 0.9;
        let diner = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: SERVICE_SUPPORT[0].into(),
                    role: "Service".into(),
                },
                Transform::from_xyz(-3.0, crate::crew::BODY_OFFSET, 0.0),
                Body::default(),
                Bloodstream::default(),
                CrewRoute::standing(),
                bundle,
                service_profile(SERVICE_SUPPORT[0]).unwrap(),
            ))
            .id();

        for _ in 0..600 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            if app
                .world()
                .get::<MealBatch>(meal)
                .unwrap()
                .servings_remaining
                < 4
            {
                break;
            }
        }

        let batch = app.world().get::<MealBatch>(meal).unwrap();
        assert!(
            batch.servings_remaining < 4,
            "the hungry resident never ate a serving"
        );

        // Hunger fell, and the food's real contents reached a real bloodstream
        // rather than an abstract "fed" flag being set.
        let needs = app
            .world()
            .get::<crate::utility_ai::NpcNeeds>(diner)
            .unwrap();
        assert!(
            needs.hunger < 0.9,
            "eating did not relieve hunger: {}",
            needs.hunger
        );
        let blood = app.world().get::<Bloodstream>(diner).unwrap();
        assert!(
            !blood.0.is_empty(),
            "the serving's chemistry never entered the diner's bloodstream"
        );

        // It physically walked to the meal.
        let at = app.world().get::<Transform>(diner).unwrap().translation;
        assert!(
            at.distance(meal_at.with_y(crate::crew::BODY_OFFSET)) <= 1.0,
            "the diner ate from {at:?} without reaching the meal at {meal_at:?}"
        );
    }

    #[test]
    fn eating_uses_real_ingestion_and_only_changes_the_exact_diner() {
        let data = chemistry();
        let amanitin = data.reagent("amanitin");
        let fibre = data.reagent("plant_fibre");
        let mut solution = chem_sim::Solution::new(chem_sim::Units::whole(20));
        assert!(solution.add(amanitin, chem_sim::Units::whole(10)).is_zero());
        assert!(solution.add(fibre, chem_sim::Units::whole(10)).is_zero());
        let worker = Entity::from_bits(41);
        let mut batch = MealBatch {
            id: 7,
            recipe: ServiceRecipe::GardenPlate,
            stage: MealStage::Served,
            servings_remaining: 4,
            hosted: true,
            quality_percent: 100,
        };
        let mut chemistry = MealChemistry {
            solution,
            ingredients: Vec::new(),
            prepared_by: worker,
            prepared_at: 2.0,
        };
        let mut diner = Body::default();
        let mut diner_blood = Bloodstream::default();
        let bystander = Body::default();
        let bystander_blood = Bloodstream::default();

        let consumed = consume_meal_serving(
            &mut batch,
            &mut chemistry,
            &mut diner,
            &mut diner_blood,
            &data,
        )
        .unwrap();

        assert_eq!(consumed.offered, SERVING_DOSE);
        assert_eq!(consumed.exposure.absorbed, chem_sim::Units::whole(3));
        assert!(diner_blood.0.stomach.volume_of(amanitin).is_positive());
        assert!(bystander_blood.0.stomach.is_empty());
        assert_eq!(bystander.0.damage, chem_sim::Damage::default());
        assert_eq!(batch.servings_remaining, 3);
        assert_eq!(batch.stage, MealStage::Served);

        for _ in 0..100 {
            chem_sim::metabolise(&mut diner.0, &mut diner_blood.0, &data);
            if diner.0.damage.toxin.is_positive() {
                break;
            }
        }
        assert!(
            diner.0.damage.toxin.is_positive(),
            "the meal's retained amanitin never produced its authored metabolic consequence"
        );
        assert_eq!(bystander.0.damage.toxin, chem_sim::Units::ZERO);
    }

    #[test]
    fn a_forced_kitchen_burn_targets_the_exact_worker_and_is_stability_scaled() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<crate::instability::StationStability>()
            .init_resource::<DepartmentProblemDirector>()
            .init_resource::<IncidentLedger>()
            .add_message::<ServiceJobCompleted>()
            .add_message::<IncidentCreated>()
            .add_systems(Update, apply_service_workplace_risk);
        let worker = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Chef Dubois".into(),
                    role: "Service".into(),
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
                    name: "Cook Navarro".into(),
                    role: "Service".into(),
                },
                Transform::from_xyz(2.5, crate::crew::BODY_OFFSET, 3.0),
                Body::default(),
                Bloodstream::default(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<DepartmentProblemDirector>()
            .force_next_service_burn();
        app.world_mut().write_message(ServiceJobCompleted {
            worker,
            kind: ServiceJobKind::PrepareMeal.id().into(),
            ingredient: None,
            batch: Some(Entity::from_bits(77)),
            risk: Normalized::new(0.08).unwrap(),
        });

        app.update();

        assert_eq!(
            app.world().get::<Body>(worker).unwrap().0.damage.burn,
            chem_sim::Units::whole(12)
        );
        assert_eq!(
            app.world().get::<Body>(bystander).unwrap().0.damage.burn,
            chem_sim::Units::ZERO
        );
        let incidents = app.world().resource::<IncidentLedger>();
        let incident = incidents.active().next().expect("a burn incident exists");
        assert_eq!(incident.subject, worker);
        assert_eq!(incident.kind, IncidentKind::Burn);
        assert_eq!(incident.department, JobDomain::Service);
        assert_eq!(service_burn_severity_from_health(1.0).get(), 0.24);
        assert_eq!(service_burn_severity_from_health(0.0).get(), 0.52);
        assert_eq!(
            service_burn_damage_from_health(0.0),
            chem_sim::Units::whole(24)
        );
    }

    #[test]
    fn replication_exposes_meal_state_but_not_private_chemistry() {
        use bevy_replicon::shared::replication::rules::ReplicationRules;

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::state::app::StatesPlugin,
            RepliconSharedPlugin::default(),
            super::super::UtilityAiPlugin,
        ));
        let batch = app.world_mut().register_component::<MealBatch>();
        let chemistry = app.world_mut().register_component::<MealChemistry>();
        let rules = app.world().resource::<ReplicationRules>();
        let replicated = |id| {
            rules
                .iter()
                .any(|rule| rule.components.iter().any(|component| component.id == id))
        };

        assert!(replicated(batch));
        assert!(!replicated(chemistry));
    }
}
