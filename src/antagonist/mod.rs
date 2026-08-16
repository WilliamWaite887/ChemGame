//! The hidden antagonist thread.
//!
//! Some crew visits are secretly someone after an illicit substance, under an
//! entirely ordinary-sounding pretext — never flagged anywhere the player can
//! see. The only tell is unrelated-looking ambient radio chatter that happens
//! to mention the same substance, aired independently of the visit. Give them
//! what they want and a hidden "underworld" standing rises, followed by a
//! delayed report of the chaos it caused; decline, and the visit just grades
//! like any other unfulfilled order — see [`crate::orders::complete_delivery`],
//! which is where that fall-through actually happens.
//!
//! Deliberately its own system rather than a branch inside `generate_orders`:
//! illicit requests are never knowledge-gated, forecast-weighted or
//! stretch-chanced — the requester already knows exactly what they want.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use chem_sim::Units;
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::crew::{spawn_crew_member, CrewMember};
use crate::interaction::Interactable;
use crate::net::is_authority;
use crate::orders::{deliverable_amount, IllicitOrder, Order, OrderResolved, Shift, StationData};
use crate::player::Chemist;
use crate::radio::{PendingBroadcasts, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::AppState;

/// How long a stung delivery's raid warning runs before the officer walks
/// in — short, since the whole point of a sting is that it reads as
/// immediate rather than a slow-building suspicion. `security::schedule_raid`
/// still owns everything past the warning (spawning the officer, the dwell,
/// the sweep) unmodified.
const STING_WARNING_SECONDS: f32 = 6.0;

/// Seconds after an antagonist order is created before the priming incident
/// airs. Uniform across a wide range on purpose: an order can resolve in
/// seconds (a beaker already in hand) or take minutes (patience nearly
/// spent), so this alone decides whether the clue lands before the visit is
/// over or only makes sense in hindsight afterwards.
const PRIMING_DELAY_SECONDS: (f32, f32) = (0.0, 90.0);

/// Seconds after a successful illicit delivery before the chaos it caused
/// gets reported back. Much longer than the priming delay — the priming
/// incident is background noise happening anyway; the chaos report is a
/// direct consequence of what the player just did, and should feel like news
/// arriving from elsewhere, not an instant reaction.
const CHAOS_DELAY_SECONDS: (f32, f32) = (60.0, 180.0);

/// How much a successful illicit delivery raises Security's hidden suspicion.
///
/// Applied as one lump at resolution rather than split across "now" and
/// "when the chaos line airs" — the meter is never shown to the player, so
/// there is nothing for a second, separately-timed bump to buy beyond
/// complexity. Both the immediate exposure and the fallout that is already in
/// motion are real the moment the delivery happens.
const SUSPICION_PER_DELIVERY: i32 = 5;

/// How much a successful illicit delivery raises underworld standing. A flat
/// constant rather than the ordinary reward curve — that curve exists to
/// punish making someone wait, and an antagonist's patience is already drawn
/// from the same range a legitimate order's is, so scaling this too would
/// double-count the same wait against the same number twice over.
const UNDERWORLD_PER_DELIVERY: i32 = 2;

pub struct AntagonistPlugin;

impl Plugin for AntagonistPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<AntagonistScript>::new(&["antagonist.ron"]))
            .init_resource::<UnderworldStanding>()
            .init_resource::<SecuritySuspicion>()
            .add_systems(Startup, start_loading)
            .add_systems(OnEnter(AppState::Playing), arm_spawner)
            .add_systems(
                Update,
                (
                    promote_script,
                    generate_antagonist_orders,
                    handle_illicit_resolutions,
                )
                    .chain()
                    .run_if(is_authority)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// Hidden state
// ---------------------------------------------------------------------------

/// How much the underworld likes you. Never replicated, never shown in any
/// UI — its only visible effects are the crew who show up and the radio
/// lines that follow, both of which already ride their own rails.
///
/// Persisted to `progress.ron` regardless: this game keeps no secrets from a
/// player willing to open a save file in a text editor, the same as every
/// other career fact.
#[derive(Resource, Default, Clone, Copy)]
pub struct UnderworldStanding(pub i32);

/// How close Security is to raiding the lab. Never replicated, never shown,
/// and never persisted — a reloaded save should not silently arm a raid the
/// instant the file opens. Read and reset by `crate::security` (M10c).
#[derive(Resource, Default, Clone, Copy)]
pub struct SecuritySuspicion(pub i32);

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// `assets/data/station.antagonist.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct AntagonistScript {
    /// Multiplied onto the *current* legitimate order gap
    /// (`current_rules(...).gap_seconds`) to get the antagonist gap, rather
    /// than a flat range of its own. Without this, antagonist visits stay at
    /// a fixed cadence while legitimate orders arrive faster and faster as
    /// the ramp tightens, so antagonists would get relatively rarer over a
    /// career instead of scaling with it. See [`generate_antagonist_orders`].
    pub gap_multiplier: (f32, f32),
    /// The underworld standing at which [`effective_gap_multiplier`] has
    /// fully narrowed the range to its own low end — content-authored to
    /// track `crisis::CrisisScript::underworld_threshold` (not code-coupled
    /// to it; `crisis` already reads `UnderworldStanding` independently and
    /// does not care what shaped the climb toward its own threshold).
    pub standing_tighten_at: i32,
    pub requests: Vec<AntagonistRequestDef>,
}

