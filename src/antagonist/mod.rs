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
use bevy_replicon::prelude::*;
use chem_sim::{ReagentId, Units};
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy};
use crate::crew::{CrewMember, CrewPhase, CrewRoute};
use crate::interaction::{InteractRequested, Interactable};
use crate::machines::{chemist_entity, ReactionsFired};
use crate::net::is_authority;
use crate::orders::{
    deliverable_amount, physical_reagent_inventory, IllicitOrder, Order, OrderResolved, Shift,
    StationData,
};
use crate::player::Chemist;
use crate::produce::{Produce, ProduceCatalog};
use crate::radio::{PendingBroadcasts, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::threat;
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

/// A named resident being busy is temporary, unlike invalid authored data.
/// Retry quickly enough that the already-due offer is preserved without
/// running the full rare-visit gap again.
const RESIDENT_OFFER_RETRY_SECONDS: f32 = 1.0;

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
        app.add_plugins(threat::ScriptPlugin::<AntagonistScript>::new(
            "data/station.antagonist.ron",
            "antagonist.ron",
        ))
        .init_resource::<UnderworldStanding>()
        .init_resource::<SecuritySuspicion>()
        .add_systems(OnEnter(AppState::Playing), arm_spawner)
        .add_systems(
            Update,
            (
                generate_antagonist_orders,
                handle_illicit_resolutions,
                handle_illicit_offer_pickup,
                expire_illicit_offers,
            )
                .chain()
                .run_if(is_authority)
                .run_if(in_state(AppState::Playing))
                .run_if(crate::session::career_session),
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
pub struct UnderworldStanding(i32);

/// The ceiling underworld standing saturates at.
///
/// Above it more dealing buys nothing: `crisis::schedule_crisis` already
/// zeroes the meter on a cure, and an unbounded one turns a single bad shift
/// into a permanent crisis treadmill nothing can drain. Guarded by
/// `every_authored_threshold_sits_under_the_ceiling_of_the_meter_that_feeds_it`.
pub const UNDERWORLD_MAX: i32 = 45;

impl UnderworldStanding {
    pub fn level(self) -> i32 {
        self.0
    }

    /// The one write that is not a nudge: restoring a career off disk.
    ///
    /// Also how a test sets up a starting state — clamped either way, so
    /// neither route can install a value the invariants forbid.
    pub fn restore(&mut self, level: i32) {
        self.0 = level.clamp(0, UNDERWORLD_MAX);
    }
}

/// Moves underworld standing, clamped to `0..=UNDERWORLD_MAX`.
///
/// The tuple field is private for the reason this function exists: four
/// modules used to reach in and `+=`/`=` it directly, and the only invariant
/// it had — never negative — was honoured at one of eleven sites. Matches the
/// `arc::nudge_plot` / `instability::nudge_instability` precedent already set
/// elsewhere in this codebase.
pub fn nudge_underworld(standing: &mut UnderworldStanding, delta: i32) {
    standing.0 = (standing.0 + delta).clamp(0, UNDERWORLD_MAX);
}

/// Drains it outright — `crisis` on a cure, `shift`'s `QuietWord` requisition.
pub fn clear_underworld(standing: &mut UnderworldStanding) {
    standing.0 = 0;
}

/// How close Security is to raiding the lab. Never replicated, never shown,
/// and never persisted — a reloaded save should not silently arm a raid the
/// instant the file opens. Read and reset by `crate::security` (M10c).
#[derive(Resource, Default, Clone, Copy)]
pub struct SecuritySuspicion(i32);

/// The ceiling suspicion saturates at. Same reasoning as [`UNDERWORLD_MAX`].
pub const SUSPICION_MAX: i32 = 60;

impl SecuritySuspicion {
    pub fn level(self) -> i32 {
        self.0
    }

    /// Sets it outright. Not used by the save — suspicion is deliberately
    /// never persisted, so a reloaded career cannot arm a raid the instant
    /// the file opens. This exists for tests staging a starting state, hence
    /// the same test-only annotation `lab::set_bridge_blocked` carries.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn restore(&mut self, level: i32) {
        self.0 = level.clamp(0, SUSPICION_MAX);
    }
}

/// Moves suspicion, clamped to `0..=SUSPICION_MAX`.
pub fn nudge_suspicion(suspicion: &mut SecuritySuspicion, delta: i32) {
    suspicion.0 = (suspicion.0 + delta).clamp(0, SUSPICION_MAX);
}

/// Clears it. Three callers, all meaning "this episode is resolved":
/// `security::schedule_raid` when a warning arms and again when a ward
/// absorbs one, and `shift::apply_requisition`'s `QuietWord`.
pub fn clear_suspicion(suspicion: &mut SecuritySuspicion) {
    suspicion.0 = 0;
}

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
    /// Chance a fired visit is an offer instead of a request — the illicit
    /// shop, fully hidden: no panel, no visible number, ever (see
    /// `IllicitOffer`). Defaulted so a `station.antagonist.ron` written
    /// before offers existed still parses with none authored yet.
    #[serde(default = "default_offer_chance")]
    pub offer_chance: f64,
    #[serde(default)]
    pub offers: Vec<AntagonistOfferDef>,
}

