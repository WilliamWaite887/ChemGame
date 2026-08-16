//! Station-wide crises: what happens once the underworld you've been
//! supplying finally spills over.
//!
//! [`crate::antagonist::UnderworldStanding`] used to be write-only — every
//! successful illicit delivery raised it and nothing ever read it back
//! (`antagonist::handle_illicit_resolutions`). This is the consumer: cross a
//! threshold and someone, somewhere in the station, is now in real trouble
//! from whatever has been circulating. The chemist has to treat them
//! directly, under a deadline, not just grade an order and move on.
//!
//! Deliberately reuses almost everything already built rather than forking a
//! parallel system:
//!
//! - The victim is an ordinary [`crate::crew::CrewMember`] carrying a
//!   [`crate::orders::CrisisOrder`]-marked [`crate::orders::Order`], so the
//!   existing counter/window delivery pipeline (`orders::complete_delivery`)
//!   grades it with no new logic at all.
//! - `Order::reagent` is set to the case's `cure_reagent` purely as a
//!   *reference* — `orders::reference_category` already derives a lenient
//!   order's category from that reagent's own `categories.first()`, so any
//!   member of the same category cures it, exactly like a normal request.
//! - The harm itself is nothing but the M12 general-exposure pipeline run in
//!   reverse: the victim is dosed with the harmful reagent directly
//!   (bypassing a beaker, like an injection) the moment they're afflicted,
//!   and a delivered cure heals/counters it through the same
//!   `Bloodstream::receive` every delivery already goes through
//!   (`orders::complete_delivery`'s M12E addition). Nothing here reaches
//!   into a body directly except the initial affliction.
//! - `orders::expire_orders` already handles the deadline (`Order::patience`
//!   reused unchanged) and already writes `OrderResolved` on timeout — this
//!   module only reacts to that message, the same shape
//!   `antagonist::handle_illicit_resolutions` reacts to a normal one.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use chem_sim::{Route, Solution, Units};
use rand::prelude::*;
use serde::Deserialize;

use crate::antagonist::UnderworldStanding;
use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::crew::{spawn_crew_member, CrewDef};
use crate::interaction::Interactable;
use crate::lab::LabLight;
use crate::net::is_authority;
use crate::orders::{
    deliverable_amount, CrisisOrder, Department, Order, OrderResolved, Shift, StationData,
};
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// How fast the alert lighting lerps toward red and back, per second. Fast
/// enough that a crisis reads as sudden, slow enough not to strobe.
const ALERT_LERP_RATE: f32 = 1.5;

pub struct CrisisPlugin;

impl Plugin for CrisisPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<CrisisScript>::new(&["crisis.ron"]))
            .init_resource::<CrisisSchedule>()
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (promote_script, schedule_crisis, handle_crisis_resolutions)
                    .chain()
                    .run_if(is_authority)
                    .run_if(in_state(AppState::Playing)),
            )
            // Presentation, not state — runs on every peer. Whether a crisis
            // is live is read straight off `CrisisOrder`, which replicates
            // (unlike `IllicitOrder`, a crisis has nothing to hide), so a
            // joining client sees the same alert lighting the host does
            // without a bespoke sync message.
            .add_systems(
                Update,
                pulse_alert_lighting.run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// `assets/data/station.crisis.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct CrisisScript {
    pub underworld_threshold: i32,
    pub warning_seconds: (f32, f32),
    pub deadline_seconds: (f32, f32),
    pub cases: Vec<CrisisCaseDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CrisisCaseDef {
    pub name: String,
    pub harm_reagent: String,
    pub harm_amount: u32,
    pub afflicted_role: String,
    /// Which departments would actually be any use here. Crew of these roles
    /// converge on the casualty; everyone else clears the area and sits it out
    /// at home.
    ///
    /// Not derivable from `afflicted_role`: that says who is *hurt*, and the
    /// department that treats an overdose is rarely the one that suffered it.
    /// `serde(default)` so a case that names nobody means "nobody can help"
    /// rather than a failed parse of the whole script.
    #[serde(default)]
    pub responders: Vec<String>,
    /// Doubles as the crisis order's reference reagent — see the module doc.
    pub cure_reagent: String,
    pub cure_amount: u32,
    /// Fires immediately over the radio the moment the warning elapses,
    /// unlike ordinary chatter's delayed report — a crisis should read as
    /// happening *now*.
    pub alarm_line: String,
    /// What the victim says at the counter/queue.
    pub plea: String,
    pub resolved_line: String,
    pub unresolved_line: String,
}

/// Which departments would be any use to this casualty.
///
/// Lives on the victim, and dies with them, so "is there a crisis and where" is
/// answered by a query rather than by bookkeeping that has to be kept in step
/// with the order expiring.
#[derive(Component)]
pub struct CrisisResponse {
    pub responders: Vec<String>,
}

#[derive(Resource)]
struct PendingCrisisScript(Handle<CrisisScript>);

#[derive(Resource, Deref)]
struct Script(CrisisScript);

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingCrisisScript(assets.load("data/station.crisis.ron")));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingCrisisScript>>,
    mut scripts: ResMut<Assets<CrisisScript>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingCrisisScript>();
}

