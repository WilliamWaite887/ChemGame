//! A ritual, escalating one delivery at a time.
//!
//! Same shape as [`crate::obsessed`] — one recurring identity, an authored
//! ordered chain rather than a random pool, advanced by matching the
//! resolution's name — but the payoff is different: the final stage asks
//! for a reagent that only exists as the product of real chemistry
//! (`flash_powder`, gated behind mixing aluminium, potassium and sulfur — see
//! `chem.reactions.ron`). Releasing it produces the reagent's genuine blinding
//! flash through the same authority-owned world-effect path as every other
//! spill. Nothing here reaches into hazards or special-cases the ritual ask.

use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::Units;
use rand::RngExt;
use serde::{Deserialize, Serialize};

use crate::body::Body;
use crate::chem_data::ChemDb;
use crate::containers::{Container, ContainerKind, HeldBy};
use crate::crew::{spawn_crew_member, Ambient, CrewDef, CrewMember, CrewRoute};
use crate::interaction::{InteractRequested, Interactable};
use crate::lab::{CrisisSpots, MapReady};
use crate::machines::chemist_entity;
use crate::net::is_authority;
use crate::orders::{reference_category, OrderResolved, Shift, StationData};
use crate::player::Chemist;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::threat;
use crate::AppState;

const REWARD_SPOT: Vec3 = Vec3::new(2.4, 1.0, 2.5);
const FIRST_WAVE_SECONDS: (f32, f32) = (360.0, 540.0);
const LATER_WAVE_SECONDS: (f32, f32) = (600.0, 840.0);
const CLOSED_CLOCK_SCALE: f32 = 0.5;
const WAVE_PLOT: i32 = 8;
const WARD_RELIEF: i32 = -4;
pub const FINALE_WARDS: usize = 5;

pub struct CultPlugin;

impl Plugin for CultPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(threat::ScriptPlugin::<CultScript>::new(
            "data/station.cult.ron",
            "cult.ron",
        ))
        .init_resource::<CultProgress>()
        .init_resource::<CultIncidentsRestored>()
        .add_systems(Startup, load_cult_visuals)
        .add_systems(
            OnEnter(AppState::Playing),
            (arm_spawner, reset_incident_restore),
        )
        .add_systems(
            Update,
            (
                restore_incidents,
                handle_cult_resolution,
                advance_ritual_clock,
                generate_cult_visit,
                handle_counter_support,
                handle_incident_delivery,
                aggro_cultists,
                credit_defeated_guards,
                expose_finale,
                start_finale,
                remember_started_finale,
            )
                .chain()
                .after(threat::PromoteScripts)
                .run_if(is_authority)
                // Only in a save that actually drew the Cult. This thread
                // was a standalone Cargo curiosity before the campaign
                // arc existed; it is now the Cult's on-station presence,
                // and a save fighting the Syndicate should never see an
                // acolyte at the counter. Department minors deliberately
                // carry no equivalent gate — they run in every save.
                .run_if(crate::arc::is_active(crate::arc::AntagId::Cult))
                // Crisis markers are collected from the loaded map. No
                // authored consequence may fall back to the old lab-local
                // coordinates while that registry is incomplete.
                .run_if(resource_exists::<MapReady>)
                .run_if(in_state(AppState::Playing))
                .run_if(crate::session::career_session),
        );
        app.add_systems(
            Update,
            dress_incidents
                .run_if(in_state(AppState::Playing))
                .run_if(crate::session::career_session),
        );
    }
}

/// Which authored stage fires next. Persisted, same as
/// `obsessed::ObsessedProgress`.
#[derive(Resource, Clone, Debug)]
pub struct CultProgress {
    /// The next timed wave/offer in `CultScript::stages`.
    pub next_stage: usize,
    /// Authority-only countdown for that wave.
    pub wave_remaining: f32,
    /// The one stage Corwin has already offered, if any.
    pub offered_stage: Option<usize>,
    /// Stable owner so a later campaign cannot inherit this one's timer.
    pub campaign: Option<crate::arc::CampaignId>,
    /// Department reports earned while no active objective needed one.
    pub banked_intel: usize,
    /// Once true, reloading may restart the confrontation but never return to
    /// the investigation phase.
    pub finale_started: bool,
}

impl Default for CultProgress {
    fn default() -> Self {
        Self {
            next_stage: 0,
            wave_remaining: 0.0,
            offered_stage: None,
            campaign: None,
            banked_intel: 0,
            finale_started: false,
        }
    }
}

/// `assets/data/station.cult.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct CultScript {
    /// The recurring acolyte, kept off `station.crew.ron` for the same
    /// reason `obsessed::ObsessedScript::name` is — see that module doc.
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
    pub gap_multiplier: (f32, f32),
    /// Ordered — each successful delivery arms the next, higher-stakes
    /// stage. Declining or failing a stage simply grades like any other
    /// unfulfilled order and the ritual never advances.
    pub stages: Vec<CultStageDef>,
    /// The base's permanent focus — present from the moment the map loads,
    /// gated on no stage, treated exactly like any other
    /// [`CultIncidentDef`]. The one manifestation a diligent chemist could
    /// find and neutralise before Corwin ever makes a single ask.
    pub altar: CultIncidentDef,
}

impl CultScript {
    /// Every discoverable manifestation of the ritual — anchors, guards, and
    /// the altar alike — has one persistent slot in
    /// [`crate::arc::Campaign::cult_incidents`], indexed so the vector always
    /// grows in exactly the order things actually appear for the player. The
    /// standing board reads its raw length as "how many have been found so
    /// far" (see `ui::arc_headline`), so an index scheme that reserved a
    /// later stage's slot before an earlier one existed would make the case
    /// file's total jump ahead of what the player could have discovered —
    /// this ordering is what keeps that from happening.
    ///
    /// The altar is unconditional — present in the world from the moment the
    /// map loads, gated on no stage — so it is also the very first slot.
    fn altar_ward_index() -> usize {
        0
    }

    /// Where a stage's own block starts: one slot after the altar, then
    /// every earlier stage's own incident count plus its one guard slot,
    /// summed in order. Deliberately **not** `1 + stage_index * 3` — this
    /// crate never assumes a stage carries exactly two incidents (see
    /// `save_restore_waits_for_the_map_and_never_wraps_incident_spots`,
    /// which authors more to prove positions stay stable regardless), so the
    /// block size has to come from what a stage actually declares.
    fn stage_ward_base(&self, stage_index: usize) -> usize {
        1 + self.stages[..stage_index]
            .iter()
            .map(|stage| stage.incidents.len() + 1)
            .sum::<usize>()
    }