fn default_offer_chance() -> f64 {
    0.35
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

/// A visit that sells to the player instead of asking for something — the
/// illicit shop, authored per department the same way [`AntagonistRequestDef`]
/// already is. Since an illicit visit already draws a real, random, named
/// crew member of the matching `role` from the actual roster, a department
/// with one member (Cargo, Engineering) resolves to that one person for
/// free — no special-casing needed.
///
/// Deliberately carries no `incident_line`/`chaos_line`/`sting_chance`: those
/// exist because *fulfilling someone else's request* causes station-wide
/// fallout worth reporting. A private purchase has no fictional reason to
/// make noise — see [`IllicitOffer`].
#[derive(Clone, Debug, Deserialize)]
pub struct AntagonistOfferDef {
    pub reagent: String,
    pub amounts: Vec<u32>,
    pub role: String,
    /// The ordinary-sounding line, same "no visible tell" convention as
    /// [`AntagonistRequestDef::pretext`] — never anything that reads as
    /// "this NPC has a shop."
    pub pretext: String,
    /// Floor on `UnderworldStanding` before this offer can ever be drawn.
    #[serde(default = "min_standing_default")]
    pub min_standing: i32,
    /// What it drains from `UnderworldStanding` on a successful pickup.
    pub cost: i32,
}

/// A visitor offering to sell something, rather than asking for it — the
/// mirror image of [`IllicitOrder`]. Deliberately **not** an [`Order`] and
/// deliberately never replicated: this is the actual mechanical enforcement
/// of "no visible tell," the same rule `IllicitOrder` itself lives by.
/// Modeled on `rogue_security::RogueOfficer` — a non-`Order`, server-only
/// "visitor with a demand" shape already proven to work cleanly through the
/// ordinary crew/route/interact machinery.
#[derive(Component)]
struct IllicitOffer {
    reagent: ReagentId,
    amount: Units,
    cost: i32,
    patience: f32,
    waited: f32,
}

/// Removes the private offer state and the offer-owned interaction prompt
/// before another controller takes the NPC. The queued world command keeps
/// [`IllicitOffer`] private and makes an unconditional call safe for entities
/// that are not currently offering anything.
pub(crate) fn cancel_illicit_offer(commands: &mut Commands, entity: Entity) {
    commands.queue(move |world: &mut World| {
        if world.get::<IllicitOffer>(entity).is_none() {
            return;
        }
        if let Ok(mut visitor) = world.get_entity_mut(entity) {
            visitor.remove::<IllicitOffer>().remove::<Interactable>();
        }
    });
}

/// This thread's authored script, once loaded.
type Script = threat::Authored<AntagonistScript>;

/// The clock between antagonist visits — its own, much rarer than the
/// legitimate `OrderSpawner`'s.
#[derive(Resource)]
struct AntagonistSpawner {
    timer: Timer,
}

/// See `threat::arm_first_visit` for why this has to re-run on
/// `OnEnter(AppState::Playing)` every session rather than only once at
/// process start.
fn arm_spawner(mut commands: Commands) {
    threat::arm_first_visit(&mut commands, threat::MINOR_FIRST_VISIT, |timer| {
        AntagonistSpawner { timer }
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
    containers: Query<&Container>,
    produce: Query<&Produce>,
    produce_catalog: Option<Res<ProduceCatalog>>,
    mut intake: crate::order_intake::Intake,
    mut residents: crate::crew::AvailableResidents,
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
    let (lo, hi) = effective_gap_multiplier(&script, underworld.level());
    let multiplier = rng.random_range(lo..=hi);
    spawner.timer = Timer::from_seconds(legit_gap * multiplier, TimerMode::Once);

    // Same lane-offset trick `generate_orders` uses, so a visit spawned here
    // never overlaps a legitimate one queuing at the same counter. Shared by
    // both branches below, computed once.
    let lane = active.iter().count() as f32 * 0.95;
    // Reuses the ordinary difficulty's patience range rather than a range of
    // its own — a visit that waited noticeably longer or shorter than normal
    // would itself be a statistical tell, which the whole point of this
    // system is to never give the player. Shared by both branches for the
    // same reason `lane` is.
    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);

    // An offer sells to the player instead of asking for something — rolled
    // first, so a roll that lands on "offer" with nothing eligible yet falls
    // straight through to the ordinary request path below rather than
    // wasting the visit. No reachability filter, unlike a request: the
    // player never has to synthesize what's being handed to them.
    let in_standing_offers: Vec<&AntagonistOfferDef> = script
        .offers
        .iter()
        .filter(|offer| offer.min_standing <= underworld.level())
        .collect();
    if !in_standing_offers.is_empty() && rng.random_bool(script.offer_chance) {
        let offer = *in_standing_offers
            .choose(&mut rng)
            .expect("checked non-empty above");
        if spawn_illicit_offer(
            &mut commands,
            &db,
            &mut rng,
            &station,
            offer,
            lane,
            patience,
            &active,
            &mut residents,
        ) == IllicitOfferSpawn::Retry
        {
            spawner.timer = Timer::from_seconds(RESIDENT_OFFER_RETRY_SECONDS, TimerMode::Once);
        }
        return;
    }

    // Only requests whose `min_standing` floor the current underworld
    // standing already clears — a reliable dealer sees bolder pretexts as
    // their standing grows, without any UI ever naming the mechanism.
    let inventory = physical_reagent_inventory(&containers, &produce, produce_catalog.as_deref());
    let reachable = db.reachable_reagents_with_inventory(inventory);
    let in_standing: Vec<&AntagonistRequestDef> = script
        .requests
        .iter()
        .filter(|request| request.min_standing <= underworld.level())
        .filter(|request| {
            db.reagents
                .id_of(&request.reagent)
                .is_some_and(|reagent| reachable.contains(&reagent))
        })
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

    let Some(context) = intake.admit(
        crate::order_intake::RequestSource::Antagonist,
        &crew_def.name,
        &mut spawner.timer,
        true,
    ) else {
        return;
    };
    let Some(crew) =
        crate::crew::recall_or_spawn_crew_member(&mut commands, &mut residents, crew_def, lane)
    else {
        intake.cancel_admission(&crew_def.name);
        return;
    };

    let reagent_name = db.reagents.get(reagent).name.clone();
    let amount = deliverable_amount(&db, reagent, Units::whole(amount as i32));
    commands.entity(crew).insert((
        crate::order_intake::PendingOrder::new(
            Order {
                reagent,
                specific: true,
                minimum_purity: 0.0,
                amount,
                plea: request.pretext.clone(),
                patience,
                waited: 0.0,
            },
            context,
        ),
        IllicitOrder,
        crate::interaction::Interactable::new("Waiting to speak"),
    ));

    let priming_delay = rng.random_range(PRIMING_DELAY_SECONDS.0..=PRIMING_DELAY_SECONDS.1);
    broadcasts.push_delayed(
        priming_delay,
        RadioEntry::new(
            crate::radio::RadioChannel::Common,
            request.incident_line.clone(),
        )
        .negative(),
    );

    info!(
        "antagonist: {} ({}) wants {}u {}",
        crew_def.name, crew_def.role, amount, reagent_name
    );
}

/// Spawns a visitor who sells, rather than asks — see [`IllicitOffer`]. A
/// plain function rather than inlined into [`generate_antagonist_orders`],
/// mirroring the separation `spawn_scripted_visit` already draws between
/// picking a visit and building one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IllicitOfferSpawn {
    Spawned,
    Retry,
    Invalid,
}

fn spawn_illicit_offer(
    commands: &mut Commands,
    db: &ChemDb,
    rng: &mut impl Rng,
    station: &StationData,
    offer: &AntagonistOfferDef,
    lane: f32,
    patience: f32,
    active: &Query<&CrewMember, crate::crew::NotResident>,
    residents: &mut crate::crew::AvailableResidents,
) -> IllicitOfferSpawn {
    let Some(reagent) = db.reagents.id_of(&offer.reagent) else {
        warn!("antagonist offer names unknown reagent '{}'", offer.reagent);
        return IllicitOfferSpawn::Invalid;
    };
    let Some(&amount) = offer.amounts.choose(rng) else {
        return IllicitOfferSpawn::Invalid;
    };
    let mut candidates: Vec<_> = station
        .crew
        .iter()
        .filter(|def| def.role == offer.role)
        .filter(|def| !active.iter().any(|member| member.name == def.name))
        .collect();
    if candidates.is_empty() {
        warn!(
            "no available crew member with role '{}' to voice an antagonist offer",
            offer.role
        );
        return IllicitOfferSpawn::Retry;
    }
    candidates.shuffle(rng);

    let Some((crew, crew_def)) = candidates.into_iter().find_map(|crew_def| {
        crate::crew::recall_or_spawn_crew_member(commands, residents, crew_def, lane)
            .map(|crew| (crew, crew_def))
    }) else {
        return IllicitOfferSpawn::Retry;
    };

    commands.entity(crew).insert((
        IllicitOffer {
            reagent,
            amount: Units::whole(amount as i32),
            cost: offer.cost,
            patience,
            waited: 0.0,
        },
        // Styled like `rogue_security::RogueOfficer`'s label, not `Order`'s
        // "hand over N X" template — this is the one client-visible
        // artifact, and it must read like an ordinary line, never a shop.
        Interactable::new(format!("{} — {}", crew_def.name, offer.pretext)),
    ));

    info!(
        "antagonist: {} ({}) is offering {}u {} for {}",
        crew_def.name, crew_def.role, amount, offer.reagent, offer.cost
    );
    IllicitOfferSpawn::Spawned
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
    mut observed: Option<ResMut<Messages<crate::social::ObservedAction>>>,
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
        nudge_underworld(&mut underworld, UNDERWORLD_PER_DELIVERY);
        nudge_suspicion(&mut suspicion, SUSPICION_PER_DELIVERY);
        if let Some(observed) = observed.as_deref_mut() {
            observed.write(crate::social::ObservedAction {
                actor: Entity::PLACEHOLDER,
                target: None,
                position: Vec3::new(
                    crate::lab::COUNTER_SPOT.x,
                    crate::lab::COUNTER_TOP,
                    crate::lab::COUNTER_SPOT.z,
                ),
                kind: crate::social::ObservationKind::IllicitDelivery,
            });
        }

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
                schedule.arm_sting(STING_WARNING_SECONDS);
            }
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Security,
                    "Something about that last delivery didn't sit right. Security's already moving.",
                )
                .speaker("Warden Bex")
                .negative()
                .urgent(),
            );
            continue;
        }

        let delay = rng.random_range(CHAOS_DELAY_SECONDS.0..=CHAOS_DELAY_SECONDS.1);
        broadcasts.push_delayed(
            delay,
            RadioEntry::new(
                crate::radio::RadioChannel::Common,
                request.chaos_line.clone(),
            )
            .negative(),
        );
    }
}

