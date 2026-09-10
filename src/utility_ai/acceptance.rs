//! Acceptance tests for the NPC thread, driven through the **real** plugin.
//!
//! Every other test in this phase registers the handful of systems it needs on
//! a bare `App`. That is the right shape for asking "does this rule hold", and
//! it is why the per-packet tests are small and fast. But it means the systems
//! under test are wired by the test, not by `register` — so a `run_if` that
//! silently never passes, a set-ordering mistake, or two systems that cannot
//! coexist in one schedule are all invisible to them.
//!
//! This module builds `UtilityAiPlugin` itself and drives it into `Playing`.
//! What it can catch that nothing else can:
//!
//! - **Query conflicts.** Bevy validates system parameter access when the
//!   schedule is first initialized. A hand-assembled two-system app never
//!   initializes the real one.
//! - **Dead run conditions.** A system gated on a state or resource that never
//!   holds does nothing, forever, with no error. The tests here assert on
//!   *effects*, so an inert system fails them.
//! - **Ordering across sets.** `offer_*` publishes into a buffer that
//!   `clear_opportunity_buffer` empties every frame. Get that order wrong and
//!   every opportunity vanishes before it is scored.
//!
//! The rule this module holds itself to: **never inject a result the executor
//! was supposed to produce.** A fixture may place bodies, arrange geometry,
//! arm an authored trigger and advance the clock. It may not write the
//! contaminant into the bowl and then assert the bowl is contaminated.

//! The module registers nothing and exists as a sibling of the department
//! adapters rather than inside one of them: the thread it exercises crosses
//! four — `covert` arms and acts, `saboteur` triggers, `service` serves,
//! `interviews` asks — so putting it in any one would imply an ownership that
//! is not real.

#[cfg(test)]
mod tests {
    use bevy::prelude::*;
    use bevy_replicon::prelude::RepliconSharedPlugin;

    use crate::chem_data::ChemDb;
    use crate::containers::{Container, ContainerKind};
    use crate::crew::{CrewMember, StationResident};
    use crate::lab::WalkableAreas;
    use crate::utility_ai::{
        IncidentKind, TamperAuthorization, UtilityAgent, UtilityAiPlugin, UtilityControlBundle,
    };

    fn chem() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    /// A real app running the real utility plugin, in `Playing`.
    ///
    /// `MinimalPlugins` supplies `Time`; `StatesPlugin` supplies the state
    /// machine the covert systems are gated on. Entering `Playing` is the whole
    /// point — in `Loading` every gameplay system is inert, which is why the
    /// existing schedule-shape tests can pass while proving nothing about
    /// behaviour.
    ///
    /// The block of resources and messages below is not boilerplate: it is the
    /// exact set `UtilityAiPlugin` needs from its sibling plugins in order to
    /// run a frame, discovered by running it and reading the validation
    /// failures. Every one is a hard `Res`/`MessageReader` — a system that hits
    /// a missing one is *skipped with an error*, not merely idle, so leaving any
    /// out would make the whole harness quietly prove less than it appears to.
    /// `TimePlugin` is deliberately disabled and `Time` initialized by hand.
    ///
    /// It would otherwise overwrite `Time` from the wall clock at the start of
    /// every frame, silently discarding the test's `advance_by` — so a fixture
    /// that "waits two minutes" actually waits about a microsecond, and any
    /// assertion about a timeout passes or fails for reasons unrelated to what
    /// it is testing. An inert `Time` the test alone advances is what every
    /// other fixture in this crate gets for free by not using `MinimalPlugins`.
    fn station() -> App {
        let mut app = App::new();
        let areas = WalkableAreas::from_floor_plan();
        app.add_plugins((
            MinimalPlugins.build().disable::<bevy::time::TimePlugin>(),
            bevy::state::app::StatesPlugin,
            RepliconSharedPlugin::default(),
        ))
        .init_resource::<Time>()
        .init_state::<crate::AppState>()
        .add_plugins(UtilityAiPlugin)
        // Owned by `CrewPlugin` in a real session.
        .init_resource::<crate::crew::CrewPosts>()
        .add_message::<crate::crew::ErrandResolved>()
        // Owned by `InstabilityPlugin`.
        .init_resource::<crate::instability::StationStability>()
        .add_message::<crate::instability::StabilityEvent>()
        // Owned by `ShiftPlugin`.
        .init_resource::<crate::orders::Shift>()
        // Owned by `AntagonistPlugin` and `RadioPlugin`.
        .init_resource::<crate::antagonist::UnderworldStanding>()
        .init_resource::<crate::radio::RadioLog>()
        // Owned by `OrdersPlugin`.
        .add_message::<crate::orders::FulfillmentApplied>()
        .insert_resource(ChemDb(chem()))
        .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
        .insert_resource(areas);
        enter_playing(&mut app);
        app
    }

