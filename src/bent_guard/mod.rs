//! Security's minor antagonist: an officer who wants what he would arrest
//! you for.
//!
//! The fifth department shenanigan thread, and the only one whose visits are
//! [`IllicitOrder`]s. That single fact is what makes it different from
//! [`crate::smuggler`], which it is otherwise built on beat for beat.
//!
//! **What the bargain is.** Fill his order and Security's next spot
//! inspection quietly does not happen — a banked `Requisition::raid_wards`,
//! the same "Look the Other Way" ward the requisition shop already sells,
//! now arriving as a favour from the man who would have carried out the
//! raid. Ignore him and he is an officer who asked a chemist for drugs and
//! got nothing for it; the cheapest way to make that conversation never have
//! happened is for Chemistry to be the thing under review, so a snub feeds
//! [`SecuritySuspicion`] directly.
//!
//! **Why it is not free money.** The sale is an ordinary illicit delivery, so
//! `antagonist::handle_illicit_resolutions` charges it the ordinary way:
//! underworld standing up, suspicion up. One ward covers exactly one raid,
//! and dealing is what makes raids happen. The bargain is real and it is also
//! a loan.
//!
//! **The silence.** The other four minors broadcast their plea on arrival.
//! This one cannot — an officer announcing on the Security channel that he
//! would like some methamphetamine is not a thread, it is a punchline — so
//! nothing at all is aired until *after* a visit resolves, and the lines that
//! do air never name him or what he took. He is also deliberately not
//! [`crate::rogue_security`]'s officer, who is a different Security story
//! about standing collapsing in the open; these two never reference each
//! other and are not meant to be read as the same person.

use bevy::prelude::*;
use rand::prelude::*;
use serde::Deserialize;

use crate::antagonist::{nudge_suspicion, SecuritySuspicion};
use crate::chem_data::ChemDb;
use crate::crew::recall_or_spawn_crew_member;
use crate::net::is_authority;
use crate::orders::{deliverable_amount, IllicitOrder, Order, OrderResolved, Shift, StationData};
use crate::player::Chemist;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::threat;
use crate::AppState;

/// How many favours he can have outstanding at once.
///
/// Without a ceiling the chain's clamp at its last entry would let a patient
/// player bank a ward per visit forever and retire from ever being raided.
/// Two is "your next inspection and the one after"; a third would have to be
/// explained to somebody.
const MAX_BANKED_WARDS: u32 = 2;

/// What an unfilled visit adds to [`SecuritySuspicion`].
///
/// Deliberately the same size as `antagonist::SUSPICION_PER_DELIVERY` (5):
/// turning him down draws exactly as much attention as selling to somebody
/// else would have, which is the joke of the whole thread.
const SUSPICION_PER_SNUB: i32 = 5;

pub struct BentGuardPlugin;

impl Plugin for BentGuardPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(threat::ScriptPlugin::<BentGuardScript>::new(
            "data/station.bentguard.ron",
            "bentguard.ron",
        ))
        .init_resource::<BentGuardProgress>()
        .add_systems(OnEnter(AppState::Playing), arm_spawner)
        .add_systems(
            Update,
            (generate_bent_guard_visit, handle_bent_guard_resolution)
                .chain()
                // The unified Reyes case relationship owns new approaches.
                // Legacy progress and already banked wards remain loadable.
                .run_if(not(resource_exists::<
                    crate::security_case::SecurityCaseState,
                >))
                .after(threat::PromoteScripts)
                .run_if(is_authority)
                // No `arc::is_active` gate — a department minor runs in every
                // save, same as the other four.
                .run_if(in_state(AppState::Playing))
                .run_if(crate::session::career_session),
        );
    }
}

/// Which authored visit fires next. Persisted, same as
/// `obsessed::ObsessedProgress`.
#[derive(Resource, Default, Clone, Copy)]
pub struct BentGuardProgress(pub usize);

/// `assets/data/station.bentguard.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct BentGuardScript {
    /// Kept off `station.crew.ron` for the same reason every other recurring
    /// identity is: so an ordinary order can never double-book him — and
    /// here for a second reason, since `Department::members()` decides whose
    /// standing Security's departmental number averages, and the bent one
    /// must not be in that average.
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
    pub gap_multiplier: (f32, f32),
    pub visits: Vec<BentGuardVisitDef>,
    /// Aired a beat after a sale, when a ward was actually banked.
    pub payoff_lines: Vec<String>,
    /// Aired after a sale he cannot pay for, at [`MAX_BANKED_WARDS`].
    pub saturated_lines: Vec<String>,
    /// Aired when a visit expires unfilled.
    pub snub_lines: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BentGuardVisitDef {
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
}

