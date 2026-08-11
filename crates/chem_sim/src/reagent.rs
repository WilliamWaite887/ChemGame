//! Reagent definitions and the registry that interns them.
//!
//! Data files refer to reagents by string name; everything at runtime uses
//! `ReagentId`, which is a plain index. The conversion happens once, at load.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::effect::ReagentEffect;
use crate::units::{Kelvin, Units};

/// Units a body works through per metabolism tick when a reagent does not say
/// otherwise. /tg/station's default, and the rate every medicine is balanced
/// against.
pub const DEFAULT_METABOLISM: Units = Units::from_raw(40);

/// An interned reagent handle. Cheap to copy, compare and sort.
///
/// Serialisable because solutions cross the wire in co-op. The id is a
/// position in the loaded reagent list, so both ends must agree on the data
/// files — which is enforced by replicon's protocol hash, not by us.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
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
    /// Whether it comes out of ground produce rather than the dispenser.
    ///
    /// The sibling of `dispensable`: both mean "obtainable without a
    /// reaction", which is what stops the reachability guardrail flagging it
    /// as a dead end in the recipe graph. What does the grinding is the game's
    /// business, not this crate's.
    #[serde(default)]
    pub from_produce: bool,
    /// What this treats, shown in the reference book even while the recipe is
    /// still locked — a chemist knows what bicaridine is for even if they have
    /// forgotten how to make it.
    #[serde(default)]
    pub treats: Option<String>,

    // ---- Body effects -----------------------------------------------------
    //
    // Every field below is `#[serde(default)]`, which is what let this land
    // without touching a line of `chem.reagents.ron`: a reagent that says
    // nothing about bodies simply passes through one unchanged.
    /// Units a body works through per tick. `None` means
    /// [`DEFAULT_METABOLISM`]. Sugar burns off at 2.0, ethanol lingers at 0.2.
    #[serde(default)]
    pub metabolism: Option<Units>,
    /// What it does each tick while it is in a bloodstream.
    #[serde(default)]
    pub effects: Vec<ReagentEffect>,
    /// Applied **in addition to** `effects` once the dose passes `overdose`.
    ///
    /// Stacking rather than replacing is what makes an overdose read the way it
    /// should: the medicine is still working, it is just also hurting you now.
    #[serde(default)]
    pub overdose_effects: Vec<ReagentEffect>,
    /// A second, worse overdose tier, stacking on top of the first.
    #[serde(default)]
    pub critical_overdose: Option<Units>,
    #[serde(default)]
    pub critical_effects: Vec<ReagentEffect>,
    /// Above this it leaves the solution as gas. Plasma at 323.15K.
    ///
    /// Note for data files: RON will not coerce an integer into this `f32`.
    /// Write `Some((323.15))`, never `Some((323))`.
    #[serde(default)]
    pub boils_at: Option<Kelvin>,
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
    pub from_produce: bool,
    pub treats: Option<String>,
    pub metabolism: Option<Units>,
    pub effects: Vec<ReagentEffect>,
    pub overdose_effects: Vec<ReagentEffect>,
    pub critical_overdose: Option<Units>,
    pub critical_effects: Vec<ReagentEffect>,
    pub boils_at: Option<Kelvin>,
}

impl Reagent {
    /// How fast a body works through this, with the default filled in.
    pub fn rate(&self) -> Units {
        self.metabolism.unwrap_or(DEFAULT_METABOLISM)
    }

    /// Whether taking this can hurt you at any dose.
    ///
    /// True for anything with a harmful effect *or* an overdose threshold — a
    /// medicine you can overdose on is not safe to hand someone unasked.
    pub fn is_harmful(&self) -> bool {
        self.overdose.is_some()
            || self
                .effects
                .iter()
                .chain(&self.overdose_effects)
                .chain(&self.critical_effects)
                .any(|effect| effect.is_harmful())
    }
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
            from_produce: def.from_produce,
            treats: def.treats,
            metabolism: def.metabolism,
            effects: def.effects,
            overdose_effects: def.overdose_effects,
            critical_overdose: def.critical_overdose,
            critical_effects: def.critical_effects,
            boils_at: def.boils_at,
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

    /// Everything obtainable without running a reaction — the roots of the
    /// recipe graph. Anything outside this set and not produced by some
    /// reaction is unreachable.
    pub fn raw(&self) -> impl Iterator<Item = &Reagent> {
        self.reagents
            .iter()
            .filter(|r| r.dispensable || r.from_produce)
    }
}
