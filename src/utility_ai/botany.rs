//! Botany production on the shared utility, job, and incident contracts.
//!
//! A plot is a real authority-side fact. Workers advance one named plot at a
//! time, and the final processing step creates a physical `Produce` entity on
//! the authored output shelf. This module does not select actors or move them;
//! it only publishes work and resolves consequences after the shared action
//! lifecycle confirms the exact ticket owner completed it.

use bevy::prelude::*;

use super::department::{control_for_resident, DepartmentRoster};
use super::jobs::{
    JobBoard, JobCapability, JobDomain, JobTicket, JobTicketId, JobTicketState, NarrativeTier,
    NpcJobProfile, UtilitySpots,
};
use super::{
    stable_text_key, ActionResult, ActionTarget, ControlOwner, DepartmentProblemCandidate,
    DepartmentProblemDirector, DepartmentProblemPolicy, IncidentCreated, IncidentKind,
    IncidentLedger, Normalized, ProblemPermitDecision, ProblemStabilitySnapshot, ReservationKey,
    UtilityActionId, UtilityActionResolved, UtilityAgent, UtilityBucket,
};
use crate::containers::HeldBy;
use crate::crew::{Ambient, CrewMember, StationResident};
use crate::produce::{ProduceCatalog, ProduceId};

pub const BOTANY_SECOND_CORE: &str = "Agronomist Vale";
pub const BOTANY_SUPPORT: [&str; 2] = crate::crew::fluff::BOTANY_SUPPORT_NAMES;
const BOTANY_CORE: [&str; 2] = ["Botanist Ivy", BOTANY_SECOND_CORE];

const INSPECT_CAPABILITY: &str = "botany.inspect";
const IRRIGATE_CAPABILITY: &str = "botany.irrigate";
const TEND_CAPABILITY: &str = "botany.tend";
const HARVEST_CAPABILITY: &str = "botany.harvest";
const PROCESS_CAPABILITY: &str = "botany.process";
const BOTANY_CAPABILITIES: [&str; 6] = [
    INSPECT_CAPABILITY,
    IRRIGATE_CAPABILITY,
    TEND_CAPABILITY,
    HARVEST_CAPABILITY,
    PROCESS_CAPABILITY,
    super::aid::assessment_capability(JobDomain::Botany),
];
pub(super) const BOTANY_ROSTER: DepartmentRoster = DepartmentRoster {
    domain: JobDomain::Botany,
    core: &BOTANY_CORE,
    support: &BOTANY_SUPPORT,
    expected_support: 2,
    capabilities: &BOTANY_CAPABILITIES,
};

const INSPECTION_SPOT: &str = "botany.plot.inspect";
const IRRIGATION_SPOT: &str = "botany.irrigation";
const TENDING_SPOT: &str = "botany.plot.tend";
const HARVEST_SPOT: &str = "botany.harvest.process";
const OUTPUT_SHELF_SPOT: &str = "botany.output.shelf";
const BOTANY_SPOTS: [&str; 5] = [
    INSPECTION_SPOT,
    IRRIGATION_SPOT,
    TENDING_SPOT,
    HARVEST_SPOT,
    OUTPUT_SHELF_SPOT,
];

/// How long a processed plot rests before it is replanted.
///
/// Long enough that a plot visibly lies fallow rather than the greenhouse
/// reading as a conveyor belt, short enough that four plots keep the department
/// — and Service downstream of it — in continuous work.
const REGROW_SECONDS: f32 = 90.0;
const MAX_PHYSICAL_OUTPUT: usize = 4;
const OUTPUT_SHELF_RADIUS_SQUARED: f32 = 1.0;
const BOTANY_TOXIC_EXPOSURE: &str = "botany.processing.toxic_exposure";
const BOTANY_PROBLEM_POLICY: DepartmentProblemPolicy = DepartmentProblemPolicy {
    domain: JobDomain::Botany,
    opening_grace: 8,
    domain_cooldown: 8,
    actor_cooldown: 8,
    unresolved_cap: 1,
    shift_cap: 4,
};

/// The lifecycle states that can create Botany work. These are deliberately
/// world facts instead of animation phases in `CurrentAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotanyPlotState {
    Dry,
    Growing,
    Stressed,
    Ripe,
    Harvested,
    Quarantined,
}

/// One stable authored crop plot. `produce` is the same interned identity used
/// by the grinder, inventory, replication, and save system.
#[derive(Clone, Debug, PartialEq)]
pub struct BotanyPlot {
    pub id: String,
    pub state: BotanyPlotState,
    pub produce: ProduceId,
    pub hazardous: bool,
    pub harvests_completed: u32,
    /// Seconds of rest left before a processed plot is replanted, or `None`
    /// when the plot is already in the working cycle.
    ///
    /// A plot that has just given up its crop should look spent for a while —
    /// this is what stops the department reading as a conveyor belt — but it
    /// must eventually come back, which the old one-harvest cap never did.
    pub regrow_in: Option<f32>,
}

/// A running Botany shift.
///
/// Was bounded to one harvest per plot, which measured as thirteen completions
/// with the last at 25.5 seconds — and took Service down with it, since Service
/// is entirely downstream of Botany produce. A plot now *rests* after a harvest
/// instead of retiring: `regrow_in` counts it back to `Dry` and the cycle runs
/// again.
///
/// Unbounded harvests are safe because the entity count was never bounded by
/// the harvest cap in the first place. `MAX_PHYSICAL_OUTPUT` and the shelf
/// occupancy gate in `publish_botany_tickets` already refuse to publish a
/// `ProcessHarvest` while the shelf is full, so backpressure comes from the
/// shelf — a world fact the player can see and empty — rather than from a
/// counter that silently ends the department's day.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct BotanyWorkState {
    pub plots: Vec<BotanyPlot>,
    pub completed_batches: u32,
}

