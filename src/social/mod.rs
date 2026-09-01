//! Career-persistent relationships for the station's named residents.
//!
//! This module deliberately owns the secret that one first-wave resident may
//! also be their department's minor antagonist.  The assignment never
//! replicates: clients receive only physical evidence, public favor summaries,
//! and the speech those authority-owned facts produce.

use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::audio::{EmitWorldSfx, Sfx};
use crate::body::{ApplyHeldRequested, Bloodstream, Body};
use crate::containers::{Container, ContainerKind, HeldBy};
use crate::crew::{Ambient, CrewMember, CrewPhase, CrewRoute};
use crate::interaction::{authority_segment_blocked, InteractRequested, Interactable};
use crate::net::is_authority;
use crate::orders::{Department, Shift};
use crate::player::Chemist;
use crate::speech::{say, Speech, SpeechTone};
use crate::AppState;

pub const OKONKWO: &str = "Nurse Okonkwo";
pub const SATO: &str = "Miner Sato";
pub const REYES: &str = "Officer Reyes";
pub const BEX: &str = "Warden Bex";

pub const RESIDENT_NAMES: [&str; 8] = [
    "Dr. Vance",
    OKONKWO,
    REYES,
    BEX,
    "Tech Lindqvist",
    SATO,
    "Botanist Ivy",
    "Chef Dubois",
];

const FAVOR_FIRST_SECONDS: f32 = 120.0;
const FAVOR_GAP_SECONDS: f32 = 180.0;
const FAVOR_TIMEOUT_SECONDS: f32 = 240.0;
const OBSERVATION_RANGE: f32 = 8.0;

