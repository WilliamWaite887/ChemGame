//! Cargo's minor antagonist: a tech with a side business.
//!
//! One of the five department shenanigan threads. Unlike a main antagonist
//! (`crate::cult`, gated on the save having drawn it), a department minor runs
//! in **every** save, in both modes, always — the station is never so busy
//! with an existential threat that Cargo stops being Cargo.
//!
//! Built on the [`crate::obsessed`] template: a recurring named identity kept
//! off `station.crew.ron`, an ordered chain of authored visits, advanced by
//! matching the resolution's name. What is its own is the hook: **ignore them
//! and they help themselves.** A visit that expires unfilled lifts an
//! unattended container off the counter — not one you are holding, and not one
//! sitting in a machine slot. You lose the batch, and you find out by looking
//! for it.
//!
//! That is deliberately the cheapest possible consequence to *avoid*: hold the
//! beaker, or put it in the window. It is a nudge toward tidiness, not a tax.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy, InSlot, Stored};
use crate::crew::{spawn_crew_member, CrewDef};
use crate::interaction::Interactable;
use crate::net::is_authority;
use crate::orders::{deliverable_amount, Order, OrderResolved, Outcome, Shift, StationData};
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::AppState;

const INITIAL_GAP_SECONDS: (f32, f32) = (240.0, 420.0);

pub struct SmugglerPlugin;

impl Plugin for SmugglerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<SmugglerScript>::new(&["smuggler.ron"]))
            .init_resource::<SmugglerProgress>()
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (
                    promote_script,
                    generate_smuggler_visit,
                    handle_smuggler_resolution,
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
pub struct SmugglerProgress(pub usize);

/// `assets/data/station.smuggler.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct SmugglerScript {
    /// Kept off `station.crew.ron` for the same reason every other recurring
    /// identity is: so an ordinary order can never double-book them.
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
    pub gap_multiplier: (f32, f32),
    pub visits: Vec<SmugglerVisitDef>,
    /// Aired when a visit expires and they take something instead.
    pub theft_lines: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SmugglerVisitDef {
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
}

#[derive(Resource)]
struct PendingSmugglerScript(Handle<SmugglerScript>);

#[derive(Resource, Deref)]
struct Script(SmugglerScript);

#[derive(Resource)]
struct SmugglerSpawner {
    timer: Timer,
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingSmugglerScript(
        assets.load("data/station.smuggler.ron"),
    ));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingSmugglerScript>>,
    mut scripts: ResMut<Assets<SmugglerScript>>,
    spawner: Option<Res<SmugglerSpawner>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    if spawner.is_none() {
        let gap = rand::rng().random_range(INITIAL_GAP_SECONDS.0..=INITIAL_GAP_SECONDS.1);
        commands.insert_resource(SmugglerSpawner {
            timer: Timer::from_seconds(gap, TimerMode::Once),
        });
    }
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingSmugglerScript>();
}

#[allow(clippy::too_many_arguments)]
fn generate_smuggler_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<SmugglerSpawner>>,
    progress: Res<SmugglerProgress>,
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
        warn!("smuggler visit names unknown reagent '{}'", visit.reagent);
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
    let amount = deliverable_amount(&db, reagent, chem_sim::Units::whole(visit.amount as i32));
    commands.entity(crew).insert((
        Order {
            reagent,
            specific: true,
            amount,
            plea: visit.plea.clone(),
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
        text: format!("{}: {}", script.name, visit.plea),
        good: false,
    });
}

/// Containers nobody is holding and nothing is holding.
///
/// Deliberately excludes anything held, slotted or shut in a locker: they lift
/// what is *unattended*, which is what makes "hold onto it" a real answer — and
/// putting it away is the same answer, with the locker as the thing that makes
/// it available to a chemist who has to walk off and do something else.
type LooseGlassware<'w, 's> = Query<
    'w,
    's,
    Entity,
    (
        With<Container>,
        Without<HeldBy>,
        Without<InSlot>,
        Without<Stored>,
    ),