/// This thread's authored script, once loaded.
type Script = threat::Authored<BentGuardScript>;

#[derive(Resource)]
struct BentGuardSpawner {
    timer: Timer,
}

/// See `threat::arm_first_visit` for why this has to re-run on
/// `OnEnter(AppState::Playing)` every session rather than only once at
/// process start.
fn arm_spawner(mut commands: Commands) {
    threat::arm_first_visit(&mut commands, threat::MINOR_FIRST_VISIT, |timer| {
        BentGuardSpawner { timer }
    });
}

#[allow(clippy::too_many_arguments)]
fn generate_bent_guard_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<BentGuardSpawner>>,
    progress: Res<BentGuardProgress>,
    shift: Res<Shift>,
    chemists: Query<(), With<Chemist>>,
    social: Option<Res<crate::social::SocialState>>,
    mut residents: crate::crew::AvailableResidents,
    mut intake: crate::order_intake::Intake,
) {
    let (Some(station), Some(script), Some(spawner)) = (station, script, spawner.as_mut()) else {
        return;
    };
    let mut rng = rand::rng();
    if social.as_deref().is_some_and(|social| {
        !social.threat_runs(crate::social::ResidentAntagonist::ReyesBentGuard)
    }) {
        return;
    }
    let name = social
        .as_deref()
        .map_or(script.name.as_str(), |social| {
            social.threat_identity(
                crate::social::ResidentAntagonist::ReyesBentGuard,
                &script.name,
            )
        })
        .to_string();
    let resident_bound = social
        .as_deref()
        .is_some_and(|social| social.selected(crate::social::ResidentAntagonist::ReyesBentGuard));
    if resident_bound
        && !residents.iter_mut().any(|(_, member, body, blood, ..)| {
            member.name == name && !body.0.collapsed && !blood.0.incapacitated()
        })
    {
        return;
    }
    let rules = current_rules(&station.config, &shift, chemists.iter().count());
    let Some(visit) = threat::due_visit(
        &time,
        &shift,
        &mut spawner.timer,
        &rules,
        &mut rng,
        script.gap_multiplier,
        threat::ChainProgress(progress.0),
        &script.visits,
    ) else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&visit.reagent) else {
        warn!("bent guard visit names unknown reagent '{}'", visit.reagent);
        return;
    };

    // Exact requirements are public once heard. IllicitOrder stays private
    // and controls the consequences of fulfilling the request.
    let Some(context) = intake.admit(
        crate::order_intake::RequestSource::BentGuard,
        &name,
        &mut spawner.timer,
        true,
    ) else {
        return;
    };
    let identity = crate::crew::CrewDef {
        name: name.clone(),
        role: script.role.clone(),
        color: script.color,
    };
    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);
    let Some(crew) = recall_or_spawn_crew_member(&mut commands, &mut residents, &identity, 0.0)
    else {
        intake.cancel_admission(&identity.name);
        return;
    };

    let reagent_name = db.reagents.get(reagent).name.clone();
    let amount = deliverable_amount(&db, reagent, chem_sim::Units::whole(visit.amount as i32));
    commands.entity(crew).insert((
        crate::order_intake::PendingOrder::new(
            Order {
                reagent,
                specific: true,
                minimum_purity: 0.0,
                amount,
                plea: visit.plea.clone(),
                patience,
                waited: 0.0,
            },
            context,
        ),
        IllicitOrder,
        crate::interaction::Interactable::new("Waiting to speak"),
    ));

    // Arrival speech and radio are selected by conversation intake.
    info!("bent guard: {} wants {}u {}", name, amount, reagent_name);
}

