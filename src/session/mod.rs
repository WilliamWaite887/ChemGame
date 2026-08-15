//! Leaving the lab, and coming back to a clean one.
//!
//! Until quitting to the menu existed, `AppState::Playing` was entered once
//! per process and never left, so every resource a session mutates could
//! simply accumulate. Now that a player can open a save, quit, and open a
//! different one, all of that has to be put back — and the failure mode when
//! it is not is the worst kind this project keeps hitting: no error, no
//! warning, just a new career quietly carrying the last one's standing,
//! addicts and antagonist.
//!
//! Two halves:
//!
//! - **Entities** are handled by `crate::until_we_leave_the_lab()`, a
//!   `DespawnOnExit(AppState::Playing)` marker every session spawn carries.
//!   Bevy despawns them during the state transition; nothing here does.
//! - **Resources** are handled by [`clear_session_state`] below, which is
//!   deliberately *one list in one place* rather than a reset system in each
//!   of fifteen modules. A missing line here is a bug you can see by reading;
//!   a missing system over there is a bug nobody finds.
//!
//! Content resources — `ChemDb`, `StationData`, `ProduceCatalog`, every
//! `Script` — are deliberately **not** cleared. They are the game's data, not
//! the session's, and reloading them on every trip through the menu would cost
//! a stall for nothing. What *is* rebuilt per session are the clocks that hang
//! off them, which each owning module re-arms on `OnEnter(AppState::Playing)`
//! — see `orders::rearm_arrival_clocks` and its seven siblings.

use bevy::prelude::*;

use crate::AppState;

pub struct SessionPlugin;

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnExit(AppState::Playing), clear_session_state);
    }
}

/// Puts every resource a session writes back the way it started.
///
/// Split in two by how the game reads each resource, which decides what is
/// safe to do with it:
///
/// - Anything read as a bare `Res<T>`/`ResMut<T>` anywhere **must be reset**,
///   never removed — removing it would panic the next system that looks at it.
/// - Anything read only as `Option<Res<T>>` is **removed**, which is stronger:
///   `arc::assign_campaign` specifically needs `Campaign` to be *absent* to
///   roll a new one, so resetting it to some default would silently keep every
///   later save on the first one's antagonist.
fn clear_session_state(world: &mut World) {
    // -- reset: read non-optionally somewhere ------------------------------
    reset::<crate::orders::Shift>(world);
    reset::<crate::radio::RadioLog>(world);
    reset::<crate::radio::PendingBroadcasts>(world);
    reset::<crate::addiction::Addictions>(world);
    reset::<crate::antagonist::UnderworldStanding>(world);
    reset::<crate::antagonist::SecuritySuspicion>(world);
    reset::<crate::security::RaidSchedule>(world);
    reset::<crate::rogue_security::RogueRedeemed>(world);
    reset::<crate::arc::ThwartedAntags>(world);
    reset::<crate::arc::DriftClock>(world);
    reset::<crate::shift::CurrentForecast>(world);
    reset::<crate::body::MetabolismClock>(world);
    reset::<crate::hazards::IncidentSchedule>(world);
    reset::<crate::crisis::CrisisSchedule>(world);
    reset::<crate::shift::PendingRestock>(world);
    reset::<crate::crew::Departments>(world);

    // The authored-chain counters. Each is "how far into this thread's script
    // the career has reached", restored from `progress.ron` on load — so a new
    // save that has no `progress.ron` must start them at zero rather than
    // inherit the last career's place in the story.
    reset::<crate::obsessed::ObsessedProgress>(world);
    reset::<crate::cult::CultProgress>(world);
    reset::<crate::smuggler::SmugglerProgress>(world);
    reset::<crate::saboteur::SaboteurProgress>(world);
    reset::<crate::quack::QuackProgress>(world);

    // System state that used to be `Local` and had to stop being, precisely
    // because a `Local` cannot be reached from here. See each type's own doc.
    reset::<crate::shift::PersistedProgress>(world);
    reset::<crate::shift::ThwartingRecorded>(world);
    reset::<crate::shift::ForecastClock>(world);
    reset::<crate::hazards::IncidentGrace>(world);
    reset::<crate::addiction::CarriedSuspicion>(world);
    reset::<crate::ui::LastPanel>(world);
    reset::<crate::ui::LastSignState>(world);
    // Holds the *last* arc's ending, and the outcome it was watching for. A
    // leftover one would have the next save's ending screen ready to draw
    // somebody else's antagonist.
    reset::<crate::ending::FinishedArc>(world);

    // -- remove: always read as `Option<Res<T>>` ---------------------------
    // The campaign has to be *gone*, not defaulted: `arc::assign_campaign`
    // rolls a new antagonist only when there is no campaign at all.
    world.remove_resource::<crate::arc::Campaign>();
    world.remove_resource::<crate::arc::CampaignChoice>();
    // The menu chooses the slot; carrying the old one over would write this
    // career into the last one's files.
    world.remove_resource::<crate::saves::SaveSlot>();
    // Armed fresh on the way back in by each thread's own `arm_spawner`.
    world.remove_resource::<crate::showdown::Showdown>();
}