>;

/// Advances the chain — and, on an expired visit, takes something.
///
/// Only [`Outcome::Expired`] triggers the theft, not a wrong delivery:
/// handing them the wrong thing is a mistake, ignoring them entirely is an
/// invitation.
#[allow(clippy::too_many_arguments)]
fn handle_smuggler_resolution(
    mut commands: Commands,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<SmugglerProgress>,
    mut radio: ResMut<RadioLog>,
    loose: LooseGlassware,
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

        let mut rng = rand::rng();
        if let Some(taken) = loose.iter().choose(&mut rng) {
            commands.entity(taken).despawn();
            let line = script
                .theft_lines
                .choose(&mut rng)
                .cloned()
                .unwrap_or_else(|| "Something has gone missing off the counter.".to_string());
            radio.push(RadioEntry {
                channel: channel_for(&script.role),
                text: line,
                good: false,
            });
            info!("smuggler: {} lifted a container", script.name);
        }

        // A minor left to get on with it is a small gift to whoever the save
        // is really about — the thread that ties five department shenanigans
        // to the campaign without making any of them mandatory.
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

    fn script() -> SmugglerScript {
        ron::from_str(include_str!("../../assets/data/station.smuggler.ron"))
            .expect("station.smuggler.ron should parse")
    }

    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(Script(script()))
            .init_resource::<SmugglerProgress>()
            .init_resource::<RadioLog>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_smuggler_resolution);
        app
    }

    /// A beaker sitting out on the counter with nobody holding it.
    fn loose_beaker(app: &mut App) -> Entity {
        app.world_mut()
            .spawn(Container {
                kind: ContainerKind::Beaker,
                solution: chem_sim::Solution::unbounded(),
            })
            .id()
    }

    fn resolve(app: &mut App, name: &str, outcome: Outcome) {
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Cargo".to_string(),
            reagent: None,
            category: None,
            outcome,
            kind: OrderKind::Normal,
        });
        app.update();
    }

    #[test]
    fn ignoring_them_costs_you_whatever_was_left_on_the_counter() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_beaker(&mut app);

        resolve(&mut app, &name, Outcome::Expired);

        assert!(
            app.world().get_entity(beaker).is_err(),
            "an unattended beaker is exactly what a side business is for"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn a_held_beaker_is_never_taken() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let holder = app.world_mut().spawn_empty().id();
        let held = app
            .world_mut()
            .spawn((
                Container {
                    kind: ContainerKind::Beaker,
                    solution: chem_sim::Solution::unbounded(),
                },
                HeldBy(holder),
            ))
            .id();

        resolve(&mut app, &name, Outcome::Expired);

        assert!(
            app.world().get_entity(held).is_ok(),
            "holding onto it has to actually be the answer"
        );
    }

    #[test]
    fn filling_the_order_costs_nothing() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_beaker(&mut app);

        resolve(&mut app, &name, Outcome::Success);

        assert!(app.world().get_entity(beaker).is_ok());
        assert_eq!(
            app.world().resource::<SmugglerProgress>().0,
            1,
            "the chain still advances — they came, they were served"
        );
    }

    #[test]
    fn an_unrelated_expiry_never_costs_anything() {
        let mut app = resolution_app();
        let beaker = loose_beaker(&mut app);

        resolve(&mut app, "Dr. Vance", Outcome::Expired);

        assert!(app.world().get_entity(beaker).is_ok());
        assert_eq!(app.world().resource::<SmugglerProgress>().0, 0);
    }

    #[test]
    fn smuggler_ron_parses_and_stays_off_the_ordinary_roster() {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let script = script();

        assert_eq!(
            Department::from_role(&script.role),
            Some(Department::Cargo),
            "this is Cargo's thread"
        );
        assert!(script.visits.len() >= 2);
        assert!(!script.theft_lines.is_empty());
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
