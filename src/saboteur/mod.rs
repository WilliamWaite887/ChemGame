//! Engineering's minor antagonist: a tech whose "repairs" aren't.
//!
//! One of the five department shenanigan threads. Unlike a main antagonist
//! (`crate::cult`, gated on the save having drawn it), a department minor runs
//! in **every** save, in both modes, always.
//!
//! Built on the [`crate::obsessed`] template, and a sibling of
//! [`crate::smuggler`] — same shape, different hook. Ignore this one and they
//! get bored and start "checking" your glassware: a visit that expires
//! unfilled splashes a few units of something into an unattended container.
//!
//! **The container is not destroyed.** That is the whole difference from the
//! smuggler's theft, and it is the nastier of the two: a stolen beaker is
//! obviously gone, while a contaminated one looks exactly like the batch you
//! made — right up until the delivery grades `Impure` and you cannot work out
//! why. The tell is real and visible for anyone who looks, because
//! `Container::solution` is what drives the liquid's colour.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use chem_sim::Units;
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy, InSlot, Stored};
use crate::crew::CrewDef;
use crate::net::is_authority;
use crate::orders::{OrderResolved, Outcome, Shift, StationData};
use crate::player::Chemist;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::{self, current_rules};
use crate::AppState;

const INITIAL_GAP_SECONDS: (f32, f32) = (240.0, 420.0);

pub struct SaboteurPlugin;

impl Plugin for SaboteurPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<SaboteurScript>::new(&["saboteur.ron"]))
            .init_resource::<SaboteurProgress>()
            .add_systems(Startup, start_loading)
            .add_systems(OnEnter(AppState::Playing), arm_spawner)
            .add_systems(
                Update,
                (
                    promote_script,
                    generate_saboteur_visit,
                    handle_saboteur_resolution,
                )
                    .chain()
                    .run_if(is_authority)
                    // No `arc::is_active` gate, unlike a main antagonist —
                    // see the module doc.
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Which authored visit fires next. Persisted, same as
/// `obsessed::ObsessedProgress`.
#[derive(Resource, Default, Clone, Copy)]
pub struct SaboteurProgress(pub usize);

/// `assets/data/station.saboteur.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct SaboteurScript {
    /// Kept off `station.crew.ron` for the same reason every other recurring
    /// identity is: so an ordinary order can never double-book them.
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
    pub gap_multiplier: (f32, f32),
    pub visits: Vec<SaboteurVisitDef>,
    /// What gets splashed into an unattended container when they are ignored.
    /// A real reagent, so the mess is a real mess: it reacts, it colours the
    /// liquid, and it grades.
    pub contaminant: String,
    pub contaminant_units: u32,
    /// Aired when they do it.
    pub meddling_lines: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SaboteurVisitDef {
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
}

#[derive(Resource)]
struct PendingSaboteurScript(Handle<SaboteurScript>);

#[derive(Resource, Deref)]
struct Script(SaboteurScript);

#[derive(Resource)]
struct SaboteurSpawner {
    timer: Timer,
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingSaboteurScript(
        assets.load("data/station.saboteur.ron"),
    ));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingSaboteurScript>>,
    mut scripts: ResMut<Assets<SaboteurScript>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingSaboteurScript>();
}

/// See `shift::arm_first_visit` for why this has to re-run on
/// `OnEnter(AppState::Playing)` every session rather than only once at
/// process start.
fn arm_spawner(mut commands: Commands) {
    shift::arm_first_visit(&mut commands, INITIAL_GAP_SECONDS, |timer| {
        SaboteurSpawner { timer }
    });
}

