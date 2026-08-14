//! The campaign arc: one main antagonist per save, and how a save ends.
//!
//! Until now every threat thread in this game ran forever. `antagonist` deals
//! contraband indefinitely, `security` raids indefinitely, `crisis` fires
//! indefinitely; `obsessed` and `cult` clamp at their last authored beat and
//! then repeat it. Nothing could ever be *finished*.
//!
//! This is the spine. Each save draws one main antagonist and tracks their
//! [`Campaign::plot`] — how close they are to their goal — on a 0..=100 meter
//! that the player is never shown directly. The save resolves exactly one of
//! three ways:
//!
//! - the plot reaches 100 and they get what they wanted ([`ArcOutcome::PlotSucceeded`]),
//! - they come for the lab and the chemist puts them down ([`ArcOutcome::StoppedDirectly`], see `crate::showdown`),
//! - the departments stop them on the strength of what the chemist supplied
//!   ([`ArcOutcome::StoppedByDepartments`], the counter-track below).
//!
//! **Which antagonist a save drew is hidden.** [`Reveal`] climbs from `Hidden`
//! through `Suspected` to `Named` as the plot advances, airing one authored
//! clue line per tier. That deliberately matches the philosophy
//! `crate::antagonist` already states in its own module doc — the tell is
//! ambient radio traffic, never a label on a screen.
//!
//! **The meter is symmetric; only the reading of it flips.** In
//! [`Mode::Antagonist`] — unlocked for an antagonist the player has already
//! thwarted — the exact same numbers move in the exact same directions, and
//! [`Campaign::player_won`] is the only thing that cares which side you are
//! on. That is why an antagonist run needs almost no mechanics of its own.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use chem_sim::Units;

use crate::chem_data::ChemDb;
use crate::crew::{spawn_crew_member, CrewDef, CrewMember};
use crate::interaction::Interactable;
use crate::net::is_authority;
use crate::orders::{
    deliverable_amount, reference_category, CounterOrder, Order, OrderKind, OrderResolved, Shift,
    StationData,
};
use crate::radio::{announce_request, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::AppState;

/// The top of the plot meter: the antagonist has what they wanted.
pub const PLOT_MAX: i32 = 100;

/// How long a minute is, for [`advance_plot`]'s drift.
const DRIFT_INTERVAL_SECONDS: f32 = 60.0;

pub struct ArcPlugin;

impl Plugin for ArcPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<ArcScript>::new(&["arc.ron"]))
            .init_resource::<ThwartedAntags>()
            .init_resource::<DriftClock>()
            .add_server_message::<CampaignSync>(Channel::Ordered)
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (
                    (
                        promote_script,
                        assign_campaign,
                        advance_plot,
                        update_reveal,
                        // After the reveal, so the frame the track opens is
                        // the frame its clock arms.
                        generate_counter_orders,
                        resolve_campaign,
                        broadcast_campaign,
                        // After the broadcast, so a client that joined this
                        // frame is not also caught by the change-detection
                        // send and told twice.
                        sync_campaign_to_new_clients,
                    )
                        .chain()
                        .run_if(is_authority),
                    apply_campaign.run_if(in_state(ClientState::Connected)),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Which main antagonist a save drew.
///
/// A named enum rather than an index into [`ArcScript::antagonists`]: the
/// index would silently re-point every existing save the first time the data
/// file is reordered, and this value is written to `progress.ron`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AntagId {
    Cult,
    Spy,
    Changeling,
    Ai,
    Blob,
}

impl AntagId {
    pub const ALL: [AntagId; 5] = [
        AntagId::Cult,
        AntagId::Spy,
        AntagId::Changeling,
        AntagId::Ai,
        AntagId::Blob,
    ];

    /// The stable string written to the cross-save unlock file.
    pub fn key(self) -> &'static str {
        match self {
            AntagId::Cult => "Cult",
            AntagId::Spy => "Spy",
            AntagId::Changeling => "Changeling",
            AntagId::Ai => "Ai",
            AntagId::Blob => "Blob",
        }
    }

    pub fn from_key(key: &str) -> Option<AntagId> {
        AntagId::ALL.into_iter().find(|id| id.key() == key)
    }

    /// A short human name, for the menu.
    ///
    /// Deliberately not [`AntagDef::display`]: the menu runs before any asset
    /// has loaded, so it cannot read the script, and the load list still needs
    /// to be able to say which antagonist a finished save beat.
    pub fn label(self) -> &'static str {
        match self {
            AntagId::Cult => "the Cult",
            AntagId::Spy => "the Syndicate",
            AntagId::Changeling => "the Changeling",
            AntagId::Ai => "the AI",
            AntagId::Blob => "the Blob",
        }
    }
}

/// How much the station has worked out about who is behind it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
pub enum Reveal {
    /// Nobody has connected anything to anything.
    #[default]
    Hidden,
    /// The chatter has narrowed it to a kind of threat, not a name.
    Suspected,
    /// Named outright — it is on the standing board now.
    Named,
}

/// How an arc finished. Deliberately *not* "won"/"lost": which of these counts
/// as a win depends on which side the save is being played from — see
/// [`Campaign::player_won`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ArcOutcome {
    /// The plot meter reached [`PLOT_MAX`].
    PlotSucceeded,
    /// The showdown was won in the lab.
    StoppedDirectly,
    /// Every counter-track step was delivered.
    StoppedByDepartments,
}

/// Which side this save is played from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Chemist,
    /// Unlocked per-antagonist once it has been thwarted in a chemist run.
    Antagonist,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// This save's campaign.
///
/// A resource, so it does not replicate on its own — [`broadcast_campaign`]
/// and [`sync_campaign_to_new_clients`] carry it, the same shape
/// `orders::ShiftSync` and `knowledge::KnowledgeSync` already use, plus a
/// targeted send on connect that neither of those has and this needs: a guest
/// joining mid-arc must not spend up to a minute believing there is no
/// campaign at all.
#[derive(Resource, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Campaign {
    pub antag: AntagId,
    /// 0..=[`PLOT_MAX`]. Never shown as a number; its effects are the clue
    /// lines, the counter-track opening, and the showdown arming.
    pub plot: i32,
    /// One flag per [`AntagDef::counter_steps`] entry, in order.
    pub countered: Vec<bool>,
    pub reveal: Reveal,
    /// `Some` once the arc has resolved. The save stays playable afterwards —
    /// this only stops the arc systems and labels the save in the load list.
    pub outcome: Option<ArcOutcome>,
    pub mode: Mode,
}

impl Campaign {
    pub fn new(antag: AntagId, mode: Mode, counter_steps: usize) -> Campaign {
        Campaign {
            antag,
            plot: 0,
            countered: vec![false; counter_steps],
            reveal: Reveal::Hidden,
            outcome: None,
            mode,
        }
    }

