//! A ritual, escalating one delivery at a time.
//!
//! Same shape as [`crate::obsessed`] — one recurring identity, an authored
//! ordered chain rather than a random pool, advanced by matching the
//! resolution's name — but the payoff is different: the final stage asks
//! for a reagent that only exists as the product of a real, hazardous
//! reaction (`flash_powder`, gated behind mixing aluminium, potassium and
//! sulfur — see `chem.reactions.ron`). Making it fires a genuine
//! `ReactionEffect::Smoke` the moment the player combines the precursors,
//! through the entirely unmodified `Container::mutate` →
//! `machines::ReactionsFired` → `hazards::spawn_hazards` pipeline every
//! other reaction already uses. The danger is in *preparing* the ritual's
//! ask, not in the hand-off — nothing here reaches into hazards at all.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use chem_sim::Units;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chem_data::ChemDb;
use crate::containers::{Container, ContainerKind, HeldBy};
use crate::crew::{spawn_crew_member, CrewDef};
use crate::interaction::{InteractRequested, Interactable};
use crate::machines::chemist_entity;
use crate::net::is_authority;
use crate::orders::{
    deliverable_amount, reference_category, Order, OrderResolved, Shift, StationData,
};
use crate::player::Chemist;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::AppState;

const INITIAL_GAP_SECONDS: (f32, f32) = (300.0, 500.0);
const INCIDENT_SPOTS: [Vec3; 6] = [
    Vec3::new(-2.8, 1.0, 0.2),
    Vec3::new(-2.2, 1.0, 2.4),
    Vec3::new(-3.5, 1.0, 2.0),
    Vec3::new(-1.7, 1.0, 0.8),
    Vec3::new(-3.0, 1.0, 3.0),
    Vec3::new(-1.5, 1.0, 2.9),
];
const REWARD_SPOT: Vec3 = Vec3::new(2.4, 1.0, 2.5);

pub struct CultPlugin;

impl Plugin for CultPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<CultScript>::new(&["cult.ron"]))
            .init_resource::<CultProgress>()
            .init_resource::<CultIncidentsRestored>()
            .add_systems(Startup, start_loading)
            .add_systems(
                OnEnter(AppState::Playing),
                (arm_spawner, reset_incident_restore),
            )
            .add_systems(
                Update,
                (
                    promote_script,
                    restore_incidents,
                    generate_cult_visit,
                    handle_cult_resolution,
                    handle_incident_delivery,
                )
                    .chain()
                    .run_if(is_authority)
                    // Only in a save that actually drew the Cult. This thread
                    // was a standalone Cargo curiosity before the campaign
                    // arc existed; it is now the Cult's on-station presence,
                    // and a save fighting the Syndicate should never see an
                    // acolyte at the counter. Department minors deliberately
                    // carry no equivalent gate — they run in every save.
                    .run_if(crate::arc::is_active(crate::arc::AntagId::Cult))
                    .run_if(in_state(AppState::Playing)),
            );
        app.add_systems(Update, dress_incidents.run_if(in_state(AppState::Playing)));
    }
}

/// Which authored stage fires next. Persisted, same as
/// `obsessed::ObsessedProgress`.
#[derive(Resource, Default, Clone, Copy)]
pub struct CultProgress(pub usize);

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
}

#[derive(Clone, Debug, Deserialize)]
pub struct CultIncidentDef {
    pub name: String,
    pub clue: String,
    /// A representative reagent. Its category is the answer; the player is
    /// never told this name in the incident UI or radio line.
    pub treatment: String,
    pub amount: u32,
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

#[derive(Resource)]
struct PendingCultScript(Handle<CultScript>);

#[derive(Resource, Deref)]
struct Script(CultScript);

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

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingCultScript(assets.load("data/station.cult.ron")));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingCultScript>>,
    mut scripts: ResMut<Assets<CultScript>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingCultScript>();
}

