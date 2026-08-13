//! Habits, and what they cost.
//!
//! Before this, dealing had **only downside**. A successful illicit delivery
//! raised two hidden meters (`antagonist::UnderworldStanding`,
//! `antagonist::SecuritySuspicion`) and eventually produced a crisis or a
//! raid; there was no reason to do it beyond curiosity. This is the other
//! half: crew who keep getting dosed with something addictive start coming
//! back on their own, and they pay in the one currency the game has.
//!
//! The bargain, in three parts:
//!
//! 1. **Getting hooked** ([`note_doses`]) reads crew bloodstreams directly
//!    rather than hooking the delivery pipeline, so it catches *every* route
//!    for free — a handover, a smoke cloud from a reaction you set off across
//!    the room, a syringe. That is the same "general exposure" principle M12
//!    established, applied once more.
//! 2. **The income** ([`generate_addict_visits`]) is an ordinary
//!    [`IllicitOrder`], so `antagonist::handle_illicit_resolutions` already
//!    picks it up and the underworld/suspicion/plot consequences need no new
//!    wiring at all. What is new is that it pays **research points** — the
//!    currency M11 deliberately made scarce, which is what makes a habit
//!    genuinely tempting against a 213-point tech tree.
//! 3. **The risk** ([`notice_the_high`]) is the good part. An addict who is
//!    visibly high in the lab *while a Security-role NPC is also there* raises
//!    suspicion, per second of overlap. That turns `SecuritySuspicion` from an
//!    invisible accumulator into something the player can watch happening —
//!    the addict swaying is already drawn by `fx`, the officer is already
//!    walking in — and, crucially, play around. Hold the delivery. Stall.
//!
//! And [`handle_withdrawal`]: an addict left dry turns up in a bad way and
//! costs their department standing. You cannot cleanly stop once you have
//! started, which is what makes the first sale a decision rather than free
//! money.
//!
//! **Keyed by name, not entity.** Crew entities are despawned when they walk
//! out; a habit outlives the visit, so it is stored against the crew member's
//! name and persisted with the career in `progress.ron`.
//!
//! **Not replicated, and it needs no sync message.** Nothing a guest draws
//! reads this table: the visits it schedules are ordinary replicated crew
//! entities, and there is deliberately no "your addicts" list anywhere in the
//! UI — such a list would hand the player the very tell this system exists to
//! make them watch for.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use chem_sim::Units;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::antagonist::SecuritySuspicion;
use crate::body::Bloodstream;
use crate::chem_data::ChemDb;
use crate::crew::{spawn_crew_member, CrewMember, CrewPhase, CrewRoute};
use crate::interaction::Interactable;
use crate::knowledge::Knowledge;
use crate::net::is_authority;
use crate::orders::{
    Department, IllicitOrder, Order, OrderKind, OrderResolved, Shift, StationData,
};
use crate::radio::{RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::AppState;

pub struct AddictionPlugin;

impl Plugin for AddictionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<AddictionScript>::new(&["addiction.ron"]))
            .init_resource::<Addictions>()
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (
                    promote_script,
                    note_doses,
                    notice_the_high,
                    generate_addict_visits,
                    handle_addict_resolutions,
                    handle_withdrawal,
                )
                    .chain()
                    .run_if(is_authority)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One crew member's habit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Habit {
    /// What they are hooked on, by reagent key.
    pub reagent: String,
    /// Accumulated exposure. Crosses [`AddictionScript::hooked_at`] to become
    /// a real habit; a fractional total is someone who has had a taste.
    pub weight: f64,
    /// Seconds since their last dose. Drives both the return visit and, past
    /// [`AddictionScript::withdrawal_after`], the withdrawal.
    pub dry_for: f32,
    /// Whether the withdrawal for this dry spell has already been delivered.
    /// Without it, an addict past the threshold produces one withdrawal visit
    /// *per frame*.
    #[serde(default)]
    pub withdrawn: bool,
}

impl Habit {
    fn hooked(&self, script: &AddictionScript) -> bool {
        self.weight >= script.hooked_at
    }
}

/// Who on the station has a habit.
///
/// See the module doc for why this is keyed by name and why it is not
/// replicated.
#[derive(Resource, Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Addictions(pub HashMap<String, Habit>);