// ---------------------------------------------------------------------------
// Offers — the illicit shop, fully hidden
// ---------------------------------------------------------------------------

/// The mirror image of `orders::handle_delivery`: the NPC gives, instead of
/// receiving. An independent reader of `InteractRequested`, the same
/// message `handle_delivery` reads — Bevy gives every system its own
/// cursor, the same pattern `rogue_security::handle_deterrent_use` already
/// proves for a second independent reader of a different message.
///
/// What an unaffordable interaction feels like: mechanically identical to
/// fumbling any other delivery already is in this game — wrong container,
/// empty hand, nothing happens, try again or walk away. There is no code
/// path here that can distinguish "you can't afford this" from "that wasn't
/// the right thing," because they are literally the same shape of no-op.
#[allow(clippy::too_many_arguments)]
fn handle_illicit_offer_pickup(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    mut offers: Query<(&IllicitOffer, &mut CrewRoute)>,
    mut containers: Query<(Entity, &mut Container, &HeldBy)>,
    mut underworld: ResMut<UnderworldStanding>,
    mut fired: MessageWriter<ReactionsFired>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok((offer, mut route)) = offers.get_mut(request.target) else {
            continue;
        };
        // Rechecked here, not just at spawn time: `handle_crisis_resolutions`
        // can zero `UnderworldStanding` outright while an offer visitor is
        // still standing there waiting.
        if underworld.level() < offer.cost {
            continue;
        }
        let Some((container_entity, mut container, _)) = containers
            .iter_mut()
            .find(|(_, _, holder)| holder.0 == player)
        else {
            continue;
        };
        let reagent = offer.reagent;
        let amount = offer.amount;
        let cost = offer.cost;
        let ph = db.reagents.get(reagent).ph;
        let (overflow, report) = container.mutate(&db, |solution| {
            solution.add_profiled(reagent, amount, 1.0, ph)
        });
        if overflow >= amount {
            // A full container accepted nothing — the same silent no-op
            // fumbling any other delivery already produces.
            continue;
        }
        if let Some(message) = ReactionsFired::from_report(container_entity, &report) {
            fired.write(message);
        }
        nudge_underworld(&mut underworld, -cost);
        commands
            .entity(request.target)
            .remove::<IllicitOffer>()
            .remove::<Interactable>();
        route.leave();
    }
}

