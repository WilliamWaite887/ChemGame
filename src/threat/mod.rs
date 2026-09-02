//! The shared shape of a threat thread.
//!
//! Eleven modules in this codebase spawn a recurring antagonist, minor or
//! major, and every one of them is the same machine wearing a different hat:
//! arm a timer, wait, pick the next beat out of an authored script, spawn a
//! crew member carrying an ordinary [`crate::orders::Order`], and react to how
//! that order resolved. What differs between them is the *consequence* — what
//! a smuggler does when ignored is not what a cult does when obeyed — and
//! that difference is the part worth writing eleven times.
//!
//! Everything else lives here.
//!
//! These helpers were extracted from `crate::shift`, which is 3000-odd lines
//! about difficulty ramps, forecasts, requisitions and the career save. The
//! threat-thread spawners were a lodger under a banner comment in the middle
//! of it. `shift::current_rules` deliberately stays where it is: the pacing a
//! thread scales off is a difficulty question, not a threat one.

use std::marker::PhantomData;

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::net::is_authority;
use crate::orders::{OrderResolved, Outcome, Shift};
use crate::radio::{RadioEntry, RadioLog};
use crate::shift::ShiftRules;
use crate::AppState;

// ---------------------------------------------------------------------------
// Authored scripts
// ---------------------------------------------------------------------------

/// The loaded, authored script for one thread.
///
/// Replaces eleven identical private `Script(T)` newtypes, one per threat
/// module, each with its own `PendingXScript` handle and its own hand-copied
/// promote system.
#[derive(Resource, Deref)]
pub struct Authored<T: Asset>(pub T);

/// The handle for a script that has not finished loading. Removed the frame it
/// promotes, so its presence *is* "still loading".
#[derive(Resource)]
pub struct Loading<T: Asset>(pub Handle<T>);

/// Every thread's script promotion runs in here, so a thread's own systems can
/// order themselves `.after(threat::PromoteScripts)` rather than chaining a
/// private copy of the promote system into their own tuple.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PromoteScripts;

/// Loads one thread's authored RON and promotes it to a resource, once.
///
/// `path` and `extension` are both given because neither can be derived from
/// the other: [`RonAssetPlugin::new`] wants `&[&'static str]`, so there is
/// nowhere to `format!` a stem into an extension at plugin-construction time.
pub struct ScriptPlugin<T: Asset> {
    path: &'static str,
    extension: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Asset + for<'de> Deserialize<'de>> ScriptPlugin<T> {
    pub fn new(path: &'static str, extension: &'static str) -> Self {
        Self {
            path,
            extension,
            _marker: PhantomData,
        }
    }
}

impl<T: Asset + for<'de> Deserialize<'de>> Plugin for ScriptPlugin<T> {
    fn build(&self, app: &mut App) {
        let path = self.path;
        app.add_plugins(RonAssetPlugin::<T>::new(&[self.extension]))
            .add_systems(
                Startup,
                move |mut commands: Commands, assets: Res<AssetServer>| {
                    commands.insert_resource(Loading::<T>(assets.load(path)));
                },
            )
            .add_systems(
                Update,
                promote_script::<T>
                    .in_set(PromoteScripts)
                    // Authority-only, matching every hand-written copy this
                    // replaces. Verified before the swap: no non-authority
                    // system reads any thread's script resource.
                    .run_if(is_authority)
                    .run_if(in_state(AppState::Playing))
                    .run_if(crate::session::career_session),
            );
    }
}

fn promote_script<T: Asset>(
    mut commands: Commands,
    pending: Option<Res<Loading<T>>>,
    mut scripts: ResMut<Assets<T>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Authored(script));
    commands.remove_resource::<Loading<T>>();
}

// ---------------------------------------------------------------------------
// First-visit pacing
// ---------------------------------------------------------------------------

