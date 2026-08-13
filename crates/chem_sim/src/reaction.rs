//! Reaction definitions.

use serde::{Deserialize, Serialize};

use crate::reagent::{ReagentId, ReagentRegistry};
use crate::solution::Solution;
use crate::thermal::Overheat;
use crate::units::{Kelvin, Units};

/// Index of a reaction within a [`ReactionSet`].
///
/// Serialisable for the same reason — and under the same contract — as
/// [`crate::ReagentId`]: co-op clients name a recipe over the wire when they
/// ask to buy a hint for it. The id is a position in the loaded reaction list,
/// so both ends must agree on the data files, which replicon's protocol hash
/// enforces rather than us.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize,
)]
pub struct ReactionId(pub u32);

impl ReactionId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Something a reaction does beyond producing reagent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ReactionEffect {
    /// Releases heat, raising the solution temperature by this many kelvin per
    /// unit of reaction.
    Heat(f32),
    /// Produces smoke with the given spread radius.
    Smoke(f32),
    /// Goes bang. The chemist's traditional reward for curiosity.
    Explosion(f32),
}

/// A reaction as written in `assets/data/reactions.ron`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReactionDef {
    pub id: String,
    /// Consumed, in ratio. `("oxygen", 1)` means one part oxygen.
    pub reactants: Vec<(String, Units)>,
    /// Must be present at at least this amount, but is **not** consumed and
    /// does not limit how much reaction can occur. One unit of plasma will
    /// catalyse any quantity of dexalin.
    #[serde(default)]
    pub catalysts: Vec<(String, Units)>,
    pub products: Vec<(String, Units)>,
    #[serde(default)]
    pub min_temp: Option<Kelvin>,
    #[serde(default)]
    pub max_temp: Option<Kelvin>,
    /// Past this the reaction **still runs**, but goes wrong.
    ///
    /// Deliberately not `max_temp`, which simply stops it. An overheat is the
    /// more interesting failure: the reactants are gone and you do not find out
    /// what it cost until you look at the yield.
    ///
    /// RON will not coerce an integer into this `f32`. Write `Some((420.0))`.
    #[serde(default)]
    pub overheat_temp: Option<Kelvin>,
    #[serde(default)]
    pub overheat: Overheat,
    /// Higher priority wins when two reactions compete for the same reagent.
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub effects: Vec<ReactionEffect>,
    /// Progressive hints shown in the reference book while this recipe is
    /// still locked, coarsest first.
    #[serde(default)]
    pub hints: Vec<String>,
}

/// A reaction with its reagent names resolved to ids.
#[derive(Clone, Debug)]
pub struct Reaction {
    pub id: ReactionId,
    pub key: String,
    pub reactants: Vec<(ReagentId, Units)>,
    pub catalysts: Vec<(ReagentId, Units)>,
    pub products: Vec<(ReagentId, Units)>,
    pub min_temp: Option<Kelvin>,
    pub max_temp: Option<Kelvin>,
    pub overheat_temp: Option<Kelvin>,
    pub overheat: Overheat,
    pub priority: i32,
    pub effects: Vec<ReactionEffect>,
    pub hints: Vec<String>,
}

impl Reaction {
    /// The largest multiplier this reaction can run at in `solution`, or `None`
    /// if it cannot run at all.
    ///
    /// The multiplier is itself a fixed-point value, so reactions can run
    /// fractionally: 0.5u each of a 1:1:1 recipe still reacts.
    pub fn max_scale(&self, solution: &Solution) -> Option<Units> {
        if let Some(min) = self.min_temp {
            if solution.temperature < min {
                return None;
            }
        }
        if let Some(max) = self.max_temp {
            if solution.temperature > max {
                return None;
            }
        }
        for &(id, required) in &self.catalysts {
            if !solution.contains_at_least(id, required) {
                return None;
            }
        }
        // A reaction with no reactants would scale infinitely.
        if self.reactants.is_empty() {
            return None;
        }

        let mut scale: Option<Units> = None;
        for &(id, required) in &self.reactants {
            if !required.is_positive() {
                return None;
            }
            let available = solution.volume_of(id);
            if !available.is_positive() {
                return None;
            }
            let limit = available.scaled(Units::ONE, required);
            scale = Some(match scale {
                Some(current) => current.min(limit),
                None => limit,
            });
        }

        scale.filter(|s| s.is_positive())
    }