    /// Where this stage's guard is credited — right after its own anchors.
    fn guard_ward_index(&self, stage_index: usize) -> usize {
        self.stage_ward_base(stage_index) + self.stages[stage_index].incidents.len()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct CultStageDef {
    pub reagent: String,
    pub amount: u32,
    pub pretext: String,
    /// Aired the moment the stage is fulfilled — the ritual's own escalating
    /// commentary track.
    pub ritual_line: String,
    /// A small legitimate stock vial makes helping Corwin tempting without
    /// making it a free answer to the manifestation he just created.
    pub reward_reagent: String,
    pub reward_amount: u32,
    pub incidents: Vec<CultIncidentDef>,
    /// The cult member stationed at the base once this stage is fulfilled —
    /// mid-escalation combat, not just another passive anchor to treat.
    pub guard: CultGuardDef,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CultIncidentDef {
    pub name: String,
    pub visual: CultVisualId,
    pub clue: String,
    /// Semantic marker authored into the map. Keeping this in content rather
    /// than deriving it from the incident index makes saves stable when the
    /// station layout changes and lets each manifestation have a deliberate
    /// home.
    pub spot: String,
    /// A representative reagent. Its category is the answer; the player is
    /// never told this name in the incident UI or radio line.
    pub treatment: String,
    pub amount: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CultGuardDef {
    pub name: String,
    pub tier: CultistTier,
    /// Aired the moment the guard is placed — the same discovery convention
    /// as [`CultIncidentDef::clue`].
    pub clue: String,
    pub spot: String,
}

/// A physical, treatable consequence of an accepted Cult request.
#[derive(Component, Serialize, Deserialize)]
pub struct RitualAnchor {
    pub index: usize,
    pub name: String,
    pub clue: String,
    treatment: String,
    amount: Units,
}

/// Stable authored visual identity shared over the network. The GLB handle is
/// local presentation; this small enum is the replicated gameplay contract.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CultVisual(pub CultVisualId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CultVisualId {
    OuterAltarWard,
    WetChalkSigil,
    WhisperingResidue,
    BleedingOfferingBowl,
    ScorchedInvocation,
    AirlessCandle,
    RiftSealScar,
    FinaleFocus,
}

impl CultVisualId {
    const ALL: [Self; 8] = [
        Self::OuterAltarWard,
        Self::WetChalkSigil,
        Self::WhisperingResidue,
        Self::BleedingOfferingBowl,
        Self::ScorchedInvocation,
        Self::AirlessCandle,
        Self::RiftSealScar,
        Self::FinaleFocus,
    ];

    fn path(self) -> &'static str {
        match self {
            Self::OuterAltarWard => "3dassets/station_starter_kit/glb/cult_outer_altar_ward.glb",
            Self::WetChalkSigil => "3dassets/station_starter_kit/glb/cult_wet_chalk_sigil.glb",
            Self::WhisperingResidue => {
                "3dassets/station_starter_kit/glb/cult_whispering_residue.glb"
            }
            Self::BleedingOfferingBowl => {
                "3dassets/station_starter_kit/glb/cult_bleeding_offering_bowl.glb"
            }
            Self::ScorchedInvocation => {
                "3dassets/station_starter_kit/glb/cult_scorched_invocation.glb"
            }
            Self::AirlessCandle => "3dassets/station_starter_kit/glb/cult_airless_candle.glb",
            Self::RiftSealScar => "3dassets/station_starter_kit/glb/cult_rift_seal_scar.glb",
            Self::FinaleFocus => "3dassets/station_starter_kit/glb/cult_finale_focus.glb",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Resource)]
struct CultVisualAssets {
    scenes: [Handle<WorldAsset>; 8],
}

fn load_cult_visuals(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(CultVisualAssets {
        scenes: CultVisualId::ALL
            .map(|visual| assets.load(GltfAssetLabel::Scene(0).from_asset(visual.path()))),
    });
}

/// The exposed outer focus. Interacting starts the Chapel confrontation; it
/// is not itself the seal target, so the first interaction consumes nothing.
#[derive(Component, Serialize, Deserialize)]
pub struct RitualFocus;

/// Marks the recurring visitor whose ordinary Cargo model has the campaign's
/// deliberately subtle Corwin variation.
#[derive(Component, Serialize, Deserialize)]
pub struct CultHerald;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CultistTier {
    #[default]
    Watching,
    Silent,
    Blooded,
}

/// A hostile cult member. Reuses `showdown::Pursuit` wholesale for
/// movement/combat — there is nothing cult-specific to duplicate there, so
/// defeating one is exactly like defeating `showdown::Assailant`: any dose
/// that flips `Body.collapsed` wins.
#[derive(Component, Serialize, Deserialize)]
pub struct Cultist {
    /// `Some(index)` credits `Campaign.cult_incidents[index]` as a ward on
    /// collapse — a base guard defending Chapel/Quiet Room. `None` marks
    /// finale muscle, scored directly by `showdown::resolve_showdown` instead.
    pub wards_incident: Option<usize>,
    #[serde(default)]
    pub tier: CultistTier,
}

/// This thread's authored script, once loaded.
type Script = threat::Authored<CultScript>;

#[derive(Resource)]
struct CultSpawner {
    timer: Timer,
}

/// Restoration runs once per playing session, after the asynchronous script
/// asset has been promoted. Active anchors are session entities, while the
/// campaign only persists their resolved/not-resolved case-file entries.
#[derive(Resource, Default)]
struct CultIncidentsRestored(bool);

fn reset_incident_restore(mut restored: ResMut<CultIncidentsRestored>) {
    restored.0 = false;
}

#[allow(clippy::too_many_arguments)]
fn restore_incidents(
    mut commands: Commands,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    campaign: Option<Res<crate::arc::Campaign>>,
    spots: Res<CrisisSpots>,
    anchors: Query<&RitualAnchor>,
    guards: Query<&Cultist>,
    mut restored: ResMut<CultIncidentsRestored>,
    mut radio: ResMut<RadioLog>,
) {
    if restored.0 {
        return;
    }
    let (Some(script), Some(campaign)) = (script, campaign) else {
        return;
    };
    if campaign.antag != crate::arc::AntagId::Cult {
        restored.0 = true;
        return;
    }
    for (stage_index, stage) in script.stages.iter().enumerate() {
        let base = script.stage_ward_base(stage_index);
        for (offset, incident) in stage.incidents.iter().enumerate() {
            let index = base + offset;
            if campaign.cult_incidents.get(index) != Some(&false)
                || anchors.iter().any(|anchor| anchor.index == index)
            {
                continue;
            }
            let Some(transform) = spots.get(&incident.spot) else {
                error!(
                    "cult incident '{}' names missing crisis spot '{}' during restore; skipping",
                    incident.name, incident.spot
                );
                continue;
            };
            commands.spawn((
                RitualAnchor {
                    index,
                    name: incident.name.clone(),
                    clue: incident.clue.clone(),
                    treatment: incident.treatment.clone(),
                    amount: Units::whole(incident.amount as i32),
                },
                CultVisual(incident.visual),
                transform,
                Visibility::default(),
                Interactable::new(format!("{} — examine and treat", incident.name)),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Lab,
                    if campaign.cult_reported.get(index) == Some(&true) {
                        objective_intel(&db, &script, index).unwrap_or_else(|| {
                            format!("The {} is still waiting for intervention.", incident.name)
                        })
                    } else {
                        "An unresolved ritual disturbance is still present somewhere on the station."
                            .to_string()
                    },
                )
                .negative()
                .urgent(),
            );
        }
    }
    // Guards — a live guard is a session entity exactly like an anchor; only
    // the ward flag persists. Gated on `Some(&false)` the same way anchors
    // are, so a stage that has never been reached (its slot never eagerly
    // grown into existence, see `spawn_stage_consequences`) is correctly
    // skipped rather than restored early.
    for (stage_index, stage) in script.stages.iter().enumerate() {
        let index = script.guard_ward_index(stage_index);
        if campaign.cult_incidents.get(index) != Some(&false)
            || guards
                .iter()
                .any(|cultist| cultist.wards_incident == Some(index))
        {
            continue;
        }
        let Some(transform) = spots.get(&stage.guard.spot) else {
            error!(
                "cult guard '{}' names missing crisis spot '{}' during restore; skipping",
                stage.guard.name, stage.guard.spot
            );
            continue;
        };
        place_guard(&mut commands, &stage.guard, index, transform);
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Lab,
                if campaign.cult_reported.get(index) == Some(&true) {
                    objective_intel(&db, &script, index).unwrap_or_else(|| stage.guard.clue.clone())
                } else {
                    "An unresolved Cult defender remains somewhere beyond the lab.".to_string()
                },
            )
            .negative()
            .urgent(),
        );
    }
    // The altar — unconditional, so it restores whenever it is not already
    // resolved, whether or not any stage has ever been reached. `!=
    // Some(&true)` deliberately accepts `None` (a fresh save, vector not yet
    // grown that far) as "should exist", unlike the stage-gated checks above.
    let altar_index = CultScript::altar_ward_index();
    if campaign.cult_incidents.get(altar_index) != Some(&true)
        && !anchors.iter().any(|anchor| anchor.index == altar_index)
    {
        if let Some(transform) = spots.get(&script.altar.spot) {
            commands.spawn((
                RitualAnchor {
                    index: altar_index,
                    name: script.altar.name.clone(),
                    clue: script.altar.clue.clone(),
                    treatment: script.altar.treatment.clone(),
                    amount: Units::whole(script.altar.amount as i32),
                },
                CultVisual(script.altar.visual),
                transform,
                Visibility::default(),
                Interactable::new(format!("{} — examine and treat", script.altar.name)),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Lab,
                    if campaign.cult_reported.get(altar_index) == Some(&true) {
                        objective_intel(&db, &script, altar_index)
                            .unwrap_or_else(|| script.altar.clue.clone())
                    } else {
                        "Service reports an old stain in the Chapel that does not belong there."
                            .to_string()
                    },
                )
                .negative()
                .urgent(),
            );
        } else {
            error!(
                "cult altar names missing crisis spot '{}'; skipping",
                script.altar.spot
            );
        }
    }
    restored.0 = true;
}

/// See `threat::arm_first_visit` for why this has to re-run on
/// `OnEnter(AppState::Playing)` every session rather than only once at
/// process start.
fn arm_spawner(mut commands: Commands) {
    threat::arm_first_visit(
        &mut commands,
        threat::MAIN_ANTAGONIST_FIRST_VISIT,
        |timer| CultSpawner { timer },
    );
}

#[allow(clippy::too_many_arguments)]
fn advance_ritual_clock(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    spots: Res<CrisisSpots>,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    mut progress: ResMut<CultProgress>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
    active_offers: Query<(Entity, &CrewMember), With<crate::orders::HostileOrder>>,
) {
    let (Some(script), Some(arc_script), Some(mut campaign)) = (script, arc_script, campaign)
    else {
        return;
    };
    if progress.campaign.is_some_and(|owner| owner != campaign.id) {
        *progress = CultProgress::default();
    }
    progress.campaign = Some(campaign.id);
    if progress.finale_started {
        campaign.plot = campaign.plot.max(arc_script.showdown_at);
        return;
    }
    if progress.next_stage >= script.stages.len() || campaign.outcome.is_some() {
        return;
    }
    if progress.wave_remaining <= 0.0 {
        progress.wave_remaining = roll_wave_delay(progress.next_stage);
        return;
    }

    if !tick_wave_clock(
        &mut progress.wave_remaining,
        time.delta_secs(),
        shift.accepting_orders,
    ) {
        return;
    }

    for (entity, crew) in &active_offers {
        if crew.name == script.name {
            commands.entity(entity).despawn();
        }
    }
    let stage_index = progress.next_stage;
    activate_stage(
        &mut commands,
        &db,
        &spots,
        &script,
        stage_index,
        &mut campaign,
        &mut progress,
        &mut radio,
        false,
        0,
    );
}

fn roll_wave_delay(stage: usize) -> f32 {
    let range = if stage == 0 {
        FIRST_WAVE_SECONDS
    } else {
        LATER_WAVE_SECONDS
    };
    rand::rng().random_range(range.0..=range.1)
}

fn tick_wave_clock(remaining: &mut f32, delta_seconds: f32, accepting_orders: bool) -> bool {
    let scale = if accepting_orders {
        1.0
    } else {
        CLOSED_CLOCK_SCALE
    };
    *remaining -= delta_seconds * scale;
    *remaining <= 0.0
}

fn enough_wards(campaign: &crate::arc::Campaign) -> bool {
    campaign.cult_incidents.iter().filter(|done| **done).count() >= FINALE_WARDS
}

#[allow(clippy::too_many_arguments)]
fn generate_cult_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<CultSpawner>>,
    mut progress: ResMut<CultProgress>,
    shift: Res<Shift>,
    chemists: Query<(), With<Chemist>>,
    mut intake: crate::order_intake::Intake,
    mut residents: crate::crew::AvailableResidents,
) {
    let (Some(station), Some(script), Some(spawner)) = (station, script, spawner.as_mut()) else {
        return;
    };
    if progress.next_stage >= script.stages.len()
        || progress.offered_stage == Some(progress.next_stage)
    {
        return;
    }
    let mut rng = rand::rng();
    let rules = current_rules(&station.config, &shift, chemists.iter().count());
    let Some(stage) = threat::due_visit(
        &time,
        &shift,
        &mut spawner.timer,
        &rules,
        &mut rng,
        script.gap_multiplier,
        threat::ChainProgress(progress.next_stage),
        &script.stages,
    ) else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&stage.reagent) else {
        warn!("cult stage names unknown reagent '{}'", stage.reagent);
        return;
    };

    let Some(mut context) = intake.admit(
        crate::order_intake::RequestSource::Cult,
        &script.name,
        &mut spawner.timer,
        true,
    ) else {
        return;
    };
    context.step = Some(progress.next_stage);
    let visitor = threat::dispatch_scripted_visit(
        &mut commands,
        &db,
        &mut rng,
        &rules,
        &mut residents,
        threat::ScriptedVisit {
            context,
            name: &script.name,
            role: &script.role,
            color: script.color,
            reagent,
            amount_units: stage.amount,
            plea: stage.pretext.clone(),
        },
    );
    commands
        .entity(visitor)
        .insert((crate::orders::HostileOrder, CultHerald));
    progress.offered_stage = Some(progress.next_stage);

    info!("cult offer: {} stage {}", script.name, progress.next_stage);
}