fn restore_incidents(
    mut commands: Commands,
    script: Option<Res<Script>>,
    campaign: Option<Res<crate::arc::Campaign>>,
    anchors: Query<&RitualAnchor>,
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
        for (offset, incident) in stage.incidents.iter().enumerate() {
            let index = stage_index * 2 + offset;
            if campaign.cult_incidents.get(index) != Some(&false)
                || anchors.iter().any(|anchor| anchor.index == index)
            {
                continue;
            }
            commands.spawn((
                RitualAnchor {
                    index,
                    name: incident.name.clone(),
                    clue: incident.clue.clone(),
                    treatment: incident.treatment.clone(),
                    amount: Units::whole(incident.amount as i32),
                },
                Transform::from_translation(INCIDENT_SPOTS[index % INCIDENT_SPOTS.len()]),
                Visibility::default(),
                Interactable::new(format!("{} — examine and treat", incident.name)),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
            radio.push(RadioEntry {
                channel: "LAB".to_string(),
                text: format!(
                    "The {} is still waiting for someone to intervene.",
                    incident.name
                ),
                good: false,
            });
        }
    }
    restored.0 = true;
}

/// Arms this thread's visit clock for a fresh session.
///
/// `OnEnter(AppState::Playing)` rather than inside `promote_script`, which
/// runs exactly once per process: quitting to the menu and opening another
/// save would otherwise leave a spent `TimerMode::Once` behind, and a spent
/// `Once` timer never reports `just_finished` again — the whole thread would
/// be silently dead for the rest of the session with nothing to show for it.
fn arm_spawner(mut commands: Commands) {
    let gap = rand::rng().random_range(INITIAL_GAP_SECONDS.0..=INITIAL_GAP_SECONDS.1);
    commands.insert_resource(CultSpawner {
        timer: Timer::from_seconds(gap, TimerMode::Once),
    });
}

#[allow(clippy::too_many_arguments)]
fn generate_cult_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<CultSpawner>>,
    progress: Res<CultProgress>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
    chemists: Query<(), With<Chemist>>,
) {
    let (Some(station), Some(script), Some(spawner)) = (station, script, spawner.as_mut()) else {
        return;
    };
    if !shift.accepting_orders {
        return;
    }
    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let mut rng = rand::rng();
    let rules = current_rules(&station.config, &shift, chemists.iter().count());
    let legit_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    let multiplier = rng.random_range(script.gap_multiplier.0..=script.gap_multiplier.1);
    spawner.timer = Timer::from_seconds(legit_gap * multiplier, TimerMode::Once);

    let Some(stage) = script
        .stages
        .get(progress.0.min(script.stages.len().saturating_sub(1)))
    else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&stage.reagent) else {
        warn!("cult stage names unknown reagent '{}'", stage.reagent);
        return;
    };

    let identity = CrewDef {
        name: script.name.clone(),
        role: script.role.clone(),
        color: script.color,
    };
    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);
    let crew = spawn_crew_member(&mut commands, &identity, 0.0);

    let reagent_name = db.reagents.get(reagent).name.clone();
    let amount = deliverable_amount(&db, reagent, chem_sim::Units::whole(stage.amount as i32));
    commands.entity(crew).insert((
        Order {
            reagent,
            specific: true,
            amount,
            plea: stage.pretext.clone(),
            patience,
            waited: 0.0,
        },
        Interactable::new(format!(
            "{} — hand over {} {}",
            script.name, amount, reagent_name
        )),
    ));

    radio.push(RadioEntry {
        channel: channel_for(&script.role),
        text: format!("{}: {}", script.name, stage.pretext),
        good: false,
    });

    info!("cult: {} stage {}", script.name, progress.0);
}

/// Advances the ritual only on a successful delivery — matched by name for
/// the same reason `obsessed::handle_obsessed_resolution` is. Declining or
/// botching a stage falls through to the ordinary reputation penalty and
/// the ritual simply never moves.
fn handle_cult_resolution(
    mut commands: Commands,
    db: Option<Res<ChemDb>>,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<CultProgress>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    let mut campaign = campaign;
    for report in resolved.read() {
        if report.name != script.name || !report.outcome.is_good() {
            continue;
        }
        let stage_index = progress.0.min(script.stages.len().saturating_sub(1));
        if let Some(stage) = script.stages.get(stage_index) {
            radio.push(RadioEntry {
                channel: channel_for(&script.role),
                text: stage.ritual_line.clone(),
                good: false,
            });
            if let Some(db) = db.as_deref() {
                spawn_stage_consequences(
                    &mut commands,
                    db,
                    stage,
                    stage_index,
                    &mut campaign,
                    &mut radio,
                );
            }
        }
        progress.0 = (progress.0 + 1).min(script.stages.len().saturating_sub(1));

        // A fulfilled stage is the ritual moving forward, so it moves the
        // campaign the same distance a fulfilled illicit order does. Without
        // this the Cult could run its entire authored chain to the finale
        // while its own plot meter sat wherever ambient drift had left it.
        if let (Some(arc_script), Some(campaign)) = (arc_script.as_deref(), campaign.as_mut()) {
            crate::arc::nudge_plot(campaign, arc_script.plot_per_aid);
        }
    }
}

