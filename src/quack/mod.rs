//! Medical's minor antagonist: a doctor running treatments nobody approved.
//!
//! One of the five department shenanigan threads, running in **every** save
//! alongside whichever main antagonist was drawn.
//!
//! Same [`crate::obsessed`] template as its siblings [`crate::smuggler`] and
//! [`crate::saboteur`], with the third distinct hook. The smuggler takes your
//! glassware; the saboteur spoils it. This one does not touch the lab at all —
//! ignore them and they go ahead **without** you, and it is a patient who pays
//! for it: the crew member currently waiting at your counter gets dosed with
//! whatever the doctor improvised.
//!
//! That reuses the M12 general-exposure path exactly as it stands
//! (`Bloodstream::receive`, the same call `orders::complete_delivery` makes
//! when a delivery lands), so the victim visibly reels through `crate::fx`
//! with no presentation code here at all — and, because a dosed crew member is
//! a dosed crew member, a habit-forming improvisation feeds
//! `crate::addiction` for free too.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use chem_sim::{Route, Solution, Units};
use rand::prelude::*;
use serde::Deserialize;

use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::crew::{spawn_crew_member, CrewDef, CrewMember, CrewPhase, CrewRoute};
use crate::interaction::Interactable;
use crate::net::is_authority;
use crate::orders::{Department, Order, OrderResolved, Outcome, Shift, StationData};
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::AppState;

const INITIAL_GAP_SECONDS: (f32, f32) = (240.0, 420.0);

pub struct QuackPlugin;

impl Plugin for QuackPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<QuackScript>::new(&["quack.ron"]))
            .init_resource::<QuackProgress>()
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (promote_script, generate_quack_visit, handle_quack_resolution)
                    .chain()
                    .run_if(is_authority)
                    // No `arc::is_active` gate, unlike a main antagonist.
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Which authored visit fires next. Persisted, same as
/// `obsessed::ObsessedProgress`.
#[derive(Resource, Default, Clone, Copy)]
pub struct QuackProgress(pub usize);

/// `assets/data/station.quack.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct QuackScript {
    /// Kept off `station.crew.ron` so an ordinary order can never
    /// double-book them.
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
    pub gap_multiplier: (f32, f32),
    pub visits: Vec<QuackVisitDef>,
    /// What they improvise with when nobody fills the order. A real reagent,
    /// dosed for real.
    pub improvised: String,
    pub improvised_units: u32,
    /// Standing cost to Medical when a patient is treated this way.
    pub malpractice_penalty: i32,
    /// Aired when it happens. `{name}` is the patient.
    pub malpractice_lines: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QuackVisitDef {
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
}

#[derive(Resource)]
struct PendingQuackScript(Handle<QuackScript>);

#[derive(Resource, Deref)]
struct Script(QuackScript);

#[derive(Resource)]
struct QuackSpawner {
    timer: Timer,
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingQuackScript(assets.load("data/station.quack.ron")));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingQuackScript>>,
    mut scripts: ResMut<Assets<QuackScript>>,
    spawner: Option<Res<QuackSpawner>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    if spawner.is_none() {
        let gap = rand::rng().random_range(INITIAL_GAP_SECONDS.0..=INITIAL_GAP_SECONDS.1);
        commands.insert_resource(QuackSpawner {
            timer: Timer::from_seconds(gap, TimerMode::Once),
        });
    }
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingQuackScript>();
}

#[allow(clippy::too_many_arguments)]
fn generate_quack_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<QuackSpawner>>,
    progress: Res<QuackProgress>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
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
    let rules = current_rules(&station.config, &shift);
    let legit_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    let multiplier = rng.random_range(script.gap_multiplier.0..=script.gap_multiplier.1);
    spawner.timer = Timer::from_seconds(legit_gap * multiplier, TimerMode::Once);

    let Some(visit) = script
        .visits
        .get(progress.0.min(script.visits.len().saturating_sub(1)))
    else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&visit.reagent) else {
        warn!("quack visit names unknown reagent '{}'", visit.reagent);
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
    commands.entity(crew).insert((
        Order {
            reagent,
            specific: true,
            amount: Units::whole(visit.amount as i32),
            plea: visit.plea.clone(),
            patience,
            waited: 0.0,
        },
        Interactable::new(format!(
            "{} — hand over {}u {}",
            script.name, visit.amount, reagent_name
        )),
    ));

    radio.push(RadioEntry {
        channel: channel_for(&script.role),
        text: format!("{}: {}", script.name, visit.plea),
        good: false,
    });
}