/// A department minor's first visit.
///
/// Shared by `smuggler`, `saboteur`, `quack` and `antagonist`, which each
/// separately hardcoded the identical `(240.0, 420.0)` — four copies that
/// happened to agree, with nothing stopping them drifting apart.
pub const MINOR_FIRST_VISIT: (f32, f32) = (240.0, 420.0);
/// A main antagonist's on-station thread. Slower to start than a minor: a
/// save's headline threat should not be the first thing that happens in it.
pub const MAIN_ANTAGONIST_FIRST_VISIT: (f32, f32) = (300.0, 500.0);
/// `obsessed`. Deliberately the longest — rare, and easy to write off the
/// first time.
pub const STALKER_FIRST_VISIT: (f32, f32) = (600.0, 900.0);
/// `rogue_security`. Short and harmless: nothing actually spawns until
/// Security's standing has already soured, so the first check may as well be
/// early.
pub const ROGUE_FIRST_CHECK: (f32, f32) = (30.0, 60.0);
/// `addiction`. Not a range — every save's first return visit lands exactly
/// here.
pub const ADDICT_FIRST_RETURN: (f32, f32) = (90.0, 90.0);

// ---------------------------------------------------------------------------
// Spawner clocks
// ---------------------------------------------------------------------------

/// Divides every threat-thread wait, for testing a thread by hand.
///
/// A threat thread is deliberately slow: a department minor's first visit is
/// four to seven minutes away, and the visit then has to sit unfilled for
/// another two to four before it expires. That is right for play and useless
/// for looking at a change — nobody re-checks a walk animation on an eleven
/// minute loop, and the alternative people actually reach for is editing the
/// constants and forgetting to put them back.
///
/// `THREAT_RUSH=20 cargo run` puts a saboteur at the counter in seconds.
/// Unset — every shipping build, and every test that does not set it — this
/// returns `1.0` and nothing about the pacing moves.
///
/// Read per call rather than cached in a resource: it is touched a handful of
/// times a minute across all eleven threads, and a `OnceLock` here would be a
/// process-global that tests then have to work around.
pub fn rush_divisor() -> f32 {
    std::env::var("THREAT_RUSH")
        .ok()
        .and_then(|set| set.parse::<f32>().ok())
        // A zero or negative divisor is a typo, not a request for an infinite
        // or backwards wait.
        .filter(|divisor| *divisor >= 1.0)
        .unwrap_or(1.0)
}

/// Arms a fresh save's first visit-thread spawn timer.
///
/// Every threat-thread module (`obsessed`, `smuggler`, `saboteur`, `quack`,
/// `cult`, `rogue_security`, `antagonist`, `addiction`) used to hand-copy an
/// identical three-line body under its own `arm_spawner` system, each
/// registered on `OnEnter(AppState::Playing)` rather than only once at
/// process start: quitting to the menu and opening another save would
/// otherwise leave a spent `TimerMode::Once` behind, and a spent `Once`
/// timer never reports `just_finished` again — the whole thread would be
/// silently dead for the rest of the session with nothing to show for it.
pub fn arm_first_visit<S: Resource>(
    commands: &mut Commands,
    gap_range: (f32, f32),
    make: impl FnOnce(Timer) -> S,
) {
    let gap = rand::rng().random_range(gap_range.0..=gap_range.1) / rush_divisor();
    commands.insert_resource(make(Timer::from_seconds(gap, TimerMode::Once)));
}

/// Rolls the next arrival gap for a scripted visit thread and returns the
/// timer to re-arm its spawner with.
///
/// Every "recurring identity" thread (`obsessed`, `smuggler`, `saboteur`,
/// `quack`, `cult`) and `rogue_security` rolled this identically: the shared
/// legitimate-order gap from [`crate::shift::current_rules`], scaled by the
/// thread's own `gap_multiplier` so each keeps its own separate pacing
/// personality.
pub fn roll_next_gap(
    rng: &mut impl Rng,
    rules: &ShiftRules,
    multiplier_range: (f32, f32),
) -> Timer {
    let legit_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    let multiplier = rng.random_range(multiplier_range.0..=multiplier_range.1);
    Timer::from_seconds(legit_gap * multiplier / rush_divisor(), TimerMode::Once)
}

// ---------------------------------------------------------------------------
// The visit itself
// ---------------------------------------------------------------------------

/// The fields a scripted visit needs from its own thread's authored data —
/// see [`dispatch_scripted_visit`].
pub struct ScriptedVisit<'a> {
    pub context: crate::order_intake::RequestContext,
    pub name: &'a str,
    pub role: &'a str,
    pub color: [f32; 3],
    pub reagent: chem_sim::ReagentId,
    pub amount_units: u32,
    pub plea: String,
}