impl Default for BotanyWorkState {
    fn default() -> Self {
        Self {
            plots: vec![
                BotanyPlot {
                    id: "botany.plot.greenhouse.alpha".into(),
                    state: BotanyPlotState::Dry,
                    produce: ProduceId(0),
                    hazardous: false,
                    harvests_completed: 0,
                    regrow_in: None,
                },
                BotanyPlot {
                    id: "botany.plot.greenhouse.beta".into(),
                    state: BotanyPlotState::Growing,
                    produce: ProduceId(3),
                    hazardous: false,
                    harvests_completed: 0,
                    regrow_in: None,
                },
                BotanyPlot {
                    id: "botany.plot.workroom.toxin".into(),
                    state: BotanyPlotState::Ripe,
                    // Destroying Angel in station.produce.ron. Its physical
                    // Produce identity carries real amanitin into the grinder.
                    produce: ProduceId(12),
                    hazardous: true,
                    harvests_completed: 0,
                    regrow_in: None,
                },
                BotanyPlot {
                    id: "botany.plot.nursery.aloe".into(),
                    state: BotanyPlotState::Stressed,
                    produce: ProduceId(1),
                    hazardous: false,
                    harvests_completed: 0,
                    regrow_in: None,
                },
            ],
            completed_batches: 0,
        }
    }
}

impl BotanyWorkState {
    pub fn plot(&self, id: &str) -> Option<&BotanyPlot> {
        self.plots.iter().find(|plot| plot.id == id)
    }

    pub fn plot_mut(&mut self, id: &str) -> Option<&mut BotanyPlot> {
        self.plots.iter_mut().find(|plot| plot.id == id)
    }

    fn work_for(&self, plot: &BotanyPlot) -> Option<BotanyJobKind> {
        if plot.state == BotanyPlotState::Quarantined {
            return Some(BotanyJobKind::CleanQuarantine);
        }
        // Resting, not retired. This used to be a hard harvest cap, which meant
        // a plot that had given its crop was finished for the shift and Botany
        // simply ran out of work — and Service, which is entirely downstream of
        // Botany produce, ran out with it.
        if plot.regrow_in.is_some() {
            return None;
        }
        Some(match plot.state {
            BotanyPlotState::Dry => BotanyJobKind::IrrigatePlot,
            BotanyPlotState::Growing => BotanyJobKind::InspectPlot,
            BotanyPlotState::Stressed => BotanyJobKind::TendPlot,
            BotanyPlotState::Ripe => BotanyJobKind::HarvestPlot,
            BotanyPlotState::Harvested => BotanyJobKind::ProcessHarvest,
            BotanyPlotState::Quarantined => unreachable!("handled above"),
        })
    }

    fn can_complete(&self, plot_index: usize, kind: BotanyJobKind) -> bool {
        self.plots
            .get(plot_index)
            .is_some_and(|plot| self.work_for(plot).is_some_and(|current| current == kind))
    }

    fn complete(&mut self, plot_index: usize, kind: BotanyJobKind) -> bool {
        if !self.can_complete(plot_index, kind) {
            return false;
        }
        let plot = &mut self.plots[plot_index];
        plot.state = match kind {
            BotanyJobKind::IrrigatePlot | BotanyJobKind::TendPlot => BotanyPlotState::Growing,
            BotanyJobKind::InspectPlot => BotanyPlotState::Ripe,
            BotanyJobKind::HarvestPlot => BotanyPlotState::Harvested,
            BotanyJobKind::ProcessHarvest => {
                plot.harvests_completed = plot.harvests_completed.saturating_add(1);
                self.completed_batches = self.completed_batches.saturating_add(1);
                // Spent, and replanted after a rest rather than retired.
                plot.regrow_in = Some(REGROW_SECONDS);
                BotanyPlotState::Dry
            }
            BotanyJobKind::CleanQuarantine => BotanyPlotState::Dry,
        };
        true
    }
}

/// Provenance retained on the actual output item for later Service, Chemistry,
/// investigation, and save integration. It is authority-only because merely
/// inspecting replicated components must not reveal hidden crop hazards.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct BotanyProduceProvenance {
    pub source_plot: String,
    pub hazardous: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BotanyJobKind {
    InspectPlot,
    IrrigatePlot,
    TendPlot,
    HarvestPlot,
    ProcessHarvest,
    CleanQuarantine,
}

impl BotanyJobKind {
    const ALL: [Self; 6] = [
        Self::InspectPlot,
        Self::IrrigatePlot,
        Self::TendPlot,
        Self::HarvestPlot,
        Self::ProcessHarvest,
        Self::CleanQuarantine,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::InspectPlot => "botany.inspect",
            Self::IrrigatePlot => "botany.irrigate",
            Self::TendPlot => "botany.tend",
            Self::HarvestPlot => "botany.harvest",
            Self::ProcessHarvest => "botany.process",
            Self::CleanQuarantine => "botany.clean_quarantine",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    fn capability(self) -> &'static str {
        match self {
            Self::InspectPlot => INSPECT_CAPABILITY,
            Self::IrrigatePlot => IRRIGATE_CAPABILITY,
            Self::TendPlot | Self::CleanQuarantine => TEND_CAPABILITY,
            Self::HarvestPlot => HARVEST_CAPABILITY,
            Self::ProcessHarvest => PROCESS_CAPABILITY,
        }
    }