/// Ticks an offer's patience down and lets it walk away, ignored — true
/// silence in both directions: no radio line, no standing consequence,
/// either way. Deliberately asymmetric with `IllicitOrder`'s own expiry
/// (`orders::expire_orders`, which *does* cost department standing, because
/// that visit has to imitate an ordinary order's consequences to stay
/// statistically invisible among them) — an offer was never wired into that
/// bookkeeping in the first place, so ignoring one has nothing to fall
/// through to.
fn expire_illicit_offers(
    time: Res<Time>,
    mut commands: Commands,
    mut offers: Query<(Entity, &mut IllicitOffer, &mut CrewRoute)>,
) {
    let dt = time.delta_secs();
    for (entity, mut offer, mut route) in &mut offers {
        if route.phase != CrewPhase::Waiting {
            continue;
        }
        offer.waited += dt;
        if offer.waited < offer.patience {
            continue;
        }
        commands
            .entity(entity)
            .remove::<IllicitOffer>()
            .remove::<Interactable>();
        route.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    // -- meter invariants ----------------------------------------------------

    #[test]
    fn nudging_a_meter_below_zero_floors_it_rather_than_going_negative() {
        let mut standing = UnderworldStanding::default();
        nudge_underworld(&mut standing, 3);
        nudge_underworld(&mut standing, -10);
        assert_eq!(standing.level(), 0, "standing must never go negative");

        let mut suspicion = SecuritySuspicion::default();
        nudge_suspicion(&mut suspicion, -5);
        assert_eq!(suspicion.level(), 0);
    }

    #[test]
    fn no_amount_of_dealing_pushes_a_meter_past_its_ceiling() {
        let mut standing = UnderworldStanding::default();
        for _ in 0..500 {
            nudge_underworld(&mut standing, UNDERWORLD_PER_DELIVERY);
        }
        assert_eq!(standing.level(), UNDERWORLD_MAX);

        let mut suspicion = SecuritySuspicion::default();
        for _ in 0..500 {
            nudge_suspicion(&mut suspicion, SUSPICION_PER_DELIVERY);
        }
        assert_eq!(suspicion.level(), SUSPICION_MAX);
    }

    #[test]
    fn every_authored_threshold_sits_under_the_ceiling_of_the_meter_that_feeds_it() {
        // The ceilings added with the mutator API are a real behaviour change,
        // not a no-op. This is what stops content ever authoring a threshold
        // the meter it watches can no longer reach — which would silently
        // disable a whole thread rather than failing loudly.
        let crisis: crate::crisis::CrisisScript =
            ron::from_str(include_str!("../../assets/data/station.crisis.ron")).unwrap();
        assert!(
            crisis.underworld_threshold < UNDERWORLD_MAX,
            "station.crisis.ron asks for {} underworld standing, but the meter              saturates at {UNDERWORLD_MAX} — the crisis could never fire",
            crisis.underworld_threshold,
        );

        let security: crate::security::SecurityScript =
            ron::from_str(include_str!("../../assets/data/station.security.ron")).unwrap();
        assert!(
            security.threshold < SUSPICION_MAX,
            "station.security.ron asks for {} suspicion, but the meter saturates              at {SUSPICION_MAX} — the raid could never fire",
            security.threshold,
        );
    }

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
            .insert_resource(threat::Authored(script))
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
            let reagent = data.reagents.get(reagent);
            assert!(
                reagent.categories.contains(&chem_sim::Category::Illicit) || reagent.controlled,
                "'{}' is requested by an antagonist but is neither illicit nor controlled",
                request.reagent
            );
        }
    }

    #[test]
    fn external_controlled_requests_wait_for_their_physical_sources() {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();

        let without_delivery = data.reachable_reagents_with_inventory([]);
        assert!(without_delivery.contains(&data.reagent("blastoff")));
        assert!(!without_delivery.contains(&data.reagent("kronkaine")));
        assert!(!without_delivery.contains(&data.reagent("saturn_x")));
        assert!(!without_delivery.contains(&data.reagent("amanitin")));
        assert!(!without_delivery.contains(&data.reagent("curare")));
        assert!(!without_delivery.contains(&data.reagent("tirizene")));
        assert!(!without_delivery.contains(&data.reagent("tiring_solution")));
        assert!(!without_delivery.contains(&data.reagent("tetrodotoxin")));
        assert!(!without_delivery.contains(&data.reagent("pancuronium")));
        assert!(!without_delivery.contains(&data.reagent("amatoxin")));
        assert!(!without_delivery.contains(&data.reagent("coniine")));
        assert!(!without_delivery.contains(&data.reagent("histamine")));
        assert!(!without_delivery.contains(&data.reagent("bungotoxin")));
        assert!(!without_delivery.contains(&data.reagent("venom")));
        assert!(without_delivery.contains(&data.reagent("initropidril")));
        assert!(without_delivery.contains(&data.reagent("sodium_thiopental")));
        assert!(without_delivery.contains(&data.reagent("polonium")));
        assert!(without_delivery.contains(&data.reagent("fentanyl")));
        assert!(without_delivery.contains(&data.reagent("lead_acetate")));
        assert!(without_delivery.contains(&data.reagent("itching_powder")));
        assert!(without_delivery.contains(&data.reagent("rotatium")));

        let with_botany = data.reachable_reagents_with_inventory([
            data.reagent("kronkus_extract"),
            data.reagent("tea"),
            data.reagent("plant_fibre"),
        ]);
        assert!(with_botany.contains(&data.reagent("kronkaine")));
        assert!(with_botany.contains(&data.reagent("saturn_x")));

        let with_toxic_botany = data.reachable_reagents_with_inventory([
            data.reagent("amanitin"),
            data.reagent("curare"),
            data.reagent("tirizene"),
            data.reagent("tetrodotoxin"),
            data.reagent("amatoxin"),
            data.reagent("coniine"),
            data.reagent("histamine"),
            data.reagent("bungotoxin"),
            data.reagent("venom"),
        ]);
        assert!(with_toxic_botany.contains(&data.reagent("amanitin")));
        assert!(with_toxic_botany.contains(&data.reagent("curare")));
        assert!(with_toxic_botany.contains(&data.reagent("tirizene")));
        assert!(with_toxic_botany.contains(&data.reagent("tiring_solution")));
        assert!(with_toxic_botany.contains(&data.reagent("tetrodotoxin")));
        assert!(with_toxic_botany.contains(&data.reagent("pancuronium")));
        assert!(with_toxic_botany.contains(&data.reagent("amatoxin")));
        assert!(with_toxic_botany.contains(&data.reagent("coniine")));
        assert!(with_toxic_botany.contains(&data.reagent("histamine")));
        assert!(with_toxic_botany.contains(&data.reagent("bungotoxin")));
        assert!(with_toxic_botany.contains(&data.reagent("venom")));
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
        app.insert_resource(crate::arc::Campaign::new(
            crate::arc::AntagId::Spy,
            crate::arc::Mode::default(),
            1,
        ));
        app.world_mut().resource_mut::<Script>().0.offer_chance = 0.0;
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
        let pending = app
            .world_mut()
            .query::<&crate::order_intake::PendingOrder>()
            .single(app.world())
            .unwrap();
        assert_eq!(
            pending.context.greeting,
            crate::order_intake::GreetingKind::Campaign
        );
        assert_eq!(
            pending.context.campaign,
            Some(app.world().resource::<crate::arc::Campaign>().id)
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
            .insert_resource(threat::Authored(script()))
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
            quality: None,
            development: false,
            campaign: None,
            counter_step: None,
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
            app.world().resource::<UnderworldStanding>().level(),
            UNDERWORLD_PER_DELIVERY,
            "the underworld notices a sale whether or not this file scripted it"
        );
        assert_eq!(
            app.world().resource::<SecuritySuspicion>().level(),
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
                .clock
                .is_armed()
            {
                break;
            }
        }

        assert!(
            app.world()
                .resource::<crate::security::RaidSchedule>()
                .clock
                .is_armed(),
            "a sting should eventually arm the raid schedule directly"
        );
    }

    // -- offers: the illicit shop, fully hidden ------------------------------

    fn forced_offer_app(underworld: i32) -> App {
        let mut app = antagonist_app();
        app.world_mut()
            .resource_mut::<UnderworldStanding>()
            .restore(underworld);
        // Deterministic: guarantees the offer branch is taken whenever at
        // least one offer clears its own `min_standing`, rather than
        // depending on a probabilistic roll.
        app.world_mut().resource_mut::<Script>().0.offer_chance = 1.0;
        app
    }

    #[test]
    fn an_offer_visit_never_carries_an_order_component() {
        let mut app = forced_offer_app(10);
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.1));
        app.update();

        let mut offers = app.world_mut().query::<&IllicitOffer>();
        assert_eq!(
            offers.iter(app.world()).count(),
            1,
            "one offer visit should have spawned"
        );
        let mut orders = app.world_mut().query::<&Order>();
        assert_eq!(
            orders.iter(app.world()).count(),
            0,
            "an offer must never carry an Order — that is the whole \
             'no visible tell' guarantee"
        );
    }

    #[test]
    fn canceling_an_offer_removes_its_private_payload_and_prompt() {
        let mut app = offer_pickup_app();
        let offer = waiting_offer(&mut app, "space_drugs", 5, 4);
        app.world_mut()
            .entity_mut(offer)
            .insert(Interactable::new("Private offer"));

        app.world_mut()
            .run_system_once(move |mut commands: Commands| {
                cancel_illicit_offer(&mut commands, offer);
                cancel_illicit_offer(&mut commands, offer);
            })
            .unwrap();

        assert!(app.world().get::<IllicitOffer>(offer).is_none());
        assert!(app.world().get::<Interactable>(offer).is_none());
    }

    #[test]
    fn a_busy_cargo_roster_keeps_the_offer_due_without_cloning_a_resident() {
        let mut app = forced_offer_app(10);
        app.world_mut()
            .resource_mut::<Script>()
            .0
            .offers
            .retain(|offer| offer.role == "Cargo");
        let sato = app
            .world_mut()
            .spawn((
                crate::crew::CrewMember {
                    name: "Miner Sato".to_string(),
                    role: "Cargo".to_string(),
                },
                crate::body::Body::default(),
                crate::body::Bloodstream::default(),
                crate::crew::StationResident,
                crate::utility_ai::UtilityControlBundle::new(crate::utility_ai::UtilityAgent::new(
                    11, 0,
                )),
            ))
            .id();
        app.world_mut().spawn((
            crate::crew::CrewMember {
                name: "Quartermaster Rhee".to_string(),
                role: "Cargo".to_string(),
            },
            crate::body::Body::default(),
            crate::body::Bloodstream::default(),
            crate::crew::StationResident,
            crate::utility_ai::UtilityControlBundle::new(crate::utility_ai::UtilityAgent::new(
                12, 0,
            )),
        ));

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.1));
        app.update();

        assert_eq!(
            app.world_mut()
                .query::<&IllicitOffer>()
                .iter(app.world())
                .count(),
            0,
            "an offer cannot take over either busy Cargo resident"
        );
        for name in ["Miner Sato", "Quartermaster Rhee"] {
            assert_eq!(
                app.world_mut()
                    .query::<&crate::crew::CrewMember>()
                    .iter(app.world())
                    .filter(|member| member.name == name)
                    .count(),
                1,
                "the offer path must not clone {name}"
            );
        }

        app.world_mut()
            .entity_mut(sato)
            .insert((crate::crew::Ambient::new(30.0), CrewRoute::arrival(0.0)));
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(
                RESIDENT_OFFER_RETRY_SECONDS + 0.1,
            ));
        app.update();

        assert!(
            app.world().get::<IllicitOffer>(sato).is_some(),
            "the already-due offer should use the same body once Sato is free"
        );
        assert_eq!(
            app.world_mut()
                .query::<&crate::crew::CrewMember>()
                .iter(app.world())
                .filter(|member| member.name == "Miner Sato")
                .count(),
            1
        );
    }

    #[test]
    fn a_high_min_standing_offer_is_unreachable_below_it() {
        let script = script();
        let underworld = 0;
        let reachable: Vec<&AntagonistOfferDef> = script
            .offers
            .iter()
            .filter(|offer| offer.min_standing <= underworld)
            .collect();
        assert!(
            reachable.len() < script.offers.len(),
            "at least one offer should be gated behind standing this data authors"
        );
    }

    #[test]
    fn every_offer_names_a_real_illicit_reagent_and_recognised_department() {
        let script = script();
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        assert!(!script.offers.is_empty(), "no offers authored");
        for offer in &script.offers {
            let reagent = data
                .reagents
                .id_of(&offer.reagent)
                .unwrap_or_else(|| panic!("'{}' names no real reagent", offer.reagent));
            let reagent = data.reagents.get(reagent);
            assert!(
                reagent.categories.contains(&chem_sim::Category::Illicit) || reagent.controlled,
                "'{}' is offered but is neither illicit nor controlled",
                offer.reagent
            );
            // The no-NPC-source invariant, enforced against authored data
            // rather than trusted. An NPC-generated offer is the station
            // spawning a reagent for the player; a player-only plot material
            // must never arrive that way, or its supply stops being the
            // player's choice. See `ReagentDef::player_only`.
            assert!(
                !reagent.player_only,
                "'{}' is player-only and must never appear in an NPC-generated offer",
                offer.reagent
            );
            assert!(
                crate::orders::Department::from_role(&offer.role).is_some(),
                "'{}' names a role no department recognises",
                offer.role
            );
            assert!(offer.cost > 0, "offer '{}' costs nothing", offer.reagent);
            assert!(!offer.pretext.trim().is_empty());
        }
    }

    use crate::containers::ContainerKind;

    fn offer_pickup_app() -> App {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .init_resource::<UnderworldStanding>()
            .add_message::<FromClient<InteractRequested>>()
            .add_message::<ReactionsFired>()
            .add_systems(Update, handle_illicit_offer_pickup);
        app
    }

    fn waiting_offer(app: &mut App, reagent: &str, amount: u32, cost: i32) -> Entity {
        let reagent = app.world().resource::<ChemDb>().reagent(reagent);
        let mut route = CrewRoute::arrival(0.0);
        route.phase = CrewPhase::Waiting;
        app.world_mut()
            .spawn((
                IllicitOffer {
                    reagent,
                    amount: Units::whole(amount as i32),
                    cost,
                    patience: 60.0,
                    waited: 0.0,
                },
                route,
            ))
            .id()
    }

    fn player_holding(app: &mut App, kind: ContainerKind) -> Entity {
        let player = app
            .world_mut()
            .spawn(crate::player::Chemist {
                client: ClientId::Server,
            })
            .id();
        app.world_mut()
            .spawn((Container::new(kind), HeldBy(player)));
        player
    }

    fn pick_up(app: &mut App, target: Entity) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: InteractRequested { target },
        });
        app.update();
    }

    #[test]
    fn a_successful_pickup_drains_underworld_standing_by_the_offers_cost() {
        let mut app = offer_pickup_app();
        app.world_mut()
            .resource_mut::<UnderworldStanding>()
            .restore(10);
        let offer = waiting_offer(&mut app, "space_drugs", 5, 4);
        player_holding(&mut app, ContainerKind::Beaker);

        pick_up(&mut app, offer);

        assert_eq!(app.world().resource::<UnderworldStanding>().level(), 6);
        assert!(
            app.world().get::<IllicitOffer>(offer).is_none(),
            "a successful pickup should remove the offer"
        );
    }

    #[test]
    fn an_unaffordable_pickup_is_a_true_no_op() {
        let mut app = offer_pickup_app();
        app.world_mut()
            .resource_mut::<UnderworldStanding>()
            .restore(2);
        let offer = waiting_offer(&mut app, "space_drugs", 5, 4);
        player_holding(&mut app, ContainerKind::Beaker);

        pick_up(&mut app, offer);

        assert_eq!(
            app.world().resource::<UnderworldStanding>().level(),
            2,
            "an unaffordable pickup must not spend anything"
        );
        assert!(
            app.world().get::<IllicitOffer>(offer).is_some(),
            "an unaffordable pickup must leave the offer standing"
        );
    }

    #[test]
    fn a_full_container_accepts_nothing_and_spends_nothing() {
        let mut app = offer_pickup_app();
        app.world_mut()
            .resource_mut::<UnderworldStanding>()
            .restore(10);
        let offer = waiting_offer(&mut app, "space_drugs", 5, 4);
        let player = player_holding(&mut app, ContainerKind::Beaker);
        let filler = app
            .world()
            .resource::<ChemDb>()
            .reagents
            .id_of("water")
            .expect("water should exist");
        {
            let mut containers = app.world_mut().query::<(&mut Container, &HeldBy)>();
            for (mut container, holder) in containers.iter_mut(app.world_mut()) {
                if holder.0 == player {
                    let capacity = container.solution.max_volume();
                    let _ = container.solution.add_profiled(filler, capacity, 1.0, 7.0);
                }
            }
        }

        pick_up(&mut app, offer);

        assert_eq!(
            app.world().resource::<UnderworldStanding>().level(),
            10,
            "a full container should accept nothing and spend nothing"
        );
        assert!(app.world().get::<IllicitOffer>(offer).is_some());
    }

    fn expire_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .add_systems(Update, expire_illicit_offers);
        app
    }

    #[test]
    fn an_ignored_offer_costs_nothing() {
        let mut app = expire_app();
        let mut route = CrewRoute::arrival(0.0);
        route.phase = CrewPhase::Waiting;
        let entity = app
            .world_mut()
            .spawn((
                IllicitOffer {
                    reagent: chem_sim::ChemData::from_ron(
                        include_str!("../../assets/data/chem.reagents.ron"),
                        include_str!("../../assets/data/chem.reactions.ron"),
                    )
                    .unwrap()
                    .reagent("space_drugs"),
                    amount: Units::whole(5),
                    cost: 4,
                    patience: 1.0,
                    waited: 0.0,
                },
                route,
            ))
            .id();

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(2.0));
        app.update();

        assert!(
            app.world().get::<IllicitOffer>(entity).is_none(),
            "expiry should remove the offer"
        );
    }
}
