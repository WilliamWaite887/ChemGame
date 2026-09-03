//! Reaction definitions.

use std::collections::BTreeMap;

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
/// so both ends must agree on the data files. The game layer includes those
/// files in its LAN/Steam compatibility fingerprint.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ReactionId(pub u32);

impl ReactionId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// The physical behavior of a radial reaction pulse.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PulseKind {
    /// Throws nearby bodies away from the reaction.
    Push,
    /// Draws nearby bodies toward the reaction.
    Pull,
    /// Disorients nearby bodies without moving them.
    Concuss,
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
    /// Data-authored energetic output. The resolver converts this to a final
    /// `Explosion` using the amount that actually reacted, so batch size
    /// matters while overheat detonations retain their existing fixed power.
    ExplosionProfile { strength: f32, modifier: f32 },
    /// A resolved radial force or concussive pulse.
    Pulse { kind: PulseKind, power: f32 },
    /// Data-authored pulse output. As with `ExplosionProfile`, the resolver
    /// turns this into a concrete pulse whose power scales with batch size.
    PulseProfile {
        kind: PulseKind,
        strength: f32,
        modifier: f32,
    },
    /// A resolved electromagnetic pulse that disables nearby equipment.
    Emp(f32),
    /// Batch-scaled electromagnetic output authored in reaction data.
    EmpProfile { strength: f32, modifier: f32 },
    /// A resolved electrical arc that shocks bodies and equipment nearby.
    Electric(f32),
    /// Batch-scaled electrical output authored in reaction data.
    ElectricProfile { strength: f32, modifier: f32 },
    /// Material combustion: proportional burn energy, never a glass-destroying blast.
    Burn(f32),
}

/// How a recipe is allowed to begin in the lab.
///
/// Most chemistry is [`Ambient`](Self::Ambient): as soon as all ingredients
/// share a solution, the normal resolver may run it.  An
/// [`Agitated`](Self::Agitated) recipe is deliberately different. Its two
/// declared sides must exist in two separate solutions immediately before a
/// Mixing Chamber combines them. That pre-combination fact cannot be inferred
/// from the combined liquid, so [`ReactionSet::activate_agitation`] captures it
/// as a short-lived [`ReactionActivation`].
///
/// Amounts are preparation ratios as well as minimum amounts. Listing them
/// here, rather than only listing reagent names, makes catalysts such as the
/// plasma in dexalin explicit in the reference-book data.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ReactionProcessDef {
    #[default]
    Ambient,
    Agitated {
        side_a: Vec<(String, Units)>,
        side_b: Vec<(String, Units)>,
    },
}

/// A resolved [`ReactionProcessDef`].
///
/// Reagent names have become ids, so the game can inspect preparation sides
/// without doing string lookups every frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReactionProcess {
    Ambient,
    Agitated {
        side_a: Vec<(ReagentId, Units)>,
        side_b: Vec<(ReagentId, Units)>,
    },
}

impl ReactionProcess {
    pub fn is_ambient(&self) -> bool {
        matches!(self, Self::Ambient)
    }
}

/// Proof that one or more agitated recipes had their two sides prepared in
/// separate solutions.
///
/// This token is intentionally independent of a [`Solution`]. The game layer
/// creates it *before* transferring one Mixing Chamber beaker into the other,
/// stores it alongside the destination beaker while the batch is running, and
/// discards it when no activated reaction remains. Merely pouring the same
/// ingredients together never creates the token and therefore never unlocks
/// an agitated recipe.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionActivation {
    reactions: Vec<ReactionId>,
}

impl ReactionActivation {
    pub fn is_empty(&self) -> bool {
        self.reactions.is_empty()
    }

    pub fn contains(&self, reaction: ReactionId) -> bool {
        self.reactions.contains(&reaction)
    }

    pub fn reactions(&self) -> impl Iterator<Item = ReactionId> + '_ {
        self.reactions.iter().copied()
    }
}