#[allow(clippy::too_many_arguments)]
fn generate_saboteur_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<SaboteurSpawner>>,
    progress: Res<SaboteurProgress>,
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
    spawner.timer = shift::roll_next_gap(&mut rng, &rules, script.gap_multiplier);

    let Some(visit) = script
        .visits
        .get(progress.0.min(script.visits.len().saturating_sub(1)))
    else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&visit.reagent) else {
        warn!("saboteur visit names unknown reagent '{}'", visit.reagent);
        return;
    };

    shift::spawn_scripted_visit(
        &mut commands,
        &db,
        &mut rng,
        &rules,
        shift::ScriptedVisit {
            name: &script.name,
            role: &script.role,
            color: script.color,
            reagent,
            amount_units: visit.amount,
            plea: visit.plea.clone(),
        },
    );

    radio.push(
        RadioEntry::new(channel_for(&script.role), visit.plea.clone())
            .speaker(&script.name)
            .negative(),
    );
}

/// Glassware they can get at, exactly as `smuggler` defines it: anything held,
/// slotted or shut in a locker is under someone's eye, and "keep hold of it"
/// has to remain the answer.
type LooseGlassware<'w, 's> =
    Query<'w, 's, &'static mut Container, (Without<HeldBy>, Without<InSlot>, Without<Stored>)>;