/// Accelerates the ritual only on a successful delivery — matched by name for
/// the same reason `obsessed::handle_obsessed_resolution` is. Declining or
/// botching a stage spends that offer while the independent wave clock keeps
/// running.
#[allow(clippy::too_many_arguments)]
fn handle_cult_resolution(
    mut commands: Commands,
    db: Option<Res<ChemDb>>,
    spots: Option<Res<CrisisSpots>>,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<CultProgress>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(db), Some(spots), Some(arc_script), Some(mut campaign)) =
        (script, db, spots, arc_script, campaign)
    else {
        resolved.clear();
        return;
    };
    for report in resolved.read() {
        if report.name != script.name || report.kind != crate::orders::OrderKind::Hostile {
            continue;
        }
        let stage_index = progress.next_stage;
        if progress.offered_stage != Some(stage_index) || !report.outcome.is_good() {
            continue;
        }
        activate_stage(
            &mut commands,
            &db,
            &spots,
            &script,
            stage_index,
            &mut campaign,
            &mut progress,
            &mut radio,
            true,
            arc_script.plot_per_aid,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn activate_stage(
    commands: &mut Commands,
    db: &ChemDb,
    spots: &CrisisSpots,
    script: &CultScript,
    stage_index: usize,
    campaign: &mut crate::arc::Campaign,
    progress: &mut CultProgress,
    radio: &mut RadioLog,
    reward: bool,
    aid_plot: i32,
) {
    let Some(stage) = script.stages.get(stage_index) else {
        return;
    };
    radio.push(
        RadioEntry::new(channel_for(&script.role), stage.ritual_line.clone())
            .speaker(&script.name)
            .negative(),
    );
    spawn_stage_consequences(
        commands,
        db,
        spots,
        script,
        stage,
        stage_index,
        campaign,
        radio,
        reward,
    );
    while progress.banked_intel > 0 && reveal_next_objective(db, script, campaign, radio) {
        progress.banked_intel -= 1;
    }
    crate::arc::nudge_plot(campaign, WAVE_PLOT + aid_plot);
    progress.next_stage = stage_index + 1;
    progress.offered_stage = None;
    progress.wave_remaining = if progress.next_stage < script.stages.len() {
        roll_wave_delay(progress.next_stage)
    } else {
        0.0
    };
    info!("cult wave activated: stage {stage_index}");
}

#[allow(clippy::too_many_arguments)]
fn spawn_stage_consequences(
    commands: &mut Commands,
    db: &ChemDb,
    spots: &CrisisSpots,
    script: &CultScript,
    stage: &CultStageDef,
    stage_index: usize,
    campaign: &mut crate::arc::Campaign,
    radio: &mut RadioLog,
    reward: bool,
) {
    let base = script.stage_ward_base(stage_index);
    let guard_index = script.guard_ward_index(stage_index);
    // The guard's slot is always the last of this stage's three, so reserving
    // up to it also reserves the two anchors — one resize covers the block.
    if campaign.cult_incidents.len() < guard_index + 1 {
        campaign.cult_incidents.resize(guard_index + 1, false);
        campaign.cult_reported.resize(guard_index + 1, false);
    }
    for (offset, incident) in stage.incidents.iter().enumerate() {
        let index = base + offset;
        if campaign.cult_incidents.get(index) == Some(&true) {
            continue;
        }
        let Some(transform) = spots.get(&incident.spot) else {
            error!(
                "cult incident '{}' names missing crisis spot '{}'; skipping",
                incident.name, incident.spot
            );
            continue;
        };
        commands.spawn((
            RitualAnchor {
                index,
                name: incident.name.clone(),
                clue: incident.clue.clone(),
                treatment: incident.treatment.clone(),
                amount: Units::whole(incident.amount as i32),
            },
            CultVisual(incident.visual),
            transform,
            Visibility::default(),
            Interactable::new(format!("{} — examine and treat", incident.name)),
            Replicated,
            crate::until_we_leave_the_lab(),
        ));
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Lab,
                "A new ritual disturbance has been reported somewhere beyond the lab.".to_string(),
            )
            .negative()
            .urgent(),
        );
    }
    // The payment is physical stock at the counter, not an invisible bonus.
    if reward {
        if let Some(reagent) = db.reagents.id_of(&stage.reward_reagent) {
            let mut vial = Container::new(ContainerKind::Bottle);
            let _ = vial.solution.add_profiled(
                reagent,
                Units::whole(stage.reward_amount as i32),
                1.0,
                db.reagents.get(reagent).ph,
            );
            commands.spawn((
                vial,
                Transform::from_translation(REWARD_SPOT),
                Visibility::default(),
                Interactable::new(format!(
                    "Corwin's payment — {}",
                    db.reagents.get(reagent).name
                )),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
        }
    }
    spawn_stage_guard(commands, spots, &stage.guard, guard_index, campaign, radio);
}

fn handle_counter_support(
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    mut progress: ResMut<CultProgress>,
    mut support: MessageReader<crate::arc::CounterSupportApplied>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(mut campaign)) = (script, campaign) else {
        support.clear();
        return;
    };
    for applied in support.read() {
        if applied.antag != crate::arc::AntagId::Cult || applied.campaign != campaign.id {
            continue;
        }
        if !reveal_next_objective(&db, &script, &mut campaign, &mut radio) {
            progress.banked_intel += 1;
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Common,
                    "The department has banked its analysis. The next ritual sign will be easier to identify."
                        .to_string(),
                )
                .positive(),
            );
        }
    }
}