    /// Whether the finished arc was a win *for the player*.
    ///
    /// The one place the two modes actually diverge. Everything else — the
    /// meter, the counter-track, the showdown — behaves identically whichever
    /// side is being played; only what the ending means changes.
    pub fn player_won(&self) -> Option<bool> {
        let outcome = self.outcome?;
        Some(match self.mode {
            Mode::Chemist => outcome != ArcOutcome::PlotSucceeded,
            Mode::Antagonist => outcome == ArcOutcome::PlotSucceeded,
        })
    }

    /// Which antagonist, and how it went, *if the menu is allowed to say*.
    ///
    /// `None` while the antagonist is still [`Reveal::Hidden`] and the arc is
    /// unresolved. The load list is the one surface that could hand the player
    /// the answer before they have even loaded the save, so the rule lives
    /// here — on the campaign itself — rather than being left to the menu to
    /// remember.
    pub fn menu_standing(&self) -> Option<(AntagId, Option<bool>)> {
        if self.reveal == Reveal::Hidden && self.outcome.is_none() {
            return None;
        }
        Some((self.antag, self.player_won()))
    }

    /// Whether the counter-track has been completed in full.
    fn fully_countered(&self) -> bool {
        !self.countered.is_empty() && self.countered.iter().all(|done| *done)
    }
}

/// Antagonists already thwarted in some earlier save on this machine.
///
/// Filled from `saves/campaign.ron` at load. Empty is the correct first-run
/// reading, and also what the headless tests get.
#[derive(Resource, Default, Clone, Debug)]
pub struct ThwartedAntags(pub Vec<AntagId>);

/// What the menu decided before the lab was built.
///
/// Absent means "roll a hidden chemist run", which is what the command-line
/// launch flags and every test get.
#[derive(Resource, Clone, Copy, Debug)]
pub struct CampaignChoice {
    pub mode: Mode,
    /// Forced antagonist. Always `Some` for an antagonist run — you pick who
    /// you are working for — and `None` for an ordinary chemist run, where the
    /// whole point is not being told.
    pub antag: Option<AntagId>,
}

/// Accumulates sub-point drift so [`Campaign`] is only written when the
/// integer meter actually moves. Change detection drives the network send, so
/// a resource touched every frame would broadcast the campaign every frame.
#[derive(Resource, Default)]
pub struct DriftClock(f32);

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// `assets/data/station.arc.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct ArcScript {
    /// Plot points the antagonist gains per minute just by existing. They are
    /// working while you work; doing nothing is not neutral.
    pub drift_per_minute: i32,
    /// Gained per successful illicit delivery — you are supplying whoever is
    /// out there, whether or not you know who that is.
    pub plot_per_aid: i32,
    /// Gained when a department minor's shenanigan goes unaddressed.
    pub plot_per_ignored_shenanigan: i32,
    /// Applied per delivered counter-track step. Negative in the data; kept
    /// signed rather than subtracted at the call site so the direction is a
    /// content decision.
    pub plot_per_counter_step: i32,
    pub suspected_at: i32,
    pub named_at: i32,
    /// Where `crate::showdown` arms.
    pub showdown_at: i32,
    /// Multiplied onto the *current* legitimate order gap to space
    /// counter-track requests, the same way every other thread's own
    /// `gap_multiplier` works — so the counter-track keeps pace with the
    /// career ramp instead of becoming relatively rarer the longer you play.
    pub counter_gap_multiplier: (f32, f32),
    /// Numbers `crate::showdown` runs on. One shared block rather than
    /// per-antagonist copies: what differs between them is the *form* their
    /// confrontation takes, not how hard it hits.
    pub showdown: ShowdownTuning,
    pub antagonists: Vec<AntagDef>,
}

/// How the confrontation in `crate::showdown` behaves once it arms.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ShowdownTuning {
    /// How long the chemist has before it is over.
    pub deadline_seconds: f32,
    /// Siege: seconds between vents.
    pub gas_every_seconds: f32,
    /// Siege: units of the harmful reagent each vent carries.
    pub gas_units: u32,
    /// Siege: how much counter-agent has to land to stop it.
    pub cure_units_needed: u32,
    /// Assailant: seconds between hits.
    pub hit_every_seconds: f32,
    /// Assailant: brute damage per hit. Well under the 100 that collapses a
    /// chemist, so the fight is a clock rather than a coin flip.
    pub hit_brute: i32,
    /// Assailant: metres per second. Below the chemist's own walk speed on
    /// purpose — you can back away from it, but not indefinitely, and not
    /// while also standing at a dispenser.
    pub speed: f32,
}

impl ArcScript {
    pub fn antagonist(&self, id: AntagId) -> Option<&AntagDef> {
        self.antagonists.iter().find(|def| def.id == id)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct AntagDef {
    pub id: AntagId,
    /// What the station calls them once [`Reveal::Named`] is reached.
    pub display: String,
    pub showdown: ShowdownForm,
    /// Aired the moment the confrontation arms — immediately, not delayed.
    pub showdown_line: String,
    /// Ordered. Delivering all of them is [`ArcOutcome::StoppedByDepartments`].
    pub counter_steps: Vec<CounterStepDef>,
    /// Aired on reaching [`Reveal::Suspected`] — narrows it to a kind of
    /// threat without naming one.
    pub suspected_line: String,
    /// Aired on reaching [`Reveal::Named`].
    pub named_line: String,
    /// Aired when the plot reaches [`PLOT_MAX`].
    pub victory_line: String,
    /// Aired when they are stopped, either way.
    pub thwarted_line: String,
}

/// Which shape this antagonist's direct confrontation takes, and the content
/// that shape needs. See `crate::showdown`.
///
/// Each variant carries its own data rather than the two sharing one flat set
/// of fields: a siege has a gas and a cure, an assailant has a name and a
/// colour, and neither has any use for the other's.
#[derive(Clone, Debug, Deserialize)]
pub enum ShowdownForm {
    /// The lab itself turns on you. There is nothing to punch.
    Siege {
        /// Vented into the room on a beat, through the ordinary smoke pipeline.
        gas_reagent: String,
        /// The *reference* reagent for what stops it — any member of its
        /// category counts, the same leniency an ordinary order has.
        cure_reagent: String,
    },
    /// A body walks in and comes for you.
    Assailant {
        /// Kept off `station.crew.ron` for the same reason every other
        /// synthetic visitor is: so the legitimate roster can never pick it.
        name: String,
        color: [f32; 3],
    },
}

#[derive(Clone, Debug, Deserialize)]
pub struct CounterStepDef {
    /// A crew role off `station.crew.ron`, exactly like every other visit
    /// def — the department asking is the department that voices it.
    pub role: String,
    /// The *reference* reagent. The spawned order is deliberately not
    /// `specific`, so `orders::reference_category` makes any member of this
    /// reagent's first category acceptable — the same leniency an ordinary
    /// request already has.
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
    pub delivered_line: String,
}

#[derive(Resource)]
struct PendingArcScript(Handle<ArcScript>);

#[derive(Resource, Deref)]
pub struct Script(pub ArcScript);

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingArcScript(assets.load("data/station.arc.ron")));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingArcScript>>,
    mut scripts: ResMut<Assets<ArcScript>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingArcScript>();
}