    /// Moves the app into `Playing` and lets the `OnEnter` systems run.
    fn enter_playing(app: &mut App) {
        app.world_mut()
            .resource_mut::<NextState<crate::AppState>>()
            .set(crate::AppState::Playing);
        app.update();
    }

    fn tick(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    /// The Reaction Bay floor, at body height.
    fn in_the_bay() -> Vec3 {
        Vec3::new(-2.0, crate::crew::BODY_OFFSET, 6.0)
    }

    /// A bench spot in that same room. Room identity matters: `can_see` refuses
    /// across rooms, and the bay is only six metres wide.
    fn on_the_bench() -> Vec3 {
        in_the_bay() + Vec3::new(2.0, -0.9, 0.0)
    }

    fn beside_the_bench() -> Vec3 {
        in_the_bay() + Vec3::new(2.0, 0.0, 1.0)
    }

    /// An ordinary utility-controlled resident.
    fn resident(app: &mut App, name: &str, role: &str, at: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: name.into(),
                    role: role.into(),
                },
                Transform::from_translation(at),
                StationResident,
                UtilityControlBundle::new(UtilityAgent::new(1, 0)),
            ))
            .id()
    }

    /// A beaker sitting out with a real batch in it.
    fn loose_batch(app: &mut App, at: Vec3) -> Entity {
        let kelotane = app.world().resource::<ChemDb>().reagent("kelotane");
        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(kelotane, chem_sim::Units::whole(30));
        app.world_mut()
            .spawn((
                Container {
                    kind: ContainerKind::Beaker,
                    solution,
                },
                Transform::from_translation(at),
            ))
            .id()
    }

    /// Arms a tampering authorization the way `saboteur`'s resolution does.
    ///
    /// This is the one thing the harness places directly, and it is legitimate:
    /// it is the authored trigger's *output*, not the executor's. `saboteur`'s
    /// own tests prove an ignored request produces it. What is under test here
    /// is everything downstream — whether the real plugin's providers and
    /// resolvers pick it up at all.
    fn arm(app: &mut App, actor: Entity, ml: i32, now: f32) {
        let reagent = app.world().resource::<ChemDb>().reagent("water");
        app.world_mut()
            .entity_mut(actor)
            .insert(TamperAuthorization::new(
                reagent,
                chem_sim::Units::whole(ml),
                now,
            ));
    }

    fn volume(app: &App, beaker: Entity) -> chem_sim::Units {
        app.world()
            .get::<Container>(beaker)
            .unwrap()
            .solution
            .total_volume()
    }

    // -----------------------------------------------------------------------
    // The harness itself
    // -----------------------------------------------------------------------

    /// The plugin builds, and entering `Playing` runs its resets.
    ///
    /// Schedule initialization is where Bevy validates every system's parameter
    /// access against every other system in the same set. If two covert systems
    /// ever take conflicting mutable access to the same component, this is the
    /// test that says so — the hand-assembled fixtures cannot, because they
    /// never build the real schedule.
    #[test]
    fn the_real_plugin_builds_and_enters_play() {
        let mut app = station();
        tick(&mut app, 0.016);

        assert!(app
            .world()
            .contains_resource::<crate::utility_ai::IllicitCustody>());
        assert!(app
            .world()
            .contains_resource::<crate::utility_ai::TamperedMeals>());
        assert_eq!(
            *app.world().resource::<State<crate::AppState>>().get(),
            crate::AppState::Playing
        );
    }

    /// The covert providers are actually reached by the real schedule.
    ///
    /// This is the test the whole module exists for. Everything the per-packet
    /// tests assert about tampering is conditional on `offer_container_tampering`
    /// running — and in the real app it runs behind three `run_if`s, inside a
    /// set nested in another set, after a system that empties the very buffer it
    /// writes to. Any one of those going wrong produces silence, not an error.
    #[test]
    fn an_armed_actor_is_offered_a_real_opportunity_through_the_real_schedule() {
        let mut app = station();
        let boyle = resident(&mut app, "Tech Boyle", "Engineering", in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        arm(&mut app, boyle, 6, 0.0);
        tick(&mut app, 0.05);

        assert!(
            crate::utility_ai::tampering_targets(&app, boyle, beaker),
            "the real plugin must offer the armed actor the beaker it can see"
        );
    }

    /// The buffer clear runs before the providers, not after.
    ///
    /// Ordering these two the wrong way round is a single-line mistake that
    /// deletes every opportunity in the game every frame, with no panic and no
    /// failing unit test. Running several frames and still seeing the offer is
    /// what rules it out.
    #[test]
    fn the_opportunity_survives_the_per_frame_buffer_clear() {
        let mut app = station();
        let boyle = resident(&mut app, "Tech Boyle", "Engineering", in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        arm(&mut app, boyle, 6, 0.0);

        for _ in 0..5 {
            tick(&mut app, 0.05);
            assert!(
                crate::utility_ai::tampering_targets(&app, boyle, beaker),
                "a standing opportunity must be re-offered every frame"
            );
        }
    }

    /// A witness in the room stops it, through the real plugin.
    ///
    /// The veto is packet C's, and packet C tests it in isolation. What this
    /// adds is that the `CovertSight` param resolves correctly when it is built
    /// by the real schedule against a world containing every other utility
    /// system's components.
    #[test]
    fn a_watcher_in_the_room_prevents_the_offer() {
        let mut app = station();
        let boyle = resident(&mut app, "Tech Boyle", "Engineering", in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        resident(&mut app, "Dr. Vance", "Medical", beside_the_bench());
        arm(&mut app, boyle, 6, 0.0);
        tick(&mut app, 0.05);

        assert!(
            !crate::utility_ai::tampering_targets(&app, boyle, beaker),
            "someone watching must cost the actor the opportunity"
        );
    }

    /// The window closing ends the attempt, through the real expiry system.
    #[test]
    fn the_window_closes_on_its_own() {
        let mut app = station();
        let boyle = resident(&mut app, "Tech Boyle", "Engineering", in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        arm(&mut app, boyle, 6, 0.0);
        tick(&mut app, 0.05);
        assert!(crate::utility_ai::tampering_targets(&app, boyle, beaker));

        // Past the 120 s window, in steps, as ordinary time passes.
        for _ in 0..30 {
            tick(&mut app, 5.0);
        }

        assert!(
            crate::utility_ai::offered_tampering(&app, boyle).is_none(),
            "an expired authorization must stop being offered"
        );
        assert!(
            app.world().get::<TamperAuthorization>(boyle).is_none(),
            "the real expiry system must clear the spent window"
        );
        assert_eq!(
            volume(&app, beaker),
            chem_sim::Units::whole(30),
            "nothing may have been added to the batch"
        );
    }

    /// Completing the act mutates the real container through real chemistry.
    ///
    /// The executor is the real one: the fixture emits the resolution the action
    /// lifecycle emits and lets `apply_container_tampering` do its own arrival
    /// rechecks. It does not touch the solution.
    #[test]
    fn a_completed_act_changes_the_batch_and_spends_the_allotment() {
        let mut app = station();
        let boyle = resident(&mut app, "Tech Boyle", "Engineering", in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        arm(&mut app, boyle, 6, 0.0);
        tick(&mut app, 0.05);

        let before = volume(&app, beaker);
        assert!(crate::utility_ai::complete_offered_tampering(&mut app, boyle));
        tick(&mut app, 0.016);

        assert!(
            volume(&app, beaker) > before,
            "a completed act must actually reach the glassware"
        );
        assert!(
            app.world().get::<TamperAuthorization>(boyle).is_none(),
            "one authorization funds one act"
        );
    }

    /// Private decision state stays off the wire.
    ///
    /// The structural replication test in `mod.rs` covers the kernel's own
    /// components. These are the ones this phase added, and the failure mode is
    /// the worst kind: a client that can read the server's private intent would
    /// let a player see an antagonist arming before anything visible happens.
    #[test]
    fn the_new_private_state_is_not_replicated() {
        use bevy_replicon::shared::replication::rules::ReplicationRules;

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::state::app::StatesPlugin,
            RepliconSharedPlugin::default(),
            UtilityAiPlugin,
        ));

        let private = [
            app.world_mut().register_component::<TamperAuthorization>(),
            app.world_mut()
                .register_component::<crate::utility_ai::CovertGoal>(),
            app.world_mut()
                .register_component::<crate::orders::RetainsDelivery>(),
        ];
        let rules = app.world().resource::<ReplicationRules>();
        let replicated = |id| {
            rules
                .iter()
                .any(|rule| rule.components.iter().any(|component| component.id == id))
        };

        assert!(
            private.into_iter().all(|id| !replicated(id)),
            "authorization, motive and delivery intent are all private"
        );
    }

    // -----------------------------------------------------------------------
    // Tuning, measured rather than assumed
    // -----------------------------------------------------------------------

    /// Runs an armed, posted Boyle through the real selector and reports what
    /// he chose.
    ///
    /// The post matters. Without an entry in `Departments` a resident has
    /// nowhere to stand, `MaintainPost` is never even offered, and tampering
    /// wins by being the only candidate on the board — which measures nothing
    /// about its appeal. Giving him a real post is what puts an actual
    /// competitor in the comparison.
    fn what_boyle_chooses() -> Option<crate::utility_ai::UtilityActionId> {
        let mut app = station();
        app.world_mut()
            .resource_mut::<crate::crew::Departments>()
            .set("Engineering".into(), in_the_bay());
        let boyle = resident(&mut app, "Tech Boyle", "Engineering", in_the_bay());
        let profile = crate::utility_ai::scripted_residents::assistant_profile("Tech Boyle")
            .expect("Boyle is a scripted resident");
        app.world_mut().entity_mut(boyle).insert(profile);
        loose_batch(&mut app, on_the_bench());
        arm(&mut app, boyle, 6, 0.0);

        // Several decision ticks, so the phase-offset clock comes due.
        for _ in 0..10 {
            tick(&mut app, 0.5);
            if let Some(action) = app.world().get::<crate::utility_ai::CurrentAction>(boyle) {
                return Some(action.key.action);
            }
        }
        None
    }

    /// The authored appeal beats standing at a post — measured, not assumed.
    ///
    /// `TAMPER_APPEAL` was authored at 0.65 with a comment claiming it sits
    /// "below urgent department work and above idling". Nothing checked it, and
    /// an appeal too low to ever win produces an antagonist who is armed, in
    /// the room, unobserved, and does nothing forever — indistinguishable from
    /// a broken provider, and impossible to notice from a passing test.
    ///
    /// The competitor here is `MaintainPost` at `MAINTAIN_POST_WEIGHT` (0.25),
    /// which is what an idle posted resident does with their time. This asserts
    /// the ordering between the two is the authored one.
    ///
    /// Deliberately paired with the negative control below: this test alone
    /// would still pass if tampering were the only candidate, so on its own it
    /// would be measuring plumbing and calling it tuning.
    #[test]
    fn the_authored_appeal_beats_standing_at_a_post() {
        assert_eq!(
            what_boyle_chooses(),
            Some(crate::utility_ai::UtilityActionId::TamperContainer),
            "an armed, unobserved actor with only post-standing to do should take the chance"
        );
    }

    /// The negative control: an unarmed Boyle stands at his post instead.
    ///
    /// This is what makes the test above a measurement. It proves
    /// `MaintainPost` really is on the board in this fixture and really is
    /// selectable — so when tampering wins, it wins a contest rather than
    /// walking over.
    #[test]
    fn without_an_authorization_the_same_actor_just_works() {
        let mut app = station();
        app.world_mut()
            .resource_mut::<crate::crew::Departments>()
            .set("Engineering".into(), in_the_bay());
        let boyle = resident(&mut app, "Tech Boyle", "Engineering", in_the_bay());
        let profile = crate::utility_ai::scripted_residents::assistant_profile("Tech Boyle")
            .expect("Boyle is a scripted resident");
        app.world_mut().entity_mut(boyle).insert(profile);
        loose_batch(&mut app, on_the_bench());
        // No `arm` call: same body, same room, same beaker, no authorization.

        let mut chosen = None;
        for _ in 0..10 {
            tick(&mut app, 0.5);
            if let Some(action) = app.world().get::<crate::utility_ai::CurrentAction>(boyle) {
                chosen = Some(action.key.action);
                break;
            }
        }

        assert_eq!(
            chosen,
            Some(crate::utility_ai::UtilityActionId::MaintainPost),
            "an unarmed resident has ordinary work to prefer, and must prefer it"
        );
    }

    // -----------------------------------------------------------------------
    // Custody arithmetic against a real capacity limit
    // -----------------------------------------------------------------------

    /// A destination that is nearly full hands the remainder back to custody.
    ///
    /// `IllicitCustody::spend` is the only place in the covert path that has to
    /// reason about a refusal, and it was untested: every other test hands it a
    /// `Solution::unbounded()`, so `add` never refuses anything and the whole
    /// refund branch is dead code as far as the suite is concerned.
    ///
    /// The distinction it protects is not cosmetic. `add` returns the overflow
    /// it *refused*, not the amount it accepted — a natural misreading that
    /// inverts the arithmetic. Under that misreading an antagonist who tips a
    /// dose into a bowl with no room would be recorded as having spent their
    /// whole batch on nothing, and their one irreplaceable player-supplied
    /// contaminant would evaporate.
    #[test]
    fn a_dose_a_full_destination_refuses_stays_in_custody() {
        use crate::utility_ai::{CustodyState, IllicitCustody, IllicitStock};

        let data = chem();
        let reagent = data.reagent("water");
        let holder = Entity::from_raw_u32(3).unwrap();

        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(reagent, chem_sim::Units::whole(20));
        let mut custody = IllicitCustody::default();
        custody.receive_from_player(
            holder,
            IllicitStock {
                solution,
                state: CustodyState::Carried,
                source_player: None,
                received_at: 0.0,
                claimed_label: "for the beds".into(),
            },
        );

        // Room for 2 of the 8 offered.
        let mut destination = chem_sim::Solution::new(chem_sim::Units::whole(10));
        let _ = destination.add(reagent, chem_sim::Units::whole(8));

        let landed = custody.spend(
            holder,
            reagent,
            chem_sim::Units::whole(8),
            &mut destination,
        );

        assert_eq!(
            landed,
            chem_sim::Units::whole(2),
            "only what the destination had room for counts as spent"
        );
        assert_eq!(
            destination.total_volume(),
            chem_sim::Units::whole(10),
            "the destination is full, not overfilled"
        );
        assert_eq!(
            custody
                .held_by(holder)
                .next()
                .expect("the batch is still held")
                .solution
                .volume_of(reagent),
            chem_sim::Units::whole(18),
            "the refused remainder returns to custody rather than vanishing"
        );
    }

    /// Draining a batch exactly marks it consumed, and it cannot fund a second
    /// act. The counterpart to the refund case: the boundary in the other
    /// direction.
    #[test]
    fn a_batch_spent_to_nothing_cannot_fund_a_second_act() {
        use crate::utility_ai::{CustodyState, IllicitCustody, IllicitStock};

        let data = chem();
        let reagent = data.reagent("water");
        let holder = Entity::from_raw_u32(4).unwrap();

        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(reagent, chem_sim::Units::whole(5));
        let mut custody = IllicitCustody::default();
        custody.receive_from_player(
            holder,
            IllicitStock {
                solution,
                state: CustodyState::Carried,
                source_player: None,
                received_at: 0.0,
                claimed_label: "for the beds".into(),
            },
        );

        let mut first = chem_sim::Solution::unbounded();
        assert_eq!(
            custody.spend(holder, reagent, chem_sim::Units::whole(5), &mut first),
            chem_sim::Units::whole(5)
        );

        let mut second = chem_sim::Solution::unbounded();
        assert_eq!(
            custody.spend(holder, reagent, chem_sim::Units::whole(1), &mut second),
            chem_sim::Units::ZERO,
            "an emptied batch funds nothing further"
        );
    }

    /// Poisoning is investigable in a real app.
    ///
    /// Packet E's eligibility change is a one-line edit to a match arm, which is
    /// exactly the kind of thing a later refactor drops. Asserting it through
    /// the real `IncidentKind` rather than a local copy is the point.
    #[test]
    fn poisoning_is_an_investigable_incident_kind() {
        assert!(
            super::super::interviews::worth_investigating(IncidentKind::Poisoning),
            "an ordinary poisoning must be investigable, culprit or not"
        );
    }
}