    fn spot(self) -> &'static str {
        match self {
            Self::InspectPlot => INSPECTION_SPOT,
            Self::IrrigatePlot => IRRIGATION_SPOT,
            Self::TendPlot | Self::CleanQuarantine => TENDING_SPOT,
            Self::HarvestPlot => HARVEST_SPOT,
            Self::ProcessHarvest => OUTPUT_SHELF_SPOT,
        }
    }

    fn urgency(self) -> f32 {
        match self {
            Self::ProcessHarvest => 1.0,
            Self::HarvestPlot => 0.99,
            Self::CleanQuarantine => 0.98,
            Self::TendPlot => 0.97,
            Self::IrrigatePlot => 0.95,
            Self::InspectPlot => 0.93,
        }
    }

    fn perform_seconds(self) -> f32 {
        match self {
            Self::InspectPlot => 4.0,
            Self::IrrigatePlot => 5.0,
            Self::TendPlot => 6.0,
            Self::HarvestPlot => 6.5,
            Self::ProcessHarvest => 7.0,
            Self::CleanQuarantine => 7.5,
        }
    }

    fn risk(self) -> Normalized {
        let risk = match self {
            Self::ProcessHarvest => 0.12,
            Self::CleanQuarantine => 0.08,
            _ => 0.0,
        };
        Normalized::new(risk).expect("Botany job risk is normalized")
    }
}

/// Resolution detail for public department summaries and workplace risk. The
/// physical output entity is present only for the processing step.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct BotanyJobCompleted {
    pub worker: Entity,
    pub plot_id: String,
    pub kind: String,
    pub risk: Normalized,
    pub output: Option<Entity>,
}

impl DepartmentProblemDirector {
    pub fn force_next_botany_exposure(&mut self) {
        self.force_next(JobDomain::Botany, BOTANY_TOXIC_EXPOSURE);
    }
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<BotanyWorkState>()
        .add_message::<BotanyJobCompleted>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_botany_production
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            PreUpdate,
            activate_botany_production
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (tick_botany_regrowth, publish_botany_tickets)
                .chain()
                .in_set(super::UtilityAiSet::BuildContext)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (apply_botany_job_results, apply_botany_workplace_risk)
                .chain()
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

fn reset_botany_production(
    mut commands: Commands,
    mut state: ResMut<BotanyWorkState>,
    mut problems: ResMut<DepartmentProblemDirector>,
    mut board: ResMut<JobBoard>,
    mut reservations: ResMut<super::ReservationBook>,
    mut incidents: ResMut<IncidentLedger>,
    outputs: Query<Entity, With<BotanyProduceProvenance>>,
) {
    BOTANY_ROSTER
        .validate()
        .expect("the Botany utility roster must be valid");
    *state = BotanyWorkState::default();
    problems.reset_domain(BOTANY_PROBLEM_POLICY);
    board.remove_domain(JobDomain::Botany);
    incidents.remove_domain(JobDomain::Botany);
    for spot in BOTANY_SPOTS {
        reservations.release_key(&ReservationKey(format!("utility.spot.{spot}")));
    }
    for output in &outputs {
        commands.entity(output).despawn();
    }
}

fn botany_profile(name: &str) -> Option<NpcJobProfile> {
    let tier = BOTANY_ROSTER.profile_for(name)?.narrative_tier;
    let capabilities: &[&str] = match name {
        "Botanist Ivy" => &[INSPECT_CAPABILITY, HARVEST_CAPABILITY, PROCESS_CAPABILITY],
        BOTANY_SECOND_CORE => &[
            INSPECT_CAPABILITY,
            TEND_CAPABILITY,
            HARVEST_CAPABILITY,
            PROCESS_CAPABILITY,
        ],
        "Grower Chen" => &[INSPECT_CAPABILITY, TEND_CAPABILITY, HARVEST_CAPABILITY],
        "Technician Mbatha" => &[IRRIGATE_CAPABILITY, TEND_CAPABILITY, PROCESS_CAPABILITY],
        _ => return None,
    };
    Some(NpcJobProfile::new(
        JobDomain::Botany,
        tier,
        capabilities
            .iter()
            .map(|capability| JobCapability::new(*capability)),
    ))
}

fn activate_botany_production(
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
        let Some((control, _)) = control_for_resident(&member.name, route, owner, BOTANY_ROSTER)
        else {
            continue;
        };
        let Some(profile) = botany_profile(&member.name) else {
            continue;
        };
        commands
            .entity(entity)
            .insert((control, profile, StationResident));
    }
}

fn plot_ticket_id(plot_id: &str) -> JobTicketId {
    JobTicketId(stable_text_key(&format!("botany.ticket.{plot_id}")))
}

fn output_shelf_count(
    outputs: &Query<(&Transform, Option<&HeldBy>), With<BotanyProduceProvenance>>,
    shelf: Vec3,
) -> usize {
    outputs
        .iter()
        .filter(|(transform, held)| {
            held.is_none()
                && transform.translation.distance_squared(shelf) <= OUTPUT_SHELF_RADIUS_SQUARED
        })
        .count()
}

/// Counts resting plots back into the working cycle.
///
/// Follows `cargo_pilot::tick_cargo_arrivals`, including the subtract-then-take
/// shape rather than assigning a fresh interval, so the cadence does not drift
/// with frame time.
fn tick_botany_regrowth(time: Res<Time>, mut state: ResMut<BotanyWorkState>) {
    let delta = time.delta_secs();
    for plot in &mut state.plots {
        let Some(remaining) = plot.regrow_in.as_mut() else {
            continue;
        };
        *remaining -= delta;
        if *remaining <= 0.0 {
            plot.regrow_in = None;
        }
    }
}