/// Puts one resource back to its default, if the app has it at all.
///
/// `init_resource` rather than `insert_resource` would leave an existing one
/// alone, which is the opposite of what this wants.
fn reset<T: Resource + Default>(world: &mut World) {
    world.insert_resource(T::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arc::{AntagId, Campaign, Mode};
    use crate::orders::{Department, Shift};
    use crate::radio::{RadioEntry, RadioLog};

    /// A world holding every resource `clear_session_state` touches, dirtied.
    fn dirty_world() -> World {
        let mut app = App::new();
        app.add_plugins(crate::session::SessionPlugin);
        let world = app.world_mut();

        world.insert_resource(Shift {
            succeeded: 41,
            botched: 7,
            shift_number: 9,
            opened_at: Some(crate::orders::ShiftSnapshot {
                succeeded: 38,
                ..default()
            }),
            called: true,
            ..default()
        });
        world
            .resource_mut::<Shift>()
            .adjust(Department::Medical, -6);
        world.insert_resource(RadioLog::default());
        world.resource_mut::<RadioLog>().push(RadioEntry {
            channel: "COM".to_string(),
            text: "from the last career".to_string(),
            good: true,
        });
        world.insert_resource(crate::antagonist::UnderworldStanding(9));
        world.insert_resource(crate::arc::ThwartedAntags(vec![AntagId::Cult]));
        world.insert_resource(Campaign::new(AntagId::Blob, Mode::Chemist, 3));
        world.insert_resource(crate::saves::SaveSlot::new("Somebody Else"));
        world.insert_resource(crate::obsessed::ObsessedProgress(4));

        std::mem::take(world)
    }

    fn clear(world: &mut World) {
        world.run_system_once(clear_session_state).unwrap();
    }

    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn a_new_session_does_not_inherit_the_last_ones_career() {
        let mut world = dirty_world();
        clear(&mut world);

        let shift = world.resource::<Shift>();
        assert_eq!(shift.succeeded, 0);
        assert_eq!(shift.botched, 0);
        assert_eq!(shift.standing(Department::Medical), 0);
        assert!(!shift.accepting_orders, "the sign starts down, so the player opens it when ready");
        // The debrief is a difference against `opened_at`. Carrying the last
        // career's snapshot over would make the next save's first debrief read
        // as a career of deliveries it never made.
        assert_eq!(shift.shift_number, 1);
        assert_eq!(shift.opened_at, None);
        assert!(!shift.called);
    }

    #[test]
    fn the_radio_does_not_carry_the_last_labs_chatter() {
        let mut world = dirty_world();
        clear(&mut world);

        assert!(world.resource::<RadioLog>().entries.is_empty());
    }

    #[test]
    fn the_campaign_is_removed_rather_than_defaulted() {
        // The load-bearing one. `arc::assign_campaign` rolls an antagonist
        // only when there is no `Campaign` at all — a defaulted one would
        // silently pin every save after the first to the same antagonist.
        let mut world = dirty_world();
        clear(&mut world);

        assert!(
            world.get_resource::<Campaign>().is_none(),
            "a leftover campaign would stop the next save ever rolling its own"
        );
    }

    #[test]
    fn the_save_slot_is_dropped_so_the_next_career_writes_its_own_files() {
        let mut world = dirty_world();
        clear(&mut world);

        assert!(world.get_resource::<crate::saves::SaveSlot>().is_none());
    }

    #[test]
    fn hidden_meters_and_authored_chain_progress_reset_too() {
        // These are the ones with no on-screen presence at all, which is
        // exactly why forgetting them would never be noticed.
        let mut world = dirty_world();
        clear(&mut world);

        assert_eq!(world.resource::<crate::antagonist::UnderworldStanding>().0, 0);
        assert_eq!(world.resource::<crate::obsessed::ObsessedProgress>().0, 0);
    }

    // -- entity teardown ---------------------------------------------------

    /// Just enough app for a real `AppState` transition to happen, which is
    /// what drives Bevy's own `DespawnOnExit` handling.
    fn state_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
            .init_state::<AppState>();
        app.update();
        app
    }

    fn enter_the_lab(app: &mut App) {
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        app.update();
    }

    fn leave_the_lab(app: &mut App) {
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::MainMenu);
        app.update();
    }

    #[test]
    fn everything_marked_for_the_session_is_gone_when_the_session_is() {
        let mut app = state_app();
        enter_the_lab(&mut app);
        let tagged = app.world_mut().spawn(crate::until_we_leave_the_lab()).id();
        let untagged = app.world_mut().spawn_empty().id();

        leave_the_lab(&mut app);

        assert!(
            app.world().get_entity(tagged).is_err(),
            "a session entity survived the trip to the menu"
        );
        assert!(
            app.world().get_entity(untagged).is_ok(),
            "the marker must not despawn things that never claimed it"
        );
    }

    #[test]
    fn a_session_entitys_children_go_with_it() {
        // Only roots carry the marker — every mesh, liquid and body part in the
        // game is a child, and the rule only holds if the hierarchy follows.
        let mut app = state_app();
        enter_the_lab(&mut app);
        let root = app.world_mut().spawn(crate::until_we_leave_the_lab()).id();
        let child = app.world_mut().spawn(ChildOf(root)).id();

        leave_the_lab(&mut app);

        assert!(app.world().get_entity(child).is_err());
    }

    #[test]
    fn every_shared_spawn_helper_marks_what_it_spawns() {
        // The four helpers that between them account for almost everything
        // spawned while the game is running. A helper that forgets the marker
        // leaks one entity per beaker, per visitor, per plant — silently, and
        // into the *next* career.
        let mut app = state_app();
        enter_the_lab(&mut app);

        let entities = {
            let world = app.world_mut();
            let mut spawned = Vec::new();
            world.commands().queue(move |world: &mut World| {
                let mut commands = world.commands();
                crate::containers::spawn_container(
                    &mut commands,
                    crate::containers::ContainerKind::Beaker,
                    Vec3::ZERO,
                );
                crate::produce::spawn_produce(
                    &mut commands,
                    crate::produce::ProduceId(0),
                    Vec3::ZERO,
                );
                crate::crew::spawn_crew_member(
                    &mut commands,
                    &crate::crew::CrewDef {
                        name: "Dr. Vance".into(),
                        role: "Medical".into(),
                        color: [0.0, 0.0, 0.0],
                    },
                    0.0,
                );
            });
            world.flush();

            let mut query = world.query_filtered::<Entity, With<Transform>>();
            for entity in query.iter(world) {
                spawned.push(entity);
            }
            spawned
        };
        assert_eq!(entities.len(), 3, "expected one entity per helper");

        leave_the_lab(&mut app);

        for entity in entities {
            assert!(
                app.world().get_entity(entity).is_err(),
                "{entity} outlived the session that spawned it"
            );
        }
    }

    #[test]
    fn the_cross_save_unlock_list_is_reread_rather_than_carried() {
        // `ThwartedAntags` is loaded from `saves/campaign.ron` by
        // `shift::load_progress` on the way in. Clearing it here is what makes
        // that read authoritative instead of merging with whatever the last
        // session happened to hold.
        let mut world = dirty_world();
        clear(&mut world);

        assert!(world.resource::<crate::arc::ThwartedAntags>().0.is_empty());
    }
}