// ---------------------------------------------------------------------------
// The warning, and the affliction
// ---------------------------------------------------------------------------

/// When the next warning or affliction is due, and which case it will be —
/// chosen at warning time so the alarm line and the eventual victim always
/// agree on what's actually going around. `None` means `UnderworldStanding`
/// has not crossed the threshold since the last crisis cleared.
#[derive(Resource, Default)]
pub struct CrisisSchedule {
    warning_in: Option<f32>,
    case: usize,
}

/// Watches `UnderworldStanding`, warns, then afflicts a crew member.
///
/// Gated on `Shift::accepting_orders` like every other new-traffic spawn —
/// and additionally on no crisis already being active, so a second one can
/// never stack on top of the first.
#[allow(clippy::too_many_arguments)]
fn schedule_crisis(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut schedule: ResMut<CrisisSchedule>,
    underworld: Res<UnderworldStanding>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
    active: Query<(), With<CrisisOrder>>,
) {
    let (Some(station), Some(script)) = (station, script) else {
        return;
    };
    if !shift.accepting_orders || !active.is_empty() {
        return;
    }

    if let Some(warning) = schedule.warning_in.as_mut() {
        *warning -= time.delta_secs();
        if *warning > 0.0 {
            return;
        }
        schedule.warning_in = None;

        let Some(case) = script.cases.get(schedule.case) else {
            return;
        };
        afflict_victim(
            &mut commands,
            &db,
            &station,
            case,
            script.deadline_seconds,
            &mut radio,
        );
        return;
    }

    if underworld.0 >= script.underworld_threshold {
        let mut rng = rand::rng();
        schedule.case = rng.random_range(0..script.cases.len().max(1));
        schedule.warning_in =
            Some(rng.random_range(script.warning_seconds.0..=script.warning_seconds.1));
        let Some(case) = script.cases.get(schedule.case) else {
            return;
        };
        radio.push(RadioEntry {
            channel: "MED".to_string(),
            text: case.alarm_line.clone(),
            good: false,
        });
    }
}

