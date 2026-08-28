//! The crew-instability meter: how close the station's own crew are to
//! falling apart on their own, entirely separate from a main antagonist's
//! plot meter (`arc::Campaign::plot`).
//!
//! Fed from several other modules' own resolution systems — the same
//! "ignored shenanigan" moment `arc::note_ignored_shenanigan` already
//! watches (`smuggler`, `saboteur`, `quack`), a personal relationship
//! burning (`estrangement`), a raid actually firing (`security`), a rogue
//! officer's shakedown (`rogue_security`), and the culmination of the
//! `obsessed` thread's own escalating sequence. None of those modules know
//! about each other; this module knows about none of them either — every
//! feeder is a call to [`nudge_instability`] at its own resolution site,
//! mirroring how `arc::nudge_plot` is fed from outside `arc` itself.
//!
//! Fully hidden by design, per the user's own resolved answer during
//! planning: no number is ever shown, and there is no network sync message
//! for it — matching `arc::Campaign::plot`'s own "never shown as a number"
//! precedent and `antagonist::UnderworldStanding`'s server-only,
//! never-replicated persistence pattern. The only player-facing signal is
//! one radio line the first time the meter reaches [`InstabilityTier::Fraying`],
//! and, once [`InstabilityTier::Breaking`] is reached, evacuation itself
//! (`ending::watch_for_evacuation`) — that screen is the payoff, so
//! `Breaking` airs no line of its own here.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::net::is_authority;
use crate::radio::{RadioChannel, RadioEntry, RadioLog};
use crate::AppState;

pub struct InstabilityPlugin;

impl Plugin for InstabilityPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Instability>().add_systems(
            Update,
            watch_instability
                .run_if(is_authority)
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// The top of the meter. Reaching it is [`InstabilityTier::Breaking`], which
/// `ending::watch_for_evacuation` watches for as the second of the two loss
/// paths, alongside `arc::Campaign::plot` reaching `arc::PLOT_MAX`.
pub const INSTABILITY_MAX: i32 = 100;

/// Crossed once the meter climbs into [`InstabilityTier::Fraying`]. Well
/// under [`INSTABILITY_MAX`] on purpose — three department minors firing
/// independently inside one shift should not alone reach it, but a genuine
/// pattern of ignored trouble should.
const FRAYING_AT: i32 = 35;

/// What `smuggler`/`saboteur`/`quack` each nudge by on an ignored visit —
/// the same moment `arc::note_ignored_shenanigan` already watches. A small
/// delta: three departments firing independently should not alone reach
/// [`FRAYING_AT`] inside one shift.
pub const INCOMPETENCE_PER_IGNORED_SHENANIGAN: i32 = 4;

/// How close the crew are to falling apart on their own. Never shown as a
/// number anywhere in the UI — see the module doc.
#[derive(Resource, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Instability {
    /// 0..=[`INSTABILITY_MAX`].
    pub level: i32,
    pub tier: InstabilityTier,
}

/// The tiered state this meter climbs through, shaped like `arc::Reveal`'s
/// own `Hidden -> Suspected -> Named` — but for "the crew is falling apart"
/// rather than "someone is plotting."
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
pub enum InstabilityTier {
    /// Nothing — the meter is fully invisible at this tier.
    #[default]
    Calm,
    /// Misdelivery (`orders::adjust_for_role`'s probabilistic grading
    /// substitution) becomes possible.
    Fraying,
    /// Triggers evacuation.
    Breaking,
}

/// Moves the meter. `pub` so every feeder — scattered one call site at a
/// time across other modules' own resolution systems — can reach it without
/// this module having to import any of them.
pub fn nudge_instability(instability: &mut Instability, delta: i32) {
    instability.level = (instability.level + delta).clamp(0, INSTABILITY_MAX);
}

/// Steps `Calm -> Fraying -> Breaking` one tier at a time, airing the
/// `Fraying` line exactly once. Mirrors `arc::update_reveal`'s own "step one
/// tier at a time even on a big jump, and never regress" idiom precisely —
/// the tier, once reached, never un-reaches, even though `level` itself
/// could in principle be nudged back down.
fn watch_instability(mut instability: ResMut<Instability>, mut radio: ResMut<RadioLog>) {
    let reached = if instability.level >= INSTABILITY_MAX {
        InstabilityTier::Breaking
    } else if instability.level >= FRAYING_AT {
        InstabilityTier::Fraying
    } else {
        InstabilityTier::Calm
    };
    if reached <= instability.tier {
        return;
    }

    let next = match instability.tier {
        InstabilityTier::Calm => InstabilityTier::Fraying,
        InstabilityTier::Fraying => InstabilityTier::Breaking,
        InstabilityTier::Breaking => return,
    };
    instability.tier = next;

    if next == InstabilityTier::Fraying {
        // Deliberately vaguer than any of `arc`'s own reveal lines — this is
        // a mood, not a whodunit, and names nobody.
        radio.push(
            RadioEntry::new(
                RadioChannel::Common,
                "Somebody's slipping. Nobody's saying who.".to_string(),
            )
            .negative(),
        );
    }
    // `Breaking` airs no line of its own — the evacuation screen is the
    // payoff, and this module has no idea evacuation exists.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<Instability>()
            .init_resource::<RadioLog>()
            .add_systems(Update, watch_instability);
        app
    }

    #[test]
    fn nudging_clamps_to_the_meters_own_range() {
        let mut instability = Instability::default();
        nudge_instability(&mut instability, -10);
        assert_eq!(instability.level, 0, "the meter must not go negative");

        nudge_instability(&mut instability, INSTABILITY_MAX + 50);
        assert_eq!(instability.level, INSTABILITY_MAX);
    }

    #[test]
    fn fraying_airs_its_line_exactly_once() {
        let mut app = app();
        app.world_mut().resource_mut::<Instability>().level = FRAYING_AT;

        app.update();
        app.update();
        app.update();

        assert_eq!(
            app.world().resource::<Instability>().tier,
            InstabilityTier::Fraying
        );
        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            1,
            "several frames past the threshold must not repeat the line"
        );
    }

    #[test]
    fn a_big_jump_still_passes_through_fraying_before_breaking() {
        // Mirrors `arc`'s own reveal test: jump straight to the top in one
        // nudge, and the player should still get the Fraying line rather
        // than skipping straight to Breaking with nothing said in between.
        let mut app = app();
        app.world_mut().resource_mut::<Instability>().level = INSTABILITY_MAX;

        app.update();
        assert_eq!(
            app.world().resource::<Instability>().tier,
            InstabilityTier::Fraying
        );
        app.update();
        assert_eq!(
            app.world().resource::<Instability>().tier,
            InstabilityTier::Breaking
        );

        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            1,
            "Breaking airs no line of its own"
        );
    }

    #[test]
    fn the_tier_never_regresses_even_if_the_level_drops_back_down() {
        let mut app = app();
        app.world_mut().resource_mut::<Instability>().level = FRAYING_AT;
        app.update();
        assert_eq!(
            app.world().resource::<Instability>().tier,
            InstabilityTier::Fraying
        );

        app.world_mut().resource_mut::<Instability>().level = 0;
        app.update();

        assert_eq!(
            app.world().resource::<Instability>().tier,
            InstabilityTier::Fraying,
            "the tier does not un-fray just because the level eased off"
        );
    }

    #[test]
    fn the_plugin_initialises_every_resource_its_systems_need() {
        let mut app = App::new();
        app.add_plugins(InstabilityPlugin);
        assert!(
            app.world().get_resource::<Instability>().is_some(),
            "InstabilityPlugin must initialise every resource it owns"
        );
    }
}
