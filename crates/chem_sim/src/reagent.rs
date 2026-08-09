//! Reagent definitions and the registry that interns them.
//!
//! Data files refer to reagents by string name; everything at runtime uses
//! `ReagentId`, which is a plain index. The conversion happens once, at load.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::units::Units;

/// An interned reagent handle. Cheap to copy, compare and sort.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ReagentId(pub u32);

impl ReagentId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A reagent as written in `assets/data/reagents.ron`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReagentDef {
    /// Stable identifier used by reactions and save files.
    pub id: String,
    /// Name shown to the player.
    pub name: String,
    /// Liquid colour, used to tint the beaker contents.
    pub color: [f32; 3],
    /// Dose above which this does harm rather than good. `None` means safe at
    /// any dose.
    #[serde(default)]
    pub overdose: Option<Units>,
    /// Whether the base chemical dispenser can produce it directly.
    #[serde(default)]
    pub dispensable: bool,
    /// What this treats, shown in the reference book even while the recipe is
    /// still locked — a chemist knows what bicaridine is for even if they have
    /// forgotten how to make it.
    #[serde(default)]
    pub treats: Option<String>,
}

/// A loaded reagent.
#[derive(Clone, Debug)]
pub struct Reagent {
    pub id: ReagentId,
    pub key: String,
    pub name: String,
    pub color: [f32; 3],
    pub overdose: Option<Units>,
    pub dispensable: bool,
    pub treats: Option<String>,
}

/// Owns every known reagent and maps names to ids.
#[derive(Clone, Debug, Default)]
pub struct ReagentRegistry {
    reagents: Vec<Reagent>,
    by_key: HashMap<String, ReagentId>,
}

impl ReagentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a reagent, or returns the existing id if the key is already
    /// known (in which case the later definition is ignored).
    pub fn insert(&mut self, def: ReagentDef) -> ReagentId {
        if let Some(&existing) = self.by_key.get(&def.id) {
            return existing;
        }
        let id = ReagentId(self.reagents.len() as u32);
        self.by_key.insert(def.id.clone(), id);
        self.reagents.push(Reagent {
            id,
            key: def.id,
            name: def.name,
            color: def.color,
            overdose: def.overdose,
            dispensable: def.dispensable,
            treats: def.treats,
        });
        id
    }

    pub fn id_of(&self, key: &str) -> Option<ReagentId> {
        self.by_key.get(key).copied()
    }

    pub fn get(&self, id: ReagentId) -> &Reagent {
        &self.reagents[id.index()]
    }

    pub fn len(&self) -> usize {
        self.reagents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reagents.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Reagent> {
        self.reagents.iter()
    }

    /// Everything the base dispenser can produce.
    pub fn dispensable(&self) -> impl Iterator<Item = &Reagent> {
        self.reagents.iter().filter(|r| r.dispensable)
    }
}