// ---------------------------------------------------------------------------
// Assignment
// ---------------------------------------------------------------------------

/// Which antagonist a fresh save should draw.
///
/// Prefers ones this machine has never beaten, so a new player meets each of
/// them before seeing a repeat, and falls back to the full roster once they
/// have all been thwarted rather than running out of game. Pure, so the
/// preference is testable without an `App` or a save file.
pub fn pick_antag(available: &[AntagId], thwarted: &[AntagId], rng: &mut impl Rng) -> Option<AntagId> {
    let unbeaten: Vec<AntagId> = available
        .iter()
        .copied()
        .filter(|id| !thwarted.contains(id))
        .collect();
    let pool = if unbeaten.is_empty() { available } else { &unbeaten };
    pool.choose(rng).copied()
}

/// Rolls this save's campaign, once, when there isn't one.
///
/// A save loaded from disk already inserted its `Campaign` in
/// `shift::load_progress`, so this only ever fires for a genuinely new one.
fn assign_campaign(
    mut commands: Commands,
    script: Option<Res<Script>>,
    campaign: Option<Res<Campaign>>,
    thwarted: Res<ThwartedAntags>,
    choice: Option<Res<CampaignChoice>>,
) {
    let (Some(script), None) = (script, campaign) else {
        return;
    };

    let available: Vec<AntagId> = script.antagonists.iter().map(|def| def.id).collect();
    let choice = choice.map(|c| *c);
    let antag = match choice.and_then(|c| c.antag) {
        Some(forced) => forced,
        None => {
            let Some(picked) = pick_antag(&available, &thwarted.0, &mut rand::rng()) else {
                warn!("station.arc.ron lists no antagonists — no campaign this save");
                return;
            };
            picked
        }
    };
    let mode = choice.map(|c| c.mode).unwrap_or_default();

    let steps = script
        .antagonist(antag)
        .map(|def| def.counter_steps.len())
        .unwrap_or(0);
    commands.insert_resource(Campaign::new(antag, mode, steps));
    // Logged, not aired: the radio must not know this yet. It goes to the
    // console because a developer needs to be able to see what a save rolled
    // without opening the save file.
    info!("campaign: {:?} ({:?} run)", antag, mode);
}

// ---------------------------------------------------------------------------
// The meter
// ---------------------------------------------------------------------------

/// Moves the plot meter, from every source that touches it.
///
/// Deliberately mode-agnostic: an illicit delivery helps the antagonist and a
/// counter step hurts them whichever side the player is on. Only
/// [`Campaign::player_won`] reads the result differently.
fn advance_plot(
    time: Res<Time>,
    script: Option<Res<Script>>,
    campaign: Option<ResMut<Campaign>>,
    mut drift: ResMut<DriftClock>,
    shift: Res<Shift>,
    mut resolved: MessageReader<OrderResolved>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(mut campaign)) = (script, campaign) else {
        resolved.clear();
        return;
    };
    if campaign.outcome.is_some() {
        resolved.clear();
        return;
    }

    let mut delta = 0;

    // Drift, banked only in whole points. `Campaign` drives a network send off
    // change detection, so writing it every frame would broadcast it every
    // frame.
    //
    // Paused while the "not accepting requests" sign is down. The sign is the
    // only pacing control the player has, and it used to be a trap: it stops
    // every visitor including the counter-track's, so taking a breather shut
    // the one route to `StoppedByDepartments` while the antagonist carried on
    // working. Closing up is now genuinely neutral — you make no progress and
    // neither do they.
    if shift.accepting_orders {
        drift.0 += time.delta_secs();
        while drift.0 >= DRIFT_INTERVAL_SECONDS {
            drift.0 -= DRIFT_INTERVAL_SECONDS;
            delta += script.drift_per_minute;
        }
    }

    // Only the counter-track needs the authored definition. This used to be a
    // `let ... else { return; }` sitting above the plot write, which meant a
    // campaign whose antagonist had no entry in `station.arc.ron` silently
    // froze the meter at zero forever — no drift, no aid, and therefore no way
    // for the arc to ever resolve. A content bug should cost the counter-track,
    // not the whole save.
    let def = script.antagonist(campaign.antag);
    let mut delivered_lines: Vec<String> = Vec::new();

    for report in resolved.read() {
        if !report.outcome.is_good() {
            continue;
        }
        match report.kind {
            OrderKind::Illicit => delta += script.plot_per_aid,
            OrderKind::Counter => {
                let Some(def) = def else {
                    continue;
                };
                // Mark the first outstanding step this department was asking
                // for. Matching by role rather than by an id threaded through
                // the order keeps this on the same "react to `OrderResolved`"
                // footing every other thread uses.
                let step = def
                    .counter_steps
                    .iter()
                    .enumerate()
                    .find(|(index, step)| {
                        step.role == report.role
                            && campaign.countered.get(*index).copied() == Some(false)
                    })
                    .map(|(index, step)| (index, step.delivered_line.clone()));
                if let Some((index, line)) = step {
                    if let Some(flag) = campaign.countered.get_mut(index) {
                        *flag = true;
                    }
                    delta += script.plot_per_counter_step;
                    delivered_lines.push(line);
                }
            }
            OrderKind::Normal | OrderKind::Crisis => {}
        }
    }

    for line in delivered_lines {
        radio.push(RadioEntry {
            channel: "COM".to_string(),
            text: line,
            good: true,
        });
    }

    if delta != 0 {
        campaign.plot = (campaign.plot + delta).clamp(0, PLOT_MAX);
    }
}

/// Moves the plot from outside this module.
///
/// Public because the sources that are not `OrderResolved`-shaped live in the
/// modules that own them — a main antagonist's own thread advancing, a
/// department minor being left to get on with it — and inverting that would
/// mean this module having to know about all of them. A resolved arc ignores
/// everything, which is the invariant worth having in one place.
pub fn nudge_plot(campaign: &mut Campaign, delta: i32) {
    if campaign.outcome.is_some() {
        return;
    }
    campaign.plot = (campaign.plot + delta).clamp(0, PLOT_MAX);
}