fn spawn_stage_consequences(
    commands: &mut Commands,
    db: &ChemDb,
    stage: &CultStageDef,
    stage_index: usize,
    campaign: &mut Option<ResMut<crate::arc::Campaign>>,
    radio: &mut RadioLog,
) {
    let base = stage_index * 2;
    if let Some(campaign) = campaign.as_mut() {
        if campaign.cult_incidents.len() < base + stage.incidents.len() {
            campaign
                .cult_incidents
                .resize(base + stage.incidents.len(), false);
        }
    }
    for (offset, incident) in stage.incidents.iter().enumerate() {
        let index = base + offset;
        if campaign
            .as_ref()
            .is_some_and(|c| c.cult_incidents.get(index) == Some(&true))
        {
            continue;
        }
        commands.spawn((
            RitualAnchor {
                index,
                name: incident.name.clone(),
                clue: incident.clue.clone(),
                treatment: incident.treatment.clone(),
                amount: Units::whole(incident.amount as i32),
            },
            Transform::from_translation(INCIDENT_SPOTS[index % INCIDENT_SPOTS.len()]),
            Visibility::default(),
            Interactable::new(format!("{} — examine and treat", incident.name)),
            Replicated,
            crate::until_we_leave_the_lab(),
        ));
        radio.push(RadioEntry {
            channel: "LAB".to_string(),
            text: incident.clue.clone(),
            good: false,
        });
    }
    // The payment is physical stock at the counter, not an invisible bonus.
    if let Some(reagent) = db.reagents.id_of(&stage.reward_reagent) {
        let mut vial = Container::new(ContainerKind::Bottle);
        let _ = vial
            .solution
            .add(reagent, Units::whole(stage.reward_amount as i32));
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
            continue;
        };
        let Some(treatment) = db.reagents.id_of(&anchor.treatment) else {
            continue;
        };
        let landed = incident_units(&db, &container.solution, treatment);
        if landed < anchor.amount {
            radio.push(RadioEntry {
                channel: "LAB".to_string(),
                text: format!("The {} rejects that mixture.", anchor.name),
                good: false,
            });
            continue;
        }
        if campaign.cult_incidents.len() <= anchor.index {
            campaign.cult_incidents.resize(anchor.index + 1, false);
        }
        if campaign.cult_incidents[anchor.index] {
            continue;
        }
        campaign.cult_incidents[anchor.index] = true;
        commands.entity(container_entity).despawn();
        commands.entity(request.target).despawn();
        radio.push(RadioEntry {
            channel: "LAB".to_string(),
            text: format!(
                "The {} gutters out. The ritual has lost some of its hold.",
                anchor.name
            ),
            good: true,
        });
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

/// A deliberately simple, hostile-looking presentation; the clue and the
/// chemistry are the puzzle, not hunting for a tiny prop in the room.
fn dress_incidents(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    new: Query<Entity, Added<RitualAnchor>>,
) {
    for entity in &new {
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cylinder::new(0.42, 0.08))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.28, 0.03, 0.07),
                emissive: LinearRgba::new(0.5, 0.02, 0.08, 1.0),
                ..default()
            })),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::{Department, Outcome};

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
                assert!(data.reagents.id_of(&incident.treatment).is_some());
                assert!(incident.amount > 0);
            }
        }
    }

    #[test]
    fn the_final_stage_reagent_actually_reacts_dangerously() {
        // Proves the "real chemistry" claim in the module doc rather than
        // trusting the RON: mixing the final stage's own reagent from base
        // reagents through the real resolver must fire a hazardous
        // `ReactionEffect`.
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
        assert!(
            !reaction.effects.is_empty(),
            "'{}' must be the product of a reaction with a real hazard effect, or the finale is all bark",
            last.reagent
        );
    }

    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(Script(script()))
            .init_resource::<CultProgress>()
            .init_resource::<RadioLog>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_cult_resolution);
        app
    }

    fn resolve(app: &mut App, name: &str, outcome: Outcome) {
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Cargo".to_string(),
            reagent: None,
            category: None,
            outcome,
            kind: crate::orders::OrderKind::Normal,
        });
        app.update();
    }

    #[test]
    fn a_successful_stage_advances_the_ritual() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Success);

        assert_eq!(app.world().resource::<CultProgress>().0, 1);
    }

    #[test]
    fn declining_a_stage_never_advances_the_ritual() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Expired);
        resolve(&mut app, &name, Outcome::Wrong);

        assert_eq!(
            app.world().resource::<CultProgress>().0,
            0,
            "only a real, good delivery should move the ritual forward"
        );
    }

    #[test]
    fn an_unrelated_resolution_never_advances_the_ritual() {
        let mut app = resolution_app();

        resolve(&mut app, "Someone Else", Outcome::Success);

        assert_eq!(app.world().resource::<CultProgress>().0, 0);
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

        let mut app = resolution_app();
        app.insert_resource(crate::arc::Script(arc_script))
            .insert_resource(crate::arc::Campaign::new(
                crate::arc::AntagId::Cult,
                crate::arc::Mode::Chemist,
                steps,
            ));
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
            per_aid,
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
        assert_eq!(app.world().resource::<CultProgress>().0, 0);
    }

    #[test]
    fn an_accepted_stage_creates_two_visible_incidents_and_a_reward() {
        let mut app = campaign_app();
        app.insert_resource(ChemDb(data()));
        let name = app.world().resource::<Script>().0.name.clone();
        resolve(&mut app, &name, Outcome::Success);
        app.world_mut().flush();
        assert_eq!(
            app.world_mut()
                .query::<&RitualAnchor>()
                .iter(app.world())
                .count(),
            2
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
            vec![false, false]
        );
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