impl Addictions {
    /// Everyone whose habit has passed the threshold.
    pub fn hooked(&self, script: &AddictionScript) -> Vec<(&String, &Habit)> {
        self.0
            .iter()
            .filter(|(_, habit)| habit.hooked(script))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// `assets/data/station.addiction.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct AddictionScript {
    /// Accumulated `addictive` weight before someone is a customer rather
    /// than a person who took something once.
    pub hooked_at: f64,
    /// Units of an addictive reagent in a bloodstream before a tick counts as
    /// a dose at all. Stops a trace left over from a smoke cloud two rooms
    /// away from slowly hooking the entire crew.
    pub dose_threshold: u32,
    /// Seconds between counting doses. Bloodstreams are read every frame
    /// otherwise, and one delivery would register as several hundred.
    pub dose_interval_seconds: f32,
    /// Multiplied onto the current legitimate order gap to space return
    /// visits — the same shape every other thread's own gap multiplier takes.
    pub gap_multiplier: (f32, f32),
    /// Units an addict asks for.
    pub amounts: Vec<u32>,
    /// Research points a fulfilled addict sale pays.
    ///
    /// Worth several ordinary deliveries (which pay 1 each,
    /// `knowledge::RESEARCH_PER_SUCCESS`). That gap *is* the temptation: it is
    /// what makes a habit visibly accelerate a 213-point tech tree.
    pub research_per_sale: u32,
    /// Suspicion per second that an addict is visibly high in the lab while a
    /// Security-role NPC is present.
    pub suspicion_per_second: f32,
    /// Seconds without a dose before an addict turns up in withdrawal.
    pub withdrawal_after: f32,
    /// Standing cost to the addict's department when they do.
    pub withdrawal_penalty: i32,
    /// What an addict says when they come back.
    pub pleas: Vec<String>,
    /// What the radio carries when one turns up in withdrawal.
    pub withdrawal_lines: Vec<String>,
}

#[derive(Resource)]
struct PendingAddictionScript(Handle<AddictionScript>);

#[derive(Resource, Deref)]
pub struct Script(pub AddictionScript);

#[derive(Resource)]
struct AddictSpawner {
    timer: Timer,
}

/// Ticks between bloodstream reads. See [`AddictionScript::dose_interval_seconds`].
#[derive(Resource)]
struct DoseClock(Timer);

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingAddictionScript(
        assets.load("data/station.addiction.ron"),
    ));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingAddictionScript>>,
    mut scripts: ResMut<Assets<AddictionScript>>,
    spawner: Option<Res<AddictSpawner>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    if spawner.is_none() {
        commands.insert_resource(AddictSpawner {
            timer: Timer::from_seconds(FIRST_VISIT_SECONDS, TimerMode::Once),
        });
        commands.insert_resource(DoseClock(Timer::from_seconds(
            script.dose_interval_seconds,
            TimerMode::Repeating,
        )));
    }
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingAddictionScript>();
}

/// How long after the first habit forms before anyone comes back. Their own
/// clock re-arms off the ramp from there.
const FIRST_VISIT_SECONDS: f32 = 90.0;

// ---------------------------------------------------------------------------
// Getting hooked
// ---------------------------------------------------------------------------