/// Raises the plot when a department minor is left to get on with it.
pub fn note_ignored_shenanigan(script: &ArcScript, campaign: &mut Campaign) {
    nudge_plot(campaign, script.plot_per_ignored_shenanigan);
}

// ---------------------------------------------------------------------------
// The counter-track
// ---------------------------------------------------------------------------

/// The clock between counter-track requests.
///
/// Only armed once the track opens, so a save that never reaches
/// [`Reveal::Suspected`] never pays for a timer it will not use.
#[derive(Resource)]
struct CounterSpawner {
    timer: Timer,
}

/// Sends the next department in to ask for what they have worked out they need.
///
/// The seventh instance of this codebase's standard threat-module spawner, and
/// deliberately the least original: a crew member off the ordinary roster, an
/// ordinary [`Order`], the ordinary counter/window pipeline. The only new part
/// is the [`CounterOrder`] marker, which is what lets [`advance_plot`] tell a
/// department's countermeasure from any other delivery.
///
/// The order is **not** `specific`, so `orders::reference_category` derives the
/// accepted category from the step's reference reagent — any member of it
/// resolves the step, exactly like an ordinary request.
#[allow(clippy::too_many_arguments)]
fn generate_counter_orders(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    campaign: Option<Res<Campaign>>,
    mut spawner: Option<ResMut<CounterSpawner>>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
    active: Query<(), With<CounterOrder>>,
    waiting: Query<&CrewMember, crate::crew::NotResident>,
) {
    let (Some(station), Some(script), Some(campaign)) = (station, script, campaign) else {
        return;
    };
    // The track only exists once the station has worked out enough to know
    // what to ask for, and stops the moment the arc is over either way.
    if campaign.reveal < Reveal::Suspected || campaign.outcome.is_some() {
        return;
    }
    // Never two at once — the track is a sequence, not a queue.
    //
    // Deliberately *not* gated on `shift.accepting_orders`, unlike every other
    // spawner in the game. A department that has worked out what it needs to
    // stop the thing killing the station is not ordinary counter traffic, and
    // making the closed sign shut off the player's only route to
    // `StoppedByDepartments` turned the one pacing control they have into a
    // trap. `advance_plot` pauses the drift for the same reason, so closing up
    // stalls neither side.
    if !active.is_empty() {
        return;
    }

    let Some(spawner) = spawner.as_mut() else {
        // First arm, the frame the track opens.
        commands.insert_resource(CounterSpawner {
            timer: Timer::from_seconds(FIRST_COUNTER_SECONDS, TimerMode::Once),
        });
        return;
    };
    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let mut rng = rand::rng();
    let rules = current_rules(&station.config, &shift);
    let legit_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    let multiplier =
        rng.random_range(script.counter_gap_multiplier.0..=script.counter_gap_multiplier.1);
    spawner.timer = Timer::from_seconds(legit_gap * multiplier, TimerMode::Once);

    let Some(def) = script.antagonist(campaign.antag) else {
        return;
    };
    // Always the lowest outstanding index. `advance_plot` resolves a delivery
    // by finding the lowest outstanding step with a matching role, so the two
    // agree by construction as long as only one is ever live at a time —
    // which the `active` check above is what guarantees.
    let Some(step) = def
        .counter_steps
        .iter()
        .enumerate()
        .find(|(index, _)| campaign.countered.get(*index).copied() == Some(false))
        .map(|(_, step)| step)
    else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&step.reagent) else {
        warn!("counter step names unknown reagent '{}'", step.reagent);
        return;
    };

    let candidates: Vec<&CrewDef> = station
        .crew
        .iter()
        .filter(|def| def.role == step.role)
        .collect();
    let Some(crew_def) = candidates.choose(&mut rng).copied() else {
        warn!("no crew member with role '{}' to ask for a countermeasure", step.role);
        return;
    };

    // Generous, and its own range rather than the difficulty ramp's: a
    // counter step is the ending the player is working toward, and losing one
    // to a clock they never saw would be the worst possible way to lose it.
    let patience = rng.random_range(COUNTER_PATIENCE_SECONDS.0..=COUNTER_PATIENCE_SECONDS.1);
    let lane = waiting.iter().count() as f32 * 0.95;
    let crew = spawn_crew_member(&mut commands, crew_def, lane);

    let want_label = reference_category(&db, reagent)
        .map(|cat| cat.want_phrase().to_string())
        .unwrap_or_else(|| db.reagents.get(reagent).name.clone());
    let amount = deliverable_amount(&db, reagent, Units::whole(step.amount as i32));
    commands.entity(crew).insert((
        Order {
            reagent,
            specific: false,
            amount,
            plea: step.plea.clone(),
            patience,
            waited: 0.0,
        },
        CounterOrder,
        Interactable::new(format!(
            "{} — hand over {} {}",
            crew_def.name, amount, want_label
        )),
    ));

    announce_request(&mut radio, &crew_def.name, &crew_def.role, &step.plea);
    info!(
        "counter-track: {} ({}) wants {} {}",
        crew_def.name, crew_def.role, amount, want_label
    );
}

/// How long after the track opens the first department turns up. Short — the
/// counter-track opening is the game telling the player there is something
/// they can *do*, and a long silence after that reads as nothing having
/// happened.
const FIRST_COUNTER_SECONDS: f32 = 20.0;

/// A counter step's deadline. Much longer than an ordinary order's, because
/// this is a route to an ending and may need a synthesis the player has not
/// done before.
const COUNTER_PATIENCE_SECONDS: (f32, f32) = (150.0, 240.0);

// ---------------------------------------------------------------------------
// The reveal
// ---------------------------------------------------------------------------

/// Climbs `Hidden → Suspected → Named` and airs one line on each step.
///
/// Monotonic on purpose: delivering a counter step pushes the plot back down,
/// but the station does not *un-work-out* what it already worked out.
fn update_reveal(
    script: Option<Res<Script>>,
    campaign: Option<ResMut<Campaign>>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(mut campaign)) = (script, campaign) else {
        return;
    };
    if campaign.outcome.is_some() {
        return;
    }

    let reached = if campaign.plot >= script.named_at {
        Reveal::Named
    } else if campaign.plot >= script.suspected_at {
        Reveal::Suspected
    } else {
        Reveal::Hidden
    };
    if reached <= campaign.reveal {
        return;
    }

    // Step one tier at a time, so a single large jump still airs the line the
    // player would otherwise never see.
    let next = match campaign.reveal {
        Reveal::Hidden => Reveal::Suspected,
        Reveal::Suspected => Reveal::Named,
        Reveal::Named => return,
    };
    campaign.reveal = next;

    // The reveal itself climbs whether or not the antagonist is authored —
    // same reasoning as `advance_plot`'s: a missing entry in `station.arc.ron`
    // should cost the *line*, not stall the whole arc at `Hidden` with the
    // counter-track permanently shut behind it.
    let Some(def) = script.antagonist(campaign.antag) else {
        return;
    };
    let line = match next {
        Reveal::Suspected => def.suspected_line.clone(),
        Reveal::Named => def.named_line.clone(),
        Reveal::Hidden => return,
    };

    // Immediate rather than `PendingBroadcasts::push_delayed`: ordinary
    // chatter is delayed to feel like news arriving, but this is the station
    // realising something, and it should land when it happens.
    radio.push(RadioEntry {
        channel: "COM".to_string(),
        text: line,
        good: false,
    });
}