/// Advances the chain — and, on an expired visit, treats someone anyway.
///
/// The dose goes into a crew member who is *waiting at your counter*, not an
/// abstraction: they are standing right there, they visibly react, and their
/// department notices. Only [`Outcome::Expired`] triggers it — handing over
/// the wrong thing is a mistake, leaving them to improvise is a decision.
#[allow(clippy::too_many_arguments)]
fn handle_quack_resolution(
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<QuackProgress>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    mut patients: Query<(&CrewMember, &CrewRoute, &mut Body, &mut Bloodstream)>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    let mut campaign = campaign;

    for report in resolved.read() {
        if report.name != script.name {
            continue;
        }
        progress.0 = (progress.0 + 1).min(script.visits.len().saturating_sub(1));
        if report.outcome != Outcome::Expired {
            continue;
        }

        let Some(improvised) = db.reagents.id_of(&script.improvised) else {
            warn!("quack improvises with unknown reagent '{}'", script.improvised);
            continue;
        };

        // Anyone still waiting, except the doctor themselves — the joke does
        // not work if they dose the person who was asking.
        let mut rng = rand::rng();
        let victim = patients
            .iter_mut()
            .filter(|(member, route, _, _)| {
                member.name != script.name && route.phase == CrewPhase::Waiting
            })
            .choose(&mut rng);
        let Some((member, _, mut body, mut blood)) = victim else {
            continue;
        };

        // The exact call `orders::complete_delivery` makes when a beaker lands
        // on someone. Nothing about being dosed is special-cased here, which
        // is why the victim reels through `fx` and can build a habit through
        // `addiction` with no code in this module for either.
        let mut dose = Solution::unbounded();
        let _ = dose.add(improvised, Units::whole(script.improvised_units as i32));
        blood
            .0
            .receive(&mut dose, Route::Injected, &mut body.0, &db.0);

        let name = member.name.clone();
        let role = member.role.clone();
        if let Some(department) = Department::from_role(&role) {
            shift.adjust(department, script.malpractice_penalty);
        }
        let line = script
            .malpractice_lines
            .choose(&mut rng)
            .cloned()
            .unwrap_or_else(|| "{name} was treated by someone who should not have.".to_string());
        radio.push(RadioEntry {
            channel: channel_for(&role),
            text: line.replace("{name}", &name),
            good: false,
        });
        info!("quack: {} treated {name} without asking", script.name);

        if let (Some(arc_script), Some(campaign)) = (arc_script.as_deref(), campaign.as_mut()) {
            crate::arc::note_ignored_shenanigan(arc_script, campaign);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::OrderKind;

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn script() -> QuackScript {
        ron::from_str(include_str!("../../assets/data/station.quack.ron"))
            .expect("station.quack.ron should parse")
    }

    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(Script(script()))
            .init_resource::<QuackProgress>()
            .init_resource::<RadioLog>()
            .init_resource::<Shift>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_quack_resolution);
        app
    }

    /// Someone standing at the counter with an empty bloodstream.
    fn patient(app: &mut App, name: &str, role: &str) -> Entity {
        let mut route = CrewRoute::arrival(0.0);
        route.phase = CrewPhase::Waiting;
        app.world_mut()
            .spawn((
                CrewMember {
                    name: name.to_string(),
                    role: role.to_string(),
                },
                route,
                Body::default(),
                Bloodstream::default(),
            ))
            .id()
    }

    fn resolve(app: &mut App, name: &str, outcome: Outcome) {
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Medical".to_string(),
            reagent: None,
            category: None,
            outcome,
            kind: OrderKind::Normal,
        });
        app.update();
    }

    #[test]
    fn ignoring_them_means_a_patient_gets_treated_anyway() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let penalty = app.world().resource::<Script>().0.malpractice_penalty;
        let victim = patient(&mut app, "Miner Sato", "Cargo");

        resolve(&mut app, &name, Outcome::Expired);

        assert!(
            !app.world().get::<Bloodstream>(victim).unwrap().0.is_empty(),
            "the whole point is that somebody real gets dosed"
        );
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Cargo),
            penalty,
            "the patient's own department is the one that notices"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn they_never_dose_themselves() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let doctor = patient(&mut app, &name, "Medical");

        resolve(&mut app, &name, Outcome::Expired);

        assert!(
            app.world().get::<Bloodstream>(doctor).unwrap().0.is_empty(),
            "the joke does not work if they treat the person who was asking"
        );
    }

    #[test]
    fn someone_already_walking_out_is_not_a_patient() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let leaving = patient(&mut app, "Miner Sato", "Cargo");
        app.world_mut().get_mut::<CrewRoute>(leaving).unwrap().leave();

        resolve(&mut app, &name, Outcome::Expired);

        assert!(app.world().get::<Bloodstream>(leaving).unwrap().0.is_empty());
    }

    #[test]
    fn an_empty_waiting_room_costs_nothing() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Expired);

        assert!(
            app.world().resource::<RadioLog>().entries.is_empty(),
            "with nobody to treat there is nothing to report"
        );
    }

    #[test]
    fn filling_the_order_costs_nothing() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let bystander = patient(&mut app, "Miner Sato", "Cargo");

        resolve(&mut app, &name, Outcome::Success);

        assert!(app.world().get::<Bloodstream>(bystander).unwrap().0.is_empty());
        assert_eq!(app.world().resource::<QuackProgress>().0, 1);
    }

    #[test]
    fn quack_ron_parses_and_stays_off_the_ordinary_roster() {
        let data = data();
        let script = script();

        assert_eq!(
            Department::from_role(&script.role),
            Some(Department::Medical),
            "this is Medical's thread"
        );
        assert!(script.visits.len() >= 2);
        assert!(script.malpractice_penalty < 0);
        assert!(script.improvised_units > 0);
        assert!(
            script.malpractice_lines.iter().all(|line| line.contains("{name}")),
            "a line that never names the patient reads as unrelated chatter"
        );
        let improvised = data
            .reagents
            .id_of(&script.improvised)
            .unwrap_or_else(|| panic!("'{}' names no real reagent", script.improvised));
        assert!(
            data.reagents.get(improvised).is_harmful(),
            "'{}' has to actually be a bad idea, or being ignored costs nothing",
            script.improvised
        );
        let roster: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        assert!(
            roster.iter().all(|member| member.name != script.name),
            "'{}' must stay off the ordinary roster or it could double-book",
            script.name
        );
        for visit in &script.visits {
            assert!(
                data.reagents.id_of(&visit.reagent).is_some(),
                "'{}' names no real reagent",
                visit.reagent
            );
            assert!(!visit.plea.trim().is_empty());
        }
    }
}