/// A reaction as written in `assets/data/reactions.ron`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReactionDef {
    /// Legacy recipe identity retained for saved knowledge; material families perform it.
    #[serde(default)]
    pub material_only: bool,
    pub id: String,
    /// Consumed, in ratio. `("oxygen", 1)` means one part oxygen.
    pub reactants: Vec<(String, Units)>,
    /// Must be present at at least this amount, but is **not** consumed and
    /// does not limit how much reaction can occur. One unit of plasma will
    /// catalyse any quantity of dexalin.
    #[serde(default)]
    pub catalysts: Vec<(String, Units)>,
    pub products: Vec<(String, Units)>,
    /// Defaults to ordinary in-solution chemistry for compatibility with all
    /// reaction data written before staged mixing existed.
    #[serde(default)]
    pub process: ReactionProcessDef,
    #[serde(default)]
    pub min_temp: Option<Kelvin>,
    #[serde(default)]
    pub max_temp: Option<Kelvin>,
    /// Optional operating window for quality-controlled chemistry.
    #[serde(default)]
    pub min_ph: Option<f32>,
    #[serde(default)]
    pub optimal_ph: Option<f32>,
    #[serde(default)]
    pub max_ph: Option<f32>,
    /// Minimum volume-weighted input purity required to begin.
    #[serde(default)]
    pub min_purity: Option<f32>,
    /// pH movement per unit of reaction, applied after products form.
    #[serde(default)]
    pub ph_shift: f32,
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
    /// How fast this runs, in reaction-units per second.
    ///
    /// `None` — the default, and what every recipe written before this existed
    /// carries — means instantaneous: the reaction completes inside the call
    /// that noticed it could happen, exactly as chemistry in this crate always
    /// has. A rate turns the same recipe into something that takes real time,
    /// which is what makes a batch watchable, rushable, and interruptible.
    ///
    /// See [`crate::resolve_step`].
    #[serde(default)]
    pub rate: Option<Units>,
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
    pub material_only: bool,
    pub residue: bool,
    pub id: ReactionId,
    pub key: String,
    pub reactants: Vec<(ReagentId, Units)>,
    pub catalysts: Vec<(ReagentId, Units)>,
    pub products: Vec<(ReagentId, Units)>,
    product_ph: Vec<(ReagentId, f32)>,
    pub process: ReactionProcess,
    pub min_temp: Option<Kelvin>,
    pub max_temp: Option<Kelvin>,
    pub min_ph: Option<f32>,
    pub optimal_ph: Option<f32>,
    pub max_ph: Option<f32>,
    pub min_purity: Option<f32>,
    pub ph_shift: f32,
    pub overheat_temp: Option<Kelvin>,
    pub overheat: Overheat,
    pub priority: i32,
    /// See [`ReactionDef::rate`]. `None` is instantaneous.
    pub rate: Option<Units>,
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
        let ph = solution.ph();
        if self.min_ph.is_some_and(|minimum| ph < minimum)
            || self.max_ph.is_some_and(|maximum| ph > maximum)
        {
            return None;
        }
        if self
            .min_purity
            .is_some_and(|minimum| self.input_purity(solution) < minimum)
        {
            return None;
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

    /// Purity retained by products at the solution's current pH. The edge of
    /// an authored range keeps half the input quality, making imprecise normal
    /// chemistry recoverable rather than a binary failure.
    pub fn product_purity(&self, solution: &Solution) -> f32 {
        let input = self.input_purity(solution);
        let Some(optimum) = self.optimal_ph else {
            return input;
        };
        let ph = solution.ph();
        let span = if ph <= optimum {
            optimum - self.min_ph.unwrap_or(optimum)
        } else {
            self.max_ph.unwrap_or(optimum) - optimum
        };
        if span <= f32::EPSILON {
            return input;
        }
        let distance = ((ph - optimum).abs() / span).clamp(0.0, 1.0);
        (input * (1.0 - 0.5 * distance)).clamp(0.0, 1.0)
    }

    /// Volume-weighted quality of the stoichiometric reactants this reaction
    /// consumes. Unrelated filler and surviving catalysts are deliberately
    /// excluded: neither may launder a bad input across a purity gate or into
    /// a high-quality product.
    pub fn input_purity(&self, solution: &Solution) -> f32 {
        let total: f32 = self
            .reactants
            .iter()
            .map(|(_, required)| required.as_f32())
            .sum();
        if total <= 0.0 {
            return 1.0;
        }
        self.reactants
            .iter()
            .map(|(id, required)| solution.purity_of(*id) * required.as_f32() / total)
            .sum::<f32>()
            .clamp(0.0, 1.0)
    }

    pub(crate) fn product_ph(&self, reagent: ReagentId) -> f32 {
        self.product_ph
            .iter()
            .find_map(|(id, ph)| (*id == reagent).then_some(*ph))
            .unwrap_or(7.0)
    }

    /// The most this reaction may advance in `dt` seconds, or `None` for "as
    /// far as it will go" — which is both what an unrated reaction always
    /// means and what an infinite `dt` asks for.
    ///
    /// The result is floored at one raw hundredth whenever the rate and the
    /// step are both positive. Without that floor, a slow reaction on a fast
    /// machine rounds its per-frame share down to zero and never advances at
    /// all — the worst kind of bug, since it only appears above some frame
    /// rate. The floor puts a hard lower bound of `SCALE` hundredths per
    /// second on any rated reaction, so rates below roughly 1u/s are not
    /// meaningfully distinguishable from each other; the shipped data does not
    /// use any, and this is the reason not to.
    ///
    /// A `rate` of zero or less is a content bug. It is read as "instant"
    /// rather than "never", because a recipe that silently cannot be made is a
    /// far worse thing to ship than one that is merely faster than intended.
    pub fn step_limit(&self, dt: f32, temperature: Kelvin) -> Option<Units> {
        let rate = self.rate?;
        if !dt.is_finite() || !rate.is_positive() {
            return None;
        }
        let thermal_factor = self
            .min_temp
            .map(|minimum| (0.5 + (temperature.0 - minimum.0) / 100.0).clamp(0.5, 2.5))
            .unwrap_or(1.0);
        let allowance = Units::from_f64(rate.as_f64() * thermal_factor as f64 * dt.max(0.0) as f64);
        if dt > 0.0 && !allowance.is_positive() {
            return Some(Units::from_raw(1));
        }
        Some(allowance)
    }
}