// ---------------------------------------------------------------------------
// The endings
// ---------------------------------------------------------------------------

/// Closes the arc the moment any of its three conditions is met.
///
/// [`ArcOutcome::StoppedDirectly`] is not decided here — `crate::showdown`
/// writes it directly, since only it knows how its own fight went.
fn resolve_campaign(
    script: Option<Res<Script>>,
    campaign: Option<ResMut<Campaign>>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(mut campaign)) = (script, campaign) else {
        return;
    };
    if campaign.outcome.is_some() {
        return;
    }

    let outcome = if campaign.plot >= PLOT_MAX {
        ArcOutcome::PlotSucceeded
    } else if campaign.fully_countered() {
        ArcOutcome::StoppedByDepartments
    } else {
        return;
    };

    finish(&script, &mut campaign, outcome, &mut radio);
}

/// Writes an outcome and airs its closing line, once.
///
/// Public so `crate::showdown` can end an arc it won or lost without
/// duplicating the "which line, and don't do it twice" rule.
pub fn finish(
    script: &ArcScript,
    campaign: &mut Campaign,
    outcome: ArcOutcome,
    radio: &mut RadioLog,
) {
    if campaign.outcome.is_some() {
        return;
    }
    campaign.outcome = Some(outcome);
    // A resolved arc should read as resolved, whichever way it went: the plot
    // meter is pinned so nothing downstream sees a half-finished campaign.
    if outcome == ArcOutcome::PlotSucceeded {
        campaign.plot = PLOT_MAX;
    }

    let Some(def) = script.antagonist(campaign.antag) else {
        return;
    };
    let won = campaign.player_won().unwrap_or(false);
    let text = if outcome == ArcOutcome::PlotSucceeded {
        def.victory_line.clone()
    } else {
        def.thwarted_line.clone()
    };
    radio.push(RadioEntry {
        channel: "COM".to_string(),
        text,
        good: won,
    });
    info!("campaign resolved: {outcome:?} ({})", if won { "win" } else { "loss" });
}

// ---------------------------------------------------------------------------
// Gating
// ---------------------------------------------------------------------------

/// Run condition: this save drew `id`, and its arc is still live.
///
/// What a main antagonist's own module hangs off, so the Cult's ritual thread
/// is silent in a save that drew the Spy. Department minors deliberately do
/// **not** use this — they run in every save.
pub fn is_active(id: AntagId) -> impl Fn(Option<Res<Campaign>>) -> bool + Clone {
    move |campaign: Option<Res<Campaign>>| {
        campaign.is_some_and(|c| c.antag == id && c.outcome.is_none())
    }
}

// ---------------------------------------------------------------------------
// Replication
// ---------------------------------------------------------------------------

/// The campaign, on the wire.
#[derive(Message, Serialize, Deserialize)]
pub struct CampaignSync(Campaign);

fn broadcast_campaign(
    campaign: Option<Res<Campaign>>,
    mut outgoing: MessageWriter<ToClients<CampaignSync>>,
) {
    let Some(campaign) = campaign else {
        return;
    };
    if !campaign.is_changed() {
        return;
    }
    outgoing.write(ToClients {
        // The host holds the authoritative copy; echoing to it would overwrite
        // the original with a copy of itself.
        targets: SendTargets::CLIENTS_ONLY,
        message: CampaignSync(campaign.clone()),
    });
}

/// Sends the campaign to whoever just joined.
///
/// Change detection alone is not enough here, and this is the exact bug shape
/// this project keeps hitting: the plot meter changes at most once a minute,
/// so a guest joining mid-arc would spend up to a minute with no campaign at
/// all — no standing-board entry, no alert lighting — and nothing would look
/// broken enough to notice.
///
/// Keyed on `AuthorizedClient` rather than `ConnectedClient` for the same
/// reason `player::spawn_joining_chemists` is: replicon drops targeted
/// messages until the client's protocol hash has been checked.
fn sync_campaign_to_new_clients(
    campaign: Option<Res<Campaign>>,
    joined: Query<Entity, Added<AuthorizedClient>>,
    mut outgoing: MessageWriter<ToClients<CampaignSync>>,
) {
    let Some(campaign) = campaign else {
        return;
    };
    for client in &joined {
        outgoing.write(ToClients {
            targets: SendTargets::Single(ClientId::Client(client)),
            message: CampaignSync(campaign.clone()),
        });
    }
}