fn reveal_next_objective(
    db: &ChemDb,
    script: &CultScript,
    campaign: &mut crate::arc::Campaign,
    radio: &mut RadioLog,
) -> bool {
    let active = campaign.cult_incidents.len();
    campaign.cult_reported.resize(active, false);
    let Some(index) = campaign
        .cult_incidents
        .iter()
        .enumerate()
        .find(|(index, resolved)| !**resolved && !campaign.cult_reported[*index])
        .map(|(index, _)| index)
    else {
        return false;
    };
    campaign.cult_reported[index] = true;
    let line = objective_intel(db, script, index).unwrap_or_else(|| {
        "A department report confirms another active ritual sign, but its analysis is incomplete."
            .to_string()
    });
    radio.push(
        RadioEntry::new(crate::radio::RadioChannel::Common, line)
            .positive()
            .urgent(),
    );
    true
}

fn objective_intel(db: &ChemDb, script: &CultScript, index: usize) -> Option<String> {
    if index == CultScript::altar_ward_index() {
        return Some(anchor_intel(db, &script.altar));
    }
    for (stage_index, stage) in script.stages.iter().enumerate() {
        let base = script.stage_ward_base(stage_index);
        if let Some(incident) = stage.incidents.get(index.checked_sub(base)?) {
            return Some(anchor_intel(db, incident));
        }
        if index == script.guard_ward_index(stage_index) {
            return Some(format!(
                "{} Security confirms this is a defender, not a bystander; incapacitating them will break another ward.",
                stage.guard.clue
            ));
        }
    }
    None
}

fn anchor_intel(db: &ChemDb, incident: &CultIncidentDef) -> String {
    let family = db
        .reagents
        .id_of(&incident.treatment)
        .and_then(|id| reference_category(db, id))
        .map(|category| category.label())
        .unwrap_or("matching");
    format!(
        "{} Department analysis points to the {family} chemical family.",
        incident.clue
    )
}

/// Stations this stage's guard at its authored spot, unless it is already
/// down. Mirrors the anchor loop's own "already resolved" skip above.
fn spawn_stage_guard(
    commands: &mut Commands,
    spots: &CrisisSpots,
    guard: &CultGuardDef,
    ward_index: usize,
    campaign: &crate::arc::Campaign,
    radio: &mut RadioLog,
) {
    if campaign.cult_incidents.get(ward_index) == Some(&true) {
        return;
    }
    let Some(transform) = spots.get(&guard.spot) else {
        error!(
            "cult guard '{}' names missing crisis spot '{}'; skipping",
            guard.name, guard.spot
        );
        return;
    };
    place_guard(commands, guard, ward_index, transform);
    radio.push(
        RadioEntry::new(
            crate::radio::RadioChannel::Lab,
            "A masked figure has been reported guarding one of the new disturbances.".to_string(),
        )
        .negative()
        .urgent(),
    );
}

/// Builds the stationed cult member itself — shared by a freshly-completed
/// stage and by save restore. A guard arrives like any other crew member
/// (`spawn_crew_member` gives it a `Body`/`Bloodstream`/`Replicated`/route),
/// then immediately stops being one: the arrival route is removed so it
/// stays put rather than walking to the counter, and `Ambient` excludes it
/// from every "how busy is the counter" count the same way a department
/// resident already is (see `crew::Ambient::new`'s own doc).
fn place_guard(
    commands: &mut Commands,
    guard: &CultGuardDef,
    ward_index: usize,
    transform: Transform,
) -> Entity {
    let def = CrewDef {
        name: guard.name.clone(),
        // Deliberately not a real department — `Department::from_role`
        // returns `None` for it everywhere that matters (standing/order
        // grading), and a guard is never routed through either, so this only
        // ever shows up as harmless "unrecognised role" behaviour, never a
        // panic. See the callers of `Department::from_role` across the crate.
        role: "Cult".to_string(),
        color: [0.42, 0.05, 0.10],
    };
    let entity = spawn_crew_member(commands, &def, 0.0);
    commands.entity(entity).remove::<CrewRoute>().insert((
        transform,
        Cultist {
            wards_incident: Some(ward_index),
            tier: guard.tier,
        },
        Ambient::new(0.0),
    ));
    entity
}

/// How close a chemist has to wander before a stationed guard notices and
/// gives chase. A guard has no `CrewRoute` arrival beat to key off — it is
/// placed directly, never walked in — so this is its own trigger, distinct
/// from `showdown::turn_hostile_on_arrival`'s route-phase check.
const GUARD_AGGRO_RADIUS: f32 = 6.0;