/// Every known reaction.
#[derive(Clone, Debug, Default)]
pub struct ReactionSet {
    reactions: Vec<Reaction>,
    pub(crate) materials: crate::material::MaterialCatalog,
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
        let process = match &def.process {
            ReactionProcessDef::Ambient => ReactionProcess::Ambient,
            ReactionProcessDef::Agitated { side_a, side_b } => ReactionProcess::Agitated {
                side_a: resolve(side_a)?,
                side_b: resolve(side_b)?,
            },
        };

        if reactants.is_empty() {
            return Err(ChemDataError::NoReactants { reaction: def.id });
        }
        if products.is_empty() {
            return Err(ChemDataError::NoProducts { reaction: def.id });
        }
        let valid_ph = def.min_ph.is_none_or(|value| (0.0..=14.0).contains(&value))
            && def.max_ph.is_none_or(|value| (0.0..=14.0).contains(&value))
            && def
                .optimal_ph
                .is_none_or(|value| (0.0..=14.0).contains(&value))
            && match (def.min_ph, def.max_ph) {
                (Some(minimum), Some(maximum)) => minimum <= maximum,
                _ => true,
            }
            && def.optimal_ph.is_none_or(|optimum| {
                def.min_ph.is_none_or(|minimum| optimum >= minimum)
                    && def.max_ph.is_none_or(|maximum| optimum <= maximum)
            })
            && def
                .min_purity
                .is_none_or(|value| (0.0..=1.0).contains(&value));
        if !valid_ph {
            return Err(ChemDataError::InvalidQualityRange { reaction: def.id });
        }
        validate_process(&def.id, &reactants, &catalysts, &process)?;

        let product_ph = products
            .iter()
            .map(|(id, _)| (*id, reagents.get(*id).ph))
            .collect();