/// The antagonist gap multiplier's effective range at a given underworld
/// standing. Narrows toward its own low end as standing climbs toward
/// `standing_tighten_at`, so a reliable dealer notices visits creeping
/// closer together in the run-up to a crisis, on top of the ordinary career
/// ramp `generate_antagonist_orders` already applies. Pure, so the trend is
/// directly testable without spinning up an `App`.
pub fn effective_gap_multiplier(script: &AntagonistScript, underworld: i32) -> (f32, f32) {
    let frac = (underworld as f32 / script.standing_tighten_at.max(1) as f32).clamp(0.0, 1.0);
    let lo = script.gap_multiplier.0;
    let hi = script.gap_multiplier.1 - (script.gap_multiplier.1 - script.gap_multiplier.0) * frac;
    (lo, hi.max(lo))
}

/// The antagonist clock's very first arm, before any `Shift`/`StationData`
/// exist to compute a ramp-scaled gap against. Only the *first* visit uses
/// this — every re-arm after that reads `gap_multiplier` against the current
/// legitimate order gap, so only the steady-state cadence needs to track the
/// ramp.
const INITIAL_GAP_SECONDS: (f32, f32) = (240.0, 420.0);

#[derive(Clone, Debug, Deserialize)]
pub struct AntagonistRequestDef {
    pub reagent: String,
    pub amounts: Vec<u32>,
    /// Which department's crew voices this, for pretext plausibility — "vent
    /// maintenance" reads as Engineering, not Medical. Also who a spawned
    /// visitor is drawn from: an existing, already-recognised name off
    /// `station.crew.ron`, never "a stranger" — a name nobody knows would
    /// itself be the tell this whole system is built to avoid.
    pub role: String,
    /// The ordinary-sounding ask, naming the reagent directly.
    pub pretext: String,
    /// The unrelated-looking station-news line naming the same reagent — the
    /// only tell, aired independently of the visit.
    pub incident_line: String,
    /// The delayed follow-up after a successful delivery.
    pub chaos_line: String,
    /// Floor on `UnderworldStanding` before this request can ever be
    /// picked. Defaults to `i32::MIN`, i.e. always available — only the
    /// bolder, higher-tier pretexts set this, so a reliable dealer sees the
    /// pretext variety visibly shift as standing climbs. See
    /// [`effective_gap_multiplier`] for the sibling tuning this pairs with.
    #[serde(default = "min_standing_default")]
    pub min_standing: i32,
    /// Chance a *successful* delivery of this request is a sting: skips the
    /// delayed `chaos_line` entirely and instead immediately arms
    /// `security`'s raid — a Spy-flavoured variant of the black-market
    /// thread, where getting caught is not a matter of accumulated
    /// suspicion but of this one deal going wrong. `0.0` (inert) unless a
    /// request opts in. See [`handle_illicit_resolutions`].
    #[serde(default)]
    pub sting_chance: f64,
}