    /// The reagents this reaction produces.
    pub fn product_ids(&self) -> impl Iterator<Item = ReagentId> + '_ {
        self.products.iter().map(|(id, _)| *id)
    }

    /// Share of the normal product yield at this temperature.
    ///
    /// [`Units::ONE`] unless the reaction names an `overheat_temp` and the
    /// solution is past it — so every recipe written before this existed is
    /// completely unaffected.
    pub fn yield_factor(&self, temperature: Kelvin) -> Units {
        match self.overheat_temp {
            Some(threshold) => self.overheat.yield_factor(threshold, temperature),
            None => Units::ONE,
        }
    }

    /// Whether this is hot enough to be going wrong right now.
    pub fn is_overheated(&self, temperature: Kelvin) -> bool {
        matches!(self.overheat_temp, Some(threshold) if temperature > threshold)
    }
}

/// Every known reaction.
#[derive(Clone, Debug, Default)]
pub struct ReactionSet {
    reactions: Vec<Reaction>,
}

impl ReactionSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves a definition's reagent names against `reagents`.
    pub fn insert(
        &mut self,
        def: ReactionDef,
        reagents: &ReagentRegistry,
    ) -> Result<ReactionId, ChemDataError> {
        let resolve =
            |pairs: &[(String, Units)]| -> Result<Vec<(ReagentId, Units)>, ChemDataError> {
                pairs
                    .iter()
                    .map(|(name, amount)| {
                        reagents.id_of(name).map(|id| (id, *amount)).ok_or_else(|| {
                            ChemDataError::UnknownReagent {
                                reaction: def.id.clone(),
                                reagent: name.clone(),
                            }
                        })
                    })
                    .collect()
            };

        let reactants = resolve(&def.reactants)?;
        let catalysts = resolve(&def.catalysts)?;
        let products = resolve(&def.products)?;

        if reactants.is_empty() {
            return Err(ChemDataError::NoReactants { reaction: def.id });
        }
        if products.is_empty() {
            return Err(ChemDataError::NoProducts { reaction: def.id });
        }

        let id = ReactionId(self.reactions.len() as u32);
        self.reactions.push(Reaction {
            id,
            key: def.id,
            reactants,
            catalysts,
            products,
            min_temp: def.min_temp,
            max_temp: def.max_temp,
            overheat_temp: def.overheat_temp,
            overheat: def.overheat,
            priority: def.priority,
            effects: def.effects,
            hints: def.hints,
        });
        Ok(id)
    }

    pub fn get(&self, id: ReactionId) -> &Reaction {
        &self.reactions[id.index()]
    }

    pub fn len(&self) -> usize {
        self.reactions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reactions.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Reaction> {
        self.reactions.iter()
    }

    pub fn find(&self, key: &str) -> Option<&Reaction> {
        self.reactions.iter().find(|r| r.key == key)
    }

    /// The reaction that produces `reagent` as one of its products, if any.
    ///
    /// Returns the first match if more than one reaction claims the same
    /// product — the data does not do this today (every reagent name appears
    /// in a `products` list at most once across `chem.reactions.ron`), but
    /// nothing in the type system enforces it, so this must never panic if
    /// it stops holding.
    pub fn producer_of(&self, reagent: ReagentId) -> Option<&Reaction> {
        self.reactions
            .iter()
            .find(|reaction| reaction.products.iter().any(|(id, _)| *id == reagent))
    }
}

/// Something wrong with the chemistry data files.
#[derive(Clone, Debug, PartialEq)]
pub enum ChemDataError {
    UnknownReagent { reaction: String, reagent: String },
    NoReactants { reaction: String },
    NoProducts { reaction: String },
}

impl std::fmt::Display for ChemDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChemDataError::UnknownReagent { reaction, reagent } => {
                write!(
                    f,
                    "reaction '{reaction}' refers to unknown reagent '{reagent}'"
                )
            }
            ChemDataError::NoReactants { reaction } => {
                write!(f, "reaction '{reaction}' has no reactants")
            }
            ChemDataError::NoProducts { reaction } => {
                write!(f, "reaction '{reaction}' has no products")
            }
        }
    }
}

impl std::error::Error for ChemDataError {}