/// A cultist that has not noticed anyone yet.
type IdleCultists<'w, 's> =
    Query<'w, 's, (Entity, &'static Transform), (With<Cultist>, Without<crate::showdown::Pursuit>)>;

fn aggro_cultists(
    mut commands: Commands,
    arc_script: Option<Res<crate::arc::Script>>,
    idle: IdleCultists,
    chemists: Query<(&Transform, &crate::body::Bloodstream), With<Chemist>>,
) {
    let Some(arc_script) = arc_script else {
        return;
    };
    let tuning = arc_script.showdown;
    for (entity, transform) in &idle {
        let noticed = chemists.iter().any(|(chemist_transform, blood)| {
            let detection_radius = GUARD_AGGRO_RADIUS * (1.0 - blood.0.concealment());
            transform
                .translation
                .distance_squared(chemist_transform.translation)
                <= detection_radius * detection_radius
        });
        if noticed {
            commands
                .entity(entity)
                .insert(crate::showdown::Pursuit::new(
                    tuning.speed,
                    tuning.hit_every_seconds,
                    tuning.hit_brute,
                ));
        }
    }
}

/// Puts down a base guard exactly like `showdown::Assailant` — any dose that
/// flips `Body.collapsed` wins — and credits its ward. Finale muscle
/// (`wards_incident: None`) is deliberately skipped here;
/// `showdown::resolve_showdown` scores those instead. Because `place_guard`
/// removed `CrewRoute` at spawn, `crew::handle_crew_collapse` (which needs
/// `&mut CrewRoute`) never fires for a guard — no spurious Medical-standing
/// penalty, matching how `Assailant`'s own collapse already never triggers
/// that either.
fn credit_defeated_guards(
    mut commands: Commands,
    mut campaign: Option<ResMut<crate::arc::Campaign>>,
    mut radio: ResMut<RadioLog>,
    // Unconditional, same as `showdown::resolve_showdown`'s own
    // `body.0.collapsed` check — a handful of live guards is cheap enough
    // that a `Changed<Body>` filter would only add a class of "did this fire
    // on the very frame the body actually collapsed" timing edge cases for
    // no real payoff.
    fallen: Query<(Entity, &Cultist, &Body)>,
) {
    let Some(campaign) = campaign.as_mut() else {
        return;
    };
    for (entity, cultist, body) in &fallen {
        let Some(index) = cultist.wards_incident else {
            continue;
        };
        if !body.0.collapsed {
            continue;
        }
        if campaign.cult_incidents.len() <= index {
            campaign.cult_incidents.resize(index + 1, false);
        }
        if campaign.cult_incidents[index] {
            continue;
        }
        campaign.cult_incidents[index] = true;
        campaign.cult_reported.resize(index + 1, false);
        crate::arc::nudge_plot(campaign, WARD_RELIEF);
        commands.entity(entity).despawn();
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Lab,
                "The guard goes down. Whatever it was protecting is exposed now.".to_string(),
            )
            .positive()
            .urgent(),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn expose_finale(
    mut commands: Commands,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<Res<crate::arc::Campaign>>,
    progress: Res<CultProgress>,
    spots: Res<CrisisSpots>,
    focuses: Query<(), With<RitualFocus>>,
    live: Option<Res<crate::showdown::Showdown>>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(arc_script), Some(campaign)) = (script, arc_script, campaign) else {
        return;
    };
    if campaign.outcome.is_some()
        || progress.finale_started
        || live.is_some()
        || !focuses.is_empty()
        || !enough_wards(&campaign)
        || campaign.plot >= arc_script.showdown_at
    {
        return;
    }
    let Some(transform) = spots.get(&script.altar.spot) else {
        error!(
            "cult finale names missing altar spot '{}'; cannot expose focus",
            script.altar.spot
        );
        return;
    };
    commands.spawn((
        RitualFocus,
        CultVisual(CultVisualId::FinaleFocus),
        transform,
        Visibility::default(),
        Interactable::new("Break the exposed outer ward — this starts the final rite"),
        Replicated,
        crate::until_we_leave_the_lab(),
    ));
    radio.push(
        RadioEntry::new(
            crate::radio::RadioChannel::Bridge,
            "The broken signs converge on the Chapel. The outer ward is exposed; Chemistry can force the rite into the open now."
                .to_string(),
        )
        .speaker("Duty Officer")
        .negative()
        .station_wide(),
    );
}

fn start_finale(
    mut commands: Commands,
    arc_script: Option<Res<crate::arc::Script>>,
    mut campaign: Option<ResMut<crate::arc::Campaign>>,
    mut progress: ResMut<CultProgress>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    focuses: Query<(), With<RitualFocus>>,
    mut showdown: MessageWriter<crate::showdown::RequestShowdown>,
) {
    let (Some(arc_script), Some(campaign)) = (arc_script, campaign.as_mut()) else {
        requests.clear();
        return;
    };
    for request in requests.read() {
        if !focuses.contains(request.target) || !enough_wards(campaign) {
            continue;
        }
        progress.finale_started = true;
        campaign.plot = campaign.plot.max(arc_script.showdown_at);
        showdown.write(crate::showdown::RequestShowdown {
            campaign: campaign.id,
        });
        commands.entity(request.target).despawn();
        break;
    }
}

fn remember_started_finale(
    live: Option<Res<crate::showdown::Showdown>>,
    mut progress: ResMut<CultProgress>,
) {
    if live.is_some() {
        progress.finale_started = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_incident_delivery(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut campaign: Option<ResMut<crate::arc::Campaign>>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    anchors: Query<&RitualAnchor>,
    containers: Query<(Entity, &Container, &HeldBy)>,
    chemists: Query<(Entity, &Chemist)>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(campaign) = campaign.as_mut() else {
        requests.clear();
        return;
    };
    for request in requests.read() {
        let Ok(anchor) = anchors.get(request.target) else {
            continue;
        };
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Some((container_entity, container, _)) =
            containers.iter().find(|(_, _, held)| held.0 == player)
        else {
            let hint = db
                .reagents
                .id_of(&anchor.treatment)
                .and_then(|id| reference_category(&db, id))
                .map(|category| category.label())
                .unwrap_or("matching");
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Lab,
                    format!(
                        "{} Its residue reacts like it needs {hint} chemistry.",
                        anchor.clue
                    ),
                )
                .urgent(),
            );
            continue;
        };
        let Some(treatment) = db.reagents.id_of(&anchor.treatment) else {
            continue;
        };
        let landed = incident_units(&db, &container.solution, treatment);
        if landed < anchor.amount {
            let hint = reference_category(&db, treatment)
                .map(|category| category.label())
                .unwrap_or("a closer match");
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Lab,
                    format!(
                        "The {} rejects that mixture without taking it. Its reaction points toward {hint} chemistry.",
                        anchor.name
                    ),
                )
                .negative(),
            );
            continue;
        }
        if campaign.cult_incidents.len() <= anchor.index {
            campaign.cult_incidents.resize(anchor.index + 1, false);
        }
        if campaign.cult_incidents[anchor.index] {
            continue;
        }
        campaign.cult_incidents[anchor.index] = true;
        campaign.cult_reported.resize(anchor.index + 1, false);
        crate::arc::nudge_plot(campaign, WARD_RELIEF);
        commands.entity(container_entity).despawn();
        commands.entity(request.target).despawn();
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Lab,
                format!(
                    "The {} gutters out. The ritual has lost some of its hold.",
                    anchor.name
                ),
            )
            .positive()
            .urgent(),
        );
    }
}

fn incident_units(
    db: &ChemDb,
    solution: &chem_sim::Solution,
    treatment: chem_sim::ReagentId,
) -> Units {
    let category = reference_category(db, treatment);
    solution
        .iter()
        .filter(|(id, amount)| {
            amount.is_positive()
                && match category {
                    Some(category) => db.reagents.get(*id).categories.contains(&category),
                    None => *id == treatment,
                }
        })
        .map(|(_, amount)| amount)
        .sum()
}

