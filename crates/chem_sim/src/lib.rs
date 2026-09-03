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
pub mod material;
pub mod reaction;
pub mod reagent;
pub mod resolver;
pub mod solution;
pub mod thermal;
pub mod units;

use std::collections::HashSet;

pub use body::{
    blast_radius, explosion_damage, metabolise, Bloodstream, ExposureReport, Health, StatusState,
    TickReport, Vitals, ANALGESIC_COLLAPSE_BONUS, COLLAPSE, CRITICAL_DAMAGE, MAX_DAMAGE_PER_KIND,
    RECOVER, STABILIZED_COLLAPSE_BONUS, TICK_SECONDS,
};
pub use effect::{Damage, DamageKind, ReagentEffect, Route, StatusKind, WorldEffect};
pub use material::{
    MaterialEvent, MaterialFamily, MaterialProperties, MaterialReaction, ReactionEnvironment,
};
pub use reaction::{
    ChemDataError, PulseKind, Reaction, ReactionActivation, ReactionDef, ReactionEffect,
    ReactionId, ReactionProcess, ReactionProcessDef, ReactionSet,
};
pub use reagent::{
    Category, ChemFamily, ExplosiveProfile, Reagent, ReagentDef, ReagentId, ReagentRegistry,
    DEFAULT_METABOLISM,
};
pub use resolver::resolve_in_environment;
pub use resolver::{
    is_reacting, is_reacting_with_activation, resolve, resolve_step, resolve_step_with_activation,
    resolve_with_activation, ReactionEvent, ResolveReport, MAX_ITERATIONS,
};
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
    /// Family products reachable from a set of material sources. Explicit family
    /// partners must be reachable too; environmental water/air only assist hazards.
    pub fn material_products_from(&self, available: &HashSet<ReagentId>) -> Vec<ReagentId> {
        let mut products = Vec::new();
        for reagent in self.reagents.iter().filter(|r| available.contains(&r.id)) {
            let p = &reagent.material;
            for rule in p.water_reactive.iter().chain(p.fuel.iter()) {
                if let Some(id) = self.reagents.id_of(&rule.product) {
                    products.push(id);
                }
            }
            if p.acid_base > 0.0
                && self
                    .reagents
                    .iter()
                    .any(|r| available.contains(&r.id) && r.material.acid_base < 0.0)
            {
                if let Some(id) = p
                    .neutral_product
                    .as_deref()
                    .and_then(|key| self.reagents.id_of(key))
                {
                    products.push(id);
                }
            }
        }
        products
    }
    pub fn from_defs(
        reagent_defs: Vec<ReagentDef>,
        reaction_defs: Vec<ReactionDef>,
    ) -> Result<Self, ChemDataError> {
        let mut reagent_keys = HashSet::new();
        for def in &reagent_defs {
            if !reagent_keys.insert(def.id.clone()) {
                return Err(ChemDataError::DuplicateReagent {
                    reagent: def.id.clone(),
                });
            }
            validate_reagent_def(def)?;
        }
        let mut reaction_keys = HashSet::new();
        for def in &reaction_defs {
            if !reaction_keys.insert(def.id.clone()) {
                return Err(ChemDataError::DuplicateReaction {
                    reaction: def.id.clone(),
                });
            }
            validate_reaction_def(def)?;
        }

        let mut reagents = ReagentRegistry::new();
        for def in reagent_defs {
            reagents.insert(def);
        }
        for reagent in reagents.iter() {
            if let Some(inverse) = reagent.inverse.as_deref() {
                if reagents.id_of(inverse).is_none() {
                    return Err(ChemDataError::UnknownInverse {
                        reagent: reagent.key.clone(),
                        inverse: inverse.to_string(),
                    });
                }
            }
            if let Some(target) = reagent.recovers_to.as_deref() {
                if reagents.id_of(target).is_none() {
                    return Err(ChemDataError::UnknownRecoveryTarget {
                        reagent: reagent.key.clone(),
                        target: target.to_string(),
                    });
                }
                if target == reagent.key {
                    return Err(ChemDataError::InvalidRecoveryTarget {
                        reagent: reagent.key.clone(),
                    });
                }
            }
            for (target, amount) in &reagent.targeted_purges {
                if reagents.id_of(target).is_none() {
                    return Err(ChemDataError::UnknownPurgeTarget {
                        reagent: reagent.key.clone(),
                        target: target.clone(),
                    });
                }
                if !amount.is_positive() {
                    return Err(ChemDataError::InvalidPurgeAmount {
                        reagent: reagent.key.clone(),
                        target: target.clone(),
                    });
                }
            }
        }
        let mut reactions = ReactionSet::new();
        reactions.materials = material::MaterialCatalog::load(&reagents)?;
        for def in reaction_defs {
            reactions.insert(def, &reagents)?;
        }
        validate_reachability(&reagents, &reactions)?;
        Ok(ChemData {
            reagents,
            reactions,
        })
    }

    /// Everything physically synthesizable from ChemMaster 5000 stock plus the
    /// supplied station inventory, without considering recipe knowledge.
    ///
    /// Hidden antagonist requests use this view: their customer may know a
    /// formula the player has not discovered, but cannot fairly demand an
    /// external botanical ingredient that has not actually arrived.
    pub fn reachable_reagents_with_inventory(
        &self,
        inventory: impl IntoIterator<Item = ReagentId>,
    ) -> HashSet<ReagentId> {
        let mut reachable: HashSet<ReagentId> = self
            .reagents
            .dispensable()
            .map(|reagent| reagent.id)
            .collect();
        reachable.extend(inventory);
        loop {
            let mut grew = false;
            for product in self.material_products_from(&reachable) {
                grew |= reachable.insert(product);
            }
            for reaction in self.reactions.iter() {
                if reaction
                    .reactants
                    .iter()
                    .chain(&reaction.catalysts)
                    .all(|(reagent, _)| reachable.contains(reagent))
                {
                    for product in reaction.product_ids() {
                        grew |= reachable.insert(product);
                    }
                }
            }
            if !grew {
                break;
            }
        }
        reachable
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

fn validate_reagent_def(def: &ReagentDef) -> Result<(), ChemDataError> {
    let invalid = |field| ChemDataError::InvalidReagentField {
        reagent: def.id.clone(),
        field,
    };
    if !def.material.validate() {
        return Err(invalid("material properties"));
    }
    if def.id.trim().is_empty() {
        return Err(invalid("id"));
    }
    if def.name.trim().is_empty() {
        return Err(invalid("name"));
    }
    if def
        .color
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(invalid("color"));
    }
    if !def.ph.is_finite() || !(0.0..=14.0).contains(&def.ph) {
        return Err(invalid("pH"));
    }
    if def.overdose.is_some_and(|value| !value.is_positive()) {
        return Err(invalid("overdose threshold"));
    }
    if def.metabolism.is_some_and(|value| !value.is_positive()) {
        return Err(invalid("metabolism rate"));
    }
    if def
        .critical_overdose
        .is_some_and(|value| !value.is_positive())
    {
        return Err(invalid("critical overdose threshold"));
    }
    if matches!((def.overdose, def.critical_overdose), (Some(first), Some(critical)) if critical <= first)
    {
        return Err(invalid("critical overdose threshold"));
    }
    if def
        .boils_at
        .is_some_and(|value| !value.0.is_finite() || value.0 <= 0.0)
    {
        return Err(invalid("boiling point"));
    }
    if !def.addictive.is_finite() || def.addictive < 0.0 {
        return Err(invalid("addictiveness"));
    }
    if let Some(explosive) = def.explosive {
        if !explosive.strength.is_finite()
            || explosive.strength <= 0.0
            || !explosive.modifier.is_finite()
            || explosive.modifier < 0.0
            || !explosive.activation_temp.0.is_finite()
            || explosive.activation_temp.0 <= 0.0
        {
            return Err(invalid("explosive profile"));
        }
    }
    if def
        .effects
        .iter()
        .chain(&def.overdose_effects)
        .chain(&def.critical_effects)
        .chain(&def.after_effects)
        .any(|effect| !effect.magnitude().is_finite() || effect.magnitude() <= 0.0)
    {
        return Err(invalid("body effect"));
    }
    if def
        .world_effects
        .iter()
        .any(|effect| !effect.magnitude().is_finite() || effect.magnitude() <= 0.0)
    {
        return Err(invalid("world effect"));
    }
    Ok(())
}