fn publish_botany_tickets(
    time: Res<Time>,
    state: Res<BotanyWorkState>,
    spots: Res<UtilitySpots>,
    catalog: Option<Res<ProduceCatalog>>,
    outputs: Query<(&Transform, Option<&HeldBy>), With<BotanyProduceProvenance>>,
    mut board: ResMut<JobBoard>,
) {
    let shelf = spots.get(OUTPUT_SHELF_SPOT);
    let shelf_has_capacity =
        shelf.is_some_and(|shelf| output_shelf_count(&outputs, shelf.at) < MAX_PHYSICAL_OUTPUT);

    for plot in &state.plots {
        let Some(kind) = state.work_for(plot) else {
            continue;
        };
        if kind == BotanyJobKind::ProcessHarvest
            && (!shelf_has_capacity
                || !catalog.as_deref().is_some_and(|catalog| {
                    catalog.iter().any(|produce| produce.id == plot.produce)
                }))
        {
            continue;
        }
        let id = plot_ticket_id(&plot.id);
        if board.ticket(id).is_some() {
            continue;
        }
        let Some(spot) = spots.get(kind.spot()) else {
            continue;
        };
        board
            .publish(JobTicket {
                id,
                domain: JobDomain::Botany,
                kind: kind.id().into(),
                target: ActionTarget::Point(spot.at),
                subject: None,
                // Keyed per *plot*, not per job kind, and this is the whole
                // Botany contention fix. The spots are shared and authored at
                // capacity 1, so keying on `kind.spot()` meant four plots that
                // had converged on the same stage collided on one key: the
                // board advertised four jobs when the occupancy filter would
                // permit exactly one, and three of four workers were locked out
                // every tick. Measured as 171 `ReservationUnavailable` failures
                // in ninety seconds.
                //
                // `target` still points at the shared workstation, which is
                // legal and precedented — `JobTicket::subject` documents that
                // "target is only where the worker walks" — so this changes who
                // may start, not where they go. Physical crowding at the shared
                // point is a separate question, answered by the arrival-reach
                // widening in the kernel and, if the log ever shows bodies
                // stacking, by authoring per-plot spots.
                reservation: ReservationKey(format!("botany.plot.{}", plot.id)),
                reservation_capacity: 1,
                bucket: UtilityBucket::Routine,
                urgency: Normalized::new(kind.urgency()).expect("Botany urgency is normalized"),
                required_capability: JobCapability::new(kind.capability()),
                created_at: time.elapsed_secs(),
                deadline: None,
                risk: kind.risk(),
                perform_seconds: kind.perform_seconds(),
                state: JobTicketState::Available,
            })
            .expect("Botany checked the stable plot ticket before publishing");
    }
}

fn output_position(shelf: Vec3, occupied: usize) -> Vec3 {
    let column = occupied % 2;
    let row = occupied / 2;
    shelf
        + Vec3::new(
            (column as f32 - 0.5) * 0.22,
            0.08,
            (row as f32 - 0.5) * 0.22,
        )
}

#[allow(clippy::too_many_arguments)]
fn apply_botany_job_results(
    mut commands: Commands,
    mut results: MessageReader<UtilityActionResolved>,
    mut completed: MessageWriter<BotanyJobCompleted>,
    mut state: ResMut<BotanyWorkState>,
    mut board: ResMut<JobBoard>,
    spots: Res<UtilitySpots>,
    catalog: Option<Res<ProduceCatalog>>,
    outputs: Query<(&Transform, Option<&HeldBy>), With<BotanyProduceProvenance>>,
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
        if ticket.domain != JobDomain::Botany
            || ticket.state != JobTicketState::Completed(result.claim)
        {
            continue;
        }
        let Some(kind) = BotanyJobKind::from_id(&ticket.kind) else {
            board.cancel(id);
            continue;
        };
        let Some(plot_index) = state
            .plots
            .iter()
            .position(|plot| plot_ticket_id(&plot.id) == id)
        else {
            board.cancel(id);
            continue;
        };
        if !state.can_complete(plot_index, kind) {
            board.cancel(id);
            continue;
        }

        let prepared_output = if kind == BotanyJobKind::ProcessHarvest {
            let Some(shelf) = spots.get(OUTPUT_SHELF_SPOT) else {
                board.cancel(id);
                continue;
            };
            let occupied = output_shelf_count(&outputs, shelf.at);
            if occupied >= MAX_PHYSICAL_OUTPUT {
                board.cancel(id);
                continue;
            }
            let Some(catalog) = catalog.as_deref() else {
                board.cancel(id);
                continue;
            };
            let produce = state.plots[plot_index].produce;
            if !catalog.iter().any(|kind| kind.id == produce) {
                board.cancel(id);
                continue;
            }
            Some((produce, output_position(shelf.at, occupied)))
        } else {
            None
        };

        if board.take_completed(id, result.claim).is_err() {
            continue;
        }
        let plot_id = state.plots[plot_index].id.clone();
        let hazardous = state.plots[plot_index].hazardous;
        let changed = state.complete(plot_index, kind);
        debug_assert!(changed, "the Botany plot was validated before commit");

        let output = prepared_output.map(|(produce, position)| {
            let entity = crate::produce::spawn_produce(&mut commands, produce, position);
            commands.entity(entity).insert(BotanyProduceProvenance {
                source_plot: plot_id.clone(),
                hazardous,
            });
            entity
        });
        completed.write(BotanyJobCompleted {
            worker: result.agent,
            plot_id,
            kind: kind.id().into(),
            risk: ticket.risk,
            output,
        });
    }
}