/// Advances the chain — and, on an expired visit, has a fiddle.
///
/// Only [`Outcome::Expired`] triggers it, not a wrong delivery: handing them
/// the wrong thing is a mistake, leaving them standing there with nothing to
/// do is what gives them the idea.
#[allow(clippy::too_many_arguments)]
fn handle_saboteur_resolution(
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<SaboteurProgress>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    mut loose: LooseGlassware,
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

        // A `SecondInspection` requisition absorbs one contamination before
        // it happens.
        if shift.requisition.saboteur_wards > 0 {
            shift.requisition.saboteur_wards -= 1;
            radio.push(
                RadioEntry::new(
                    channel_for(&script.role),
                    "Engineering signed off without touching the glassware.",
                )
                .positive(),
            );
            continue;
        }

        let mut rng = rand::rng();
        if let Some(contaminant) = db.reagents.id_of(&script.contaminant) {
            // Only a container that already holds something: splashing into
            // an empty beaker is not sabotage, it is a free ingredient, and
            // the player would never even notice.
            if let Some(mut container) = loose
                .iter_mut()
                .filter(|container| container.solution.total_volume().is_positive())
                .choose(&mut rng)
            {
                let amount = Units::whole(script.contaminant_units as i32);
                // Through `mutate`, not a raw `solution.add`, so the splash
                // resolves reactions and re-tints the liquid exactly as if the
                // player had poured it in themselves.
                container.mutate(&db, |solution| {
                    let _ = solution.add(contaminant, amount);
                });

                let line = script
                    .meddling_lines
                    .choose(&mut rng)
                    .cloned()
                    .unwrap_or_else(|| "Something in the lab looks different.".to_string());
                radio.push(RadioEntry::new(channel_for(&script.role), line).negative());
                info!("saboteur: {} had a fiddle with the glassware", script.name);
            }
        }

        // A minor left to get on with it is a small gift to whoever the save
        // is really about.
        if let (Some(arc_script), Some(campaign)) = (arc_script.as_deref(), campaign.as_mut()) {
            crate::arc::note_ignored_shenanigan(arc_script, campaign);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers::ContainerKind;
    use crate::orders::{Department, OrderKind};

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn script() -> SaboteurScript {
        ron::from_str(include_str!("../../assets/data/station.saboteur.ron"))
            .expect("station.saboteur.ron should parse")
    }

    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(Script(script()))
            .init_resource::<SaboteurProgress>()
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_saboteur_resolution);
        app
    }

    /// A beaker sitting out on the counter with a real batch in it.
    fn loose_batch(app: &mut App) -> Entity {
        let kelotane = app.world().resource::<ChemDb>().reagent("kelotane");
        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(kelotane, Units::whole(30));
        app.world_mut()
            .spawn(Container {
                kind: ContainerKind::Beaker,
                solution,
            })
            .id()
    }

    fn resolve(app: &mut App, name: &str, outcome: Outcome) {
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Engineering".to_string(),
            reagent: None,
            category: None,
            outcome,
            kind: OrderKind::Normal,
        });
        app.update();
    }

    #[test]
    fn ignoring_them_leaves_the_batch_there_but_wrong() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_batch(&mut app);
        let before = app
            .world()
            .get::<Container>(beaker)
            .unwrap()
            .solution
            .clone();

        resolve(&mut app, &name, Outcome::Expired);

        let after = &app.world().get::<Container>(beaker).unwrap().solution;
        assert!(
            app.world().get_entity(beaker).is_ok(),
            "they meddle — they do not steal; that is the smuggler's job"
        );
        assert_ne!(
            after.total_volume(),
            before.total_volume(),
            "an untouched batch means nothing actually happened"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn an_empty_beaker_is_left_alone() {
        // Splashing into an empty one is not sabotage, it is a free
        // ingredient — and the player would never notice either way.
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let empty = app
            .world_mut()
            .spawn(Container {
                kind: ContainerKind::Beaker,
                solution: chem_sim::Solution::unbounded(),
            })
            .id();

        resolve(&mut app, &name, Outcome::Expired);

        assert!(app
            .world()
            .get::<Container>(empty)
            .unwrap()
            .solution
            .is_empty());
        assert!(app.world().resource::<RadioLog>().entries.is_empty());
    }

    #[test]
    fn a_held_beaker_is_never_touched() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_batch(&mut app);
        let holder = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(beaker).insert(HeldBy(holder));
        let before = app
            .world()
            .get::<Container>(beaker)
            .unwrap()
            .solution
            .clone();

        resolve(&mut app, &name, Outcome::Expired);

        assert_eq!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .total_volume(),
            before.total_volume(),
            "keeping hold of it has to actually be the answer"
        );
    }

    #[test]
    fn filling_the_order_costs_nothing() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_batch(&mut app);
        let before = app
            .world()
            .get::<Container>(beaker)
            .unwrap()
            .solution
            .clone();

        resolve(&mut app, &name, Outcome::Success);

        assert_eq!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .total_volume(),
            before.total_volume()
        );
        assert_eq!(app.world().resource::<SaboteurProgress>().0, 1);
    }

    #[test]
    fn a_banked_ward_absorbs_the_expiry_and_nothing_happens() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_batch(&mut app);
        let before = app
            .world()
            .get::<Container>(beaker)
            .unwrap()
            .solution
            .clone();
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .saboteur_wards = 1;

        resolve(&mut app, &name, Outcome::Expired);

        assert_eq!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .total_volume(),
            before.total_volume(),
            "a Second Inspection requisition should have covered this one"
        );
        assert_eq!(
            app.world().resource::<Shift>().requisition.saboteur_wards,
            0,
            "the ward is spent, not banked indefinitely"
        );
        assert_eq!(
            app.world().resource::<SaboteurProgress>().0,
            1,
            "the chain still advances even though nothing was touched"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn an_unrelated_expiry_never_costs_anything() {
        let mut app = resolution_app();
        let beaker = loose_batch(&mut app);
        let before = app
            .world()
            .get::<Container>(beaker)
            .unwrap()
            .solution
            .clone();

        resolve(&mut app, "Dr. Vance", Outcome::Expired);

        assert_eq!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .total_volume(),
            before.total_volume()
        );
        assert_eq!(app.world().resource::<SaboteurProgress>().0, 0);
    }

    #[test]
    fn saboteur_ron_parses_and_stays_off_the_ordinary_roster() {
        let data = data();
        let script = script();

        assert_eq!(
            Department::from_role(&script.role),
            Some(Department::Engineering),
            "this is Engineering's thread"
        );
        assert!(script.visits.len() >= 2);
        assert!(!script.meddling_lines.is_empty());
        assert!(script.contaminant_units > 0);
        assert!(
            data.reagents.id_of(&script.contaminant).is_some(),
            "'{}' names no real reagent, so the sabotage would silently do nothing",
            script.contaminant
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