// ---------------------------------------------------------------------------
// Authored chains
// ---------------------------------------------------------------------------

/// How far into one thread's authored chain the career has reached.
///
/// Clamped at the last entry rather than wrapping or panicking: once the
/// authored content runs out the final beat simply repeats. That is the rule
/// all five chain threads already implemented, character-for-character, five
/// times over.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainProgress(pub usize);

impl ChainProgress {
    /// The entry index that is live right now.
    pub fn index(self, len: usize) -> usize {
        self.0.min(len.saturating_sub(1))
    }

    /// The entry that is live right now.
    pub fn current<T>(self, chain: &[T]) -> Option<&T> {
        chain.get(self.index(chain.len()))
    }

    /// Steps on, clamped.
    ///
    /// Returns whether this step is the one that *reached* the final entry.
    /// `obsessed` is the only thread that cares, and it is exactly what its
    /// hand-written "was not already on the last beat" guard was for: the
    /// finale nudge has to fire once, not on every subsequent visit.
    pub fn advance(&mut self, len: usize) -> bool {
        let last = len.saturating_sub(1);
        let was_at_last = self.0 >= last;
        self.0 = (self.0 + 1).min(last);
        !was_at_last && self.0 == last
    }
}

/// Which resolution a thread's consequence hangs off.
///
/// Deliberately an enum named at every call site rather than an "on
/// resolution" callback that quietly does something different per caller: a
/// reader of `smuggler` should be able to see which of these it is without
/// opening this file. The shared scaffolding must not paper over that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The visit expired unfilled — `smuggler`, `saboteur`, `quack`. Not a
    /// wrong delivery: handing them the wrong thing is a mistake, leaving
    /// them standing there with nothing to do is what gives them the idea.
    Ignored,
    /// Nothing fires; the chain only moves. `obsessed`, whose whole weight is
    /// in the plea rather than any mechanical consequence.
    Never,
}

/// When the authored chain moves on.
///
/// Kept explicit at each caller even though every remaining minor thread moves
/// on after a spent visit. That makes the lifecycle visible without baking it
/// into `step_chain`'s name or quietly changing the callers' contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advance {
    /// Every resolution of this identity's name moves the chain on, however
    /// it graded.
    EveryVisit,
}

/// Dispatches an admitted scripted request. Reuses an available resident;
/// only identities absent from the station are spawned by the fallback.
/// Callers must acquire Intake admission first so busy residents cannot clone.
pub fn dispatch_scripted_visit(
    commands: &mut Commands,
    db: &ChemDb,
    rng: &mut impl Rng,
    rules: &ShiftRules,
    residents: &mut Query<
        (
            Entity,
            &crate::crew::CrewMember,
            &crate::body::Body,
            &crate::body::Bloodstream,
            &mut crate::crew::CrewRoute,
        ),
        (
            With<crate::crew::Ambient>,
            Without<crate::social::NpcCommitment>,
        ),
    >,
    visit: ScriptedVisit,
) -> Entity {
    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);
    let crew =
        crate::crew::recall_resident_for_order(commands, residents, visit.name, visit.role, 0.0)
            .unwrap_or_else(|| {
                let identity = crate::crew::CrewDef {
                    name: visit.name.to_string(),
                    role: visit.role.to_string(),
                    color: visit.color,
                };
                crate::crew::spawn_crew_member(commands, &identity, 0.0)
            });
    let amount = crate::orders::deliverable_amount(
        db,
        visit.reagent,
        chem_sim::Units::whole(visit.amount_units as i32),
    );
    commands.entity(crew).insert((
        crate::order_intake::PendingOrder::new(
            crate::orders::Order {
                reagent: visit.reagent,
                specific: true,
                minimum_purity: 0.0,
                amount,
                plea: visit.plea,
                patience,
                waited: 0.0,
            },
            visit.context,
        ),
        crate::interaction::Interactable::new("Waiting to speak"),
    ));
    crew
}

