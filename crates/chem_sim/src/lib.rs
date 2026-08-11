//! The chemistry simulation behind ChemGame.
//!
//! Deliberately engine-free: no bevy, no rendering, no globals. That keeps the
//! part of the game most likely to grow testable in milliseconds, and lets a
//! dedicated server run it headless when co-op arrives.
//!
//! ```
//! use chem_sim::{ChemData, Solution, Units, resolve};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let data = ChemData::from_ron(
//!     r#"[(id: "oxygen", name: "Oxygen", color: (0.6, 0.8, 1.0), dispensable: true)]"#,
//!     r#"[]"#,
//! )?;
//! let oxygen = data.reagents.id_of("oxygen").unwrap();
//!
//! let mut beaker = Solution::new(Units::whole(100));
//! let _ = beaker.add(oxygen, Units::whole(15));
//! let report = resolve(&mut beaker, &data.reactions);
//!
//! assert!(!report.reacted());
//! assert_eq!(beaker.volume_of(oxygen), Units::whole(15));
//! # Ok(())
//! # }
//! ```

pub mod body;
pub mod effect;
pub mod reaction;
pub mod reagent;
pub mod resolver;
pub mod solution;
pub mod thermal;
pub mod units;

pub use body::{
    blast_radius, explosion_damage, metabolise, Bloodstream, ExposureReport, Health, StatusState,
    TickReport, Vitals, COLLAPSE, MAX_DAMAGE_PER_KIND, RECOVER, TICK_SECONDS,
};
pub use effect::{Damage, DamageKind, ReagentEffect, Route, StatusKind};
pub use reaction::{ChemDataError, Reaction, ReactionDef, ReactionEffect, ReactionId, ReactionSet};
pub use reagent::{Category, Reagent, ReagentDef, ReagentId, ReagentRegistry, DEFAULT_METABOLISM};
pub use resolver::{resolve, ReactionEvent, ResolveReport, MAX_ITERATIONS};
pub use solution::Solution;
pub use thermal::{approach, boil_off, Overheat};
pub use units::{Kelvin, Units};

/// Every reagent and reaction in the game, loaded from data.
#[derive(Clone, Debug, Default)]
pub struct ChemData {
    pub reagents: ReagentRegistry,
    pub reactions: ReactionSet,
}

impl ChemData {
    pub fn from_defs(
        reagent_defs: Vec<ReagentDef>,
        reaction_defs: Vec<ReactionDef>,
    ) -> Result<Self, ChemDataError> {
        let mut reagents = ReagentRegistry::new();
        for def in reagent_defs {
            reagents.insert(def);
        }
        let mut reactions = ReactionSet::new();
        for def in reaction_defs {
            reactions.insert(def, &reagents)?;
        }
        Ok(ChemData {
            reagents,
            reactions,
        })
    }

    /// Loads from RON source. The game feeds the same strings in through
    /// bevy's asset pipeline so the data hot-reloads.
    pub fn from_ron(reagents: &str, reactions: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let reagent_defs: Vec<ReagentDef> = ron::from_str(reagents)?;
        let reaction_defs: Vec<ReactionDef> = ron::from_str(reactions)?;
        Ok(ChemData::from_defs(reagent_defs, reaction_defs)?)
    }

    /// Looks up a reagent id, panicking with a useful message if the key is
    /// wrong. For tests and hardcoded references to known reagents.
    pub fn reagent(&self, key: &str) -> ReagentId {
        self.reagents
            .id_of(key)
            .unwrap_or_else(|| panic!("no reagent '{key}' in the chemistry data"))
    }
}