/// Notices what is in crew bloodstreams, on a slow tick.
///
/// Reads the bloodstream rather than hooking `orders::complete_delivery`
/// specifically so that it does not care *how* the dose got there. A beaker
/// handed over the counter, a cloud from a reaction that went wrong, a
/// syringe — they all end up in the same place, and they all count.
fn note_doses(
    time: Res<Time>,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    clock: Option<ResMut<DoseClock>>,
    mut addictions: ResMut<Addictions>,
    crew: Query<(&CrewMember, &Bloodstream)>,
) {
    let (Some(script), Some(mut clock)) = (script, clock) else {
        return;
    };
    let elapsed = time.delta_secs();
    if !clock.0.tick(time.delta()).just_finished() {
        // Dry time still runs between reads, or withdrawal would only ever
        // advance on the frames a habit happened to be looked at.
        for habit in addictions.0.values_mut() {
            habit.dry_for += elapsed;
        }
        return;
    }

    let threshold = Units::whole(script.dose_threshold as i32);
    let mut dosed: Vec<(String, String, f64)> = Vec::new();
    for (member, blood) in &crew {
        // Both compartments: swallowed-but-not-absorbed still counts, or a
        // drink handed over the counter would only register once it had made
        // it through the stomach, several ticks later.
        for (id, amount) in blood.0.blood.iter().chain(blood.0.stomach.iter()) {
            let reagent = db.reagents.get(id);
            if reagent.addictive <= 0.0 || amount < threshold {
                continue;
            }
            dosed.push((member.name.clone(), reagent.key.clone(), reagent.addictive));
        }
    }

    let interval = script.dose_interval_seconds;
    for habit in addictions.0.values_mut() {
        habit.dry_for += interval;
    }

    for (name, reagent, weight) in dosed {
        let habit = addictions.0.entry(name).or_insert_with(|| Habit {
            reagent: reagent.clone(),
            weight: 0.0,
            dry_for: 0.0,
            withdrawn: false,
        });
        // Switching what they are hooked on rather than tracking several at
        // once: a habit is a relationship with one substance, and the last
        // thing they took is the thing they will ask you for.
        habit.reagent = reagent;
        habit.weight += weight;
        habit.dry_for = 0.0;
        habit.withdrawn = false;
    }
}

// ---------------------------------------------------------------------------
// The risk
// ---------------------------------------------------------------------------

/// Raises suspicion while an addict is visibly high in front of Security.
///
/// The payoff of the whole system: this is the one thing in the game that
/// makes `SecuritySuspicion` *visible*. The addict's stagger and tint are
/// already drawn by `crate::fx` off the same statuses this reads, and the
/// officer walking in is already drawn by `dress_crew` — so a player who is
/// paying attention can see the raid coming and stall until one of them
/// leaves, and a player who is not, cannot.
fn notice_the_high(
    time: Res<Time>,
    script: Option<Res<Script>>,
    addictions: Res<Addictions>,
    mut suspicion: ResMut<SecuritySuspicion>,
    mut carried: Local<f32>,
    crew: Query<(&CrewMember, &CrewRoute, Option<&Bloodstream>)>,
) {
    let Some(script) = script else {
        return;
    };

    // Only crew actually in the room count. Someone still walking in through
    // the door has not been seen by anyone yet.
    let present = || crew.iter().filter(|(_, route, _)| route.phase != CrewPhase::Leaving);

    let security_watching = present().any(|(member, _, _)| {
        Department::from_role(&member.role) == Some(Department::Security)
    });
    if !security_watching {
        return;
    }

    let visibly_high = present()
        .filter(|(member, _, _)| {
            addictions
                .0
                .get(&member.name)
                .is_some_and(|habit| habit.hooked(&script))
        })
        // "Visibly" high is exactly the set of statuses `crate::fx` already
        // draws as a stagger and a tint, so what Security can notice is the
        // same thing the player can see across the room.
        .any(|(_, _, blood)| {
            blood.is_some_and(|blood| blood.0.active_statuses().next().is_some())
        });
    if !visibly_high {
        return;
    }

    // Accumulated in a float and spent in whole points, because
    // `SecuritySuspicion` is an integer and a fractional rise per frame would
    // otherwise truncate to nothing at all.
    *carried += script.suspicion_per_second * time.delta_secs();
    while *carried >= 1.0 {
        *carried -= 1.0;
        suspicion.0 += 1;
    }
}

// ---------------------------------------------------------------------------
// The income
// ---------------------------------------------------------------------------