/// Spawns the victim, doses them with the case's harmful reagent directly —
/// bypassing a beaker, the way an injection would — and attaches the
/// [`CrisisOrder`] the ordinary delivery pipeline grades.
fn afflict_victim(
    commands: &mut Commands,
    db: &ChemDb,
    station: &StationData,
    case: &CrisisCaseDef,
    deadline_seconds: (f32, f32),
    radio: &mut RadioLog,
) {
    let Some(harm) = db.reagents.id_of(&case.harm_reagent) else {
        warn!(
            "crisis case names unknown harm reagent '{}'",
            case.harm_reagent
        );
        return;
    };
    let Some(cure) = db.reagents.id_of(&case.cure_reagent) else {
        warn!(
            "crisis case names unknown cure reagent '{}'",
            case.cure_reagent
        );
        return;
    };

    let candidates: Vec<&CrewDef> = station
        .crew
        .iter()
        .filter(|def| def.role == case.afflicted_role)
        .collect();
    let Some(crew_def) = candidates.choose(&mut rand::rng()).copied() else {
        warn!(
            "no crew member with role '{}' for a crisis",
            case.afflicted_role
        );
        return;
    };

    let mut rng = rand::rng();
    let deadline = rng.random_range(deadline_seconds.0..=deadline_seconds.1);

    // The affliction itself: an injected dose, computed in plain Rust before
    // the entity even exists — `Bloodstream::receive` needs no ECS access at
    // all, so there is no need to spawn first and mutate after. `receive`
    // alone only ever applies `Contact` damage (the same reason a delivered
    // medicine does not visibly heal the instant it lands, M12E) — a few
    // ticks of `metabolise` on top is what turns "dosed" into "already
    // hurting" by the time anyone sees them, and it means the harm keeps
    // building for real if the deadline runs out, not just once.
    let mut body = chem_sim::Vitals::default();
    let mut blood = chem_sim::Bloodstream::default();
    let mut dose = Solution::unbounded();
    let _ = dose.add(harm, Units::whole(case.harm_amount as i32));
    blood.receive(&mut dose, Route::Injected, &mut body, &db.0);
    for _ in 0..3 {
        chem_sim::metabolise(&mut body, &mut blood, &db.0);
    }

    let victim = spawn_crew_member(commands, crew_def, 0.0);
    commands.entity(victim).insert((
        Body(body),
        Bloodstream(blood),
        Order {
            reagent: cure,
            specific: false,
            // Only the *cure* is clamped. `harm_amount` above is what the
            // casualty was dosed with before they walked in — a poisoning is
            // meant to be past the threshold, and that is the whole crisis.
            amount: deliverable_amount(db, cure, Units::whole(case.cure_amount as i32)),
            plea: case.plea.clone(),
            patience: deadline,
            waited: 0.0,
        },
        CrisisOrder,
        // Carried on the casualty rather than held in a "current crisis"
        // resource: the thing crew need to know is *where* to converge, and the
        // casualty already is that. It also means a second crisis would simply
        // work, with each drawing its own responders.
        CrisisResponse {
            responders: case.responders.clone(),
        },
        Interactable::new(format!("{} — {}", crew_def.name, case.name)),
    ));

    radio.push(RadioEntry {
        channel: "MED".to_string(),
        text: format!("{} is down in the chem lab waiting area.", crew_def.name),
        good: false,
    });
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Reacts to a crisis order's `OrderResolved`, the same way
/// `antagonist::handle_illicit_resolutions` reacts to an illicit one.
///
/// A cure drains `UnderworldStanding` back to zero — "you got ahead of the
/// damage". Anything else (a wrong delivery, or the deadline running out via
/// `orders::expire_orders`) leaves it exactly where it was: still at or past
/// threshold, so the next crisis is free to arm again as soon as this one
/// clears `active` in `schedule_crisis` — declining or failing to keep up
/// with the fallout has a real, escalating cost.
fn handle_crisis_resolutions(
    script: Option<Res<Script>>,
    mut resolved: MessageReader<OrderResolved>,
    mut underworld: ResMut<UnderworldStanding>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    for report in resolved.read() {
        if !report.kind.is_crisis() {
            continue;
        }
        let Some(department) = Department::from_role(&report.role) else {
            continue;
        };
        if report.outcome.is_good() {
            underworld.0 = 0;
            if let Some(line) = script
                .cases
                .iter()
                .find(|case| case.afflicted_role == report.role)
                .map(|case| case.resolved_line.clone())
            {
                radio.push(RadioEntry {
                    channel: "MED".to_string(),
                    text: line,
                    good: true,
                });
            }
        } else {
            // The ordinary department penalty already landed in
            // `orders::complete_delivery`/`expire_orders`
            // (`adjust_for_role`, unconditional for a non-illicit order) — a
            // second, harsher notch on top is what makes failing a crisis
            // cost more than botching an ordinary request.
            shift.adjust(department, -2);
            if let Some(line) = script
                .cases
                .iter()
                .find(|case| case.afflicted_role == report.role)
                .map(|case| case.unresolved_line.clone())
            {
                radio.push(RadioEntry {
                    channel: "MED".to_string(),
                    text: line,
                    good: false,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Presentation: the room itself reacting
// ---------------------------------------------------------------------------

/// Component-wise lerp between two colours, done by hand rather than
/// reaching for a `Color`/`Srgba` blend method this project hasn't already
/// verified exists in Bevy 0.19 — plain arithmetic on four `f32`s needs no
/// API to get right.
fn lerp_color(from: Color, to: Color, t: f32) -> Color {
    let from = from.to_srgba();
    let to = to.to_srgba();
    Color::srgba(
        from.red + (to.red - from.red) * t,
        from.green + (to.green - from.green) * t,
        from.blue + (to.blue - from.blue) * t,
        1.0,
    )
}

/// How close a colour must get to its target before the pulse snaps it there
/// and stops writing.
///
/// The lerp approaches asymptotically and never exactly arrives, so without a
/// snap there is no settled state to detect at all: every light would sit a
/// hair off its target and be rewritten forever. One part in four hundred is
/// far below what the eye resolves on a lamp.
const ALERT_SETTLE_EPSILON: f32 = 0.0025;

fn colors_match(a: Color, b: Color) -> bool {
    let (a, b) = (a.to_srgba(), b.to_srgba());
    (a.red - b.red).abs() < ALERT_SETTLE_EPSILON
        && (a.green - b.green).abs() < ALERT_SETTLE_EPSILON
        && (a.blue - b.blue).abs() < ALERT_SETTLE_EPSILON
}

/// Pulls every `LabLight` toward red while a crisis is live, and back to its
/// own room tint once it isn't. Reads `Has<CrisisOrder>` directly rather than
/// a bespoke resource — see the plugin doc for why that is enough to stay in
/// sync across a co-op session with no new replicated state.
///
/// Settles rather than lerping forever. This runs on every peer with no
/// authority gate (it is presentation), and the overwhelmingly common state is
/// "no crisis, every lamp already at its own tint" — where an unconditional
/// write marked all eleven `PointLight`s plus the global ambient as changed
/// every frame, for the entire session, to assign them the colour they already
/// had.
fn pulse_alert_lighting(
    time: Res<Time>,
    active: Query<(), With<CrisisOrder>>,
    mut lights: Query<(&LabLight, &mut PointLight)>,
    mut ambient: Option<ResMut<GlobalAmbientLight>>,
) {
    let target_alert = if active.is_empty() { 0.0 } else { 1.0 };
    let t = (time.delta_secs() * ALERT_LERP_RATE).clamp(0.0, 1.0);
    let alarm = Color::srgb(0.85, 0.08, 0.08);

    for (light, mut point) in &mut lights {
        let target = lerp_color(light.base_color, alarm, target_alert);
        if colors_match(point.color, target) {
            if point.color != target {
                point.color = target;
            }
            continue;
        }
        point.color = lerp_color(point.color, target, t);
    }

    if let Some(ambient) = ambient.as_mut() {
        let base = Color::srgb(0.62, 0.68, 0.80);
        let target = lerp_color(base, alarm, target_alert * 0.6);
        if colors_match(ambient.color, target) {
            if ambient.color != target {
                ambient.color = target;
            }
            return;
        }
        ambient.color = lerp_color(ambient.color, target, t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::OrderConfig;
    use std::time::Duration;

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn script() -> CrisisScript {
        ron::from_str(include_str!("../../assets/data/station.crisis.ron")).unwrap()
    }

    /// Just enough world to drive `schedule_crisis` and `afflict_victim`: no
    /// renderer, no window, no other order traffic.
    fn crisis_app() -> App {
        let crew: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        let config: OrderConfig =
            ron::from_str(include_str!("../../assets/data/station.orders.ron")).unwrap();

        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(StationData { crew, config })
            .insert_resource(Script(script()))
            .insert_resource(UnderworldStanding(0))
            .init_resource::<CrisisSchedule>()
            .insert_resource(Shift {
                accepting_orders: true,
                ..Default::default()
            })
            .init_resource::<Time>()
            .init_resource::<RadioLog>()
            .add_message::<OrderResolved>()
            .add_systems(Update, (schedule_crisis, handle_crisis_resolutions).chain());
        app
    }

    /// A leaner app for resolution-only tests — `schedule_crisis` is
    /// deliberately *not* registered, so setting `UnderworldStanding` high
    /// enough to matter to `handle_crisis_resolutions` can't also
    /// accidentally arm a brand new warning in the same `app.update()`.
    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(Script(script()))
            .insert_resource(UnderworldStanding(0))
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_crisis_resolutions);
        app
    }

    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(seconds));
        app.update();
    }

    #[test]
    fn nothing_happens_below_the_threshold() {
        let mut app = crisis_app();
        app.world_mut().resource_mut::<UnderworldStanding>().0 = 4;

        advance(&mut app, 1.0);

        assert!(app
            .world()
            .resource::<CrisisSchedule>()
            .warning_in
            .is_none());
        let mut victims = app.world_mut().query::<&CrisisOrder>();
        assert!(victims.iter(app.world()).next().is_none());
    }

    #[test]
    fn crossing_the_threshold_arms_an_immediate_warning() {
        // Unlike ordinary chatter, the alarm is not delayed — it should be on
        // the log the same frame the threshold is crossed.
        let mut app = crisis_app();
        app.world_mut().resource_mut::<UnderworldStanding>().0 = 8;

        advance(&mut app, 0.1);

        assert!(app
            .world()
            .resource::<CrisisSchedule>()
            .warning_in
            .is_some());
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn the_warning_elapsing_afflicts_a_real_victim() {
        let mut app = crisis_app();
        app.world_mut().resource_mut::<UnderworldStanding>().0 = 8;
        advance(&mut app, 0.1);
        let warning = app.world().resource::<CrisisSchedule>().warning_in.unwrap();

        advance(&mut app, warning + 0.5);

        let mut victims = app.world_mut().query::<(&CrisisOrder, &Order, &Body)>();
        let (_, order, body) = victims
            .iter(app.world())
            .next()
            .expect("a crisis should spawn exactly one victim once the warning elapses");
        assert!(order.patience > 0.0, "the deadline should be real");
        assert!(
            body.0.is_hurt(),
            "the victim should already be in trouble the moment they appear"
        );

        // No second crisis should arm while this one is still active, even
        // though `UnderworldStanding` is still at the threshold.
        advance(&mut app, 60.0);
        assert_eq!(
            app.world_mut()
                .query::<&CrisisOrder>()
                .iter(app.world())
                .count(),
            1,
            "a crisis must not stack on top of itself"
        );
    }

    #[test]
    fn a_cured_crisis_drains_underworld_standing() {
        let mut app = resolution_app();
        app.world_mut().resource_mut::<UnderworldStanding>().0 = 12;
        let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");

        app.world_mut().write_message(OrderResolved {
            name: "Dr. Vance".to_string(),
            role: "Medical".to_string(),
            reagent: Some(dylovene),
            category: None,
            outcome: crate::orders::Outcome::Success,
            kind: crate::orders::OrderKind::Crisis,
        });
        app.update();

        assert_eq!(
            app.world().resource::<UnderworldStanding>().0,
            0,
            "a successful cure should mean you got ahead of it"
        );
        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            1,
            "the resolved line should air"
        );
    }

    #[test]
    fn an_unresolved_crisis_leaves_the_standing_where_it_was() {
        let mut app = resolution_app();
        app.world_mut().resource_mut::<UnderworldStanding>().0 = 12;

        app.world_mut().write_message(OrderResolved {
            name: "Dr. Vance".to_string(),
            role: "Medical".to_string(),
            reagent: None,
            category: None,
            outcome: crate::orders::Outcome::Expired,
            kind: crate::orders::OrderKind::Crisis,
        });
        app.update();

        assert_eq!(
            app.world().resource::<UnderworldStanding>().0,
            12,
            "failing to keep up should not reset the meter — the fallout is still live"
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Medical),
            -2,
            "an unresolved crisis costs standing beyond the ordinary botched-order penalty"
        );
    }

    #[test]
    fn a_non_crisis_resolution_is_ignored() {
        let mut app = resolution_app();
        app.world_mut().resource_mut::<UnderworldStanding>().0 = 12;
        let dylovene = app.world().resource::<ChemDb>().reagent("dylovene");

        app.world_mut().write_message(OrderResolved {
            name: "Dr. Vance".to_string(),
            role: "Medical".to_string(),
            reagent: Some(dylovene),
            category: None,
            outcome: crate::orders::Outcome::Success,
            kind: crate::orders::OrderKind::Normal,
        });
        app.update();

        assert_eq!(
            app.world().resource::<UnderworldStanding>().0,
            12,
            "an ordinary delivery must never touch the crisis meter"
        );
    }

    #[test]
    fn every_crisis_case_names_real_reagents_and_a_real_department() {
        let data = data();
        let script = script();
        assert!(!script.cases.is_empty());
        for case in &script.cases {
            assert!(
                data.reagents.id_of(&case.harm_reagent).is_some(),
                "'{}' names no real reagent",
                case.harm_reagent
            );
            assert!(
                data.reagents.id_of(&case.cure_reagent).is_some(),
                "'{}' names no real reagent",
                case.cure_reagent
            );
            assert!(
                Department::from_role(&case.afflicted_role).is_some(),
                "'{}' names a role no department recognises",
                case.afflicted_role
            );
        }
    }
}
