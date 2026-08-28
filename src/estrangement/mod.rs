//! Personal estrangement: what happens when one specific NPC's own hidden
//! standing sinks deep enough that it stops being a private grudge.
//!
//! `orders::Shift::npc_standing` gives every named crew member their own
//! hidden relationship with the player, separate from their department's
//! shared average — see `orders::Department::members`. This module is the
//! individual-relationship sibling of `orders::STANDING_FLOOR`'s "debt can
//! exist without being a wall" idea, but with a real consequence attached
//! rather than just a purchase gate — mirroring `rogue_security`'s own
//! `hostile_below`/`redeemed_at` two-threshold hysteresis, so crossing back
//! and forth right at a single boundary cannot flap.
//!
//! The consequence needs no new machinery: entering estrangement bumps
//! [`crate::antagonist::SecuritySuspicion`], which `security::schedule_raid`
//! already watches and already turns into a warning-then-officer-then-sweep
//! visit. Membership itself is a durable, queryable fact — `shift::
//! apply_npc_requisition` already refuses a purchase from someone estranged,
//! and a later pass can gate a hostile personal visit on it the same way
//! `rogue_security` gates one off `Department::Security` standing.

use std::collections::HashSet;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::antagonist::SecuritySuspicion;
use crate::net::is_authority;
use crate::orders::{Shift, StationData};
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::AppState;

pub struct EstrangementPlugin;

impl Plugin for EstrangementPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Estranged>().add_systems(
            Update,
            watch_estrangement
                .run_if(is_authority)
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// Standing below this enters estrangement. Deliberately between
/// `orders::STANDING_FLOOR` (-25) and `rogue_security`'s own
/// `hostile_below` (-8) — the same scale the codebase already established
/// for "this is a serious, not cosmetic, threshold."
pub const ESTRANGED_BELOW: i32 = -15;

/// Must climb back past this, not merely above [`ESTRANGED_BELOW`], to
/// leave — the same hysteresis gap `rogue_security`'s `hostile_below`/
/// `redeemed_at` uses, so hovering right at one boundary cannot flap in and
/// out every time an order resolves.
pub const RECONCILED_AT: i32 = -8;

/// Comparable to `antagonist::SUSPICION_PER_DELIVERY` (5) — a personal
/// relationship burned this badly is exactly as loud, once, as one illicit
/// sale.
const ESTRANGEMENT_SUSPICION: i32 = 5;

/// What entering estrangement nudges `instability::Instability` by — see
/// `instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN` for the same scale.
const ESTRANGEMENT_INSTABILITY: i32 = 4;

/// Who is currently estranged, by `station.crew.ron` name. A career fact,
/// persisted in `ProgressSave.estranged` — not reset when a shift is called,
/// the same reasoning `rogue_security::RogueRedeemed` uses.
#[derive(Resource, Default, Clone, Serialize, Deserialize)]
pub struct Estranged(pub HashSet<String>);

/// Watches every named crew member's individual standing and fires the
/// transition exactly once on each crossing, in either direction.
fn watch_estrangement(
    shift: Res<Shift>,
    station: Option<Res<StationData>>,
    mut estranged: ResMut<Estranged>,
    mut suspicion: ResMut<SecuritySuspicion>,
    mut instability: Option<ResMut<crate::instability::Instability>>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(station) = station else {
        return;
    };
    for member in &station.crew {
        let standing = shift.npc_standing(&member.name);
        let already = estranged.0.contains(&member.name);

        if !already && standing < ESTRANGED_BELOW {
            estranged.0.insert(member.name.clone());
            suspicion.0 += ESTRANGEMENT_SUSPICION;
            // A relationship burning this badly is itself evidence the crew
            // is fraying — a separate, station-wide signal from the personal
            // one `SecuritySuspicion` above already captures.
            if let Some(instability) = instability.as_mut() {
                crate::instability::nudge_instability(instability, ESTRANGEMENT_INSTABILITY);
            }
            radio.push(
                RadioEntry::new(
                    channel_for(&member.role),
                    format!("{} isn't dealing with the chemist anymore.", member.name),
                )
                .speaker(&member.name)
                .negative(),
            );
        } else if already && standing >= RECONCILED_AT {
            // Reconciling is quiet on purpose — the estrangement itself was
            // the loud beat; climbing back out is just the door reopening.
            estranged.0.remove(&member.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::CrewDef;
    use crate::orders::OrderConfig;

    fn crew() -> Vec<CrewDef> {
        ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap()
    }

    fn config() -> OrderConfig {
        ron::from_str(include_str!("../../assets/data/station.orders.ron")).unwrap()
    }

    /// Enough app to run `watch_estrangement` directly, headless.
    fn app() -> App {
        let mut app = App::new();
        app.insert_resource(StationData {
            crew: crew(),
            config: config(),
        })
        .init_resource::<Shift>()
        .init_resource::<Estranged>()
        .init_resource::<SecuritySuspicion>()
        .init_resource::<crate::instability::Instability>()
        .init_resource::<RadioLog>()
        .add_systems(Update, watch_estrangement);
        app
    }

    fn ivy(app: &mut App, standing: i32) {
        app.world_mut()
            .resource_mut::<Shift>()
            .npc_standing
            .insert("Botanist Ivy".to_string(), standing);
    }

    #[test]
    fn crossing_below_the_threshold_estranges_exactly_once() {
        let mut app = app();
        ivy(&mut app, ESTRANGED_BELOW - 1);

        app.update();
        app.update();
        app.update();

        assert!(app
            .world()
            .resource::<Estranged>()
            .0
            .contains("Botanist Ivy"));
        assert_eq!(
            app.world().resource::<SecuritySuspicion>().0,
            ESTRANGEMENT_SUSPICION,
            "several frames below the floor must not stack the bump"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
        assert_eq!(
            app.world().resource::<crate::instability::Instability>().level,
            ESTRANGEMENT_INSTABILITY,
            "several frames below the floor must not stack this bump either"
        );
    }

    #[test]
    fn hovering_between_the_two_thresholds_does_not_retrigger() {
        let mut app = app();
        ivy(&mut app, ESTRANGED_BELOW - 1);
        app.update();
        assert_eq!(app.world().resource::<SecuritySuspicion>().0, ESTRANGEMENT_SUSPICION);

        // Climbs back above `ESTRANGED_BELOW` but not past `RECONCILED_AT` —
        // must stay estranged, and must not fire a second bump.
        ivy(&mut app, ESTRANGED_BELOW + 1);
        app.update();

        assert!(app
            .world()
            .resource::<Estranged>()
            .0
            .contains("Botanist Ivy"));
        assert_eq!(app.world().resource::<SecuritySuspicion>().0, ESTRANGEMENT_SUSPICION);
    }

    #[test]
    fn reconciling_clears_membership_without_a_second_suspicion_event() {
        let mut app = app();
        ivy(&mut app, ESTRANGED_BELOW - 1);
        app.update();

        ivy(&mut app, RECONCILED_AT);
        app.update();

        assert!(!app
            .world()
            .resource::<Estranged>()
            .0
            .contains("Botanist Ivy"));
        assert_eq!(
            app.world().resource::<SecuritySuspicion>().0,
            ESTRANGEMENT_SUSPICION,
            "reconciling must not add or remove suspicion"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn the_plugin_initialises_every_resource_its_systems_need() {
        let mut app = App::new();
        app.add_plugins(EstrangementPlugin);
        assert!(
            app.world().get_resource::<Estranged>().is_some(),
            "EstrangementPlugin must initialise every resource it owns"
        );
    }
}