        let id = ReactionId(self.reactions.len() as u32);
        self.reactions.push(Reaction {
            material_only: def.material_only,
            residue: products
                .first()
                .is_some_and(|(id, _)| reagents.get(*id).material.residue),
            id,
            key: def.id,
            reactants,
            catalysts,
            products,
            product_ph,
            process,
            min_temp: def.min_temp,
            max_temp: def.max_temp,
            min_ph: def.min_ph,
            optimal_ph: def.optimal_ph,
            max_ph: def.max_ph,
            min_purity: def.min_purity,
            ph_shift: def.ph_shift,
            overheat_temp: def.overheat_temp,
            overheat: def.overheat,
            priority: def.priority,
            rate: def.rate,
            effects: def.effects,
            hints: def.hints,
        });
        Ok(id)
    }

    pub fn get(&self, id: ReactionId) -> &Reaction {
        &self.reactions[id.index()]
    }

    pub fn recipe_count(&self) -> usize {
        self.reactions.iter().filter(|r| !r.residue).count()
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

    /// Captures every agitated recipe whose declared sides are present in two
    /// separate solutions.
    ///
    /// Orientation is deliberately ignored. `side_a` and `side_b` describe
    /// recipe preparation groups, not physical slot names, so both "A -> B"
    /// and "B -> A" controls on a Mixing Chamber behave identically. Extra
    /// reagents are allowed (and remain contamination); each declared amount
    /// still has to be present on its own side.
    ///
    /// Call this before transferring either solution. The returned token is
    /// then passed to [`crate::resolve_step_with_activation`] while the
    /// combined batch runs.
    pub fn activate_agitation(&self, first: &Solution, second: &Solution) -> ReactionActivation {
        let reactions = self
            .reactions
            .iter()
            .filter_map(|reaction| {
                let ReactionProcess::Agitated { side_a, side_b } = &reaction.process else {
                    return None;
                };
                let forward = side_matches(first, side_a) && side_matches(second, side_b);
                let reversed = side_matches(first, side_b) && side_matches(second, side_a);
                (forward || reversed).then_some(reaction.id)
            })
            .collect();
        ReactionActivation { reactions }
    }

    /// The reaction whose primary product is `reagent`, if any.
    ///
    /// Only the first authored product defines the synthesis path. Later
    /// products are coproducts or waste: exposing one of those as a second
    /// producer would make recipe-tree recursion and order batch sizing depend
    /// on file order. Data tests keep primary synthesis unique.
    pub fn producer_of(&self, reagent: ReagentId) -> Option<&Reaction> {
        self.reactions.iter().find(|reaction| {
            reaction
                .products
                .first()
                .is_some_and(|(id, _)| *id == reagent)
        })
    }
}

fn side_matches(solution: &Solution, requirements: &[(ReagentId, Units)]) -> bool {
    let Some(&(reference_reagent, reference_required)) = requirements.first() else {
        return false;
    };
    if !reference_required.is_positive()
        || !solution.contains_at_least(reference_reagent, reference_required)
    {
        return false;
    }

    let reference_present = solution.volume_of(reference_reagent).raw() as i64;
    requirements.iter().all(|&(reagent, required)| {
        if !required.is_positive() || !solution.contains_at_least(reagent, required) {
            return false;
        }
        // Compare ratios by cross multiplication. This is exact for fixed-point
        // Units, independent of requirement order, and avoids float drift.
        solution.volume_of(reagent).raw() as i64 * reference_required.raw() as i64
            == reference_present * required.raw() as i64
    })
}

fn validate_process(
    reaction: &str,
    reactants: &[(ReagentId, Units)],
    catalysts: &[(ReagentId, Units)],
    process: &ReactionProcess,
) -> Result<(), ChemDataError> {
    let ReactionProcess::Agitated { side_a, side_b } = process else {
        return Ok(());
    };

    let valid = !side_a.is_empty()
        && !side_b.is_empty()
        && side_a.iter().all(|(_, amount)| amount.is_positive())
        && side_b.iter().all(|(_, amount)| amount.is_positive())
        && !side_a
            .iter()
            .any(|(reagent, _)| side_b.iter().any(|(other, _)| other == reagent))
        && ingredient_totals(side_a.iter().chain(side_b.iter()).copied())
            == ingredient_totals(reactants.iter().chain(catalysts.iter()).copied());

    if valid {
        Ok(())
    } else {
        Err(ChemDataError::InvalidAgitationProcess {
            reaction: reaction.to_string(),
        })
    }
}