/// The favour, and the grudge.
///
/// The first thread to need both directions out of one resolution, which is
/// why `threat::ChainStep` carries its `outcome` — see that field's doc.
/// `Trigger::Never` because neither branch is "the" trigger; the chain
/// advances on every visit regardless, same as the other minors.
#[allow(clippy::too_many_arguments)]
fn handle_bent_guard_resolution(
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    instability: Option<ResMut<crate::instability::Instability>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<BentGuardProgress>,
    mut shift: ResMut<Shift>,
    mut suspicion: ResMut<SecuritySuspicion>,
    mut radio: ResMut<RadioLog>,
    social: Option<Res<crate::social::SocialState>>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    let mut campaign = campaign;
    let mut instability = instability;
    let identity = social
        .as_deref()
        .map_or(script.name.as_str(), |social| {
            social.threat_identity(
                crate::social::ResidentAntagonist::ReyesBentGuard,
                &script.name,
            )
        })
        .to_string();

    let mut chain = threat::ChainProgress(progress.0);
    let steps = threat::step_chain(
        &mut resolved,
        &mut chain,
        &identity,
        script.visits.len(),
        threat::Trigger::Never,
        threat::Advance::EveryVisit,
    );
    progress.0 = chain.0;

    let mut rng = rand::rng();
    for step in steps {
        if step.outcome.is_good() {
            // Handing him the wrong thing is a mistake, not a bargain — only
            // an outcome he was actually pleased with buys anything.
            let banked = &mut shift.requisition.raid_wards;
            let paid = *banked < MAX_BANKED_WARDS;
            if paid {
                *banked += 1;
            }
            let pool = if paid {
                &script.payoff_lines
            } else {
                &script.saturated_lines
            };
            if let Some(line) = pool.choose(&mut rng) {
                radio.push(
                    RadioEntry::new(channel_for(&script.role), line.clone())
                        .speaker(&identity)
                        .positive(),
                );
            }
            info!("bent guard: sale to {} (ward banked: {paid})", identity);
            continue;
        }

        if step.outcome != crate::orders::Outcome::Expired {
            // A wrong delivery is a fumble, the same way it is for every
            // other thread. He waits, or he leaves on his own clock.
            continue;
        }

        // Nothing absorbs this one. The four `threat::Ward`s each cover a
        // *physical* consequence — a theft, a sabotage, a raid, a malpractice
        // claim — and a requisition cannot buy off somebody having formed an
        // opinion about you.
        nudge_suspicion(&mut suspicion, SUSPICION_PER_SNUB);
        if let Some(line) = script.snub_lines.choose(&mut rng) {
            radio.push(RadioEntry::new(channel_for(&script.role), line.clone()).negative());
        }
        info!("bent guard: {} was turned down", identity);

        // A minor left to get on with it is a small gift to whoever the save
        // is really about — the same tie into the campaign every other
        // department shenanigan has.
        if let (Some(arc_script), Some(campaign)) = (arc_script.as_deref(), campaign.as_mut()) {
            crate::arc::note_ignored_shenanigan(arc_script, campaign);
        }
        if let Some(instability) = instability.as_mut() {
            crate::instability::nudge_instability(
                instability,
                crate::instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::CrewDef;
    use crate::orders::{Department, OrderKind, Outcome};

    fn script() -> BentGuardScript {
        ron::from_str(include_str!("../../assets/data/station.bentguard.ron"))
            .expect("station.bentguard.ron should parse")
    }

    #[test]
    fn the_plugin_initialises_every_resource_its_systems_need() {
        // Same guard `security` carries, and for the same reason: every other
        // test here drives `handle_bent_guard_resolution` directly, so a
        // missing `init_resource` would only surface as a panic on the first
        // real frame past `Playing`.
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), BentGuardPlugin));
        assert!(
            app.world().get_resource::<BentGuardProgress>().is_some(),
            "BentGuardPlugin must initialise every resource its own systems require"
        );
    }

    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(threat::Authored(script()))
            .init_resource::<BentGuardProgress>()
            .init_resource::<Shift>()
            .init_resource::<SecuritySuspicion>()
            .init_resource::<RadioLog>()
            .init_resource::<crate::instability::Instability>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_bent_guard_resolution);
        app
    }

    fn resolve(app: &mut App, name: &str, outcome: Outcome) {
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Security".to_string(),
            reagent: None,
            category: None,
            outcome,
            kind: OrderKind::Illicit,
            quality: None,
            development: false,
            campaign: None,
            counter_step: None,
        });
        app.update();
    }

    fn wards(app: &App) -> u32 {
        app.world().resource::<Shift>().requisition.raid_wards
    }

    fn suspicion(app: &App) -> i32 {
        app.world().resource::<SecuritySuspicion>().level()
    }

    #[test]
    fn selling_to_him_buys_the_raid_that_was_coming() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Success);

        assert_eq!(wards(&app), 1, "the favour is a banked Look the Other Way");
        assert_eq!(
            app.world().resource::<BentGuardProgress>().0,
            1,
            "the chain advances — he came, he was served"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn the_favour_he_can_do_you_runs_out() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        for _ in 0..MAX_BANKED_WARDS + 2 {
            resolve(&mut app, &name, Outcome::Success);
        }

        assert_eq!(
            wards(&app),
            MAX_BANKED_WARDS,
            "banking a ward per visit forever would retire the raid system"
        );
    }

    #[test]
    fn a_sale_he_cannot_pay_for_still_says_so() {
        // Silence here would read as a bug: the batch left the counter and
        // apparently nothing happened.
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .raid_wards = MAX_BANKED_WARDS;

        resolve(&mut app, &name, Outcome::Success);

        let log = app.world().resource::<RadioLog>();
        let saturated = &app.world().resource::<Script>().0.saturated_lines;
        assert!(
            saturated.contains(&log.entries[0].text),
            "a sale that buys nothing should say which of the two things happened"
        );
    }

    #[test]
    fn turning_him_down_puts_the_lab_under_review() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Expired);

        assert_eq!(suspicion(&app), SUSPICION_PER_SNUB);
        assert_eq!(wards(&app), 0, "a snub certainly buys no favours");
        assert_eq!(
            app.world()
                .resource::<crate::instability::Instability>()
                .value,
            crate::instability::STABILITY_MAX
                - crate::instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN as f32,
            "an ignored shenanigan is exactly the signal the instability meter watches for"
        );
    }

    #[test]
    fn a_banked_ward_never_absorbs_a_grudge() {
        // The four wards each cover a physical consequence. Suspicion from a
        // snub is somebody's opinion, and there is nothing to requisition.
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .raid_wards = 1;

        resolve(&mut app, &name, Outcome::Expired);

        assert_eq!(suspicion(&app), SUSPICION_PER_SNUB);
        assert_eq!(
            wards(&app),
            1,
            "the ward is for the raid itself, and must not be silently spent here"
        );
    }

    #[test]
    fn handing_him_the_wrong_thing_is_a_fumble_and_not_a_bargain() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Wrong);

        assert_eq!(wards(&app), 0, "he did not get what he asked for");
        assert_eq!(
            suspicion(&app),
            0,
            "and he has no grievance either — he is still standing there"
        );
        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            0,
            "nothing has happened worth a line"
        );
    }

    #[test]
    fn an_unrelated_resolution_never_touches_this_thread() {
        let mut app = resolution_app();

        resolve(&mut app, "Officer Reyes", Outcome::Expired);

        assert_eq!(suspicion(&app), 0);
        assert_eq!(app.world().resource::<BentGuardProgress>().0, 0);
    }

    #[test]
    fn bentguard_ron_parses_and_asks_only_for_things_worth_hiding() {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let script = script();

        assert_eq!(
            Department::from_role(&script.role),
            Some(Department::Security),
            "this is Security's thread, and the role string is what routes its radio channel"
        );
        assert!(script.visits.len() >= 2);
        assert!(!script.payoff_lines.is_empty());
        assert!(!script.saturated_lines.is_empty());
        assert!(!script.snub_lines.is_empty());

        for visit in &script.visits {
            let id = data
                .reagents
                .id_of(&visit.reagent)
                .unwrap_or_else(|| panic!("'{}' names no real reagent", visit.reagent));
            assert!(
                data.reagents
                    .get(id)
                    .categories
                    .contains(&chem_sim::Category::Illicit),
                "'{}' is legal — he could just requisition that, and the whole thread \
                 depends on every ask being one he cannot make through channels",
                visit.reagent
            );
            assert!(!visit.plea.trim().is_empty());
        }
    }

    #[test]
    fn he_is_nobody_the_station_already_knows() {
        let script = script();
        let roster: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        assert!(
            roster.iter().all(|member| member.name != script.name),
            "'{}' must stay off the ordinary roster or an ordinary order could double-book him",
            script.name
        );
        assert!(
            !Department::Security
                .members()
                .contains(&script.name.as_str()),
            "'{}' must stay out of Security's standing average — a department minor is \
             not one of the two officers whose opinion that number reports",
            script.name
        );
        let rogue: crate::rogue_security::RogueSecurityScript =
            ron::from_str(include_str!("../../assets/data/station.rogue_security.ron")).unwrap();
        assert_ne!(
            script.name, rogue.officer_name,
            "Security's two threads are two different people and must not be confused"
        );
    }
}