fn min_standing_default() -> i32 {
    i32::MIN
}

#[derive(Resource)]
struct PendingAntagonistScript(Handle<AntagonistScript>);

#[derive(Resource, Deref)]
struct Script(AntagonistScript);

/// The clock between antagonist visits — its own, much rarer than the
/// legitimate `OrderSpawner`'s.
#[derive(Resource)]
struct AntagonistSpawner {
    timer: Timer,
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingAntagonistScript(
        assets.load("data/station.antagonist.ron"),
    ));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingAntagonistScript>>,
    mut scripts: ResMut<Assets<AntagonistScript>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingAntagonistScript>();
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
    commands.insert_resource(AntagonistSpawner {
        timer: Timer::from_seconds(gap, TimerMode::Once),
    });
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn generate_antagonist_orders(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<AntagonistSpawner>>,
    shift: Res<Shift>,
    underworld: Res<UnderworldStanding>,
    mut broadcasts: ResMut<PendingBroadcasts>,
    active: Query<&CrewMember, crate::crew::NotResident>,
    chemists: Query<(), With<Chemist>>,
) {
    let (Some(station), Some(script), Some(spawner)) = (station, script, spawner.as_mut()) else {
        return;
    };
    // The same sign that stops a legitimate visitor stops this one — a raid
    // and an antagonist visit are both "new traffic" in the sense the sign
    // controls, unlike an order already in progress.
    if !shift.accepting_orders {
        return;
    }
    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let mut rng = rand::rng();
    // Scaled off the *current* legitimate gap rather than a flat range, so
    // antagonist visits keep pace as the ramp tightens instead of becoming
    // relatively rarer the longer a career runs. The multiplier's own range
    // additionally narrows as underworld standing climbs — see
    // `effective_gap_multiplier`. Also scaled by however many chemists are
    // in the lab, exactly like the legitimate stream it tracks.
    let rules = current_rules(&station.config, &shift, chemists.iter().count());
    let legit_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    let (lo, hi) = effective_gap_multiplier(&script, underworld.0);
    let multiplier = rng.random_range(lo..=hi);
    spawner.timer = Timer::from_seconds(legit_gap * multiplier, TimerMode::Once);

    // Only requests whose `min_standing` floor the current underworld
    // standing already clears — a reliable dealer sees bolder pretexts as
    // their standing grows, without any UI ever naming the mechanism.
    let in_standing: Vec<&AntagonistRequestDef> = script
        .requests
        .iter()
        .filter(|request| request.min_standing <= underworld.0)
        .collect();
    let Some(request) = in_standing.choose(&mut rng).copied() else {
        return;
    };
    let candidates: Vec<_> = station
        .crew
        .iter()
        .filter(|def| def.role == request.role)
        .collect();
    let Some(crew_def) = candidates.choose(&mut rng).copied() else {
        warn!(
            "no crew member with role '{}' to voice an antagonist request",
            request.role
        );
        return;
    };
    let Some(reagent) = db.reagents.id_of(&request.reagent) else {
        warn!(
            "antagonist request names unknown reagent '{}'",
            request.reagent
        );
        return;
    };
    let Some(&amount) = request.amounts.choose(&mut rng) else {
        return;
    };

    // Reuses the ordinary difficulty's patience range rather than a range of
    // its own — a visit that waited noticeably longer or shorter than normal
    // would itself be a statistical tell, which the whole point of this
    // system is to never give the player. `rules` was already computed above
    // for the gap; reused here rather than recomputed.
    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);

    // Same lane-offset trick `generate_orders` uses, so a visit spawned here
    // never overlaps a legitimate one queuing at the same counter.
    let lane = active.iter().count() as f32 * 0.95;
    let crew = spawn_crew_member(&mut commands, crew_def, lane);

    let reagent_name = db.reagents.get(reagent).name.clone();
    let amount = deliverable_amount(&db, reagent, Units::whole(amount as i32));
    commands.entity(crew).insert((
        Order {
            reagent,
            // `IllicitOrder` is what makes this exact, not this field — the
            // two never stack. See `Order::specific`'s own doc comment.
            specific: false,
            amount,
            plea: request.pretext.clone(),
            patience,
            waited: 0.0,
        },
        IllicitOrder,
        Interactable::new(format!(
            "{} — hand over {} {}",
            crew_def.name, amount, reagent_name
        )),
    ));

    let priming_delay = rng.random_range(PRIMING_DELAY_SECONDS.0..=PRIMING_DELAY_SECONDS.1);
    broadcasts.push_delayed(
        priming_delay,
        RadioEntry {
            channel: "COM".to_string(),
            text: request.incident_line.clone(),
            good: false,
        },
    );

    info!(
        "antagonist: {} ({}) wants {}u {}",
        crew_def.name, crew_def.role, amount, reagent_name
    );
}