fn apply_botany_workplace_risk(
    time: Res<Time>,
    stability: Res<crate::instability::StationStability>,
    mut completions: MessageReader<BotanyJobCompleted>,
    mut created: MessageWriter<IncidentCreated>,
    mut state: ResMut<BotanyWorkState>,
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
    problems.ensure_policy(BOTANY_PROBLEM_POLICY);

    struct PreparedBotanyProblem {
        candidate: DepartmentProblemCandidate,
        location: Vec3,
        plot_id: String,
    }

    let mut candidates = Vec::new();
    {
        let workers = workers.p0();
        for completion in completions.read() {
            if BotanyJobKind::from_id(&completion.kind) != Some(BotanyJobKind::ProcessHarvest) {
                continue;
            }
            let Some(plot) = state.plot(&completion.plot_id) else {
                continue;
            };
            if !plot.hazardous || plot.state != BotanyPlotState::Dry {
                continue;
            }
            let Ok((member, transform)) = workers.get(completion.worker) else {
                continue;
            };
            candidates.push(PreparedBotanyProblem {
                candidate: DepartmentProblemCandidate::new(
                    BOTANY_TOXIC_EXPOSURE,
                    JobDomain::Botany,
                    IncidentKind::Poisoning,
                    completion.worker,
                    completion.risk,
                    stable_text_key(&format!("{}:{}", member.name, completion.plot_id)),
                ),
                location: transform.translation,
                plot_id: completion.plot_id.clone(),
            });
        }
    }
    candidates.sort_by(|left, right| left.candidate.deterministic_cmp(&right.candidate));

    let mut workers = workers.p1();
    for prepared in candidates {
        let subject = prepared.candidate.subject;
        if !state
            .plot(&prepared.plot_id)
            .is_some_and(|plot| plot.hazardous && plot.state == BotanyPlotState::Dry)
        {
            continue;
        }
        let Ok((mut body, blood)) = workers.get_mut(subject) else {
            continue;
        };
        let ProblemPermitDecision::Permit(permit) =
            problems.request_permit(prepared.candidate, &stability, &incidents)
        else {
            continue;
        };
        let snapshot = permit.stability();
        let severity = botany_exposure_severity_from_snapshot(snapshot);
        let Ok(id) = incidents.create(
            IncidentKind::Poisoning,
            JobDomain::Botany,
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
            chem_sim::DamageKind::Toxin,
            botany_exposure_damage_from_snapshot(snapshot),
        ));
        if let Some(blood) = blood {
            blood
                .0
                .reconcile_collapse(&mut body.0, previously_collapsed);
        }
        state
            .plot_mut(&prepared.plot_id)
            .expect("the Botany plot was rechecked above")
            .state = BotanyPlotState::Quarantined;
        problems.commit(permit);
        created.write(IncidentCreated {
            id,
            kind: IncidentKind::Poisoning,
            department: JobDomain::Botany,
            subject,
            location: prepared.location,
            severity,
        });
    }
}

fn botany_exposure_severity_from_snapshot(snapshot: ProblemStabilitySnapshot) -> Normalized {
    botany_exposure_severity_from_health(snapshot.health())
}

fn botany_exposure_severity_from_health(health: f32) -> Normalized {
    let deterioration = 1.0 - health;
    Normalized::new(0.30 + deterioration * 0.30)
        .expect("the clamped Botany exposure severity is normalized")
}

fn botany_exposure_damage_from_snapshot(snapshot: ProblemStabilitySnapshot) -> chem_sim::Units {
    botany_exposure_damage_from_health(snapshot.health())
}

