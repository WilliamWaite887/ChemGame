//! Departments getting fed up with a lab that will not take their requests.
//!
//! [`Shift::accepting_orders`](crate::orders::Shift::accepting_orders) is
//! described by this module's parent as "a sign the player can flip when they
//! need a break, not a clock nobody controls". It stayed free, though: nothing
//! anywhere cared how long it had been down, so the optimal play was to leave
//! it down whenever the counter got uncomfortable and reopen only when ready.
//! A break with no cost is not a decision.
//!
//! So the departments notice. Stay shut past a grace period and every one of
//! them sours, a point at a time, and says so on the radio while it happens.
//!
//! # The two kinds of closed
//!
//! They are not the same thing and are deliberately not priced the same.
//!
//! - **Sign down, shift still running.** A mid-shift breather. Costs at the
//!   full rate: this is the one the player reaches for to dodge pressure.
//! - **Called it a shift.** The debrief — this module's parent calls it "the
//!   reflection beat", somewhere to stop and read your numbers. Slowed by
//!   [`DEBRIEF_SLOWDOWN`] rather than exempted: stopping is legitimate and
//!   should stay comfortable, but a career left parked for an hour should
//!   still find the station has got on without it.
//!
//! # Three things that would each be a bug
//!
//! **A fresh career starts with the sign down**, deliberately, so a co-op
//! group can gather before anyone walks in. If the clock ran from the first
//! frame, a team taking three minutes to sort themselves out would be
//! penalised before the first order existed. So the clock **arms on the lab
//! being opened**, not on it being shut, and a save that has never been open
//! this session never accumulates anything. The same property covers reloading
//! a save that was closed when it was written — `accepting_orders` is
//! persisted, so that is a real and ordinary case.
//!
//! **A queue still at the counter pauses it.** Flipping the sign to work
//! through what is already waiting is exactly what the sign is for, and
//! charging for it would teach the player never to use it. The exploit this
//! opens is self-limiting: an order cannot be kept alive indefinitely, because
//! its own patience clock resolves or expires it either way.
//!
//! **Nothing here is persisted.** Reloading gives a fresh grace period, for
//! the reason `SecuritySuspicion` is deliberately never saved: opening a file
//! must not immediately punish the player for how they left it.

use bevy::prelude::*;
use rand::prelude::*;
use serde::Deserialize;

use crate::crew::NotResident;
use crate::net::is_authority;
use crate::orders::{Department, Order, Shift};
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::threat;
use crate::AppState;

/// Seconds shut before the departments start caring at all.
///
/// Generous on purpose. The sign has a legitimate use and the penalty is for
/// leaving it down, not for using it.
const GRACE_SECONDS: f32 = 120.0;
/// Seconds between each point of standing lost, once the grace has lapsed.
const INTERVAL_SECONDS: f32 = 60.0;
/// How much more slowly the debrief accumulates than a sign-down break.
///
/// Not immunity — see the module doc. Three minutes of debrief costs what one
/// minute of hiding behind the sign does.
const DEBRIEF_SLOWDOWN: f32 = 3.0;
/// Standing taken per point, from every member of every department.
///
/// One, so the unit of this penalty is *time*, not size. A clean delivery is
/// worth two, so a department is roughly two minutes of neglect behind per
/// delivery it takes to put right.
const PER_POINT: i32 = -1;
/// Points between spoken complaints. Every point would be a line a minute,
/// which is nagging rather than pressure.
const COMPLAIN_EVERY: u32 = 3;

pub struct ImpatiencePlugin;