// ---------------------------------------------------------------------------
// Consequences
// ---------------------------------------------------------------------------

/// Reacts to a successful illicit delivery. Independent of
/// `orders::complete_delivery`'s own reading of the same messages — Bevy
/// gives every system its own cursor into a message queue, so this and
/// `radio::queue_reports` both see every `OrderResolved` in full regardless
/// of what the other already read.
#[allow(clippy::too_many_arguments)]
fn handle_illicit_resolutions(
    script: Option<Res<Script>>,
    db: Res<ChemDb>,
    mut resolved: MessageReader<OrderResolved>,
    mut underworld: ResMut<UnderworldStanding>,
    mut suspicion: ResMut<SecuritySuspicion>,
    mut broadcasts: ResMut<PendingBroadcasts>,
    mut radio: ResMut<RadioLog>,
    mut raid_schedule: Option<ResMut<crate::security::RaidSchedule>>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };

    for report in resolved.read() {
        if !report.kind.is_illicit() || !report.outcome.is_good() {
            continue;
        }
        // An illicit order is always exact-match (see `orders::wanted_for`),
        // so a good outcome always names its reagent — `None` here would be
        // a contradiction in `OrderResolved` itself, not a reason to panic.
        let Some(reagent) = report.reagent else {
            warn!("illicit order resolved successfully but named no reagent");
            continue;
        };
        let reagent_key = &db.reagents.get(reagent).key;

        // The meters move for *any* successful illicit delivery, whether or
        // not this file happens to author a request for that reagent.
        //
        // They used to be inside the lookup below, which was fine while this
        // module was the only thing that ever created an `IllicitOrder`. Since
        // `crate::addiction`, it is not: a returning addict asks for whatever
        // they are hooked on, which may have no `station.antagonist.ron` entry
        // at all — and every sale to them would silently have cost nothing.
        underworld.0 += UNDERWORLD_PER_DELIVERY;
        suspicion.0 += SUSPICION_PER_DELIVERY;

        // Only the *flavour* needs an authored request. No entry simply means
        // no chaos line and no sting for this one, which is exactly right for
        // a private sale to a regular: there is no story to report.
        let Some(request) = script
            .requests
            .iter()
            .find(|request| &request.reagent == reagent_key)
        else {
            continue;
        };

        let mut rng = rand::rng();
        // A sting skips the delayed chaos report entirely — the raid it
        // arms immediately below *is* the consequence — rather than queuing
        // a redundant second one.
        if request.sting_chance > 0.0 && rng.random_bool(request.sting_chance) {
            if let Some(schedule) = raid_schedule.as_mut() {
                if schedule.warning_in.is_none() {
                    schedule.warning_in = Some(STING_WARNING_SECONDS);
                }
            }
            radio.push(RadioEntry {
                channel: "SEC".to_string(),
                text: "Something about that last delivery didn't sit right. Security's already moving.".to_string(),
                good: false,
            });
            continue;
        }

        let delay = rng.random_range(CHAOS_DELAY_SECONDS.0..=CHAOS_DELAY_SECONDS.1);
        broadcasts.push_delayed(
            delay,
            RadioEntry {
                channel: "COM".to_string(),
                text: request.chaos_line.clone(),
                good: false,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::OrderConfig;

    fn antagonist_app() -> App {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let crew: Vec<crate::crew::CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        let config: OrderConfig =
            ron::from_str(include_str!("../../assets/data/station.orders.ron")).unwrap();
        let script: AntagonistScript =
            ron::from_str(include_str!("../../assets/data/station.antagonist.ron")).unwrap();

        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .insert_resource(StationData { crew, config })
            .insert_resource(Script(script))
            .insert_resource(AntagonistSpawner {
                // Effectively due on the first real tick, without depending
                // on a zero-duration timer's edge-case semantics.
                timer: Timer::from_seconds(0.01, TimerMode::Once),
            })
            .insert_resource(Shift {
                accepting_orders: true,
                ..Default::default()
            })
            .init_resource::<Time>()
            .init_resource::<PendingBroadcasts>()
            .init_resource::<UnderworldStanding>()
            .add_systems(Update, generate_antagonist_orders);
        app
    }

    #[test]
    fn a_visit_primes_an_ambient_incident() {
        // The only tell this whole system is allowed to give: an unrelated-
        // looking radio line, queued the moment the visit is created.
        let mut app = antagonist_app();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.1));
        app.update();

        let mut illicit = app.world_mut().query::<&IllicitOrder>();
        assert_eq!(
            illicit.iter(app.world()).count(),
            1,
            "one antagonist visit should have spawned"
        );
        assert_eq!(
            app.world().resource::<PendingBroadcasts>().len(),
            1,
            "the priming incident should be queued alongside it"
        );
    }

    #[test]
    fn the_antagonist_script_parses_and_names_real_departments() {
        let script: AntagonistScript =
            ron::from_str(include_str!("../../assets/data/station.antagonist.ron"))
                .expect("antagonist data should parse");
        assert!(!script.requests.is_empty());
        for request in &script.requests {
            assert!(
                crate::orders::Department::from_role(&request.role).is_some(),
                "'{}' names a role no department recognises",
                request.role
            );
            assert!(!request.pretext.trim().is_empty());
            assert!(!request.incident_line.trim().is_empty());
            assert!(!request.chaos_line.trim().is_empty());
        }
    }

    #[test]
    fn every_antagonist_reagent_is_a_real_illicit_reagent() {
        let script: AntagonistScript =
            ron::from_str(include_str!("../../assets/data/station.antagonist.ron")).unwrap();
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        for request in &script.requests {
            let reagent = data
                .reagents
                .id_of(&request.reagent)
                .unwrap_or_else(|| panic!("'{}' names no real reagent", request.reagent));
            assert!(
                data.reagents
                    .get(reagent)
                    .categories
                    .contains(&chem_sim::Category::Illicit),
                "'{}' is requested by an antagonist but is not Category::Illicit",
                request.reagent
            );
        }
    }

    // -- deepened Traitor: min_standing / gap tightening --------------------

    fn script() -> AntagonistScript {
        ron::from_str(include_str!("../../assets/data/station.antagonist.ron")).unwrap()
    }

    #[test]
    fn the_gap_multiplier_narrows_as_underworld_standing_rises() {
        let script = script();
        let (lo0, hi0) = effective_gap_multiplier(&script, 0);
        let (lo_mid, hi_mid) = effective_gap_multiplier(&script, script.standing_tighten_at / 2);
        let (lo_full, hi_full) = effective_gap_multiplier(&script, script.standing_tighten_at);

        assert_eq!(lo0, script.gap_multiplier.0, "the floor never moves");
        assert_eq!(lo_mid, script.gap_multiplier.0);
        assert_eq!(lo_full, script.gap_multiplier.0);
        assert!(
            hi0 >= hi_mid,
            "the ceiling must not rise as standing climbs"
        );
        assert!(hi_mid >= hi_full);
        assert_eq!(
            hi_full, script.gap_multiplier.0,
            "fully tightened, the range should have collapsed to its floor"
        );
    }

    #[test]
    fn the_multiplier_never_widens_past_standing_tighten_at() {
        let script = script();
        let (_, hi_full) = effective_gap_multiplier(&script, script.standing_tighten_at);
        let (_, hi_past) = effective_gap_multiplier(&script, script.standing_tighten_at * 3);
        assert_eq!(hi_full, hi_past, "the tightening must clamp, not overshoot");
    }

    #[test]
    fn a_low_standing_request_is_reachable_from_the_start() {
        let mut app = antagonist_app();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.1));
        app.update();

        let mut illicit = app.world_mut().query::<&IllicitOrder>();
        assert_eq!(
            illicit.iter(app.world()).count(),
            1,
            "at least one request has no min_standing floor, so a fresh career must still see a visit"
        );
    }

    #[test]
    fn a_high_min_standing_request_is_unreachable_below_it() {
        let script = script();
        let underworld = 0;
        let reachable: Vec<&AntagonistRequestDef> = script
            .requests
            .iter()
            .filter(|r| r.min_standing <= underworld)
            .collect();
        assert!(
            reachable.len() < script.requests.len(),
            "at least one request should be gated behind standing this data authors"
        );
    }

    // -- Spy: sting_chance ----------------------------------------------------

    fn resolution_app() -> App {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .insert_resource(Script(script()))
            .init_resource::<UnderworldStanding>()
            .init_resource::<SecuritySuspicion>()
            .init_resource::<PendingBroadcasts>()
            .init_resource::<crate::radio::RadioLog>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_illicit_resolutions);
        app
    }

    fn resolve_illicit(app: &mut App, reagent: &str) {
        let id = app.world().resource::<ChemDb>().reagent(reagent);
        app.world_mut().write_message(OrderResolved {
            name: "Test".to_string(),
            role: "Security".to_string(),
            reagent: Some(id),
            category: None,
            outcome: crate::orders::Outcome::Success,
            kind: crate::orders::OrderKind::Illicit,
        });
        app.update();
    }

    #[test]
    fn a_non_sting_delivery_only_ever_queues_the_delayed_chaos_line() {
        let mut app = resolution_app();
        resolve_illicit(&mut app, "space_drugs"); // sting_chance defaults to 0.0

        assert_eq!(app.world().resource::<PendingBroadcasts>().len(), 1);
        assert!(app
            .world()
            .resource::<crate::radio::RadioLog>()
            .entries
            .is_empty());
    }

    #[test]
    fn a_sale_of_something_this_file_never_authored_still_costs_you() {
        // The regression guard for `crate::addiction`. A returning addict asks
        // for whatever they are hooked on, which may have no request authored
        // here at all — and the meters used to sit *inside* the script lookup,
        // so every sale to a regular silently cost nothing: no underworld
        // standing, no suspicion, no plot. The flavour (a chaos line, a sting)
        // still needs an authored request; the consequences do not.
        let mut app = resolution_app();
        let unauthored = "hooch";
        assert!(
            !script()
                .requests
                .iter()
                .any(|request| request.reagent == unauthored),
            "this test is pointless if '{unauthored}' gains a request entry — \
             pick another reagent nobody asks for"
        );

        resolve_illicit(&mut app, unauthored);

        assert_eq!(
            app.world().resource::<UnderworldStanding>().0,
            UNDERWORLD_PER_DELIVERY,
            "the underworld notices a sale whether or not this file scripted it"
        );
        assert_eq!(
            app.world().resource::<SecuritySuspicion>().0,
            SUSPICION_PER_DELIVERY,
            "and so does Security"
        );
        assert_eq!(
            app.world().resource::<PendingBroadcasts>().len(),
            0,
            "but with nothing authored there is no story to report"
        );
    }

    #[test]
    fn a_certain_sting_arms_the_raid_schedule_immediately() {
        let mut app = resolution_app();
        app.insert_resource(crate::security::RaidSchedule::default());
        // zombie_powder carries the highest sting_chance in the data; drive
        // it enough times that at least one hit is overwhelmingly likely,
        // rather than depending on a specific seed.
        for _ in 0..200 {
            resolve_illicit(&mut app, "zombie_powder");
            if app
                .world()
                .resource::<crate::security::RaidSchedule>()
                .warning_in
                .is_some()
            {
                break;
            }
        }

        assert!(
            app.world()
                .resource::<crate::security::RaidSchedule>()
                .warning_in
                .is_some(),
            "a sting should eventually arm the raid schedule directly"
        );
    }
}