/// Attach the authored GLB as a child so the replicated gameplay transform
/// stays authoritative and presentation can later pulse/dissolve locally.
fn dress_incidents(
    mut commands: Commands,
    assets: Option<Res<CultVisualAssets>>,
    new: Query<(Entity, &CultVisual), Added<CultVisual>>,
) {
    let Some(assets) = assets else { return };
    for (entity, visual) in &new {
        commands.entity(entity).insert_if_new(Visibility::default());
        commands.spawn((
            Name::new(format!("Cult {:?} visual", visual.0)),
            WorldAssetRoot(assets.scenes[visual.0.index()].clone()),
            // Crisis spots use the same one-metre body-origin convention as
            // guards. Props are authored from the floor, so only their visual
            // child moves down; the replicated interaction root stays put.
            Transform::from_xyz(0.0, -1.0, 0.0),
            Visibility::default(),
            ChildOf(entity),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::{Department, Outcome};
    use std::collections::HashSet;

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn script() -> CultScript {
        ron::from_str(include_str!("../../assets/data/station.cult.ron")).unwrap()
    }

    #[test]
    fn cult_ron_parses_and_the_identity_is_off_the_ordinary_roster() {
        let data = data();
        let script = script();
        assert!(!script.name.trim().is_empty());
        assert!(
            Department::from_role(&script.role).is_some(),
            "'{}' names a role no department recognises",
            script.role
        );
        assert!(
            script.stages.len() >= 2,
            "no escalation without at least two stages"
        );
        let roster: Vec<crate::crew::CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        assert!(roster.iter().all(|member| member.name != script.name));
        let mut spots = HashSet::new();
        for stage in &script.stages {
            assert!(
                data.reagents.id_of(&stage.reagent).is_some(),
                "'{}' names no real reagent",
                stage.reagent
            );
            assert!(!stage.pretext.trim().is_empty());
            assert!(!stage.ritual_line.trim().is_empty());
            assert_eq!(
                stage.incidents.len(),
                2,
                "each accepted deal must leave two leads"
            );
            assert!(data.reagents.id_of(&stage.reward_reagent).is_some());
            for incident in &stage.incidents {
                assert!(!incident.name.trim().is_empty());
                assert!(!incident.clue.trim().is_empty());
                assert!(!incident.spot.trim().is_empty());
                assert!(
                    spots.insert(incident.spot.as_str()),
                    "'{}' is reused; each manifestation needs its own authored place",
                    incident.spot
                );
                assert!(data.reagents.id_of(&incident.treatment).is_some());
                assert!(incident.amount > 0);
            }
            assert!(!stage.guard.name.trim().is_empty());
            assert!(!stage.guard.clue.trim().is_empty());
            assert!(!stage.guard.spot.trim().is_empty());
            assert!(
                spots.insert(stage.guard.spot.as_str()),
                "'{}' is reused; the guard needs its own authored place",
                stage.guard.spot
            );
            assert!(
                roster.iter().all(|member| member.name != stage.guard.name),
                "'{}' is on the ordinary roster, so a legitimate order could pick it",
                stage.guard.name
            );
        }
        assert!(!script.altar.name.trim().is_empty());
        assert!(!script.altar.clue.trim().is_empty());
        assert!(!script.altar.spot.trim().is_empty());
        assert!(
            spots.insert(script.altar.spot.as_str()),
            "'{}' is reused; the altar needs its own authored place",
            script.altar.spot
        );
        assert!(data.reagents.id_of(&script.altar.treatment).is_some());
        assert!(script.altar.amount > 0);

        let expected_visuals = [
            CultVisualId::WetChalkSigil,
            CultVisualId::WhisperingResidue,
            CultVisualId::BleedingOfferingBowl,
            CultVisualId::ScorchedInvocation,
            CultVisualId::AirlessCandle,
            CultVisualId::RiftSealScar,
        ];
        let authored_visuals: Vec<_> = script
            .stages
            .iter()
            .flat_map(|stage| stage.incidents.iter().map(|incident| incident.visual))
            .collect();
        assert_eq!(authored_visuals, expected_visuals);
        assert_eq!(script.altar.visual, CultVisualId::OuterAltarWard);
        assert_eq!(
            script
                .stages
                .iter()
                .map(|stage| stage.guard.tier)
                .collect::<Vec<_>>(),
            [
                CultistTier::Watching,
                CultistTier::Silent,
                CultistTier::Blooded
            ]
        );
    }

    #[test]
    fn every_cult_visual_is_a_valid_game_asset() {
        for visual in CultVisualId::ALL {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(visual.path());
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
            let gltf = bevy::gltf::gltf::Gltf::from_slice(&bytes)
                .unwrap_or_else(|error| panic!("{} is not valid glTF: {error}", path.display()));
            assert_eq!(
                gltf.scenes().count(),
                1,
                "{} needs one runtime scene",
                path.display()
            );
        }
    }

    #[test]
    fn the_final_stage_reagent_has_real_release_behavior() {
        // Proves the "real chemistry" claim in the module doc rather than
        // trusting its prose. Flash powder is now transportable until it is
        // released, so the hazard belongs to its product profile instead of
        // firing as a generic reaction smoke effect while it is prepared.
        let data = data();
        let script = script();
        let last = script.stages.last().expect("at least one stage");
        let reaction = data
            .reactions
            .iter()
            .find(|reaction| {
                reaction
                    .products
                    .iter()
                    .any(|(id, _)| data.reagents.get(*id).key == last.reagent)
            })
            .unwrap_or_else(|| panic!("'{}' is never produced by any reaction", last.reagent));
        let product = reaction
            .products
            .iter()
            .find_map(|(id, _)| (data.reagents.get(*id).key == last.reagent).then_some(*id))
            .expect("the reaction was selected by this product");
        assert!(
            data.reagents
                .get(product)
                .world_effects
                .iter()
                .copied()
                .any(chem_sim::WorldEffect::is_harmful),
            "'{}' needs a real hazardous release effect, or the finale is all bark",
            last.reagent
        );
    }

    fn resolution_app() -> App {
        let arc_script: crate::arc::ArcScript =
            ron::from_str(include_str!("../../assets/data/station.arc.ron")).unwrap();
        let steps = arc_script
            .antagonist(crate::arc::AntagId::Cult)
            .unwrap()
            .counter_steps
            .len();
        let mut app = App::new();
        app.insert_resource(threat::Authored(script()))
            .insert_resource(crate::threat::Authored(arc_script))
            .insert_resource(crate::arc::Campaign::new(
                crate::arc::AntagId::Cult,
                crate::arc::Mode::Chemist,
                steps,
            ))
            .insert_resource(ChemDb(data()))
            .insert_resource(CrisisSpots::default())
            .init_resource::<CultProgress>()
            .init_resource::<RadioLog>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_cult_resolution);
        app
    }

    fn resolve(app: &mut App, name: &str, outcome: Outcome) {
        let cult_name = app.world().resource::<Script>().name.clone();
        if name == cult_name {
            let stage = app.world().resource::<CultProgress>().next_stage;
            app.world_mut().resource_mut::<CultProgress>().offered_stage = Some(stage);
        }
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Cargo".to_string(),
            reagent: None,
            category: None,
            outcome,
            kind: crate::orders::OrderKind::Hostile,
            quality: None,
            development: false,
            campaign: None,
            counter_step: None,
        });
        app.update();
    }

    #[test]
    fn a_successful_stage_advances_the_ritual() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Success);

        assert_eq!(app.world().resource::<CultProgress>().next_stage, 1);
    }

    #[test]
    fn declining_a_stage_never_advances_the_ritual() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Expired);
        resolve(&mut app, &name, Outcome::Wrong);

        assert_eq!(
            app.world().resource::<CultProgress>().next_stage,
            0,
            "only a real, good delivery should move the ritual forward"
        );
    }

    #[test]
    fn an_unrelated_resolution_never_advances_the_ritual() {
        let mut app = resolution_app();

        resolve(&mut app, "Someone Else", Outcome::Success);

        assert_eq!(app.world().resource::<CultProgress>().next_stage, 0);
    }

    // -- the campaign arc --------------------------------------------------

    /// `resolution_app` with a live Cult campaign attached.
    fn campaign_app() -> App {
        let arc_script: crate::arc::ArcScript =
            ron::from_str(include_str!("../../assets/data/station.arc.ron")).unwrap();
        let steps = arc_script
            .antagonist(crate::arc::AntagId::Cult)
            .unwrap()
            .counter_steps
            .len();

        let cult_script = script();
        let mut spots = CrisisSpots::default();
        for (index, incident) in cult_script
            .stages
            .iter()
            .flat_map(|stage| &stage.incidents)
            .enumerate()
        {
            spots.insert(
                incident.spot.clone(),
                Transform::from_xyz(index as f32 * 3.0, 1.0, index as f32 * -2.0),
            );
        }
        for (index, stage) in cult_script.stages.iter().enumerate() {
            spots.insert(
                stage.guard.spot.clone(),
                Transform::from_xyz(100.0 + index as f32 * 3.0, 1.0, -100.0),
            );
        }
        spots.insert(
            cult_script.altar.spot.clone(),
            Transform::from_xyz(200.0, 1.0, 200.0),
        );

        let mut app = resolution_app();
        app.insert_resource(crate::threat::Authored(arc_script))
            .insert_resource(crate::arc::Campaign::new(
                crate::arc::AntagId::Cult,
                crate::arc::Mode::Chemist,
                steps,
            ))
            .insert_resource(spots);
        app
    }

    #[test]
    fn a_fulfilled_stage_moves_the_ritual_toward_its_end() {
        let mut app = campaign_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let per_aid = app.world().resource::<crate::arc::Script>().plot_per_aid;

        resolve(&mut app, &name, Outcome::Success);

        assert_eq!(
            app.world().resource::<crate::arc::Campaign>().plot,
            WAVE_PLOT + per_aid,
            "the ritual advancing is the Cult advancing — otherwise the whole \
             authored chain could run with the plot meter untouched"
        );
    }

    #[test]
    fn declining_a_stage_moves_nothing_at_all() {
        let mut app = campaign_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Wrong);

        assert_eq!(app.world().resource::<crate::arc::Campaign>().plot, 0);
        assert_eq!(app.world().resource::<CultProgress>().next_stage, 0);
    }

    #[test]
    fn an_accepted_stage_creates_two_visible_incidents_and_a_reward() {
        let mut app = campaign_app();
        app.insert_resource(ChemDb(data()));
        let name = app.world().resource::<Script>().0.name.clone();
        resolve(&mut app, &name, Outcome::Success);
        app.world_mut().flush();
        let mut anchors = app.world_mut().query::<(&RitualAnchor, &Transform)>();
        let mut placed: Vec<(usize, Vec3)> = anchors
            .iter(app.world())
            .map(|(anchor, transform)| (anchor.index, transform.translation))
            .collect();
        placed.sort_by_key(|(index, _)| *index);
        assert_eq!(
            placed,
            vec![
                (1, Vec3::new(0.0, 1.0, 0.0)),
                (2, Vec3::new(3.0, 1.0, -2.0))
            ]
        );
        assert_eq!(
            app.world_mut()
                .query::<&Container>()
                .iter(app.world())
                .count(),
            1
        );
        assert_eq!(
            app.world()
                .resource::<crate::arc::Campaign>()
                .cult_incidents,
            // Index 0 is the altar's reserved slot — see
            // `CultScript::stage_ward_base` — resized-into alongside stage
            // 0's own two anchors and guard even though nothing has spawned
            // there yet.
            vec![false, false, false, false]
        );
    }

    #[test]
    fn a_missing_cult_spot_skips_only_that_manifestation() {
        let mut app = campaign_app();
        app.insert_resource(ChemDb(data()));
        let first = script().stages[0].incidents[0].clone();
        let mut spots = CrisisSpots::default();
        spots.insert(
            first.spot,
            Transform::from_translation(Vec3::new(9.0, 1.0, 4.0)),
        );
        app.insert_resource(spots);
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Success);
        app.world_mut().flush();

        let mut anchors = app.world_mut().query::<(&RitualAnchor, &Transform)>();
        let spawned: Vec<(usize, Vec3)> = anchors
            .iter(app.world())
            .map(|(anchor, transform)| (anchor.index, transform.translation))
            .collect();
        assert_eq!(spawned, vec![(1, Vec3::new(9.0, 1.0, 4.0))]);
        assert_eq!(
            app.world()
                .resource::<crate::arc::Campaign>()
                .cult_incidents,
            vec![false, false, false, false],
            "the skipped flag stays unresolved so a later session can restore it"
        );
    }

    #[test]
    fn save_restore_waits_for_the_map_and_never_wraps_incident_spots() {
        let mut content = script();
        let template = content.stages[0].incidents[0].clone();
        for index in 6..8 {
            let mut extra = template.clone();
            extra.name = format!("Restored manifestation {index}");
            extra.spot = format!("cult.restore_{index}");
            content.stages.last_mut().unwrap().incidents.push(extra);
        }
        let incidents: Vec<CultIncidentDef> = content
            .stages
            .iter()
            .flat_map(|stage| stage.incidents.iter().cloned())
            .collect();
        assert!(incidents.len() > 6);

        let mut spots = CrisisSpots::default();
        let expected: Vec<Vec3> = incidents
            .iter()
            .enumerate()
            .map(|(index, incident)| {
                let position = Vec3::new(index as f32 * 2.0, 1.0, index as f32 * -3.0);
                spots.insert(incident.spot.clone(), Transform::from_translation(position));
                position
            })
            .collect();
        let mut campaign =
            crate::arc::Campaign::new(crate::arc::AntagId::Cult, crate::arc::Mode::Chemist, 0);
        // Not `incidents.len()`: the ward vector also reserves the altar's
        // slot and one guard slot per stage (see `CultScript::guard_ward_index`),
        // and the padded last stage means its block is wider than the
        // shipped content's. The guard slot of the last stage is always the
        // highest index in the whole scheme.
        let vector_len = content.guard_ward_index(content.stages.len() - 1) + 1;
        campaign.cult_incidents = vec![false; vector_len];

        let mut app = App::new();
        app.insert_resource(threat::Authored(content))
            .insert_resource(campaign)
            .insert_resource(ChemDb(data()))
            .insert_resource(spots)
            .init_resource::<CultIncidentsRestored>()
            .init_resource::<RadioLog>()
            .add_systems(
                Update,
                restore_incidents.run_if(resource_exists::<MapReady>),
            );

        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&RitualAnchor>()
                .iter(app.world())
                .count(),
            0,
            "save entities must not restore against an incomplete map"
        );
        assert!(!app.world().resource::<CultIncidentsRestored>().0);

        app.insert_resource(MapReady);
        app.update();

        let mut query = app.world_mut().query::<(&RitualAnchor, &Transform)>();
        let mut restored: Vec<(usize, Vec3)> = query
            .iter(app.world())
            .map(|(anchor, transform)| (anchor.index, transform.translation))
            .collect();
        restored.sort_by_key(|(index, _)| *index);
        assert_eq!(restored.len(), incidents.len());
        for ((index, actual), expected) in restored.iter().zip(expected) {
            assert_eq!(*actual, expected, "incident {index} reused another spot");
        }
    }

    #[test]
    fn treatment_accepts_a_category_peer_but_rejects_an_unrelated_chemical() {
        let db = ChemDb(data());
        let cure = db.reagents.id_of("dylovene").unwrap();
        let tricordrazine = db.reagents.id_of("tricordrazine").unwrap();
        let water = db.reagents.id_of("water").unwrap();
        let mut valid = chem_sim::Solution::unbounded();
        let mut invalid = chem_sim::Solution::unbounded();
        let _ = valid.add(tricordrazine, Units::whole(10));
        let _ = invalid.add(water, Units::whole(10));
        assert_eq!(incident_units(&db, &valid, cure), Units::whole(10));
        assert!(!incident_units(&db, &invalid, cure).is_positive());
    }

    #[test]
    fn the_closed_lab_slows_the_ritual_clock_without_stopping_it() {
        let mut open = 10.0;
        let mut closed = 10.0;
        assert!(!tick_wave_clock(&mut open, 4.0, true));
        assert!(!tick_wave_clock(&mut closed, 4.0, false));
        assert_eq!(open, 6.0);
        assert_eq!(closed, 8.0);

        assert!(tick_wave_clock(&mut closed, 16.0, false));
    }

    #[test]
    fn wave_delays_use_the_authored_first_and_later_windows() {
        for _ in 0..64 {
            let first = roll_wave_delay(0);
            let later = roll_wave_delay(1);
            assert!((FIRST_WAVE_SECONDS.0..=FIRST_WAVE_SECONDS.1).contains(&first));
            assert!((LATER_WAVE_SECONDS.0..=LATER_WAVE_SECONDS.1).contains(&later));
        }
    }

    #[test]
    fn five_direct_interventions_expose_the_finale_but_four_do_not() {
        let mut campaign =
            crate::arc::Campaign::new(crate::arc::AntagId::Cult, crate::arc::Mode::Chemist, 4);
        campaign.cult_incidents = vec![true; FINALE_WARDS - 1];
        assert!(!enough_wards(&campaign));
        campaign.cult_incidents.push(true);
        assert!(enough_wards(&campaign));
    }

    #[test]
    fn department_intel_reports_one_live_objective_at_a_time() {
        let db = ChemDb(data());
        let script = script();
        let mut campaign =
            crate::arc::Campaign::new(crate::arc::AntagId::Cult, crate::arc::Mode::Chemist, 4);
        campaign.cult_incidents = vec![false, false, false];
        let mut radio = RadioLog::default();

        assert!(reveal_next_objective(
            &db,
            &script,
            &mut campaign,
            &mut radio
        ));
        assert_eq!(campaign.cult_reported, vec![true, false, false]);
        assert!(reveal_next_objective(
            &db,
            &script,
            &mut campaign,
            &mut radio
        ));
        assert_eq!(campaign.cult_reported, vec![true, true, false]);
    }

    // -- guards and the altar -----------------------------------------------

    #[test]
    fn a_fulfilled_stage_spawns_a_guard_at_its_authored_ward_index() {
        let mut app = campaign_app();
        app.insert_resource(ChemDb(data()));
        let name = app.world().resource::<Script>().0.name.clone();
        let expected_index = app.world().resource::<Script>().0.guard_ward_index(0);

        resolve(&mut app, &name, Outcome::Success);
        app.world_mut().flush();

        let mut guards = app.world_mut().query::<&Cultist>();
        let wards: Vec<Option<usize>> =
            guards.iter(app.world()).map(|c| c.wards_incident).collect();
        assert_eq!(wards, vec![Some(expected_index)]);
    }

    #[test]
    fn a_missing_guard_spot_skips_only_that_guard() {
        let mut app = campaign_app();
        app.insert_resource(ChemDb(data()));
        // A registry with the stage's two anchor spots but not its guard's.
        let cult_script = script();
        let mut spots = CrisisSpots::default();
        for incident in &cult_script.stages[0].incidents {
            spots.insert(incident.spot.clone(), Transform::from_xyz(1.0, 1.0, 1.0));
        }
        app.insert_resource(spots);
        let name = app.world().resource::<Script>().0.name.clone();
        let guard_index = cult_script.guard_ward_index(0);

        resolve(&mut app, &name, Outcome::Success);
        app.world_mut().flush();

        assert_eq!(
            app.world_mut()
                .query::<&Cultist>()
                .iter(app.world())
                .count(),
            0,
            "no map spot, no guard — but the anchors it did have a place for should still land"
        );
        assert_eq!(
            app.world_mut()
                .query::<&RitualAnchor>()
                .iter(app.world())
                .count(),
            2
        );
        assert_eq!(
            app.world()
                .resource::<crate::arc::Campaign>()
                .cult_incidents
                .get(guard_index),
            Some(&false),
            "the skipped guard's ward stays unresolved so a later session can restore it"
        );
    }

    #[test]
    fn putting_a_guard_down_credits_its_ward_and_despawns_it() {
        let mut app = campaign_app();
        app.add_systems(Update, credit_defeated_guards);
        let index = app.world().resource::<Script>().0.guard_ward_index(0);
        app.world_mut()
            .resource_mut::<crate::arc::Campaign>()
            .cult_incidents = vec![false; index + 1];
        let entity = app
            .world_mut()
            .spawn((
                Cultist {
                    wards_incident: Some(index),
                    tier: CultistTier::Watching,
                },
                Body::default(),
            ))
            .id();
        app.world_mut().get_mut::<Body>(entity).unwrap().0.collapsed = true;

        app.update();

        assert!(
            app.world()
                .resource::<crate::arc::Campaign>()
                .cult_incidents[index]
        );
        assert_eq!(
            app.world_mut()
                .query::<&Cultist>()
                .iter(app.world())
                .count(),
            0,
            "a defeated guard should be gone, not left standing"
        );
    }

    #[test]
    fn an_idle_guard_notices_a_nearby_chemist_and_gives_chase() {
        let mut app = campaign_app();
        app.add_systems(Update, aggro_cultists);
        let near = app
            .world_mut()
            .spawn((
                Cultist {
                    wards_incident: Some(3),
                    tier: CultistTier::Watching,
                },
                Transform::from_xyz(0.0, 0.0, 0.0),
            ))
            .id();
        let far = app
            .world_mut()
            .spawn((
                Cultist {
                    wards_incident: Some(6),
                    tier: CultistTier::Silent,
                },
                Transform::from_xyz(0.0, 0.0, GUARD_AGGRO_RADIUS * 10.0),
            ))
            .id();
        app.world_mut().spawn((
            Chemist {
                client: bevy_replicon::prelude::ClientId::Server,
            },
            Transform::from_xyz(1.0, 0.0, 0.0),
            crate::body::Bloodstream::default(),
        ));

        app.update();

        assert!(
            app.world().get::<crate::showdown::Pursuit>(near).is_some(),
            "a chemist standing right next to it should be noticed"
        );
        assert!(
            app.world().get::<crate::showdown::Pursuit>(far).is_none(),
            "nothing should chase from clear across the map"
        );
    }

    #[test]
    fn saturn_x_concealment_shortens_hostile_visual_detection() {
        let mut app = campaign_app();
        app.add_systems(Update, aggro_cultists);
        let guard = app
            .world_mut()
            .spawn((
                Cultist {
                    wards_incident: Some(3),
                    tier: CultistTier::Watching,
                },
                Transform::from_xyz(0.0, 0.0, 0.0),
            ))
            .id();
        let mut blood = crate::body::Bloodstream::default();
        blood
            .0
            .add_status(chem_sim::StatusKind::Obscured, 10.0, 1.5);
        app.world_mut().spawn((
            Chemist {
                client: bevy_replicon::prelude::ClientId::Server,
            },
            Transform::from_xyz(4.0, 0.0, 0.0),
            blood,
        ));

        app.update();

        assert!(
            app.world().get::<crate::showdown::Pursuit>(guard).is_none(),
            "a concealed chemist beyond the shortened radius should remain unnoticed"
        );
    }

    #[test]
    fn the_altar_is_restored_like_any_other_incident() {
        let mut app = campaign_app();
        app.insert_resource(MapReady);
        app.init_resource::<CultIncidentsRestored>();
        app.insert_resource(ChemDb(data()))
            .add_systems(Update, restore_incidents);

        app.update();
        app.world_mut().flush();

        let altar_index = CultScript::altar_ward_index();
        let mut anchors = app.world_mut().query::<&RitualAnchor>();
        let placed: Vec<usize> = anchors
            .iter(app.world())
            .map(|anchor| anchor.index)
            .collect();
        assert_eq!(
            placed,
            vec![altar_index],
            "the altar should exist from the start, unlike any stage-gated anchor"
        );
    }

    #[test]
    fn treating_the_altar_grants_its_own_ward() {
        let mut app = campaign_app();
        app.insert_resource(ChemDb(data()));
        app.insert_resource(MapReady);
        app.init_resource::<CultIncidentsRestored>();
        app.add_systems(
            Update,
            (restore_incidents, handle_incident_delivery).chain(),
        );
        app.add_message::<FromClient<InteractRequested>>();

        app.update();
        app.world_mut().flush();

        let altar_index = CultScript::altar_ward_index();
        let altar_entity = app
            .world_mut()
            .query::<(Entity, &RitualAnchor)>()
            .iter(app.world())
            .find(|(_, anchor)| anchor.index == altar_index)
            .map(|(entity, _)| entity)
            .expect("the altar should already be in the world");

        let cult_script = script();
        let treatment = data().reagents.id_of(&cult_script.altar.treatment).unwrap();
        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(treatment, Units::whole(cult_script.altar.amount as i32));
        let mut vial = Container::new(ContainerKind::Bottle);
        vial.solution = solution;
        let player = app
            .world_mut()
            .spawn(Chemist {
                client: bevy_replicon::prelude::ClientId::Server,
            })
            .id();
        app.world_mut().spawn((vial, HeldBy(player)));
        app.world_mut().write_message(FromClient {
            client_id: bevy_replicon::prelude::ClientId::Server,
            message: InteractRequested {
                target: altar_entity,
            },
        });

        app.update();

        assert!(
            app.world()
                .resource::<crate::arc::Campaign>()
                .cult_incidents[altar_index],
            "a properly-dosed altar should credit its ward exactly like any other incident"
        );
    }

    #[test]
    fn the_ritual_is_silent_in_a_save_that_drew_someone_else() {
        // The gate this thread gained when it was promoted from a Cargo
        // curiosity to one of the five main antagonists.
        let mut app = App::new();
        app.insert_resource(crate::arc::Campaign::new(
            crate::arc::AntagId::Spy,
            crate::arc::Mode::Chemist,
            4,
        ));
        assert!(
            !app.world_mut().run_system_cached(cult_runs).unwrap(),
            "a save fighting the Syndicate should never see an acolyte"
        );

        app.insert_resource(crate::arc::Campaign::new(
            crate::arc::AntagId::Cult,
            crate::arc::Mode::Chemist,
            4,
        ));
        assert!(app.world_mut().run_system_cached(cult_runs).unwrap());
    }

    fn cult_runs(campaign: Option<Res<crate::arc::Campaign>>) -> bool {
        crate::arc::is_active(crate::arc::AntagId::Cult)(campaign)
    }
}