impl Plugin for ImpatiencePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(threat::ScriptPlugin::<ImpatienceScript>::new(
            "data/station.impatience.ron",
            "impatience.ron",
        ))
        .init_resource::<Impatience>()
        // Per session, not at startup: a spent closure must not be inherited
        // by the next lab this process opens. The same reason
        // `threat::arm_first_visit` is registered here rather than on
        // `Startup`.
        .add_systems(OnEnter(AppState::Playing), reset_impatience)
        .add_systems(
            Update,
            tick_impatience
                .after(threat::PromoteScripts)
                .run_if(is_authority)
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// `assets/data/station.impatience.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct ImpatienceScript {
    /// Aired once, when the grace lapses and before anything has been lost.
    /// The warning shot.
    pub warnings: Vec<String>,
    /// Aired as standing is actually taken, in the complaining department's
    /// own voice.
    pub complaints: Vec<ComplaintDef>,
    /// Aired on reopening, but only if they had started to care.
    pub relief: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ComplaintDef {
    /// Which department is doing the complaining. Every department needs at
    /// least one, or a draw can come up empty.
    pub role: String,
    pub text: String,
}

type Script = threat::Authored<ImpatienceScript>;

/// How long the lab has been shut, and what that has already cost.
///
/// Server-side scratch. The one thing a client needs — how far this has gone,
/// for the HUD banner — rides on [`Shift::closure_pressure`], which is already
/// replicated.
#[derive(Resource, Default)]
pub struct Impatience {
    /// Effective seconds shut: real seconds while the sign is down, and
    /// [`DEBRIEF_SLOWDOWN`]-th of a second per second during a debrief.
    ///
    /// Accumulating an *effective* figure rather than scaling the thresholds
    /// is what lets a closure change kind partway through — flip the sign,
    /// then call the shift — without the schedule jumping.
    shut_for: f32,
    /// Points already taken this closure. Mirrored onto
    /// [`Shift::closure_pressure`] for the HUD.
    taken: u32,
    /// Whether the lab has been open at all this session. See the module doc:
    /// without this, a fresh career — which starts shut on purpose — is
    /// penalised before it has begun.
    armed: bool,
    /// Whether the warning shot has already been fired this closure.
    warned: bool,
}

impl Impatience {
    fn reset(&mut self) {
        self.shut_for = 0.0;
        self.taken = 0;
        self.warned = false;
    }
}

fn reset_impatience(mut impatience: ResMut<Impatience>) {
    *impatience = Impatience::default();
}

/// How many points a closure of this length has cost.
///
/// Pure, and the whole schedule: `0` through the grace, then one more every
/// [`INTERVAL_SECONDS`]. Kept a function rather than inlined for the reason
/// this module's parent keeps its ramp arithmetic pure — "balance that can
/// only be checked by playing for an hour is balance nobody checks".
pub fn points_due(shut_for: f32) -> u32 {
    ((shut_for - GRACE_SECONDS).max(0.0) / INTERVAL_SECONDS) as u32
}

/// Whether a closure this long has gone on long enough for anyone to mention
/// it — the moment the warning airs and the HUD banner changes its tone.
pub fn past_grace(shut_for: f32) -> bool {
    shut_for >= GRACE_SECONDS
}

#[allow(clippy::too_many_arguments)]
fn tick_impatience(
    time: Res<Time>,
    script: Option<Res<Script>>,
    mut shift: ResMut<Shift>,
    mut impatience: ResMut<Impatience>,
    mut radio: ResMut<RadioLog>,
    mut stability: MessageWriter<crate::instability::StabilityEvent>,
    waiting: Query<(), (With<Order>, NotResident)>,
) {
    let mut rng = rand::rng();

    if shift.accepting_orders {
        // Reopening is what arms the clock in the first place, and what
        // forgives whatever the last closure had built up.
        let had_noticed = impatience.warned;
        impatience.armed = true;
        impatience.reset();
        if shift.closure_pressure != 0 {
            shift.closure_pressure = 0;
        }
        if had_noticed {
            if let Some(script) = script.as_ref() {
                if let Some(line) = script.relief.choose(&mut rng) {
                    radio.push(
                        RadioEntry::new(crate::radio::RadioChannel::Bridge, line.clone())
                            .speaker("Duty Officer")
                            .positive(),
                    );
                }
            }
        }
        return;
    }

    // Never been open this session — a fresh career, or a save reloaded shut.
    if !impatience.armed {
        return;
    }
    // Still serving what is already at the counter. Working the queue with the
    // sign down is the sign doing its job.
    if !waiting.is_empty() {
        return;
    }

    let scale = if shift.called { DEBRIEF_SLOWDOWN } else { 1.0 };
    impatience.shut_for += time.delta_secs() / scale;

    if past_grace(impatience.shut_for) && !impatience.warned {
        impatience.warned = true;
        if let Some(script) = script.as_ref() {
            if let Some(line) = script.warnings.choose(&mut rng) {
                radio.push(
                    RadioEntry::new(crate::radio::RadioChannel::Bridge, line.clone())
                        .speaker("Duty Officer"),
                );
            }
        }
    }

    let due = points_due(impatience.shut_for);
    while impatience.taken < due {
        impatience.taken += 1;
        stability.write(crate::instability::StabilityEvent::ClosureNeglect {
            pressure: impatience.taken,
        });
        for department in Department::ALL {
            shift.adjust(department, PER_POINT);
        }
        if impatience.taken % COMPLAIN_EVERY == 1 {
            complain(
                script.as_ref().map(|script| &script.0),
                &mut radio,
                &mut rng,
            );
        }
    }
    if shift.closure_pressure != impatience.taken {
        shift.closure_pressure = impatience.taken;
    }
}

/// One department says what it thinks, in its own voice and on its own
/// channel.
fn complain(script: Option<&ImpatienceScript>, radio: &mut RadioLog, rng: &mut impl Rng) {
    let Some(script) = script else {
        return;
    };
    let Some(line) = script.complaints.choose(rng) else {
        return;
    };
    radio.push(
        RadioEntry::new(channel_for(&line.role), line.text.clone())
            .speaker(format!("{} Desk", line.role))
            .negative(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> ImpatienceScript {
        ron::from_str(include_str!("../../assets/data/station.impatience.ron")).unwrap()
    }

    fn impatience_app() -> App {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(threat::Authored(script()))
            .init_resource::<Impatience>()
            .init_resource::<RadioLog>()
            .add_message::<crate::instability::StabilityEvent>()
            .insert_resource(Shift {
                accepting_orders: true,
                ..default()
            })
            .add_systems(Update, tick_impatience);
        // One frame open, which is what arms the clock — the same thing
        // opening the lab does in a real session.
        app.update();
        app
    }

    fn shut(app: &mut App) {
        app.world_mut().resource_mut::<Shift>().accepting_orders = false;
    }

    fn wait(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    fn standing(app: &App) -> i32 {
        app.world()
            .resource::<Shift>()
            .standing(Department::Medical)
    }

    #[test]
    fn the_schedule_is_a_grace_period_and_then_a_point_a_minute() {
        assert_eq!(points_due(0.0), 0);
        assert_eq!(points_due(GRACE_SECONDS - 0.1), 0);
        // The grace lapsing is a warning, not yet a cost.
        assert_eq!(points_due(GRACE_SECONDS), 0);
        assert_eq!(points_due(GRACE_SECONDS + INTERVAL_SECONDS), 1);
        assert_eq!(points_due(GRACE_SECONDS + INTERVAL_SECONDS * 4.0), 4);
        // Monotonic, so a closure can never become cheaper by going on longer.
        let mut last = 0;
        for step in 0..200 {
            let due = points_due(step as f32 * 10.0);
            assert!(due >= last);
            last = due;
        }
    }

    #[test]
    fn a_break_inside_the_grace_period_costs_nothing() {
        // The sign has a legitimate use. If the shortest realistic breather
        // cost standing, the answer would be to never use it.
        let mut app = impatience_app();
        shut(&mut app);
        wait(&mut app, GRACE_SECONDS - 1.0);

        assert_eq!(standing(&app), 0, "a short break was charged for");
        assert_eq!(app.world().resource::<Shift>().closure_pressure, 0);
    }

    #[test]
    fn staying_shut_sours_every_department() {
        let mut app = impatience_app();
        shut(&mut app);
        wait(&mut app, GRACE_SECONDS + INTERVAL_SECONDS * 3.0);

        for department in Department::ALL {
            assert_eq!(
                app.world().resource::<Shift>().standing(department),
                -3,
                "{department:?} did not notice the lab was shut"
            );
        }
        assert_eq!(app.world().resource::<Shift>().closure_pressure, 3);
    }

    #[test]
    fn a_fresh_career_is_not_penalised_before_it_has_opened() {
        // A new save starts with the sign DOWN on purpose, so a co-op group
        // can gather. A clock that ran from the first frame would charge a
        // team for taking three minutes to sort themselves out, before a
        // single order existed.
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(threat::Authored(script()))
            .init_resource::<Impatience>()
            .init_resource::<RadioLog>()
            .add_message::<crate::instability::StabilityEvent>()
            // Exactly `Shift::default()` — which is closed.
            .init_resource::<Shift>()
            .add_systems(Update, tick_impatience);

        wait(&mut app, GRACE_SECONDS + INTERVAL_SECONDS * 10.0);

        assert_eq!(
            standing(&app),
            0,
            "a career that has never opened was charged for being shut"
        );
    }

    #[test]
    fn working_through_the_queue_with_the_sign_down_is_free() {
        // Flipping the sign to serve what is already waiting is the sign
        // doing its job. Charging for it teaches the player never to use it.
        let mut app = impatience_app();
        shut(&mut app);
        app.world_mut().spawn(crate::orders::Order {
            reagent: chem_sim::ReagentId(0),
            specific: false,
            minimum_purity: 0.0,
            amount: chem_sim::Units::whole(5),
            plea: "Still waiting".to_string(),
            patience: 600.0,
            waited: 0.0,
        });

        wait(&mut app, GRACE_SECONDS + INTERVAL_SECONDS * 5.0);

        assert_eq!(
            standing(&app),
            0,
            "the lab was charged for serving the queue it closed to catch up on"
        );
    }

    #[test]
    fn a_debrief_costs_less_than_hiding_behind_the_sign() {
        let sign_down = {
            let mut app = impatience_app();
            shut(&mut app);
            wait(&mut app, GRACE_SECONDS * DEBRIEF_SLOWDOWN + 600.0);
            standing(&app)
        };
        let debrief = {
            let mut app = impatience_app();
            shut(&mut app);
            app.world_mut().resource_mut::<Shift>().called = true;
            wait(&mut app, GRACE_SECONDS * DEBRIEF_SLOWDOWN + 600.0);
            standing(&app)
        };

        assert!(
            debrief > sign_down,
            "the debrief ({debrief}) cost as much as hiding behind the sign \
             ({sign_down}) — calling a shift is the game's own stopping point"
        );
        assert!(
            debrief < 0,
            "a career parked in the debrief indefinitely cost nothing at all"
        );
    }

    #[test]
    fn reopening_forgives_the_clock_but_not_the_damage() {
        let mut app = impatience_app();
        shut(&mut app);
        wait(&mut app, GRACE_SECONDS + INTERVAL_SECONDS * 2.0);
        let lost = standing(&app);
        assert!(lost < 0);

        app.world_mut().resource_mut::<Shift>().accepting_orders = true;
        wait(&mut app, 1.0);

        assert_eq!(
            standing(&app),
            lost,
            "reopening handed the standing back, so closing costs nothing"
        );
        assert_eq!(
            app.world().resource::<Shift>().closure_pressure,
            0,
            "the banner still warns about a closure that has ended"
        );

        // ...and a second closure starts from a full grace period again.
        shut(&mut app);
        wait(&mut app, GRACE_SECONDS - 1.0);
        assert_eq!(standing(&app), lost, "the second closure skipped its grace");
    }

    #[test]
    fn the_station_says_so_while_it_happens() {
        // An invisible penalty is indistinguishable from a bug. The radio is
        // how the station has always told the player what it thinks.
        let mut app = impatience_app();
        shut(&mut app);
        wait(&mut app, GRACE_SECONDS + 1.0);
        assert!(
            !app.world().resource::<RadioLog>().entries.is_empty(),
            "the grace lapsed with no warning at all"
        );

        let after_warning = app.world().resource::<RadioLog>().entries.len();
        wait(&mut app, INTERVAL_SECONDS * (COMPLAIN_EVERY as f32 + 1.0));
        assert!(
            app.world().resource::<RadioLog>().entries.len() > after_warning,
            "standing drained for minutes without a department complaining"
        );
    }

    #[test]
    fn every_department_can_complain_in_its_own_voice() {
        // A draw that comes up empty for one department is a department that
        // silently never complains, which is exactly the kind of content gap
        // that only shows up in play.
        let script = script();
        for department in Department::ALL {
            assert!(
                script
                    .complaints
                    .iter()
                    .filter(|line| line.role == department.label())
                    .count()
                    >= 2,
                "{} needs two complaints of its own",
                department.label()
            );
        }
        assert!(script.warnings.len() >= 3);
        assert!(script.relief.len() >= 3);
        assert!(script
            .warnings
            .iter()
            .chain(script.relief.iter())
            .chain(script.complaints.iter().map(|line| &line.text))
            .all(|line| !line.trim().is_empty()));
    }
}