/// What one of this thread's resolutions meant to it.
pub struct ChainStep {
    /// Whether this resolution is this thread's own [`Trigger`].
    pub fires: bool,
    /// Whether advancing landed on the final authored entry for the first
    /// time — `obsessed`'s one-shot finale.
    pub reached_finale: bool,
    /// How the visit actually graded.
    ///
    /// [`Trigger`] answers "is this the one thing my thread hangs off," which
    /// is all five original callers ever needed. `bent_guard` is the first
    /// with *two* consequences pointing opposite ways — a sale buys a favour,
    /// a snub buys a grudge — and a single `fires` bool cannot express that.
    /// Handing back what `step_chain` already read, rather than adding a
    /// second `Trigger` variant, keeps the choice at the call site.
    pub outcome: Outcome,
}

/// Reads this thread's own resolutions off the shared queue, advances its
/// chain, and reports what each one meant.
///
/// Replaces the identical `if report.name != script.name { continue; }` guard
/// that opened five modules' resolution handlers, plus their five copies of
/// the clamp arithmetic.
///
/// Returns a `Vec` rather than an iterator because advancing needs `&mut
/// ChainProgress` for the whole walk; an empty `Vec` does not allocate, and a
/// frame carrying more than a couple of resolutions does not happen.
pub fn step_chain(
    resolved: &mut MessageReader<'_, '_, OrderResolved>,
    progress: &mut ChainProgress,
    name: &str,
    len: usize,
    trigger: Trigger,
    advance: Advance,
) -> Vec<ChainStep> {
    let mut steps = Vec::new();
    for report in resolved.read() {
        if report.name != name {
            continue;
        }
        let fires = match trigger {
            Trigger::Ignored => report.outcome == Outcome::Expired,
            Trigger::Never => false,
        };
        let moves = match advance {
            Advance::EveryVisit => true,
        };
        let reached_finale = moves && progress.advance(len);
        steps.push(ChainStep {
            fires,
            reached_finale,
            outcome: report.outcome,
        });
    }
    steps
}

// ---------------------------------------------------------------------------
// Requisition wards
// ---------------------------------------------------------------------------

/// The four `orders::Requisition` wards a thread can spend to absorb one
/// consequence before it happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ward {
    Quack,
    Raid,
    Smuggler,
    Saboteur,
}

impl Ward {
    fn count(self, shift: &mut Shift) -> &mut u32 {
        match self {
            Ward::Quack => &mut shift.requisition.quack_wards,
            Ward::Raid => &mut shift.requisition.raid_wards,
            Ward::Smuggler => &mut shift.requisition.smuggler_wards,
            Ward::Saboteur => &mut shift.requisition.saboteur_wards,
        }
    }
}

/// Spends one ward if the player banked one, airing the reassuring line.
///
/// `true` means the consequence was absorbed and the caller must stop.
///
/// The ward is spent, not merely checked — the four call sites each wrote
/// their own `if wards > 0 { wards -= 1; push line; continue; }`, and the
/// failure mode of getting that wrong is a ward that absorbs forever, which
/// is invisible until someone plays for an hour. The caller supplies the
/// whole [`RadioEntry`] because the four of them disagree about speaker and
/// channel, and none of that is this function's business.
pub fn ward_absorbed(
    shift: &mut Shift,
    radio: &mut RadioLog,
    ward: Ward,
    reassurance: RadioEntry,
) -> bool {
    let banked = ward.count(shift);
    if *banked == 0 {
        return false;
    }
    *banked -= 1;
    radio.push(reassurance);
    true
}

/// The prefix every recurring-identity spawner shares: gated on the sign, due
/// on its own clock, re-armed off the live difficulty, and pointed at the
/// chain entry that is live now.
///
/// The order of the first two checks is load-bearing and matches every
/// hand-written copy: a closed sign returns *before* the timer is ticked, so a
/// declared break holds a thread where it is rather than banking its gap.
#[allow(clippy::too_many_arguments)]
pub fn due_visit<'a, T>(
    time: &Time,
    shift: &Shift,
    spawner: &mut Timer,
    rules: &ShiftRules,
    rng: &mut impl Rng,
    multiplier: (f32, f32),
    progress: ChainProgress,
    chain: &'a [T],
) -> Option<&'a T> {
    if !shift.accepting_orders {
        return None;
    }
    if !spawner.tick(time.delta()).just_finished() {
        return None;
    }
    *spawner = roll_next_gap(rng, rules, multiplier);
    progress.current(chain)
}

// ---------------------------------------------------------------------------
// Threshold escalation
// ---------------------------------------------------------------------------