fn botany_exposure_damage_from_health(health: f32) -> chem_sim::Units {
    let deterioration = 1.0 - health;
    chem_sim::Units::whole(15 + (deterioration * 13.0).round() as i32)
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
    struct CompletionLog(Vec<BotanyJobCompleted>);

    fn record_completions(
        mut messages: MessageReader<BotanyJobCompleted>,
        mut log: ResMut<CompletionLog>,
    ) {
        log.0.extend(messages.read().cloned());
    }

    fn insert_test_spots(app: &mut App) {
        for (index, id) in BOTANY_SPOTS.into_iter().enumerate() {
            app.world_mut().resource_mut::<UtilitySpots>().insert(
                id,
                Vec3::new(-4.0 + index as f32 * 2.0, 0.0, 0.0),
                1,
            );
        }
    }

    fn produce_catalog() -> ProduceCatalog {
        let config: crate::produce::ProduceConfig =
            ron::from_str(include_str!("../../assets/data/station.produce.ron"))
                .expect("produce data should parse");
        let chemistry = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should parse");
        ProduceCatalog::from_config(&config, &chemistry.reagents)
    }

    #[test]
    fn authored_botany_cast_has_distinct_tiers_and_qualifications() {
        BOTANY_ROSTER.validate().unwrap();
        let ivy = botany_profile("Botanist Ivy").unwrap();
        let vale = botany_profile(BOTANY_SECOND_CORE).unwrap();
        let chen = botany_profile(BOTANY_SUPPORT[0]).unwrap();
        let mbatha = botany_profile(BOTANY_SUPPORT[1]).unwrap();

        assert_eq!(ivy.narrative_tier, NarrativeTier::Core);
        assert_eq!(vale.narrative_tier, NarrativeTier::Core);
        assert_eq!(chen.narrative_tier, NarrativeTier::Support);
        assert_eq!(mbatha.narrative_tier, NarrativeTier::Support);
        assert!(ivy.can_do(&JobCapability::new(PROCESS_CAPABILITY)));
        assert!(!ivy.can_do(&JobCapability::new(IRRIGATE_CAPABILITY)));
        assert!(chen.can_do(&JobCapability::new(TEND_CAPABILITY)));
        assert!(!chen.can_do(&JobCapability::new(PROCESS_CAPABILITY)));
        assert!(mbatha.can_do(&JobCapability::new(IRRIGATE_CAPABILITY)));
        assert!(mbatha.can_do(&JobCapability::new(PROCESS_CAPABILITY)));
    }

    #[test]
    fn only_the_four_authored_botany_workers_migrate() {
        let mut app = App::new();
        app.add_systems(Update, (activate_botany_production, ApplyDeferred).chain());
        for (name, role) in [
            ("Botanist Ivy", "Service"),
            (BOTANY_SECOND_CORE, "Botany"),
            (BOTANY_SUPPORT[0], "Botany"),
            (BOTANY_SUPPORT[1], "Botany"),
            ("Chef Dubois", "Service"),
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
        assert!(!names.contains("Chef Dubois"));
        assert!(BOTANY_CORE.into_iter().all(|name| names.contains(name)));
        assert!(BOTANY_SUPPORT.into_iter().all(|name| names.contains(name)));
    }

    #[test]
    fn ticket_generation_is_bounded_to_one_ticket_per_named_plot() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<BotanyWorkState>()
            .init_resource::<UtilitySpots>()
            .init_resource::<JobBoard>()
            .insert_resource(produce_catalog())
            .add_systems(Update, publish_botany_tickets);
        insert_test_spots(&mut app);

        for _ in 0..20 {
            app.update();
        }

        assert_eq!(app.world().resource::<JobBoard>().len(), 4);
        assert!(app
            .world()
            .resource::<JobBoard>()
            .iter()
            .all(|ticket| ticket.domain == JobDomain::Botany));
    }

    #[test]
    fn a_job_advances_only_its_named_plot_through_the_real_lifecycle() {
        let mut state = BotanyWorkState::default();
        let target = state
            .plots
            .iter()
            .position(|plot| plot.id == "botany.plot.greenhouse.alpha")
            .unwrap();
        let untouched = state.plot("botany.plot.greenhouse.beta").unwrap().clone();

        assert!(state.complete(target, BotanyJobKind::IrrigatePlot));
        assert_eq!(state.plots[target].state, BotanyPlotState::Growing);
        assert_eq!(state.plot(&untouched.id), Some(&untouched));
        assert!(!state.complete(target, BotanyJobKind::HarvestPlot));
        assert_eq!(state.plots[target].state, BotanyPlotState::Growing);

        for kind in [
            BotanyJobKind::InspectPlot,
            BotanyJobKind::HarvestPlot,
            BotanyJobKind::ProcessHarvest,
        ] {
            assert!(state.complete(target, kind));
        }
        assert_eq!(state.plots[target].state, BotanyPlotState::Dry);
        assert_eq!(state.plots[target].harvests_completed, 1);
        assert_eq!(state.completed_batches, 1);
        assert!(state.work_for(&state.plots[target]).is_none());
    }

    /// Four plots in the same state must be four claimable jobs.
    ///
    /// The reservation key used to be `utility.spot.{kind.spot()}`, and the
    /// spots are authored at capacity 1, so four plots that had converged on the
    /// same stage collided on one key. The board advertised four jobs when the
    /// occupancy filter would permit exactly one, and three of four workers were
    /// locked out on every tick — 171 `ReservationUnavailable` failures in
    /// ninety seconds of live play.
    ///
    /// Falsifies the per-plot key: restore `kind.spot()` in
    /// `publish_botany_tickets` and only one of the four tickets is claimable.
    #[test]
    fn four_plots_in_the_same_state_are_all_claimable() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<ReservationBook>()
            .init_resource::<BotanyWorkState>()
            .insert_resource(produce_catalog())
            .add_systems(Update, publish_botany_tickets);
        insert_test_spots(&mut app);

        // Every plot wanting the same service is the case that collided.
        {
            let mut state = app.world_mut().resource_mut::<BotanyWorkState>();
            for plot in &mut state.plots {
                plot.state = BotanyPlotState::Stressed;
                plot.regrow_in = None;
            }
        }
        app.update();

        let tickets: Vec<_> = app
            .world()
            .resource::<JobBoard>()
            .iter()
            .filter(|ticket| ticket.domain == JobDomain::Botany)
            .map(|ticket| (ticket.reservation.clone(), ticket.reservation_capacity))
            .collect();
        assert_eq!(tickets.len(), 4, "one ticket per plot: {tickets:?}");

        // The kernel's occupancy filter is `claims_on(key) < capacity`, so what
        // matters is that holding one claim never blocks the other three.
        let mut book = ReservationBook::default();
        let mut startable = 0;
        for (index, (key, capacity)) in tickets.iter().enumerate() {
            if book.claims_on(key) < (*capacity).max(1) {
                startable += 1;
                let owner = super::super::ReservationOwner {
                    agent: Entity::from_raw_u32(index as u32 + 1).expect("a valid test entity"),
                    action_instance: 1,
                };
                let _ = book.reserve(key.clone(), *capacity, owner);
            }
        }
        assert_eq!(
            startable, 4,
            "all four plots must be workable at once; only {startable} were, so \
             the rest of Botany is locked out every tick",
        );
    }

    /// A plot that has given its crop must come back.
    ///
    /// `MAX_HARVESTS_PER_PLOT = 1` retired each plot after one harvest, so
    /// Botany produced thirteen completions and stopped at 25.5 seconds — and
    /// Service, which is entirely downstream of Botany produce, stopped with it.
    ///
    /// Falsifies regrowth: remove `tick_botany_regrowth` and the rested plot
    /// never returns to the working cycle.
    #[test]
    fn a_rested_plot_returns_to_work() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<BotanyWorkState>()
            .insert_resource(produce_catalog())
            .add_systems(
                Update,
                (tick_botany_regrowth, publish_botany_tickets).chain(),
            );
        insert_test_spots(&mut app);

        {
            let mut state = app.world_mut().resource_mut::<BotanyWorkState>();
            for plot in &mut state.plots {
                plot.state = BotanyPlotState::Dry;
                plot.harvests_completed = 1;
                plot.regrow_in = Some(REGROW_SECONDS);
            }
        }
        app.update();
        assert!(
            app.world().resource::<JobBoard>().is_empty(),
            "a resting plot must not advertise work"
        );

        // Step in small frames rather than jumping the interval: a single large
        // advance can sail past the transition under test.
        let mut back = false;
        for _ in 0..((REGROW_SECONDS / 0.5) as usize + 20) {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.5));
            app.update();
            if !app.world().resource::<JobBoard>().is_empty() {
                back = true;
                break;
            }
        }
        assert!(
            back,
            "a rested plot never came back, so Botany ends its shift after one \
             pass and starves Service downstream",
        );
    }

    #[test]
    fn four_workers_advance_named_plots_into_bounded_physical_produce() {
        let mut app = App::new();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        app.init_resource::<Time>()
            .init_resource::<CrewPosts>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<ReservationBook>()
            .init_resource::<UtilityDecisionLog>()
            .init_resource::<JobBoard>()
            .init_resource::<UtilitySpots>()
            .init_resource::<CompletionLog>()
            .init_resource::<BotanyWorkState>()
            .insert_resource(produce_catalog())
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .add_message::<ErrandResolved>()
            .add_message::<UtilityActionResolved>()
            .add_message::<BotanyJobCompleted>()
            .add_systems(
                Update,
                (
                    tick_current_actions,
                    publish_botany_tickets,
                    select_reference_actions,
                    ApplyDeferred,
                    begin_reference_actions,
                    ApplyDeferred,
                    crate::crew::run_errands,
                    consume_utility_arrivals,
                    perform_reference_actions,
                    resolve_reference_actions,
                    apply_botany_job_results,
                    ApplyDeferred,
                    record_completions,
                )
                    .chain(),
            );
        insert_test_spots(&mut app);

        let mut worker_entities = Vec::new();
        for (index, name) in BOTANY_CORE.into_iter().chain(BOTANY_SUPPORT).enumerate() {
            let worker = app
                .world_mut()
                .spawn((
                    CrewMember {
                        name: name.into(),
                        role: "Botany".into(),
                    },
                    Transform::from_xyz(-3.0 + index as f32 * 2.0, crate::crew::BODY_OFFSET, -2.0),
                    Body::default(),
                    Bloodstream::default(),
                    CrewRoute::standing(),
                    UtilityControlBundle::new(UtilityAgent::new(200 + index as u64, 0)),
                    botany_profile(name).unwrap(),
                ))
                .id();
            worker_entities.push(worker);
        }

        let mut largest_board = 0;
        for _ in 0..6_000 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.1));
            app.update();
            largest_board = largest_board.max(app.world().resource::<JobBoard>().len());
            let state = app.world().resource::<BotanyWorkState>();
            if state.completed_batches == state.plots.len() as u32
                && app.world().resource::<JobBoard>().is_empty()
            {
                break;
            }
        }

        let state = app.world().resource::<BotanyWorkState>();
        assert_eq!(state.completed_batches, state.plots.len() as u32);
        // Each plot has produced, and is now resting rather than retired. This
        // used to assert `== MAX_HARVESTS_PER_PLOT`, which pinned the very
        // exhaustion that ended Botany's day at 25 seconds; what actually
        // matters is that the shift converges without the board running away,
        // which the `largest_board` and emptiness assertions below still check.
        assert!(state.plots.iter().all(|plot| plot.harvests_completed >= 1));
        assert!(
            state.plots.iter().all(|plot| plot.regrow_in.is_some()),
            "a processed plot must be resting, so it can come back"
        );
        assert!(largest_board <= state.plots.len());
        assert!(app.world().resource::<JobBoard>().is_empty());
        assert_eq!(
            app.world()
                .resource::<ReservationBook>()
                .active_claim_count(),
            0
        );

        let plot_count = state.plots.len();
        let completed_batches = state.completed_batches;
        assert_eq!(completed_batches, plot_count as u32);
        let world = app.world_mut();
        let mut output_query =
            world.query::<(&crate::produce::Produce, &BotanyProduceProvenance)>();
        let outputs: Vec<_> = output_query
            .iter(world)
            .map(|(produce, provenance)| (produce.0, provenance.clone()))
            .collect();
        assert_eq!(outputs.len(), plot_count);
        assert!(outputs.len() <= MAX_PHYSICAL_OUTPUT);
        assert!(outputs.iter().any(|(produce, provenance)| {
            *produce == ProduceId(12)
                && provenance.source_plot == "botany.plot.workroom.toxin"
                && provenance.hazardous
        }));

        let completions = &world.resource::<CompletionLog>().0;
        let workers: HashSet<_> = completions.iter().map(|event| event.worker).collect();
        assert_eq!(
            workers.len(),
            4,
            "Botany did not distribute work across all four residents: {completions:?}"
        );
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
                    && profile.primary == JobDomain::Botany
            }));
        assert_eq!(worker_entities.len(), 4);
    }

    #[test]
    fn forced_toxic_processing_targets_the_exact_worker_and_quarantines_the_plot() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<crate::instability::StationStability>()
            .init_resource::<DepartmentProblemDirector>()
            .init_resource::<IncidentLedger>()
            .init_resource::<super::super::MedicalCaseLedger>()
            .init_resource::<ReservationBook>()
            .init_resource::<BotanyWorkState>()
            .add_message::<BotanyJobCompleted>()
            .add_message::<IncidentCreated>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_systems(
                Update,
                (
                    apply_botany_workplace_risk,
                    super::super::medical::open_cases_from_incidents,
                    ApplyDeferred,
                )
                    .chain(),
            );
        let toxin_plot = "botany.plot.workroom.toxin";
        app.world_mut()
            .resource_mut::<BotanyWorkState>()
            .plot_mut(toxin_plot)
            .unwrap()
            .state = BotanyPlotState::Dry;
        let worker = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Botanist Ivy".into(),
                    role: "Service".into(),
                },
                Transform::from_xyz(2.0, crate::crew::BODY_OFFSET, 3.0),
                Body::default(),
                Bloodstream::default(),
                UtilityControlBundle::new(UtilityAgent::new(700, 0)),
                botany_profile("Botanist Ivy").unwrap(),
            ))
            .id();
        let bystander = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Grower Chen".into(),
                    role: "Botany".into(),
                },
                Transform::from_xyz(2.5, crate::crew::BODY_OFFSET, 3.0),
                Body::default(),
                Bloodstream::default(),
                UtilityControlBundle::new(UtilityAgent::new(701, 0)),
                botany_profile("Grower Chen").unwrap(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<DepartmentProblemDirector>()
            .force_next_botany_exposure();
        app.world_mut().write_message(BotanyJobCompleted {
            worker,
            plot_id: toxin_plot.into(),
            kind: BotanyJobKind::ProcessHarvest.id().into(),
            risk: Normalized::new(0.12).unwrap(),
            output: None,
        });

        app.update();

        assert_eq!(
            app.world().get::<Body>(worker).unwrap().0.damage.toxin,
            chem_sim::Units::whole(15)
        );
        assert_eq!(
            app.world().get::<Body>(bystander).unwrap().0.damage.toxin,
            chem_sim::Units::ZERO
        );
        assert_eq!(
            app.world()
                .resource::<BotanyWorkState>()
                .plot(toxin_plot)
                .unwrap()
                .state,
            BotanyPlotState::Quarantined
        );
        let incidents = app.world().resource::<IncidentLedger>();
        let incident = incidents
            .active()
            .next()
            .expect("toxic processing opens a Medical-compatible incident");
        assert_eq!(incident.subject, worker);
        assert_eq!(incident.kind, IncidentKind::Poisoning);
        assert_eq!(incident.department, JobDomain::Botany);
        assert_eq!(incidents.active().count(), 1);
        let first_incident = incident.id;
        let cases = app.world().resource::<super::super::MedicalCaseLedger>();
        let case = cases
            .active()
            .next()
            .expect("the shared Medical adapter opens a poisoning case");
        assert_eq!(case.patient, worker);
        assert_eq!(case.incident, first_incident);
        assert_eq!(case.kind, IncidentKind::Poisoning);
        assert_eq!(cases.active().count(), 1);
        assert!(app
            .world()
            .get::<super::super::medical::MedicalPatient>(worker)
            .is_some());
        assert_eq!(
            app.world().get::<ControlOwner>(worker),
            Some(&ControlOwner::MedicalTransport)
        );

        // A second forced exposure remains queued while the shared unresolved
        // cap is occupied. Once Medical resolves that exact case, the same
        // real processing candidate may consume it.
        app.world_mut()
            .resource_mut::<BotanyWorkState>()
            .plot_mut(toxin_plot)
            .unwrap()
            .state = BotanyPlotState::Dry;
        app.world_mut()
            .resource_mut::<DepartmentProblemDirector>()
            .force_next_botany_exposure();
        app.world_mut().write_message(BotanyJobCompleted {
            worker: bystander,
            plot_id: toxin_plot.into(),
            kind: BotanyJobKind::ProcessHarvest.id().into(),
            risk: Normalized::new(0.12).unwrap(),
            output: None,
        });
        app.update();
        assert_eq!(
            app.world().get::<Body>(bystander).unwrap().0.damage.toxin,
            chem_sim::Units::ZERO
        );
        assert_eq!(
            app.world()
                .resource::<DepartmentProblemDirector>()
                .forced_count(JobDomain::Botany, BOTANY_TOXIC_EXPOSURE),
            1
        );

        app.world_mut()
            .resource_mut::<IncidentLedger>()
            .resolve(first_incident)
            .unwrap();
        app.world_mut().write_message(BotanyJobCompleted {
            worker: bystander,
            plot_id: toxin_plot.into(),
            kind: BotanyJobKind::ProcessHarvest.id().into(),
            risk: Normalized::new(0.12).unwrap(),
            output: None,
        });
        app.update();
        assert_eq!(
            app.world().get::<Body>(bystander).unwrap().0.damage.toxin,
            chem_sim::Units::whole(15)
        );
        assert_eq!(
            app.world()
                .resource::<DepartmentProblemDirector>()
                .forced_count(JobDomain::Botany, BOTANY_TOXIC_EXPOSURE),
            0
        );
        assert_eq!(app.world().resource::<IncidentLedger>().active().count(), 1);
    }

    #[test]
    fn poor_stability_increases_exposure_severity_without_exceeding_bounds() {
        assert_eq!(botany_exposure_severity_from_health(1.0).get(), 0.30);
        assert_eq!(
            botany_exposure_damage_from_health(1.0),
            chem_sim::Units::whole(15)
        );
        assert_eq!(botany_exposure_severity_from_health(0.0).get(), 0.60);
        assert_eq!(
            botany_exposure_damage_from_health(0.0),
            chem_sim::Units::whole(28)
        );
        assert!(botany_exposure_severity_from_health(0.0).get() <= 1.0);
    }
}