/// Installs the campaign on a guest.
///
/// `insert_resource` rather than `ResMut`, because on a guest this resource
/// does not exist until the first sync arrives — `assign_campaign` is
/// authority-gated and never runs there.
fn apply_campaign(mut commands: Commands, mut incoming: MessageReader<CampaignSync>) {
    for sync in incoming.read() {
        commands.insert_resource(sync.0.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::Outcome;
    use std::time::Duration;

    fn script() -> ArcScript {
        ron::from_str(include_str!("../../assets/data/station.arc.ron"))
            .expect("station.arc.ron should parse")
    }

    /// Just enough world to drive the meter: no renderer, no orders, no crew.
    fn arc_app(antag: AntagId) -> App {
        let steps = script()
            .antagonist(antag)
            .map(|def| def.counter_steps.len())
            .unwrap_or(0);
        let mut app = App::new();
        app.insert_resource(Script(script()))
            .insert_resource(Campaign::new(antag, Mode::Chemist, steps))
            .init_resource::<DriftClock>()
            .init_resource::<RadioLog>()
            .init_resource::<Time>()
            // `advance_plot` reads the accepting-orders sign to decide whether
            // the drift runs at all.
            .init_resource::<Shift>()
            .add_message::<OrderResolved>()
            .add_systems(
                Update,
                (advance_plot, update_reveal, resolve_campaign).chain(),
            );
        app
    }

    fn close_the_sign(app: &mut App) {
        app.world_mut().resource_mut::<Shift>().accepting_orders = false;
    }

    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(seconds));
        app.update();
    }

    fn resolve(app: &mut App, role: &str, kind: OrderKind, outcome: Outcome) {
        app.world_mut().write_message(OrderResolved {
            name: "Someone".to_string(),
            role: role.to_string(),
            reagent: None,
            category: None,
            outcome,
            kind,
        });
        app.update();
    }

    fn plot(app: &App) -> i32 {
        app.world().resource::<Campaign>().plot
    }

    // -- the meter ---------------------------------------------------------

    #[test]
    fn the_plot_drifts_upward_on_its_own() {
        // Doing nothing is not neutral — they are working while you work.
        let mut app = arc_app(AntagId::Cult);
        let per_minute = app.world().resource::<Script>().drift_per_minute;

        advance(&mut app, 60.0);

        assert_eq!(plot(&app), per_minute);
    }

    #[test]
    fn drift_below_a_whole_point_never_rewrites_the_campaign() {
        // `Campaign` drives a network send off change detection. A resource
        // touched every frame would broadcast the whole campaign every frame.
        let mut app = arc_app(AntagId::Cult);
        app.update();
        let before = app.world().resource_ref::<Campaign>().last_changed();

        advance(&mut app, 1.0);

        assert_eq!(
            app.world().resource_ref::<Campaign>().last_changed(),
            before,
            "a sub-minute tick must not mark the campaign changed"
        );
    }

    #[test]
    fn closing_the_sign_stops_the_drift() {
        // The sign is the only pacing control the player has. While it also
        // shut off the counter-track, taking a breather was strictly bad: the
        // antagonist kept working and the one route to stopping them was
        // closed. Closing up is now neutral on both sides.
        let mut app = arc_app(AntagId::Cult);
        close_the_sign(&mut app);

        advance(&mut app, 60.0 * 5.0);

        assert_eq!(plot(&app), 0, "the plot must not creep up while closed");
    }

    #[test]
    fn reopening_starts_the_drift_again() {
        let mut app = arc_app(AntagId::Cult);
        let per_minute = app.world().resource::<Script>().drift_per_minute;
        close_the_sign(&mut app);
        advance(&mut app, 60.0);

        app.world_mut().resource_mut::<Shift>().accepting_orders = true;
        advance(&mut app, 60.0);

        assert_eq!(
            plot(&app),
            per_minute,
            "exactly the open minute should have counted"
        );
    }

    #[test]
    fn an_unauthored_antagonist_still_drifts() {
        // This used to be a `let ... else { return; }` above the plot write, so
        // a campaign whose antagonist had no entry in `station.arc.ron` froze
        // the meter at zero forever and the arc could never resolve at all.
        let mut app = arc_app(AntagId::Cult);
        let per_minute = app.world().resource::<Script>().drift_per_minute;
        // A script with the roster emptied out — the shape a content bug takes.
        let mut stripped = script();
        stripped.antagonists.clear();
        app.insert_resource(Script(stripped));

        advance(&mut app, 60.0);

        assert_eq!(
            plot(&app),
            per_minute,
            "a missing definition should cost the counter-track, not the arc"
        );
    }

    #[test]
    fn a_successful_illicit_delivery_helps_them() {
        let mut app = arc_app(AntagId::Cult);
        let per_aid = app.world().resource::<Script>().plot_per_aid;

        resolve(&mut app, "Service", OrderKind::Illicit, Outcome::Success);

        assert_eq!(plot(&app), per_aid);
    }

    #[test]
    fn a_botched_illicit_delivery_helps_nobody() {
        let mut app = arc_app(AntagId::Cult);

        resolve(&mut app, "Service", OrderKind::Illicit, Outcome::Wrong);
        resolve(&mut app, "Service", OrderKind::Illicit, Outcome::Expired);

        assert_eq!(plot(&app), 0);
    }

    #[test]
    fn an_ordinary_delivery_never_touches_the_meter() {
        let mut app = arc_app(AntagId::Cult);

        resolve(&mut app, "Medical", OrderKind::Normal, Outcome::Success);
        resolve(&mut app, "Medical", OrderKind::Crisis, Outcome::Success);

        assert_eq!(plot(&app), 0);
    }

    #[test]
    fn the_meter_clamps_at_both_ends() {
        let mut app = arc_app(AntagId::Cult);
        app.world_mut().resource_mut::<Campaign>().plot = 2;

        // Down: a counter step is worth far more than 2 points.
        let role = app.world().resource::<Script>().antagonist(AntagId::Cult).unwrap()
            .counter_steps[0]
            .role
            .clone();
        resolve(&mut app, &role, OrderKind::Counter, Outcome::Success);

        assert_eq!(plot(&app), 0, "the meter must not go negative");
    }

    // -- the counter-track -------------------------------------------------

    /// `arc_app` plus everything `generate_counter_orders` needs to spawn a
    /// crew member: the roster, the difficulty config, and a chemistry db.
    fn counter_app(antag: AntagId) -> App {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let crew: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        let config: crate::orders::OrderConfig =
            ron::from_str(include_str!("../../assets/data/station.orders.ron")).unwrap();

        let mut app = arc_app(antag);
        app.insert_resource(ChemDb(data))
            .insert_resource(StationData { crew, config })
            .init_resource::<Shift>()
            .add_systems(Update, generate_counter_orders);
        app
    }

    /// Drives the track open and past its first-arm delay.
    fn open_the_track(app: &mut App) {
        let suspected_at = app.world().resource::<Script>().suspected_at;
        app.world_mut().resource_mut::<Campaign>().plot = suspected_at;
        // One update to cross the reveal, one to arm the clock.
        app.update();
        app.update();
        advance(app, FIRST_COUNTER_SECONDS + 1.0);
    }

    #[test]
    fn the_counter_track_stays_shut_until_the_station_suspects_something() {
        let mut app = counter_app(AntagId::Cult);

        advance(&mut app, FIRST_COUNTER_SECONDS * 3.0);

        assert_eq!(
            app.world_mut().query::<&CounterOrder>().iter(app.world()).count(),
            0,
            "there is nothing to ask for before anyone knows there is a problem"
        );
    }

    #[test]
    fn the_track_opening_sends_the_first_department_in() {
        let mut app = counter_app(AntagId::Cult);
        open_the_track(&mut app);

        let mut counters = app.world_mut().query::<(&CounterOrder, &Order, &CrewMember)>();
        let (_, order, member) = counters
            .iter(app.world())
            .next()
            .expect("a counter-track request should be at the counter");

        let expected = app.world().resource::<Script>().antagonist(AntagId::Cult).unwrap()
            .counter_steps[0]
            .role
            .clone();
        assert_eq!(member.role, expected, "the track runs in its authored order");
        assert!(
            !order.specific,
            "a counter step must stay lenient, or it demands one exact reagent \
             instead of the category the department actually needs"
        );
    }

    #[test]
    fn the_counter_track_ignores_the_closed_sign() {
        // Every other spawner in the game stops at the sign. This one must not:
        // a department bringing the chemist the thing that stops the station
        // dying is not ordinary counter traffic, and gating it made the sign a
        // trap — the drift kept running while the only counter to it was shut.
        let mut app = counter_app(AntagId::Cult);
        close_the_sign(&mut app);
        open_the_track(&mut app);

        assert_eq!(
            app.world_mut().query::<&CounterOrder>().iter(app.world()).count(),
            1,
            "a countermeasure has to be able to reach a closed lab"
        );
    }

    #[test]
    fn only_one_department_asks_at_a_time() {
        let mut app = counter_app(AntagId::Cult);
        open_the_track(&mut app);

        // Long enough for several more gaps to have elapsed.
        advance(&mut app, 600.0);

        assert_eq!(
            app.world_mut().query::<&CounterOrder>().iter(app.world()).count(),
            1,
            "the counter-track is a sequence, not a queue"
        );
    }

    #[test]
    fn delivering_a_step_marks_it_and_pushes_the_plot_back() {
        let mut app = counter_app(AntagId::Cult);
        let before = {
            let suspected_at = app.world().resource::<Script>().suspected_at;
            app.world_mut().resource_mut::<Campaign>().plot = suspected_at;
            suspected_at
        };
        let role = app.world().resource::<Script>().antagonist(AntagId::Cult).unwrap()
            .counter_steps[0]
            .role
            .clone();

        resolve(&mut app, &role, OrderKind::Counter, Outcome::Success);

        let campaign = app.world().resource::<Campaign>();
        assert!(campaign.countered[0], "the step should be marked done");
        assert!(
            campaign.plot < before,
            "a delivered countermeasure has to actually set them back"
        );
    }

    #[test]
    fn a_botched_counter_step_leaves_it_outstanding() {
        let mut app = counter_app(AntagId::Cult);
        let role = app.world().resource::<Script>().antagonist(AntagId::Cult).unwrap()
            .counter_steps[0]
            .role
            .clone();

        resolve(&mut app, &role, OrderKind::Counter, Outcome::Wrong);
        resolve(&mut app, &role, OrderKind::Counter, Outcome::Expired);

        assert!(
            !app.world().resource::<Campaign>().countered[0],
            "only a delivery that actually landed should close a step"
        );
    }

    #[test]
    fn a_department_that_appears_twice_advances_one_step_per_delivery() {
        // The Syndicate's track asks Security twice. Matching by role must not
        // let one delivery close both.
        let script = script();
        let steps = &script.antagonist(AntagId::Spy).unwrap().counter_steps;
        let security: Vec<usize> = steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.role == "Security")
            .map(|(index, _)| index)
            .collect();
        assert!(
            security.len() >= 2,
            "this test is pointless unless a role really does repeat"
        );

        let mut app = counter_app(AntagId::Spy);
        resolve(&mut app, "Security", OrderKind::Counter, Outcome::Success);

        let countered = app.world().resource::<Campaign>().countered.clone();
        assert!(countered[security[0]]);
        assert!(
            !countered[security[1]],
            "one delivery closes one step, even when the role repeats"
        );
    }

    // -- the reveal --------------------------------------------------------

    #[test]
    fn the_reveal_climbs_one_tier_at_a_time_and_airs_each_line_once() {
        let mut app = arc_app(AntagId::Cult);
        let named_at = app.world().resource::<Script>().named_at;

        // Jump straight past both thresholds in one go: the player should
        // still get the Suspected line rather than skipping it.
        app.world_mut().resource_mut::<Campaign>().plot = named_at;
        app.update();
        assert_eq!(app.world().resource::<Campaign>().reveal, Reveal::Suspected);
        app.update();
        assert_eq!(app.world().resource::<Campaign>().reveal, Reveal::Named);

        let lines = app.world().resource::<RadioLog>().entries.len();
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            lines,
            "a tier already reached must never air its line again"
        );
    }

    #[test]
    fn the_reveal_never_goes_backwards() {
        // Pushing the plot back down with a counter step does not un-work-out
        // what the station already worked out.
        let mut app = arc_app(AntagId::Cult);
        let named_at = app.world().resource::<Script>().named_at;
        app.world_mut().resource_mut::<Campaign>().plot = named_at;
        app.update();
        app.update();
        assert_eq!(app.world().resource::<Campaign>().reveal, Reveal::Named);

        app.world_mut().resource_mut::<Campaign>().plot = 0;
        app.update();

        assert_eq!(app.world().resource::<Campaign>().reveal, Reveal::Named);
    }

    // -- the endings -------------------------------------------------------

    #[test]
    fn the_plot_reaching_the_top_ends_the_arc() {
        let mut app = arc_app(AntagId::Cult);
        app.world_mut().resource_mut::<Campaign>().plot = PLOT_MAX;

        app.update();

        let campaign = app.world().resource::<Campaign>();
        assert_eq!(campaign.outcome, Some(ArcOutcome::PlotSucceeded));
        assert_eq!(
            campaign.player_won(),
            Some(false),
            "in a chemist run, them getting what they wanted is a loss"
        );
    }

    #[test]
    fn completing_the_counter_track_ends_the_arc() {
        let mut app = arc_app(AntagId::Cult);
        for flag in app.world_mut().resource_mut::<Campaign>().countered.iter_mut() {
            *flag = true;
        }

        app.update();

        let campaign = app.world().resource::<Campaign>();
        assert_eq!(campaign.outcome, Some(ArcOutcome::StoppedByDepartments));
        assert_eq!(campaign.player_won(), Some(true));
    }

    #[test]
    fn an_outcome_is_written_once_and_stops_the_meter() {
        let mut app = arc_app(AntagId::Cult);
        app.world_mut().resource_mut::<Campaign>().plot = PLOT_MAX;
        app.update();
        let lines = app.world().resource::<RadioLog>().entries.len();

        // Everything that would normally move the meter, after the fact.
        advance(&mut app, 300.0);
        resolve(&mut app, "Service", OrderKind::Illicit, Outcome::Success);

        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            lines,
            "a resolved arc must not keep airing closing lines"
        );
        assert_eq!(plot(&app), PLOT_MAX);
    }

    #[test]
    fn an_antagonist_run_reads_the_same_outcomes_the_other_way_round() {
        // The whole reason the inversion needs no mechanics of its own.
        let steps = script().antagonist(AntagId::Cult).unwrap().counter_steps.len();
        let mut run = Campaign::new(AntagId::Cult, Mode::Antagonist, steps);

        run.outcome = Some(ArcOutcome::PlotSucceeded);
        assert_eq!(run.player_won(), Some(true));

        run.outcome = Some(ArcOutcome::StoppedByDepartments);
        assert_eq!(run.player_won(), Some(false));

        run.outcome = Some(ArcOutcome::StoppedDirectly);
        assert_eq!(run.player_won(), Some(false));
    }

    // -- assignment --------------------------------------------------------

    #[test]
    fn a_fresh_machine_can_draw_anyone() {
        let mut rng = rand::rng();
        let picked = pick_antag(&AntagId::ALL, &[], &mut rng);
        assert!(picked.is_some());
    }

    #[test]
    fn an_unbeaten_antagonist_is_preferred() {
        let mut rng = rand::rng();
        let thwarted = [AntagId::Cult, AntagId::Spy, AntagId::Changeling, AntagId::Ai];
        for _ in 0..50 {
            assert_eq!(
                pick_antag(&AntagId::ALL, &thwarted, &mut rng),
                Some(AntagId::Blob),
                "the only unbeaten antagonist should always be the draw"
            );
        }
    }

    #[test]
    fn beating_everyone_reopens_the_whole_roster() {
        let mut rng = rand::rng();
        assert!(
            pick_antag(&AntagId::ALL, &AntagId::ALL, &mut rng).is_some(),
            "running out of unbeaten antagonists must not mean running out of game"
        );
    }

    #[test]
    fn is_active_gates_on_this_saves_antagonist_and_an_unfinished_arc() {
        let live = Campaign::new(AntagId::Cult, Mode::Chemist, 3);
        let mut done = live.clone();
        done.outcome = Some(ArcOutcome::StoppedDirectly);

        let mut app = App::new();
        app.insert_resource(live);
        assert!(app.world_mut().run_system_cached(is_cult_active).unwrap());

        app.insert_resource(done);
        assert!(
            !app.world_mut().run_system_cached(is_cult_active).unwrap(),
            "a resolved arc must stop its antagonist's own thread"
        );
    }

    fn is_cult_active(campaign: Option<Res<Campaign>>) -> bool {
        is_active(AntagId::Cult)(campaign)
    }

    // -- content guardrails ------------------------------------------------

    #[test]
    fn every_antagonist_names_real_reagents_and_real_departments() {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let script = script();
        assert!(!script.antagonists.is_empty());

        for def in &script.antagonists {
            assert!(!def.display.trim().is_empty());
            assert!(!def.suspected_line.trim().is_empty());
            assert!(!def.named_line.trim().is_empty());
            assert!(!def.victory_line.trim().is_empty());
            assert!(!def.thwarted_line.trim().is_empty());
            assert!(
                def.counter_steps.len() >= 2,
                "'{}' needs a counter-track long enough to be a route, not a button",
                def.display
            );
            for step in &def.counter_steps {
                assert!(
                    crate::orders::Department::from_role(&step.role).is_some(),
                    "'{}' names a role no department recognises",
                    step.role
                );
                let reagent = data
                    .reagents
                    .id_of(&step.reagent)
                    .unwrap_or_else(|| panic!("'{}' names no real reagent", step.reagent));
                assert!(
                    !data.reagents.get(reagent).categories.is_empty(),
                    "'{}' has no category, so a lenient counter order could not be graded",
                    step.reagent
                );
                assert!(!step.plea.trim().is_empty());
                assert!(!step.delivered_line.trim().is_empty());
            }
        }
    }

    #[test]
    fn every_antagonist_id_has_exactly_one_entry() {
        let script = script();
        for id in AntagId::ALL {
            assert_eq!(
                script.antagonists.iter().filter(|def| def.id == id).count(),
                1,
                "{id:?} must be authored exactly once"
            );
        }
    }

    #[test]
    fn the_reveal_thresholds_are_ordered_and_reachable() {
        let script = script();
        assert!(script.suspected_at < script.named_at);
        assert!(script.named_at < script.showdown_at);
        assert!(script.showdown_at < PLOT_MAX);
        assert!(script.drift_per_minute > 0, "a stalled meter can never resolve");
        assert!(script.plot_per_aid > 0);
        assert!(
            script.plot_per_counter_step < 0,
            "a counter step has to push the plot back or the track is decoration"
        );
    }

    #[test]
    fn every_department_has_a_minor_antagonist_and_none_of_them_are_gated() {
        // The user requirement this milestone was built to: *every* department
        // keeps a permanent minor antagonist causing shenanigans, whichever
        // main antagonist a save happens to have drawn.
        //
        // Checked against the data rather than the module list, because the
        // module list is what would silently drift: adding a sixth thread and
        // pointing it at a department that already has one is the mistake, and
        // it shows up here as a duplicate.
        use crate::orders::Department;

        let mut covered: Vec<Department> = Vec::new();
        let mut claim = |role: &str, who: &str| {
            let department = Department::from_role(role)
                .unwrap_or_else(|| panic!("{who} names a role no department recognises"));
            assert!(
                !covered.contains(&department),
                "{department:?} has two minor threads — {who} is one too many"
            );
            covered.push(department);
        };

        let obsessed: crate::obsessed::ObsessedScript =
            ron::from_str(include_str!("../../assets/data/station.obsessed.ron")).unwrap();
        claim(&obsessed.role, "obsessed");

        let smuggler: crate::smuggler::SmugglerScript =
            ron::from_str(include_str!("../../assets/data/station.smuggler.ron")).unwrap();
        claim(&smuggler.role, "smuggler");

        let saboteur: crate::saboteur::SaboteurScript =
            ron::from_str(include_str!("../../assets/data/station.saboteur.ron")).unwrap();
        claim(&saboteur.role, "saboteur");

        let quack: crate::quack::QuackScript =
            ron::from_str(include_str!("../../assets/data/station.quack.ron")).unwrap();
        claim(&quack.role, "quack");

        // `rogue_security` is Security's, and is bespoke enough not to share
        // the others' script shape — it names no `role` field because it *is*
        // Security, by construction.
        covered.push(Department::Security);

        for department in Department::ALL {
            assert!(
                covered.contains(&department),
                "{department:?} has nobody causing trouble in it"
            );
        }
    }

    #[test]
    fn an_antag_id_survives_a_round_trip_through_its_key() {
        // The key is what goes in the cross-save unlock file.
        for id in AntagId::ALL {
            assert_eq!(AntagId::from_key(id.key()), Some(id));
        }
        assert_eq!(AntagId::from_key("Nobody"), None);
    }
}