/// Sends an addict back to the counter, asking by name.
///
/// Marked [`IllicitOrder`], which is what makes it exact-match, invisible to
/// the ordinary UI, and — for free — subject to everything
/// `antagonist::handle_illicit_resolutions` already does with a successful
/// illicit delivery.
#[allow(clippy::too_many_arguments)]
fn generate_addict_visits(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<AddictSpawner>>,
    addictions: Res<Addictions>,
    shift: Res<Shift>,
    active: Query<&CrewMember>,
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

    // Nobody already at the counter — an addict who is standing in front of
    // you cannot also be walking back in.
    let here: Vec<&str> = active.iter().map(|member| member.name.as_str()).collect();
    let hooked = addictions.hooked(&script);
    let candidates: Vec<&(&String, &Habit)> = hooked
        .iter()
        .filter(|(name, _)| !here.contains(&name.as_str()))
        .collect();
    let Some((name, habit)) = candidates.choose(&mut rng).copied() else {
        return;
    };
    let Some(crew_def) = station.crew.iter().find(|def| &def.name == *name) else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&habit.reagent) else {
        return;
    };
    let Some(&amount) = script.amounts.choose(&mut rng) else {
        return;
    };
    let Some(plea) = script.pleas.choose(&mut rng) else {
        return;
    };

    // The ordinary difficulty's patience, for the same reason an antagonist
    // visit uses it: a visit that waited noticeably longer or shorter than
    // normal would itself be a statistical tell.
    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);
    let lane = active.iter().count() as f32 * 0.95;
    let crew = spawn_crew_member(&mut commands, crew_def, lane);

    let reagent_name = db.reagents.get(reagent).name.clone();
    commands.entity(crew).insert((
        Order {
            reagent,
            specific: false,
            amount: Units::whole(amount as i32),
            plea: plea.clone(),
            patience,
            waited: 0.0,
        },
        IllicitOrder,
        Interactable::new(format!(
            "{} — hand over {}u {}",
            crew_def.name, amount, reagent_name
        )),
    ));

    info!("addiction: {} is back for {}u {}", crew_def.name, amount, reagent_name);
}

/// Pays for a sale, and resets the customer's clock.
///
/// The underworld standing, the security suspicion and the arc's plot are all
/// already moved by `antagonist::handle_illicit_resolutions` reading the same
/// message — this only adds the part that is genuinely new: the money.
fn handle_addict_resolutions(
    script: Option<Res<Script>>,
    mut resolved: MessageReader<OrderResolved>,
    mut addictions: ResMut<Addictions>,
    mut knowledge: ResMut<Knowledge>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    for report in resolved.read() {
        if report.kind != OrderKind::Illicit || !report.outcome.is_good() {
            continue;
        }
        let Some(habit) = addictions.0.get_mut(&report.name) else {
            continue;
        };
        if !habit.hooked(&script) {
            continue;
        }
        habit.dry_for = 0.0;
        habit.withdrawn = false;
        knowledge.award_research(script.research_per_sale);
    }
}

// ---------------------------------------------------------------------------
// Withdrawal
// ---------------------------------------------------------------------------