fn ingredient_totals(
    ingredients: impl Iterator<Item = (ReagentId, Units)>,
) -> BTreeMap<ReagentId, Units> {
    let mut totals = BTreeMap::new();
    for (reagent, amount) in ingredients {
        *totals.entry(reagent).or_insert(Units::ZERO) += amount;
    }
    totals
}

/// Something wrong with the chemistry data files.
#[derive(Clone, Debug, PartialEq)]
pub enum ChemDataError {
    DuplicateReagent {
        reagent: String,
    },
    DuplicateReaction {
        reaction: String,
    },
    InvalidReagentField {
        reagent: String,
        field: &'static str,
    },
    InvalidReactionField {
        reaction: String,
        field: &'static str,
    },
    UnreachableReactionProduct {
        reagent: String,
    },
    UnknownReagent {
        reaction: String,
        reagent: String,
    },
    UnknownInverse {
        reagent: String,
        inverse: String,
    },
    UnknownRecoveryTarget {
        reagent: String,
        target: String,
    },
    InvalidRecoveryTarget {
        reagent: String,
    },
    UnknownPurgeTarget {
        reagent: String,
        target: String,
    },
    InvalidPurgeAmount {
        reagent: String,
        target: String,
    },
    NoReactants {
        reaction: String,
    },
    NoProducts {
        reaction: String,
    },
    InvalidAgitationProcess {
        reaction: String,
    },
    InvalidQualityRange {
        reaction: String,
    },
}

impl std::fmt::Display for ChemDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChemDataError::DuplicateReagent { reagent } => {
                write!(f, "reagent '{reagent}' is defined more than once")
            }
            ChemDataError::DuplicateReaction { reaction } => {
                write!(f, "reaction '{reaction}' is defined more than once")
            }
            ChemDataError::InvalidReagentField { reagent, field } => {
                write!(f, "reagent '{reagent}' has an invalid {field}")
            }
            ChemDataError::InvalidReactionField { reaction, field } => {
                write!(f, "reaction '{reaction}' has an invalid {field}")
            }
            ChemDataError::UnreachableReactionProduct { reagent } => write!(
                f,
                "reaction product '{reagent}' is unreachable from ChemMaster 5000 or produce sources (check for a circular dependency)"
            ),
            ChemDataError::UnknownReagent { reaction, reagent } => {
                write!(
                    f,
                    "reaction '{reaction}' refers to unknown reagent '{reagent}'"
                )
            }
            ChemDataError::UnknownInverse { reagent, inverse } => {
                write!(f, "reagent '{reagent}' names unknown inverse '{inverse}'")
            }
            ChemDataError::UnknownRecoveryTarget { reagent, target } => write!(
                f,
                "reagent '{reagent}' names unknown HPLC recovery target '{target}'"
            ),
            ChemDataError::InvalidRecoveryTarget { reagent } => write!(
                f,
                "reagent '{reagent}' cannot use itself as an HPLC recovery target"
            ),
            ChemDataError::UnknownPurgeTarget { reagent, target } => write!(
                f,
                "reagent '{reagent}' names unknown targeted purge reagent '{target}'"
            ),
            ChemDataError::InvalidPurgeAmount { reagent, target } => write!(
                f,
                "reagent '{reagent}' targets '{target}' with a non-positive purge amount"
            ),
            ChemDataError::NoReactants { reaction } => {
                write!(f, "reaction '{reaction}' has no reactants")
            }
            ChemDataError::NoProducts { reaction } => {
                write!(f, "reaction '{reaction}' has no products")
            }
            ChemDataError::InvalidAgitationProcess { reaction } => write!(
                f,
                "reaction '{reaction}' has agitation sides that do not exactly partition its reactants and catalysts"
            ),
            ChemDataError::InvalidQualityRange { reaction } => {
                write!(f, "reaction '{reaction}' has an invalid pH or purity range")
            }
        }
    }
}

impl std::error::Error for ChemDataError {}