/// A "wait, then escalate" clock — the exact shape `security::schedule_raid`
/// and `crisis::schedule_crisis` each hand-rolled out of an `Option<f32>`,
/// never noticing they were the same machine.
///
/// Deliberately *only* the clock. Which meter it watches, what threshold it
/// crosses, whether arming resets that meter, whether a ward can absorb it,
/// and what actually fires all stay in the owning module — those are the four
/// things the two callers genuinely disagree about, and a shared callback
/// would have hidden every one of them.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct Countdown {
    remaining: Option<f32>,
    variant: usize,
}

/// What one tick of a [`Countdown`] meant.
///
/// Three states rather than a bool, so a caller can never confuse "nothing is
/// armed" with "armed and not due yet" — a distinction both hand-written
/// implementations got right only by the shape of an `if let`.
pub enum Ticked {
    /// Nothing armed; the caller may consider arming one.
    Idle,
    /// Armed and still counting. The caller must do nothing at all.
    Waiting,
    /// Due now, carrying whichever variant armed it.
    Fires(usize),
}

impl Countdown {
    pub fn is_armed(&self) -> bool {
        self.remaining.is_some()
    }

    /// Seconds left, if armed. Test-only today — `crisis`'s "the warning
    /// elapsing afflicts a real victim" needs to know how far to advance.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn remaining(&self) -> Option<f32> {
        self.remaining
    }

    /// Arms the clock.
    ///
    /// `variant` is `0` for a thread with a single path (`security`), while
    /// `crisis` stores the case index it rolled, so the alarm line and the
    /// eventual victim always agree on what is going around.
    pub fn arm(&mut self, seconds: f32, variant: usize) {
        self.remaining = Some(seconds);
        self.variant = variant;
    }

    pub fn tick(&mut self, dt: f32) -> Ticked {
        let Some(remaining) = self.remaining.as_mut() else {
            return Ticked::Idle;
        };
        *remaining -= dt;
        if *remaining > 0.0 {
            return Ticked::Waiting;
        }
        self.remaining = None;
        Ticked::Fires(self.variant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property that matters about the testing lever: it is not on.
    ///
    /// `THREAT_RUSH` divides every threat wait in the game, so the one thing
    /// that must never happen is it quietly applying to a shipping build or to
    /// the rest of the suite. `rush_divisor` is read live from the process
    /// environment, which the test harness does not set.
    #[test]
    fn threat_pacing_is_untouched_unless_the_rush_lever_is_set() {
        assert!(
            std::env::var_os("THREAT_RUSH").is_none(),
            "the suite must not run with the rush lever set, or every pacing \
             assertion below is measuring something else",
        );
        assert_eq!(rush_divisor(), 1.0);
    }

    /// A first visit is armed straight off the authored range, undivided.
    #[test]
    fn an_unrushed_first_visit_lands_inside_its_authored_range() {
        let mut app = App::new();
        #[derive(Resource)]
        struct Spawner(Timer);

        let mut commands = app.world_mut().commands();
        arm_first_visit(&mut commands, MINOR_FIRST_VISIT, Spawner);
        app.world_mut().flush();

        let armed = app.world().resource::<Spawner>().0.duration().as_secs_f32();
        assert!(
            (MINOR_FIRST_VISIT.0..=MINOR_FIRST_VISIT.1).contains(&armed),
            "armed {armed}s, outside the authored {MINOR_FIRST_VISIT:?}",
        );
    }

    /// And so is a repeat gap: the thread's own multiplier against the live
    /// legitimate-order gap, and nothing else.
    #[test]
    fn an_unrushed_repeat_gap_is_the_multiplier_against_the_legitimate_gap() {
        let rules = ShiftRules {
            gap_seconds: (40.0, 40.0),
            patience_seconds: (100.0, 100.0),
            max_active: 3,
            stretch_chance: 0.0,
            forecast_boost: 1.0,
        };
        let rolled = roll_next_gap(&mut rand::rng(), &rules, (2.0, 2.0));
        assert!(
            (rolled.duration().as_secs_f32() - 80.0).abs() < 0.001,
            "40s gap at a 2x multiplier should be 80s, got {}s",
            rolled.duration().as_secs_f32(),
        );
    }
}
