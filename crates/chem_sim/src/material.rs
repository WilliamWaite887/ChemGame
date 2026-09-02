//! Game-scale material interactions. Properties describe substances, never delivery items.
//! Environmental supplies are contact budgets, not free Solution ingredients.
use crate::{ChemDataError, ReactionEffect, ReagentId, ReagentRegistry, Solution, Units};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaterialProperties {
    /// Byproducts do not occupy synthesis recipe cards or earn research.
    pub residue: bool,
    pub water: bool,
    pub oxidizer: bool,
    pub water_reactive: Option<MaterialReaction>,
    pub fuel: Option<MaterialReaction>,
    /// Signed neutralization capacity per unit; zero leaves existing pH controls alone.
    pub acid_base: f32,
    pub neutral_product: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaterialReaction {
    pub product: String,
    pub activation_temp: f32,
    /// None means immediate when an explicit partner is present.
    pub rate: Option<f32>,
    /// Heat and hazard energy per consumed unit, scaled by input purity.
    pub energy: f32,
}

impl MaterialProperties {
    pub fn description(&self) -> String {
        let mut parts = Vec::new();
        if self.water_reactive.is_some() {
            parts.push("Reacts with water, including inside a body");
        }
        if self.fuel.is_some() {
            parts.push("Flammable when ignited; oxidizers accelerate burning");
        }
        if self.oxidizer {
            parts.push("Feeds activated fires");
        }
        if self.acid_base > 0.0 {
            parts.push("Neutralizes alkaline material");
        }
        if self.acid_base < 0.0 {
            parts.push("Neutralizes acidic material");
        }
        parts.join(". ")
    }

    pub(crate) fn validate(&self) -> bool {
        self.acid_base.is_finite()
            && self.acid_base.abs() <= 10.0
            && (self.acid_base == 0.0 || self.neutral_product.is_some())
            && self.water_reactive.iter().chain(self.fuel.iter()).all(|p| {
                p.activation_temp.is_finite()
                    && p.activation_temp > 0.0
                    && p.energy.is_finite()
                    && p.energy > 0.0
                    && p.rate.is_none_or(|r| r.is_finite() && r >= 0.1)
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialFamily {
    WaterReactive,
    Neutralization,
    Combustion,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaterialEvent {
    pub family: MaterialFamily,
    pub consumed: Vec<(ReagentId, Units)>,
    pub products: Vec<(ReagentId, Units)>,
    pub environmental_units: Units,
    pub energy: f32,
}

/// Reuse one environment for a body's stomach and blood in a metabolism beat.
#[derive(Clone, Debug)]
pub struct ReactionEnvironment {
    pub water: Units,
    pub oxygen: Units,
    pub ignited: bool,
}

impl ReactionEnvironment {
    pub fn room(dt: f32) -> Self {
        Self {
            water: Units::ZERO,
            oxygen: budget(dt, 0.5),
            ignited: false,
        }
    }
    pub fn body(dt: f32) -> Self {
        Self {
            water: budget(dt, 0.5),
            oxygen: Units::ZERO,
            ignited: false,
        }
    }
}

fn budget(dt: f32, rate: f32) -> Units {
    if dt.is_finite() && dt > 0.0 {
        Units::from_f64((dt * rate) as f64)
    } else {
        Units::ZERO
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MaterialCatalog {
    entries: Vec<(ReagentId, MaterialProperties)>,
    products: Vec<(String, ReagentId, f32)>,
}

impl MaterialCatalog {
    pub fn load(registry: &ReagentRegistry) -> Result<Self, ChemDataError> {
        let mut catalog = Self::default();
        for reagent in registry.iter() {
            let p = &reagent.material;
            for key in p
                .water_reactive
                .iter()
                .chain(p.fuel.iter())
                .map(|r| &r.product)
                .chain(p.neutral_product.iter())
            {
                let Some(id) = registry.id_of(key) else {
                    return Err(ChemDataError::InvalidReagentField {
                        reagent: reagent.key.clone(),
                        field: "material product",
                    });
                };
                if id == reagent.id {
                    return Err(ChemDataError::InvalidReagentField {
                        reagent: reagent.key.clone(),
                        field: "self-producing material",
                    });
                }
                catalog
                    .products
                    .push((key.clone(), id, registry.get(id).ph));
            }
            if p != &MaterialProperties::default() {
                catalog.entries.push((reagent.id, p.clone()));
            }
        }
        Ok(catalog)
    }

    /// One transition at a time lets heat/products participate in the next resolver pass.
    pub fn advance(
        &self,
        solution: &mut Solution,
        dt: f32,
        environment: &mut ReactionEnvironment,
        spent: &mut Vec<(ReagentId, MaterialFamily, Units)>,
        slow: bool,
    ) -> Option<MaterialEvent> {
        for (id, properties) in &self.entries {
            let available = solution.volume_of(*id);
            if !available.is_positive() {
                continue;
            }
            for (family, profile) in [
                (
                    MaterialFamily::WaterReactive,
                    properties.water_reactive.as_ref(),
                ),
                (MaterialFamily::Combustion, properties.fuel.as_ref()),
            ] {
                let Some(profile) = profile else {
                    continue;
                };
                if solution.temperature.0 < profile.activation_temp
                    && !(family == MaterialFamily::Combustion && environment.ignited)
                {
                    continue;
                }
                let used = spent
                    .iter()
                    .find(|(r, f, _)| r == id && *f == family)
                    .map_or(Units::ZERO, |v| v.2);
                let allowance = profile.rate.map_or(available, |rate| {
                    if dt.is_infinite() {
                        available
                    } else {
                        (budget(dt, rate) - used).clamp_non_negative()
                    }
                });
                // Immediate interactions precede recipes; rated/contact reactions follow them.
                if !slow && profile.rate.is_some() && family != MaterialFamily::Combustion {
                    continue;
                }
                let partner = self
                    .entries
                    .iter()
                    .find(|(other, p)| {
                        other != id
                            && (if family == MaterialFamily::WaterReactive {
                                p.water
                            } else {
                                p.oxidizer
                            })
                            && solution.volume_of(*other).is_positive()
                    })
                    .map(|v| v.0);
                let ambient = if family == MaterialFamily::WaterReactive {
                    environment.water
                } else {
                    environment.oxygen
                };
                if partner.is_none() && !slow {
                    continue;
                }
                let partner_amount = partner.map_or(ambient, |r| solution.volume_of(r));
                let amount = available.min(allowance).min(partner_amount);
                if !amount.is_positive() {
                    continue;
                }
                let purity = partner.map_or(solution.purity_of(*id), |r| {
                    solution.purity_of(*id).min(solution.purity_of(r))
                });
                let energy = amount.as_f32() * profile.energy * purity;
                let mut consumed = vec![(*id, solution.remove(*id, amount))];
                let environmental_units = if let Some(partner) = partner {
                    consumed.push((partner, solution.remove(partner, amount)));
                    Units::ZERO
                } else {
                    if family == MaterialFamily::WaterReactive {
                        environment.water -= amount;
                    } else {
                        environment.oxygen -= amount;
                    }
                    amount
                };
                // Environmental gas/water is not bottled: only tracked input volume becomes residue.
                let yield_amount: Units = consumed.iter().map(|v| v.1).sum();
                let products = self.produce(solution, &profile.product, yield_amount, purity);
                add_heat(solution, energy);
                if let Some(entry) = spent.iter_mut().find(|(r, f, _)| r == id && *f == family) {
                    entry.2 += amount;
                } else {
                    spent.push((*id, family, amount));
                }
                return Some(MaterialEvent {
                    family,
                    consumed,
                    products,
                    environmental_units,
                    energy,
                });
            }
            if !slow || properties.acid_base <= 0.0 {
                continue;
            }
            let Some((base, base_properties)) = self
                .entries
                .iter()
                .find(|(r, p)| p.acid_base < 0.0 && solution.volume_of(*r).is_positive())
            else {
                continue;
            };
            let used = spent
                .iter()
                .find(|(r, f, _)| r == id && *f == MaterialFamily::Neutralization)
                .map_or(Units::ZERO, |v| v.2);
            let allowance = if dt.is_infinite() {
                available
            } else {
                (budget(dt, 1.0) - used).clamp_non_negative()
            };
            let ratio = properties.acid_base / -base_properties.acid_base;
            let amount = available.min(allowance).min(Units::from_f64(
                solution.volume_of(*base).as_f64() / ratio as f64,
            ));
            let base_amount =
                Units::from_f64(amount.as_f64() * ratio as f64).min(solution.volume_of(*base));
            if !amount.is_positive() || !base_amount.is_positive() {
                continue;
            }
            let purity = solution.purity_of(*id).min(solution.purity_of(*base));
            let consumed = vec![
                (*id, solution.remove(*id, amount)),
                (*base, solution.remove(*base, base_amount)),
            ];
            let products = self.produce(
                solution,
                properties.neutral_product.as_deref().unwrap(),
                amount + base_amount,
                purity,
            );
            let energy = amount.as_f32() * purity;
            add_heat(solution, energy);
            if let Some(entry) = spent
                .iter_mut()
                .find(|(r, f, _)| r == id && *f == MaterialFamily::Neutralization)
            {
                entry.2 += amount;
            } else {
                spent.push((*id, MaterialFamily::Neutralization, amount));
            }
            return Some(MaterialEvent {
                family: MaterialFamily::Neutralization,
                consumed,
                products,
                environmental_units: Units::ZERO,
                energy,
            });
        }
        None
    }

    fn produce(
        &self,
        solution: &mut Solution,
        key: &str,
        amount: Units,
        purity: f32,
    ) -> Vec<(ReagentId, Units)> {
        let (_, id, ph) = self
            .products
            .iter()
            .find(|p| p.0 == key)
            .expect("validated material product");
        let overflow = solution.add_profiled(*id, amount, purity, *ph);
        let added = amount - overflow;
        vec![(*id, added)]
    }

    pub fn active(&self, solution: &Solution) -> bool {
        let mut probe = solution.clone();
        self.advance(
            &mut probe,
            0.1,
            &mut ReactionEnvironment::room(0.1),
            &mut Vec::new(),
            true,
        )
        .is_some()
    }
}

fn add_heat(solution: &mut Solution, energy: f32) {
    solution.temperature.0 = (solution.temperature.0
        + energy * 20.0 / solution.total_volume().as_f32().max(1.0))
    .min(2000.0);
}

pub(crate) fn effects(event: &MaterialEvent) -> Vec<ReactionEffect> {
    let mut effects = vec![ReactionEffect::Heat(event.energy)];
    if event.family == MaterialFamily::WaterReactive {
        // No flat bonus: a trace never gets a full-strength blast.
        effects.push(ReactionEffect::Explosion(event.energy));
    } else if event.family == MaterialFamily::Combustion {
        effects.push(ReactionEffect::Burn(event.energy));
    }
    effects
}