fn validate_reaction_def(def: &ReactionDef) -> Result<(), ChemDataError> {
    let invalid = |field| ChemDataError::InvalidReactionField {
        reaction: def.id.clone(),
        field,
    };
    if def.id.trim().is_empty() {
        return Err(invalid("id"));
    }
    for (field, ingredients) in [
        ("reactant amount", def.reactants.as_slice()),
        ("catalyst amount", def.catalysts.as_slice()),
        ("product amount", def.products.as_slice()),
    ] {
        if ingredients.iter().any(|(_, amount)| !amount.is_positive()) {
            return Err(invalid(field));
        }
        let mut names = HashSet::new();
        if ingredients.iter().any(|(name, _)| !names.insert(name)) {
            return Err(invalid("duplicate ingredient"));
        }
    }
    if def
        .reactants
        .iter()
        .any(|(name, _)| def.catalysts.iter().any(|(other, _)| name == other))
    {
        return Err(invalid("reactant/catalyst overlap"));
    }
    let valid_temperature =
        |value: Option<Kelvin>| value.is_none_or(|value| value.0.is_finite() && value.0 > 0.0);
    if !valid_temperature(def.min_temp)
        || !valid_temperature(def.max_temp)
        || !valid_temperature(def.overheat_temp)
        || matches!((def.min_temp, def.max_temp), (Some(min), Some(max)) if min > max)
    {
        return Err(invalid("temperature range"));
    }
    if !def.ph_shift.is_finite() {
        return Err(invalid("pH drift"));
    }
    if def.rate.is_some_and(|rate| !rate.is_positive()) {
        return Err(invalid("reaction rate"));
    }
    let valid_overheat = match def.overheat {
        Overheat::ReducedYield { over } => over.is_finite() && over > 0.0,
        Overheat::Detonate { power } => power.is_finite() && power > 0.0,
        Overheat::Ruin => true,
    };
    if !valid_overheat {
        return Err(invalid("overheat profile"));
    }
    let valid_effects = def.effects.iter().all(|effect| match *effect {
        ReactionEffect::Heat(value) => value.is_finite(),
        ReactionEffect::Burn(value) => value.is_finite() && value > 0.0,
        ReactionEffect::Smoke(value) | ReactionEffect::Explosion(value) => {
            value.is_finite() && value > 0.0
        }
        ReactionEffect::ExplosionProfile { strength, modifier } => {
            strength.is_finite() && strength > 0.0 && modifier.is_finite() && modifier >= 0.0
        }
        ReactionEffect::Pulse { power, .. } => power.is_finite() && power > 0.0,
        ReactionEffect::PulseProfile {
            strength, modifier, ..
        } => strength.is_finite() && strength > 0.0 && modifier.is_finite() && modifier >= 0.0,
        ReactionEffect::Emp(value) | ReactionEffect::Electric(value) => {
            value.is_finite() && value > 0.0
        }
        ReactionEffect::EmpProfile { strength, modifier }
        | ReactionEffect::ElectricProfile { strength, modifier } => {
            strength.is_finite() && strength > 0.0 && modifier.is_finite() && modifier >= 0.0
        }
    });
    if !valid_effects {
        return Err(invalid("reaction effect"));
    }
    Ok(())
}

fn validate_reachability(
    reagents: &ReagentRegistry,
    reactions: &ReactionSet,
) -> Result<(), ChemDataError> {
    let mut reachable: HashSet<ReagentId> = reagents.raw().map(|reagent| reagent.id).collect();
    loop {
        let mut grew = false;
        for reaction in reactions.iter() {
            if reaction
                .reactants
                .iter()
                .chain(&reaction.catalysts)
                .all(|(reagent, _)| reachable.contains(reagent))
            {
                for (product, _) in &reaction.products {
                    grew |= reachable.insert(*product);
                }
            }
        }
        if !grew {
            break;
        }
    }
    if let Some(product) = reactions
        .iter()
        .flat_map(|reaction| reaction.products.iter().map(|(reagent, _)| *reagent))
        .find(|reagent| !reachable.contains(reagent))
    {
        return Err(ChemDataError::UnreachableReactionProduct {
            reagent: reagents.get(product).key.clone(),
        });
    }
    Ok(())
}