/// What happens when the supply stops.
///
/// The trap: you cannot cleanly walk away from a customer. Fires **once** per
/// dry spell, guarded by [`Habit::withdrawn`] — without it an addict past the
/// threshold would file a complaint every frame forever.
fn handle_withdrawal(
    script: Option<Res<Script>>,
    mut addictions: ResMut<Addictions>,
    station: Option<Res<StationData>>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(station)) = (script, station) else {
        return;
    };
    let mut lines: Vec<(String, String)> = Vec::new();

    for (name, habit) in addictions.0.iter_mut() {
        if habit.withdrawn
            || !habit.hooked(&script)
            || habit.dry_for < script.withdrawal_after
        {
            continue;
        }
        habit.withdrawn = true;
        let Some(role) = station
            .crew
            .iter()
            .find(|def| &def.name == name)
            .map(|def| def.role.clone())
        else {
            continue;
        };
        lines.push((name.clone(), role));
    }

    let mut rng = rand::rng();
    for (name, role) in lines {
        if let Some(department) = Department::from_role(&role) {
            shift.adjust(department, script.withdrawal_penalty);
        }
        let line = script
            .withdrawal_lines
            .choose(&mut rng)
            .cloned()
            .unwrap_or_else(|| "{name} has not been fit to work all shift.".to_string());
        radio.push(RadioEntry {
            channel: crate::radio::channel_for(&role),
            text: line.replace("{name}", &name),
            good: false,
        });
        info!("addiction: {name} is in withdrawal");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::CrewDef;
    use crate::orders::{OrderConfig, Outcome};
    use chem_sim::Route;
    use std::time::Duration;

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn script() -> AddictionScript {
        ron::from_str(include_str!("../../assets/data/station.addiction.ron"))
            .expect("station.addiction.ron should parse")
    }

    fn crew_roster() -> Vec<CrewDef> {
        ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap()
    }

    fn addiction_app() -> App {
        let config: OrderConfig =
            ron::from_str(include_str!("../../assets/data/station.orders.ron")).unwrap();
        let script = script();
        let interval = script.dose_interval_seconds;

        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(StationData {
                crew: crew_roster(),
                config,
            })
            .insert_resource(Script(script))
            .insert_resource(DoseClock(Timer::from_seconds(interval, TimerMode::Repeating)))
            .init_resource::<Addictions>()
            .init_resource::<SecuritySuspicion>()
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .init_resource::<Time>()
            .add_message::<OrderResolved>()
            .add_systems(
                Update,
                (note_doses, notice_the_high, handle_withdrawal).chain(),
            );
        app
    }

    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(seconds));
        app.update();
    }

    /// A crew member standing in the lab with `reagent` in their blood.
    ///
    /// Metabolises a few ticks after receiving, because `receive` alone only
    /// moves the dose into the bloodstream — it is `metabolise` that turns a
    /// dose into the *statuses* `notice_the_high` reads and `fx` draws. The
    /// real game gets there on the 2-second metabolism tick; this just skips
    /// the wait.
    fn dosed_crew(app: &mut App, name: &str, role: &str, reagent: &str, units: i32) -> Entity {
        let id = app.world().resource::<ChemDb>().reagent(reagent);
        let db = app.world().resource::<ChemDb>().0.clone();
        let mut blood = chem_sim::Bloodstream::default();
        let mut vitals = chem_sim::Vitals::default();
        let mut dose = chem_sim::Solution::unbounded();
        let _ = dose.add(id, Units::whole(units));
        blood.receive(&mut dose, Route::Injected, &mut vitals, &db);
        for _ in 0..3 {
            chem_sim::metabolise(&mut vitals, &mut blood, &db);
        }

        app.world_mut()
            .spawn((
                CrewMember {
                    name: name.to_string(),
                    role: role.to_string(),
                },
                CrewRoute::arrival(0.0),
                Bloodstream(blood),
            ))
            .id()
    }

    fn interval(app: &App) -> f32 {
        app.world().resource::<Script>().dose_interval_seconds
    }

    // -- getting hooked ----------------------------------------------------

    #[test]
    fn repeated_doses_of_something_addictive_build_a_habit() {
        let mut app = addiction_app();
        dosed_crew(&mut app, "Chef Dubois", "Service", "space_drugs", 20);
        let tick = interval(&app);
        let hooked_at = app.world().resource::<Script>().hooked_at;

        // Enough reads to cross the threshold several times over.
        for _ in 0..20 {
            advance(&mut app, tick + 0.01);
        }

        let addictions = app.world().resource::<Addictions>();
        let habit = addictions
            .0
            .get("Chef Dubois")
            .expect("someone kept full of space drugs should end up with a habit");
        assert_eq!(habit.reagent, "space_drugs");
        assert!(habit.weight >= hooked_at, "the habit should have taken hold");
    }

    #[test]
    fn a_reagent_nobody_gets_hooked_on_never_builds_one() {
        let mut app = addiction_app();
        // Kelotane is ordinary burn medicine — `addictive` is absent, so 0.0.
        dosed_crew(&mut app, "Dr. Vance", "Medical", "kelotane", 40);
        let tick = interval(&app);

        for _ in 0..20 {
            advance(&mut app, tick + 0.01);
        }

        assert!(
            app.world().resource::<Addictions>().0.is_empty(),
            "an ordinary medicine must never build a habit, at any dose"
        );
    }

    #[test]
    fn a_trace_dose_is_not_a_dose() {
        // Otherwise a whiff of a smoke cloud drifting across the room would
        // slowly hook the entire crew.
        let mut app = addiction_app();
        let threshold = app.world().resource::<Script>().dose_threshold;
        dosed_crew(
            &mut app,
            "Chef Dubois",
            "Service",
            "space_drugs",
            threshold.saturating_sub(1).max(1) as i32 / 2,
        );
        let tick = interval(&app);

        for _ in 0..20 {
            advance(&mut app, tick + 0.01);
        }

        assert!(app.world().resource::<Addictions>().0.is_empty());
    }

    // -- the risk ----------------------------------------------------------

    /// An addict who is present and visibly high.
    fn plant_addict(app: &mut App, name: &str, role: &str) -> Entity {
        let entity = dosed_crew(app, name, role, "space_drugs", 20);
        let hooked_at = app.world().resource::<Script>().hooked_at;
        app.world_mut().resource_mut::<Addictions>().0.insert(
            name.to_string(),
            Habit {
                reagent: "space_drugs".to_string(),
                weight: hooked_at * 2.0,
                dry_for: 0.0,
                withdrawn: false,
            },
        );
        entity
    }

    #[test]
    fn an_addict_alone_in_the_lab_raises_no_suspicion() {
        let mut app = addiction_app();
        plant_addict(&mut app, "Chef Dubois", "Service");

        advance(&mut app, 30.0);

        assert_eq!(
            app.world().resource::<SecuritySuspicion>().0,
            0,
            "nobody saw anything"
        );
    }

    #[test]
    fn security_watching_an_addict_get_high_is_what_costs_you() {
        let mut app = addiction_app();
        plant_addict(&mut app, "Chef Dubois", "Service");
        dosed_crew(&mut app, "Officer Reyes", "Security", "kelotane", 5);

        advance(&mut app, 30.0);

        assert!(
            app.world().resource::<SecuritySuspicion>().0 > 0,
            "an addict swaying in front of an officer is the whole risk of dealing"
        );
    }

    #[test]
    fn security_alone_is_harmless() {
        let mut app = addiction_app();
        dosed_crew(&mut app, "Officer Reyes", "Security", "kelotane", 5);

        advance(&mut app, 30.0);

        assert_eq!(app.world().resource::<SecuritySuspicion>().0, 0);
    }

    #[test]
    fn suspicion_stops_the_moment_either_of_them_leaves() {
        let mut app = addiction_app();
        let addict = plant_addict(&mut app, "Chef Dubois", "Service");
        dosed_crew(&mut app, "Officer Reyes", "Security", "kelotane", 5);
        advance(&mut app, 20.0);
        let while_watched = app.world().resource::<SecuritySuspicion>().0;
        assert!(while_watched > 0);

        app.world_mut().get_mut::<CrewRoute>(addict).unwrap().leave();
        advance(&mut app, 60.0);

        assert_eq!(
            app.world().resource::<SecuritySuspicion>().0,
            while_watched,
            "walking out is the play, and it has to actually work"
        );
    }

    // -- withdrawal --------------------------------------------------------

    #[test]
    fn an_addict_left_dry_files_exactly_one_complaint() {
        let mut app = addiction_app();
        app.world_mut().resource_mut::<Addictions>().0.insert(
            "Chef Dubois".to_string(),
            Habit {
                reagent: "space_drugs".to_string(),
                weight: 99.0,
                dry_for: 0.0,
                withdrawn: false,
            },
        );
        let after = app.world().resource::<Script>().withdrawal_after;
        let penalty = app.world().resource::<Script>().withdrawal_penalty;

        // No crew entity, so nothing re-doses them; dry time accumulates.
        let tick = interval(&app);
        for _ in 0..((after / tick).ceil() as u32 + 4) {
            advance(&mut app, tick + 0.01);
        }
        let after_first = app.world().resource::<RadioLog>().entries.len();

        // And keep going well past it.
        for _ in 0..20 {
            advance(&mut app, tick + 0.01);
        }

        assert_eq!(after_first, 1, "withdrawal should have landed exactly once");
        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            1,
            "and must not repeat every frame afterwards"
        );
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Service),
            penalty,
            "their department notices"
        );
    }

    #[test]
    fn someone_who_only_had_a_taste_never_goes_into_withdrawal() {
        let mut app = addiction_app();
        app.world_mut().resource_mut::<Addictions>().0.insert(
            "Chef Dubois".to_string(),
            Habit {
                reagent: "space_drugs".to_string(),
                // Below `hooked_at`.
                weight: 0.1,
                dry_for: 0.0,
                withdrawn: false,
            },
        );
        let after = app.world().resource::<Script>().withdrawal_after;
        let tick = interval(&app);

        for _ in 0..((after / tick).ceil() as u32 + 10) {
            advance(&mut app, tick + 0.01);
        }

        assert!(app.world().resource::<RadioLog>().entries.is_empty());
    }

    // -- the sale ----------------------------------------------------------

    #[test]
    fn a_sale_pays_in_the_currency_that_is_actually_scarce() {
        let db = data();
        let mut app = App::new();
        app.insert_resource(ChemDb(db.clone()))
            .insert_resource(Script(script()))
            .insert_resource(Knowledge::new(&db))
            .init_resource::<Addictions>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_addict_resolutions);

        let hooked_at = app.world().resource::<Script>().hooked_at;
        let pay = app.world().resource::<Script>().research_per_sale;
        app.world_mut().resource_mut::<Addictions>().0.insert(
            "Chef Dubois".to_string(),
            Habit {
                reagent: "space_drugs".to_string(),
                weight: hooked_at * 2.0,
                dry_for: 500.0,
                withdrawn: true,
            },
        );

        let space_drugs = app.world().resource::<ChemDb>().reagent("space_drugs");
        app.world_mut().write_message(OrderResolved {
            name: "Chef Dubois".to_string(),
            role: "Service".to_string(),
            reagent: Some(space_drugs),
            category: None,
            outcome: Outcome::Success,
            kind: OrderKind::Illicit,
        });
        app.update();

        assert_eq!(app.world().resource::<Knowledge>().research_points, pay);
        let addictions = app.world().resource::<Addictions>();
        let habit = &addictions.0["Chef Dubois"];
        assert_eq!(habit.dry_for, 0.0, "they are topped up again");
        assert!(!habit.withdrawn, "and the next dry spell can complain afresh");
    }

    #[test]
    fn selling_to_someone_without_a_habit_pays_nothing() {
        let db = data();
        let mut app = App::new();
        app.insert_resource(ChemDb(db.clone()))
            .insert_resource(Script(script()))
            .insert_resource(Knowledge::new(&db))
            .init_resource::<Addictions>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_addict_resolutions);

        app.world_mut().write_message(OrderResolved {
            name: "Someone Else".to_string(),
            role: "Service".to_string(),
            reagent: None,
            category: None,
            outcome: Outcome::Success,
            kind: OrderKind::Illicit,
        });
        app.update();

        assert_eq!(
            app.world().resource::<Knowledge>().research_points,
            0,
            "an ordinary antagonist deal is not a repeat customer"
        );
    }

    // -- content guardrails ------------------------------------------------

    #[test]
    fn the_addiction_script_is_coherent() {
        let script = script();
        assert!(script.hooked_at > 0.0);
        assert!(script.dose_threshold > 0);
        assert!(script.dose_interval_seconds > 0.0);
        assert!(!script.amounts.is_empty());
        assert!(!script.pleas.is_empty());
        assert!(!script.withdrawal_lines.is_empty());
        assert!(
            script.withdrawal_lines.iter().all(|line| line.contains("{name}")),
            "a withdrawal line that never names anyone reads as unrelated chatter"
        );
        assert!(script.withdrawal_penalty < 0);
        assert!(
            script.research_per_sale > crate::knowledge::RESEARCH_PER_SUCCESS,
            "if a sale paid no better than an honest delivery there would be no temptation"
        );
        assert!(script.suspicion_per_second > 0.0);
    }

    #[test]
    fn every_illicit_reagent_can_actually_hook_someone() {
        // The economy is built on the Illicit shelf. One that nobody can get
        // hooked on is a dead end the player would have to discover by
        // dealing it fruitlessly for an hour.
        let data = data();
        for reagent in data.reagents.iter() {
            if !reagent.categories.contains(&chem_sim::Category::Illicit) {
                continue;
            }
            assert!(
                reagent.addictive > 0.0,
                "'{}' is Illicit but nobody ever comes back for it",
                reagent.key
            );
        }
    }

    #[test]
    fn ordinary_medicine_is_never_addictive() {
        let data = data();
        for reagent in data.reagents.iter() {
            let treats_something = reagent.categories.iter().any(|category| {
                matches!(
                    category,
                    chem_sim::Category::Trauma
                        | chem_sim::Category::Burns
                        | chem_sim::Category::Antitoxins
                        | chem_sim::Category::Airloss
                        | chem_sim::Category::Radiation
                )
            });
            assert!(
                !(treats_something && reagent.addictive > 0.0),
                "'{}' is medicine crew legitimately order — an accidental habit \
                 from filling honest orders is a trap, not a decision",
                reagent.key
            );
        }
    }
}