/// The department grouping shown in the social directory. This is authored
/// identity, not a secret assignment, and is shared by the relationship cards
/// and their nearby department shop.
pub fn resident_department(name: &str) -> Option<Department> {
    match name {
        "Dr. Vance" | OKONKWO => Some(Department::Medical),
        REYES | BEX => Some(Department::Security),
        "Tech Lindqvist" => Some(Department::Engineering),
        SATO => Some(Department::Cargo),
        "Botanist Ivy" | "Chef Dubois" => Some(Department::Service),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Temperament {
    Warm,
    Blunt,
    Cautious,
    Exacting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    People,
    Procedure,
    Resources,
    Recognition,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationshipTier {
    Burned,
    Wary,
    #[default]
    Neutral,
    Trusted,
}

impl RelationshipTier {
    fn helped(self) -> Self {
        match self {
            Self::Burned => Self::Wary,
            Self::Wary | Self::Neutral | Self::Trusted => Self::Trusted,
        }
    }

    fn refused(self) -> Self {
        match self {
            Self::Trusted => Self::Neutral,
            Self::Neutral => Self::Wary,
            Self::Wary | Self::Burned => Self::Burned,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialProfile {
    pub temperament: Temperament,
    pub priority: Priority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResidentAntagonist {
    OkonkwoQuack,
    SatoSmuggler,
    ReyesBentGuard,
}

impl ResidentAntagonist {
    pub const ALL: [Self; 3] = [Self::OkonkwoQuack, Self::SatoSmuggler, Self::ReyesBentGuard];

    pub const fn resident(self) -> &'static str {
        match self {
            Self::OkonkwoQuack => OKONKWO,
            Self::SatoSmuggler => SATO,
            Self::ReyesBentGuard => REYES,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AntagonistResolution {
    #[default]
    Hidden,
    Exposed,
    Reported,
    Protected,
    Turned,
}

impl AntagonistResolution {
    pub const fn stops_threat(self) -> bool {
        matches!(self, Self::Reported | Self::Turned)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FavorOutcome {
    #[default]
    Unresolved,
    Helped,
    Compromised,
    Refused,
    Deceived,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidentRelationship {
    pub tier: RelationshipTier,
    #[serde(default)]
    pub helpful: u8,
    #[serde(default)]
    pub reckless: u8,
    #[serde(default)]
    pub dishonest: u8,
    #[serde(default)]
    pub chain_stage: u8,
    #[serde(default)]
    pub last_outcome: FavorOutcome,
}

/// A line the player actually heard from a persistent resident during this
/// save. It is saved with the shared social state, then exposed through a
/// separate public component so a co-op guest can review the same shared
/// conversation without receiving any hidden profile or allegiance data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogueLine {
    pub speaker: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersonalHistory {
    #[default]
    New,
    InProgress,
    Established,
}

/// The deliberately qualitative relationship facts that may cross the wire.
/// Exact impressions, temperament, priority and antagonist assignment remain
/// exclusively inside [`SocialState`].
#[derive(Component, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRelationship {
    pub tier: RelationshipTier,
    pub last_outcome: FavorOutcome,
    pub personal_history: PersonalHistory,
}

/// The complete already-spoken transcript that may cross the wire.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationHistory {
    pub lines: Vec<DialogueLine>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveFavor {
    pub owner: String,
    pub stage: u8,
    pub remaining_seconds: u32,
}

/// The complete authority-owned social save seam.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialState {
    /// False is the migration marker for a career written before this module.
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub profiles: BTreeMap<String, SocialProfile>,
    #[serde(default)]
    pub relationships: BTreeMap<String, ResidentRelationship>,
    /// `None` is deliberately the legacy-career value. Fresh careers always
    /// receive exactly one assignment.
    #[serde(default)]
    pub resident_antagonist: Option<ResidentAntagonist>,
    #[serde(default)]
    pub antagonist_resolution: AntagonistResolution,
    #[serde(default)]
    pub evidence_progress: u8,
    #[serde(default)]
    pub evidence_spawned: bool,
    #[serde(default)]
    pub active_favor: Option<ActiveFavor>,
    #[serde(default)]
    pub favor_shift: u32,
    #[serde(default)]
    pub favor_stages_this_shift: u8,
    #[serde(default)]
    pub favored_this_shift: BTreeSet<String>,
    #[serde(default)]
    pub dialogue_history: BTreeMap<String, Vec<DialogueLine>>,
    #[serde(default = "first_incident_id")]
    pub next_incident_id: u64,
}

const fn first_incident_id() -> u64 {
    1
}

impl SocialState {
    pub fn fresh() -> Self {
        Self::fresh_with(&mut rand::rng())
    }

    pub fn fresh_with(rng: &mut impl Rng) -> Self {
        let temperaments = [
            Temperament::Warm,
            Temperament::Blunt,
            Temperament::Cautious,
            Temperament::Exacting,
        ];
        let priorities = [
            Priority::People,
            Priority::Procedure,
            Priority::Resources,
            Priority::Recognition,
        ];
        let mut profiles = BTreeMap::new();
        let mut relationships = BTreeMap::new();
        for name in RESIDENT_NAMES {
            profiles.insert(
                name.to_string(),
                SocialProfile {
                    temperament: *temperaments
                        .choose(rng)
                        .expect("non-empty temperament pool"),
                    priority: *priorities.choose(rng).expect("non-empty priority pool"),
                },
            );
            relationships.insert(name.to_string(), ResidentRelationship::default());
        }
        Self {
            initialized: true,
            profiles,
            relationships,
            resident_antagonist: Some(
                *ResidentAntagonist::ALL
                    .choose(rng)
                    .expect("non-empty resident antagonist pool"),
            ),
            next_incident_id: first_incident_id(),
            ..default()
        }
    }

    /// A pre-feature save receives profiles but no surprise mid-career
    /// antagonist replacement.
    pub fn migrate_legacy() -> Self {
        let mut migrated = Self::fresh();
        migrated.resident_antagonist = None;
        migrated
    }

    pub fn profile(&self, name: &str) -> Option<SocialProfile> {
        self.profiles.get(name).copied()
    }

    pub fn selected(&self, antagonist: ResidentAntagonist) -> bool {
        self.resident_antagonist == Some(antagonist)
    }

    pub fn threat_identity<'a>(
        &'a self,
        antagonist: ResidentAntagonist,
        outsider: &'a str,
    ) -> &'a str {
        if self.selected(antagonist) {
            antagonist.resident()
        } else {
            outsider
        }
    }

    pub fn threat_runs(&self, antagonist: ResidentAntagonist) -> bool {
        !self.selected(antagonist) || !self.antagonist_resolution.stops_threat()
    }

    pub fn relationship_mut(&mut self, name: &str) -> &mut ResidentRelationship {
        self.relationships.entry(name.to_string()).or_default()
    }

    fn next_incident(&mut self) -> u64 {
        let id = self.next_incident_id.max(1);
        self.next_incident_id = id.saturating_add(1);
        id
    }

    fn remember_dialogue(&mut self, resident: &str, text: &str) {
        let lines = self
            .dialogue_history
            .entry(resident.to_string())
            .or_default();
        lines.push(DialogueLine {
            speaker: resident.to_string(),
            text: text.to_string(),
        });
    }
}

/// Prevents another scheduler from assigning the same persistent resident.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct NpcCommitment;

/// Safe-to-replicate summary of one active personal request.
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalFavor {
    pub owner: String,
    pub stage: u8,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParcelSeal {
    Sealed,
    Opened,
    Tampered,
}

/// Public parcel facts. The payload and whether it is evidence live only in
/// [`ParcelPayload`].
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialParcel {
    pub recipient: String,
    pub priority: bool,
    pub seal: ParcelSeal,
}

#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParcelPayload {
    pub incident_id: u64,
    pub sender: String,
    pub evidence_for: Option<String>,
    pub reagent: Option<String>,
}

#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub incident_id: u64,
    pub against: String,
    pub description: String,
}

#[derive(Component, Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct EvidenceLockbox;

#[derive(Component)]
pub(crate) struct SocialVisual;

#[derive(Resource)]
struct FavorClock(Timer);

impl Default for FavorClock {
    fn default() -> Self {
        Self(Timer::from_seconds(FAVOR_FIRST_SECONDS, TimerMode::Once))
    }
}

#[derive(Message, Clone, Copy, Debug)]
pub struct ObservedAction {
    pub actor: Entity,
    pub target: Option<Entity>,
    pub position: Vec3,
    pub kind: ObservationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationKind {
    AidCrew,
    UnsafeDose,
    IllicitDelivery,
    OpenParcel,
    DivertParcel,
    RelabelEvidence,
    SurrenderEvidence,
}

pub struct SocialPlugin;

impl Plugin for SocialPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SocialState>()
            .init_resource::<FavorClock>()
            .add_message::<ObservedAction>()
            .add_systems(
                OnEnter(AppState::Playing),
                (reset_favor_clock, spawn_evidence_lockbox).run_if(is_authority),
            )
            .add_systems(
                Update,
                (
                    restore_active_favor,
                    schedule_personal_favor,
                    tick_personal_favor,
                    handle_personal_favor_delivery,
                    handle_parcel_delivery,
                    handle_parcel_opening,
                    handle_evidence_resolution,
                    observe_chemical_exposures,
                    observe_actions,
                    sync_antagonist_evidence,
                    sync_resolution_prompts,
                    remember_resident_dialogue,
                    sync_public_social_views,
                )
                    .chain()
                    .run_if(is_authority)
                    .run_if(in_state(AppState::Playing)),
            )
            .add_systems(
                Update,
                (dress_social_parcels, dress_lockbox).run_if(in_state(AppState::Playing)),
            );
    }
}

fn reset_favor_clock(mut clock: ResMut<FavorClock>) {
    clock.0 = Timer::from_seconds(FAVOR_FIRST_SECONDS, TimerMode::Once);
}

fn remember_resident_dialogue(
    mut social: ResMut<SocialState>,
    spoken: Query<(&CrewMember, &Speech), Changed<Speech>>,
) {
    for (member, speech) in &spoken {
        if RESIDENT_NAMES.contains(&member.name.as_str()) {
            social.remember_dialogue(&member.name, &speech.text);
        }
    }
}

fn public_relationship(relationship: &ResidentRelationship) -> PublicRelationship {
    PublicRelationship {
        tier: relationship.tier,
        last_outcome: relationship.last_outcome,
        personal_history: match relationship.chain_stage {
            0 => PersonalHistory::New,
            1 | 2 => PersonalHistory::InProgress,
            _ => PersonalHistory::Established,
        },
    }
}

fn sync_public_social_views(
    mut commands: Commands,
    social: Res<SocialState>,
    residents: Query<(
        Entity,
        &CrewMember,
        Option<&PublicRelationship>,
        Option<&ConversationHistory>,
    )>,
) {
    for (entity, member, current_relationship, current_history) in &residents {
        let Some(relationship) = social.relationships.get(&member.name) else {
            continue;
        };
        let public_relationship = public_relationship(relationship);
        let history = ConversationHistory {
            lines: social
                .dialogue_history
                .get(&member.name)
                .cloned()
                .unwrap_or_default(),
        };
        if current_relationship != Some(&public_relationship) || current_history != Some(&history) {
            commands
                .entity(entity)
                .insert((public_relationship, history));
        }
    }
}

fn restore_active_favor(
    mut commands: Commands,
    social: Res<SocialState>,
    active: Query<(), With<PersonalFavor>>,
    residents: Query<(Entity, &CrewMember), (With<Ambient>, Without<NpcCommitment>)>,
) {
    if !active.is_empty() {
        return;
    }
    let Some(saved) = social.active_favor.as_ref() else {
        return;
    };
    let Some((entity, member)) = residents
        .iter()
        .find(|(_, member)| member.name == saved.owner)
    else {
        return;
    };
    let antagonist = social
        .resident_antagonist
        .is_some_and(|selected| selected.resident() == member.name);
    commands.entity(entity).insert((
        NpcCommitment,
        PersonalFavor {
            owner: saved.owner.clone(),
            stage: saved.stage,
            summary: favor_summary(&saved.owner, saved.stage, antagonist),
        },
        Interactable::new(format!("{} — personal request", member.name)),
    ));
}

fn spawn_evidence_lockbox(mut commands: Commands, existing: Query<(), With<EvidenceLockbox>>) {
    if !existing.is_empty() {
        return;
    }
    commands.spawn((
        EvidenceLockbox,
        Interactable::new("Security evidence lockbox"),
        Transform::from_xyz(
            crate::lab::COUNTER_SPOT.x + 1.1,
            0.45,
            crate::lab::COUNTER_DROP_Z,
        ),
        Replicated,
        crate::until_we_leave_the_lab(),
    ));
}

fn favor_summary(name: &str, stage: u8, antagonist: bool) -> String {
    match (name, stage, antagonist) {
        (OKONKWO, 0, false) => "Ward stock: bring a safe, honestly labelled treatment.".into(),
        (OKONKWO, 0, true) => "Ward stock: an off-chart dose, kept off the log.".into(),
        (OKONKWO, 1, false) => "Triage consult: prepare a safe stabilising sample.".into(),
        (OKONKWO, 1, true) => {
            "Triage consult: replace an unlogged sample before Medical sees it.".into()
        }
        (OKONKWO, _, false) => {
            "Authorization: supply a lawful controlled treatment or safe substitute.".into()
        }
        (OKONKWO, _, true) => "Authorization: return the real chart and retained sample.".into(),
        (SATO, 0, false) => "Returns: bring Sato a clean, empty beaker.".into(),
        (SATO, 0, true) => {
            "Returns: bring back the beaker before anyone checks its residue.".into()
        }
        (SATO, 1, _) => "Priority parcel: deliver it intact to the named recipient.".into(),
        (SATO, _, false) => {
            "Manifest mismatch: preserve the parcel and decide who receives it.".into()
        }
        (SATO, _, true) => {
            "Manifest mismatch: the sealed contents do not match Sato's paperwork.".into()
        }
        (REYES, 0, false) => {
            "What he saw: bring the sample from the incident for an honest review.".into()
        }
        (REYES, 0, true) => "What he saw: Reyes wants the controlled sample off the log.".into(),
        (REYES, 1, false) => {
            "Calibration: provide a small, truthfully labelled reference sample.".into()
        }
        (REYES, 1, true) => {
            "Calibration: Security's inventory does not match Reyes's request.".into()
        }
        (REYES, _, false) => "Manifest case: surrender the intact evidence.".into(),
        (REYES, _, true) => {
            "Manifest case: a retained sample links Reyes to the missing evidence.".into()
        }
        _ => "A personal request is waiting.".into(),
    }
}

fn schedule_personal_favor(
    mut commands: Commands,
    time: Res<Time>,
    mut clock: ResMut<FavorClock>,
    shift: Res<Shift>,
    mut social: ResMut<SocialState>,
    active: Query<(), With<PersonalFavor>>,
    residents: Query<
        (Entity, &CrewMember, &Body, &Bloodstream),
        (With<Ambient>, Without<NpcCommitment>),
    >,
    mut sounds: MessageWriter<EmitWorldSfx>,
) {
    if !social.initialized
        || !shift.accepting_orders
        || !active.is_empty()
        || social.active_favor.is_some()
    {
        return;
    }
    if social.favor_shift != shift.shift_number {
        social.favor_shift = shift.shift_number;
        social.favor_stages_this_shift = 0;
        social.favored_this_shift.clear();
    }
    if social.favor_stages_this_shift >= 2 || !clock.0.tick(time.delta()).just_finished() {
        return;
    }

    let mut eligible: Vec<_> = residents
        .iter()
        .filter(|(_, member, body, blood)| {
            matches!(member.name.as_str(), OKONKWO | SATO | REYES)
                && !body.0.collapsed
                && !blood.0.incapacitated()
                && !social.favored_this_shift.contains(&member.name)
                && social
                    .relationships
                    .get(&member.name)
                    .is_none_or(|relationship| relationship.chain_stage < 3)
                && !(social
                    .resident_antagonist
                    .is_some_and(|selected| selected.resident() == member.name)
                    && social.antagonist_resolution == AntagonistResolution::Reported)
        })
        .collect();
    eligible.shuffle(&mut rand::rng());
    let Some((entity, member, _, _)) = eligible.first().copied() else {
        clock.0 = Timer::from_seconds(30.0, TimerMode::Once);
        return;
    };
    let stage = social.relationship_mut(&member.name).chain_stage.min(2);
    let antagonist = social
        .resident_antagonist
        .is_some_and(|selected| selected.resident() == member.name);
    let summary = favor_summary(&member.name, stage, antagonist);
    commands.entity(entity).insert((
        NpcCommitment,
        PersonalFavor {
            owner: member.name.clone(),
            stage,
            summary: summary.clone(),
        },
        Interactable::new(format!("{} — personal request", member.name)),
    ));
    social.active_favor = Some(ActiveFavor {
        owner: member.name.clone(),
        stage,
        remaining_seconds: FAVOR_TIMEOUT_SECONDS as u32,
    });
    social.favor_stages_this_shift += 1;
    social.favored_this_shift.insert(member.name.clone());
    say(&mut commands, entity, summary, SpeechTone::Wary);
    if member.name == SATO && stage > 0 {
        let incident_id = social.next_incident();
        let recipient = if stage == 1 { "Chef Dubois" } else { BEX };
        let evidence_for = (antagonist && stage >= 1).then(|| SATO.to_string());
        let position = Vec3::new(
            crate::lab::COUNTER_SPOT.x,
            crate::lab::COUNTER_TOP + 0.1,
            crate::lab::COUNTER_DROP_Z,
        );
        commands.spawn((
            SocialParcel {
                recipient: recipient.to_string(),
                priority: true,
                seal: ParcelSeal::Sealed,
            },
            ParcelPayload {
                incident_id,
                sender: SATO.to_string(),
                evidence_for,
                reagent: antagonist.then(|| "space_drugs".to_string()),
            },
            Transform::from_translation(position),
            Replicated,
            crate::until_we_leave_the_lab(),
        ));
        sounds.write(EmitWorldSfx::new(Sfx::CargoBeep, position));
    }
    clock.0 = Timer::from_seconds(FAVOR_GAP_SECONDS, TimerMode::Once);
}

fn tick_personal_favor(
    mut commands: Commands,
    time: Res<Time>,
    shift: Res<Shift>,
    mut social: ResMut<SocialState>,
    favors: Query<(Entity, &CrewMember, &PersonalFavor)>,
    parcels: Query<(Entity, &ParcelPayload)>,
) {
    if !shift.accepting_orders {
        return;
    }
    let Some(active) = social.active_favor.as_mut() else {
        return;
    };
    active.remaining_seconds = active
        .remaining_seconds
        .saturating_sub(time.delta_secs().ceil() as u32);
    if active.remaining_seconds > 0 {
        return;
    }
    let owner = active.owner.clone();
    let relationship = social.relationship_mut(&owner);
    relationship.last_outcome = FavorOutcome::Refused;
    relationship.tier = relationship.tier.refused();
    relationship.chain_stage = (relationship.chain_stage + 1).min(3);
    social.active_favor = None;
    for (entity, member, _) in &favors {
        if member.name == owner {
            commands
                .entity(entity)
                .remove::<PersonalFavor>()
                .remove::<NpcCommitment>()
                .insert(Interactable::new(format!(
                    "{} — {}",
                    member.name, member.role
                )));
            say(
                &mut commands,
                entity,
                "I found another way. I won't forget the wait.",
                SpeechTone::Wary,
            );
        }
    }
    for (entity, payload) in &parcels {
        if payload.sender == owner {
            commands.entity(entity).despawn();
        }
    }
}

fn finish_favor(
    commands: &mut Commands,
    social: &mut SocialState,
    shift: &mut Shift,
    entity: Entity,
    member: &CrewMember,
    outcome: FavorOutcome,
) {
    let warm = social
        .profile(&member.name)
        .is_some_and(|profile| profile.temperament == Temperament::Warm);
    let sincere = !social
        .resident_antagonist
        .is_some_and(|selected| selected.resident() == member.name);
    let relationship = social.relationship_mut(&member.name);
    let previous_stage = relationship.chain_stage;
    relationship.last_outcome = outcome;
    relationship.chain_stage = (relationship.chain_stage + 1).min(3);
    match outcome {
        FavorOutcome::Helped => {
            relationship.helpful = relationship.helpful.saturating_add(1).min(3);
            relationship.tier = relationship.tier.helped();
            shift.adjust_npc(&member.name, 2);
        }
        FavorOutcome::Compromised => {
            relationship.reckless = relationship.reckless.saturating_add(1).min(3);
            if !warm {
                shift.adjust_npc(&member.name, -1);
            }
        }
        FavorOutcome::Refused => {
            relationship.tier = relationship.tier.refused();
            shift.adjust_npc(&member.name, -4);
        }
        FavorOutcome::Deceived => {
            relationship.dishonest = relationship.dishonest.saturating_add(1).min(3);
            relationship.tier = RelationshipTier::Burned;
            shift.adjust_npc(&member.name, -5);
        }
        FavorOutcome::Unresolved => return,
    }
    let earned_protection = previous_stage < 3
        && relationship.chain_stage == 3
        && relationship.tier == RelationshipTier::Trusted
        && outcome == FavorOutcome::Helped
        && sincere;
    if earned_protection {
        match member.name.as_str() {
            OKONKWO => shift.requisition.quack_wards += 1,
            SATO => shift.requisition.smuggler_wards += 1,
            REYES => shift.requisition.raid_wards += 1,
            _ => {}
        }
    }
    social.active_favor = None;
    commands
        .entity(entity)
        .remove::<PersonalFavor>()
        .remove::<NpcCommitment>()
        .insert(Interactable::new(format!(
            "{} — {}",
            member.name, member.role
        )));
}

#[allow(clippy::too_many_arguments)]
fn handle_personal_favor_delivery(
    mut commands: Commands,
    db: Res<crate::chem_data::ChemDb>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<(Entity, &HeldBy)>,
    containers: Query<&Container>,
    labels: Query<&crate::labels::Label>,
    mut social: ResMut<SocialState>,
    mut shift: ResMut<Shift>,
    favors: Query<(&CrewMember, &PersonalFavor)>,
) {
    for request in requests.read() {
        let Some(player) = crate::machines::chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Some((held_entity, _)) = held.iter().find(|(_, holder)| holder.0 == player) else {
            continue;
        };
        let Ok((member, favor)) = favors.get(request.target) else {
            continue;
        };
        let Ok(container) = containers.get(held_entity) else {
            continue;
        };
        let total = container.solution.total_volume();
        let empty = total.is_zero();
        let honest_label = labels.get(held_entity).ok().is_none_or(|label| {
            db.reagents
                .id_of(&label.0.to_lowercase().replace(' ', "_"))
                .is_some_and(|claimed| {
                    container
                        .solution
                        .contains_at_least(claimed, chem_sim::Units::from_raw(1))
                })
        });
        let useful = container.solution.iter().any(|(id, amount)| {
            amount.is_positive()
                && !db.reagents.get(id).categories.is_empty()
                && !db.reagents.get(id).controlled
        });
        let outcome = match (member.name.as_str(), favor.stage) {
            (SATO, 0)
                if empty
                    && matches!(
                        container.kind,
                        ContainerKind::Beaker | ContainerKind::LargeBeaker
                    ) =>
            {
                FavorOutcome::Helped
            }
            (SATO, 0) => FavorOutcome::Compromised,
            (_, _) if useful && honest_label => FavorOutcome::Helped,
            (_, _) if useful => FavorOutcome::Compromised,
            (_, _) if !honest_label => FavorOutcome::Deceived,
            _ => FavorOutcome::Compromised,
        };
        finish_favor(
            &mut commands,
            &mut social,
            &mut shift,
            request.target,
            member,
            outcome,
        );
        commands.entity(held_entity).despawn();
        let line = match outcome {
            FavorOutcome::Helped => "That's what I needed. Thank you.",
            FavorOutcome::Compromised => "I can use this, but it isn't what we agreed.",
            FavorOutcome::Deceived => "The label and the bottle disagree. We're done pretending.",
            _ => "I'll remember how this went.",
        };
        say(
            &mut commands,
            request.target,
            line,
            if outcome == FavorOutcome::Helped {
                SpeechTone::Friendly
            } else {
                SpeechTone::Wary
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_parcel_delivery(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<(Entity, &HeldBy, &SocialParcel, &ParcelPayload)>,
    targets: Query<(&CrewMember, &Transform)>,
    favors: Query<(Entity, &CrewMember, &PersonalFavor)>,
    mut social: ResMut<SocialState>,
    mut shift: ResMut<Shift>,
    mut sounds: MessageWriter<EmitWorldSfx>,
    mut observed: MessageWriter<ObservedAction>,
) {
    for request in requests.read() {
        let Some(player) = crate::machines::chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Some((parcel_entity, _, parcel, payload)) =
            held.iter().find(|(_, holder, _, _)| holder.0 == player)
        else {
            continue;
        };
        let Ok((recipient, transform)) = targets.get(request.target) else {
            continue;
        };
        if recipient.name != parcel.recipient {
            observed.write(ObservedAction {
                actor: player,
                target: Some(request.target),
                position: transform.translation,
                kind: ObservationKind::DivertParcel,
            });
            continue;
        }
        let Some((owner_entity, owner, _)) = favors
            .iter()
            .find(|(_, _, favor)| favor.owner == payload.sender)
        else {
            continue;
        };
        let outcome = if parcel.seal == ParcelSeal::Sealed {
            FavorOutcome::Helped
        } else {
            FavorOutcome::Compromised
        };
        finish_favor(
            &mut commands,
            &mut social,
            &mut shift,
            owner_entity,
            owner,
            outcome,
        );
        commands.entity(parcel_entity).despawn();
        sounds.write(EmitWorldSfx::new(Sfx::CargoBeep, transform.translation));
        observed.write(ObservedAction {
            actor: player,
            target: Some(request.target),
            position: transform.translation,
            kind: if payload.evidence_for.is_some() {
                ObservationKind::IllicitDelivery
            } else {
                ObservationKind::AidCrew
            },
        });
        say(
            &mut commands,
            request.target,
            if outcome == FavorOutcome::Helped {
                "Seal intact. I'll sign for it."
            } else {
                "This seal was disturbed. It goes in the discrepancy log."
            },
            if outcome == FavorOutcome::Helped {
                SpeechTone::Friendly
            } else {
                SpeechTone::Wary
            },
        );
    }
}

fn handle_parcel_opening(
    mut commands: Commands,
    db: Res<crate::chem_data::ChemDb>,
    mut requests: MessageReader<FromClient<ApplyHeldRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<(Entity, &HeldBy, &SocialParcel, &ParcelPayload)>,
    mut social: ResMut<SocialState>,
    mut sounds: MessageWriter<EmitWorldSfx>,
    mut observed: MessageWriter<ObservedAction>,
    transforms: Query<&Transform>,
) {
    for request in requests.read() {
        let Some(player) = crate::machines::chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Some((parcel_entity, _, parcel, payload)) =
            held.iter().find(|(_, holder, _, _)| holder.0 == player)
        else {
            continue;
        };
        if parcel.seal != ParcelSeal::Sealed {
            continue;
        }
        let position = transforms.get(player).map_or(Vec3::ZERO, |t| t.translation);
        let bottle =
            crate::containers::spawn_container(&mut commands, ContainerKind::Bottle, position);
        if let Some(reagent_key) = &payload.reagent {
            if let Some(reagent) = db.reagents.id_of(reagent_key) {
                let mut container = Container::new(ContainerKind::Bottle);
                let _ = container.solution.add(reagent, chem_sim::Units::whole(10));
                commands.entity(bottle).insert(container);
            }
        }
        if let Some(against) = &payload.evidence_for {
            commands.entity(bottle).insert(EvidenceItem {
                incident_id: payload.incident_id,
                against: against.clone(),
                description: "Opened parcel contents and mismatched manifest".into(),
            });
        }
        social.relationship_mut(&payload.sender).reckless = social
            .relationship_mut(&payload.sender)
            .reckless
            .saturating_add(1)
            .min(3);
        commands.entity(parcel_entity).despawn();
        sounds.write(EmitWorldSfx::new(Sfx::PaperScribble, position));
        observed.write(ObservedAction {
            actor: player,
            target: None,
            position,
            kind: ObservationKind::OpenParcel,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_evidence_resolution(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<(Entity, &HeldBy, &EvidenceItem)>,
    targets: Query<(Entity, Option<&CrewMember>, Has<EvidenceLockbox>)>,
    mut social: ResMut<SocialState>,
    mut shift: ResMut<Shift>,
    mut underworld: ResMut<crate::antagonist::UnderworldStanding>,
    mut suspicion: ResMut<crate::antagonist::SecuritySuspicion>,
    mut sounds: MessageWriter<EmitWorldSfx>,
    transforms: Query<&Transform>,
    mut residents: Query<(
        Entity,
        &CrewMember,
        &mut CrewRoute,
        Has<crate::crew::ReturnsToDuty>,
    )>,
) {
    if social.antagonist_resolution != AntagonistResolution::Exposed {
        requests.clear();
        return;
    }
    let Some(selected) = social.resident_antagonist else {
        requests.clear();
        return;
    };
    for request in requests.read() {
        let Some(player) = crate::machines::chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Some((evidence_entity, _, _evidence)) = held.iter().find(|(_, holder, evidence)| {
            holder.0 == player && evidence.against == selected.resident()
        }) else {
            continue;
        };
        let Ok((target_entity, member, lockbox)) = targets.get(request.target) else {
            continue;
        };
        let resolution = if lockbox {
            AntagonistResolution::Reported
        } else if member.is_some_and(|member| member.name == BEX) {
            AntagonistResolution::Turned
        } else if member.is_some_and(|member| member.name == selected.resident()) {
            AntagonistResolution::Protected
        } else {
            continue;
        };
        social.antagonist_resolution = resolution;
        let resident = selected.resident();
        match resolution {
            AntagonistResolution::Reported => {
                shift.adjust_npc(resident, -8);
            }
            AntagonistResolution::Protected => {
                shift.adjust_npc(resident, 4);
                crate::antagonist::nudge_underworld(&mut underworld, 4);
                crate::antagonist::nudge_suspicion(&mut suspicion, 5);
                match selected {
                    ResidentAntagonist::OkonkwoQuack => shift.requisition.quack_wards += 1,
                    ResidentAntagonist::SatoSmuggler => shift.requisition.smuggler_wards += 1,
                    ResidentAntagonist::ReyesBentGuard => shift.requisition.raid_wards += 1,
                }
            }
            AntagonistResolution::Turned => match selected {
                ResidentAntagonist::OkonkwoQuack => shift.requisition.quack_wards += 1,
                ResidentAntagonist::SatoSmuggler => shift.requisition.smuggler_wards += 1,
                ResidentAntagonist::ReyesBentGuard => shift.requisition.raid_wards += 1,
            },
            _ => {}
        }
        commands.entity(evidence_entity).despawn();
        if matches!(
            resolution,
            AntagonistResolution::Reported | AntagonistResolution::Turned
        ) {
            social.active_favor = None;
            for (entity, member, mut route, returning) in &mut residents {
                if member.name != resident {
                    continue;
                }
                commands
                    .entity(entity)
                    .remove::<PersonalFavor>()
                    .remove::<NpcCommitment>()
                    .remove::<crate::orders::Order>()
                    .remove::<crate::orders::IllicitOrder>()
                    .insert(Interactable::new(format!(
                        "{} — {}",
                        member.name, member.role
                    )));
                if returning {
                    route.leave();
                }
            }
        }
        let position = transforms
            .get(target_entity)
            .map_or(Vec3::ZERO, |transform| transform.translation);
        sounds.write(EmitWorldSfx::new(Sfx::EvidenceScan, position));
        let (line, tone) = match resolution {
            AntagonistResolution::Reported => (
                "Evidence received. The department will take it from here.",
                SpeechTone::Friendly,
            ),
            AntagonistResolution::Protected => (
                "You chose me over the file. That choice has weight.",
                SpeechTone::Wary,
            ),
            AntagonistResolution::Turned => (
                "This stays quiet. In return, they work for us now.",
                SpeechTone::Wary,
            ),
            _ => unreachable!(),
        };
        say(&mut commands, target_entity, line, tone);
        if let Some(member) = member {
            commands
                .entity(target_entity)
                .insert(Interactable::new(format!(
                    "{} — {}",
                    member.name, member.role
                )));
        }
        commands.write_message(ObservedAction {
            actor: player,
            target: Some(target_entity),
            position,
            kind: ObservationKind::SurrenderEvidence,
        });
        info!(
            "resident antagonist {} resolved as {:?}",
            resident, resolution
        );
        break;
    }
}

fn observe_actions(
    mut actions: MessageReader<ObservedAction>,
    mut social: ResMut<SocialState>,
    crew: Query<(
        Entity,
        &CrewMember,
        &Transform,
        &Body,
        &Bloodstream,
        &CrewRoute,
    )>,
    solids: Query<(&Transform, &crate::lab::Solid)>,
) {
    for action in actions.read() {
        let position = action.position;
        for (entity, member, transform, body, blood, route) in &crew {
            let directly_involved = action.target == Some(entity);
            if body.0.collapsed
                || blood.0.incapacitated()
                || route.phase == CrewPhase::Leaving
                || entity == action.actor
                || (!directly_involved
                    && transform.translation.distance(position) > OBSERVATION_RANGE)
            {
                continue;
            }
            let eye = transform.translation + Vec3::Y * 1.3;
            let blocked = solids.iter().any(|(solid_transform, solid)| {
                authority_segment_blocked(
                    eye,
                    position,
                    solid_transform.translation,
                    solid.half_extents,
                )
            });
            if blocked {
                continue;
            }
            let relationship = social.relationship_mut(&member.name);
            match action.kind {
                ObservationKind::AidCrew => {
                    relationship.helpful = relationship.helpful.saturating_add(1).min(3)
                }
                ObservationKind::UnsafeDose
                | ObservationKind::OpenParcel
                | ObservationKind::DivertParcel => {
                    relationship.reckless = relationship.reckless.saturating_add(1).min(3)
                }
                ObservationKind::IllicitDelivery | ObservationKind::RelabelEvidence => {
                    relationship.dishonest = relationship.dishonest.saturating_add(1).min(3)
                }
                ObservationKind::SurrenderEvidence => {
                    relationship.helpful = relationship.helpful.saturating_add(1).min(3)
                }
            }
        }
    }
}

fn observe_chemical_exposures(
    mut exposures: MessageReader<crate::chem_world::ChemicalExposure>,
    transforms: Query<&Transform>,
    mut observed: MessageWriter<ObservedAction>,
) {
    for exposure in exposures.read() {
        let Some(actor) = exposure.actor else {
            continue;
        };
        if exposure.source != crate::chem_world::ExposureSource::Direct {
            continue;
        }
        let kind =
            if exposure.harmful || exposure.overdose || (!exposure.authorized && !exposure.helpful)
            {
                ObservationKind::UnsafeDose
            } else if exposure.helpful {
                ObservationKind::AidCrew
            } else {
                continue;
            };
        observed.write(ObservedAction {
            actor,
            target: Some(exposure.target),
            position: transforms
                .get(exposure.target)
                .map_or(Vec3::ZERO, |transform| transform.translation),
            kind,
        });
    }
}

fn sync_antagonist_evidence(
    mut commands: Commands,
    mut social: ResMut<SocialState>,
    quack: Res<crate::quack::QuackProgress>,
    smuggler: Res<crate::smuggler::SmugglerProgress>,
    bent: Res<crate::bent_guard::BentGuardProgress>,
) {
    let Some(selected) = social.resident_antagonist else {
        return;
    };
    if social.antagonist_resolution != AntagonistResolution::Hidden || social.evidence_spawned {
        return;
    }
    let progress = match selected {
        ResidentAntagonist::OkonkwoQuack => quack.0,
        ResidentAntagonist::SatoSmuggler => smuggler.0,
        ResidentAntagonist::ReyesBentGuard => bent.0,
    };
    social.evidence_progress = progress.min(3) as u8;
    if social.evidence_progress < 3 {
        return;
    }
    let incident = social.next_incident();
    let evidence = crate::containers::spawn_container(
        &mut commands,
        ContainerKind::Bottle,
        Vec3::new(
            crate::lab::COUNTER_SPOT.x,
            crate::lab::COUNTER_TOP + 0.08,
            crate::lab::COUNTER_DROP_Z,
        ),
    );
    commands.entity(evidence).insert((
        EvidenceItem {
            incident_id: incident,
            against: selected.resident().to_string(),
            description: match selected {
                ResidentAntagonist::OkonkwoQuack => "Real chart and unlogged retained sample",
                ResidentAntagonist::SatoSmuggler => "False manifest and opened cargo sample",
                ResidentAntagonist::ReyesBentGuard => {
                    "Unlogged requisition and retained evidence sample"
                }
            }
            .into(),
        },
        Interactable::new("Linked evidence — report, protect, or turn"),
    ));
    social.evidence_spawned = true;
    social.antagonist_resolution = AntagonistResolution::Exposed;
}

fn sync_resolution_prompts(
    social: Res<SocialState>,
    mut residents: Query<(&CrewMember, &mut Interactable), (With<Ambient>, Without<NpcCommitment>)>,
) {
    if social.antagonist_resolution != AntagonistResolution::Exposed {
        return;
    }
    let Some(selected) = social.resident_antagonist else {
        return;
    };
    for (member, mut prompt) in &mut residents {
        let label = if member.name == selected.resident() {
            Some(format!("{} — return the linked evidence", member.name))
        } else if member.name == BEX {
            Some("Warden Bex — surrender evidence and turn the suspect".into())
        } else {
            None
        };
        if let Some(label) = label {
            prompt.label = label;
        }
    }
}

pub(crate) fn dress_social_parcels(
    mut commands: Commands,
    assets: Res<AssetServer>,
    parcels: Query<(Entity, &SocialParcel), (Added<SocialParcel>, Without<SocialVisual>)>,
) {
    for (entity, parcel) in &parcels {
        commands.entity(entity).insert((
            WorldAssetRoot(assets.load("3dassets/station_starter_kit/glb/decor_cargo_parcel.glb")),
            Interactable::new(format!(
                "{} parcel for {} ({})",
                if parcel.priority {
                    "Priority"
                } else {
                    "Sealed"
                },
                parcel.recipient,
                match parcel.seal {
                    ParcelSeal::Sealed => "seal intact",
                    ParcelSeal::Opened => "opened",
                    ParcelSeal::Tampered => "tampered",
                }
            )),
            SocialVisual,
        ));
    }
}

fn dress_lockbox(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    lockboxes: Query<Entity, (Added<EvidenceLockbox>, Without<SocialVisual>)>,
) {
    for entity in &lockboxes {
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cuboid::new(0.42, 0.34, 0.32))),
            MeshMaterial3d(materials.add(Color::srgb(0.18, 0.24, 0.32))),
            SocialVisual,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn fresh_careers_assign_every_profile_and_exactly_one_antagonist() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(12);
        let social = SocialState::fresh_with(&mut rng);
        assert!(social.initialized);
        assert_eq!(social.profiles.len(), RESIDENT_NAMES.len());
        assert_eq!(social.relationships.len(), RESIDENT_NAMES.len());
        assert!(social.resident_antagonist.is_some());
    }

    #[test]
    fn all_three_antagonists_are_reachable_without_coupling_personality() {
        let mut seen = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        for seed in 0..512 {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let social = SocialState::fresh_with(&mut rng);
            let antagonist = social.resident_antagonist.unwrap();
            seen.insert(format!("{antagonist:?}"));
            pairs.insert((
                format!("{antagonist:?}"),
                format!(
                    "{:?}",
                    social.profile(antagonist.resident()).unwrap().temperament
                ),
            ));
        }
        assert_eq!(seen.len(), 3);
        for antagonist in seen {
            assert!(pairs.iter().filter(|(who, _)| who == &antagonist).count() > 1);
        }
    }

    #[test]
    fn legacy_migration_never_rewrites_an_existing_career_identity() {
        let social = SocialState::migrate_legacy();
        assert!(social.initialized);
        assert_eq!(social.resident_antagonist, None);
        assert_eq!(social.profiles.len(), RESIDENT_NAMES.len());
    }

    #[test]
    fn resident_departments_match_the_authored_station_roster() {
        for department in Department::ALL {
            for &resident in department.members() {
                assert_eq!(resident_department(resident), Some(department));
            }
        }
        assert_eq!(
            Department::ALL
                .into_iter()
                .flat_map(Department::members)
                .count(),
            RESIDENT_NAMES.len()
        );
    }

    #[test]
    fn remembered_dialogue_is_complete_and_belongs_to_this_social_save() {
        let mut social = SocialState::fresh();
        for index in 0..37 {
            social.remember_dialogue(OKONKWO, &format!("line {index}"));
        }
        let lines = &social.dialogue_history[OKONKWO];
        assert_eq!(lines.len(), 37);
        assert_eq!(lines.first().unwrap().text, "line 0");
        assert_eq!(lines.last().unwrap().text, "line 36");
        assert!(
            SocialState::fresh().dialogue_history.is_empty(),
            "a different save must not inherit remembered dialogue"
        );

        let mut shared = SocialState::fresh();
        shared.remember_dialogue(SATO, "Same question, first chemist.");
        shared.remember_dialogue(SATO, "Same question, first chemist.");
        assert_eq!(
            shared.dialogue_history[SATO].len(),
            2,
            "repeated conversations by different co-op players are still two shared events"
        );
    }

    #[test]
    fn every_resident_utterance_joins_the_one_shared_coop_transcript() {
        let mut app = App::new();
        app.insert_resource(SocialState::fresh())
            .add_systems(Update, remember_resident_dialogue);
        let resident = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: SATO.into(),
                    role: "Cargo".into(),
                },
                Speech {
                    text: "First chemist's conversation.".into(),
                    tone: SpeechTone::Neutral,
                },
            ))
            .id();
        app.update();
        app.world_mut().entity_mut(resident).insert(Speech {
            text: "Second chemist's conversation.".into(),
            tone: SpeechTone::Neutral,
        });
        app.update();

        let lines = &app.world().resource::<SocialState>().dialogue_history[SATO];
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "First chemist's conversation.");
        assert_eq!(lines[1].text, "Second chemist's conversation.");
    }

    #[test]
    fn report_and_turn_stop_only_the_selected_minor() {
        let mut social = SocialState::fresh();
        social.resident_antagonist = Some(ResidentAntagonist::SatoSmuggler);
        social.antagonist_resolution = AntagonistResolution::Reported;
        assert!(!social.threat_runs(ResidentAntagonist::SatoSmuggler));
        assert!(social.threat_runs(ResidentAntagonist::OkonkwoQuack));
        assert!(social.threat_runs(ResidentAntagonist::ReyesBentGuard));
    }

    #[test]
    fn exactly_one_outsider_identity_is_replaced_for_each_assignment() {
        let outsiders = ["Dr. Halloway", "Deckhand Prewitt", "Cadet Ferreira"];
        for selected in ResidentAntagonist::ALL {
            let mut social = SocialState::fresh();
            social.resident_antagonist = Some(selected);
            let resolved = ResidentAntagonist::ALL.map(|threat| {
                social
                    .threat_identity(
                        threat,
                        outsiders[ResidentAntagonist::ALL
                            .iter()
                            .position(|candidate| *candidate == threat)
                            .unwrap()],
                    )
                    .to_string()
            });
            assert_eq!(
                resolved
                    .iter()
                    .zip(outsiders)
                    .filter(|(actual, outsider)| actual.as_str() != *outsider)
                    .count(),
                1
            );
            assert!(resolved.contains(&selected.resident().to_string()));
        }
    }

    #[test]
    fn only_nearby_unblocked_residents_witness_an_action() {
        let mut app = App::new();
        app.insert_resource(SocialState::fresh())
            .add_message::<ObservedAction>()
            .add_systems(Update, observe_actions);
        let actor = app.world_mut().spawn_empty().id();
        let spawn_witness = |world: &mut World, name: &str, position: Vec3| {
            world
                .spawn((
                    CrewMember {
                        name: name.into(),
                        role: "Resident".into(),
                    },
                    Transform::from_translation(position),
                    Body::default(),
                    Bloodstream::default(),
                    CrewRoute::to(position),
                ))
                .id()
        };
        spawn_witness(app.world_mut(), OKONKWO, Vec3::new(2.0, 0.0, 3.0));
        spawn_witness(app.world_mut(), SATO, Vec3::new(-2.0, 0.0, 0.0));
        spawn_witness(app.world_mut(), REYES, Vec3::new(20.0, 0.0, 0.0));
        app.world_mut().spawn((
            Transform::from_xyz(0.0, 0.8, 0.0),
            crate::lab::Solid {
                half_extents: Vec3::new(0.45, 1.0, 1.0),
            },
        ));
        app.world_mut()
            .resource_mut::<Messages<ObservedAction>>()
            .write(ObservedAction {
                actor,
                target: None,
                position: Vec3::new(2.0, 0.0, 0.0),
                kind: ObservationKind::AidCrew,
            });

        app.update();

        let social = app.world().resource::<SocialState>();
        assert_eq!(social.relationships[OKONKWO].helpful, 1);
        assert_eq!(social.relationships[SATO].helpful, 0);
        assert_eq!(social.relationships[REYES].helpful, 0);
    }
}
