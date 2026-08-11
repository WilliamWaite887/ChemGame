//! Heating, cooling and what happens when a reaction runs away.
//!
//! `Solution` has carried a temperature since M1 and `Reaction` has gated on
//! it, but nothing ever moved it. This is the arithmetic that does — kept here
//! rather than in the game layer so a reaction chamber and a beaker cooling on
//! a bench are the same two lines of maths.

use serde::{Deserialize, Serialize};

use crate::reagent::ReagentRegistry;
use crate::solution::Solution;
use crate::units::{Kelvin, Units};

/// Close enough to the target to call it arrived, in kelvin.
///
/// Without this, [`approach`] is an asymptote: a chamber targeted at exactly a
/// recipe's `min_temp` would creep toward it forever and the reaction would
/// never fire. Snapping is not a rounding convenience, it is what makes the
/// machine usable.
pub const SETTLE: f32 = 0.05;

/// Moves `current` toward `target`, closing `rate` of the remaining gap per
/// second.
///
/// Exponential rather than linear for two reasons. It reads correctly — a
/// chamber slows as it arrives, the way a real one does. And it is frame-rate
/// independent: ten 0.1s steps land in the same place one 1.0s step does,
/// which a linear step emphatically does not.
pub fn approach(current: Kelvin, target: Kelvin, rate: f32, dt: f32) -> Kelvin {
    if dt <= 0.0 || rate <= 0.0 {
        return current;
    }
    let gap = target.0 - current.0;
    if gap.abs() <= SETTLE {
        return target;
    }
    // (1 - rate) of the gap survives each second; over dt seconds that is
    // (1 - rate)^dt. Composing two steps multiplies the exponents, which is
    // exactly the frame-rate independence we want.
    let remaining = (1.0 - rate.min(1.0)).powf(dt);
    let moved = Kelvin(target.0 - gap * remaining);
    if (target.0 - moved.0).abs() <= SETTLE {
        target
    } else {
        moved
    }
}

/// What an overheated reaction does.
///
/// Distinct from `max_temp`, which simply stops a reaction from running.
/// Overheating means it *still runs*, but goes wrong — which is the more
/// interesting failure, because you do not find out until the yield is short.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Overheat {
    /// Yield falls linearly to nothing over this many kelvin past the
    /// threshold. Reactants are consumed in full regardless — that is the cost,
    /// and it is what makes running hot a real mistake rather than a slow path.
    ReducedYield { over: f32 },
    /// Goes bang. The batch is gone and the room finds out.
    Detonate { power: f32 },
    /// Ruins the batch outright, without a blast.
    Ruin,
}

impl Default for Overheat {
    /// Most overheats in SS13 just waste the batch, so that is what a recipe
    /// gets if it names an `overheat_temp` and nothing else.
    fn default() -> Self {
        Overheat::ReducedYield { over: 60.0 }
    }
}

impl Overheat {
    /// Share of the normal product yield at `temperature`, given the reaction
    /// overheats above `threshold`.
    ///
    /// Returns [`Units::ONE`] when not overheated, so a recipe that never names
    /// a threshold is completely unaffected.
    pub fn yield_factor(self, threshold: Kelvin, temperature: Kelvin) -> Units {
        let excess = temperature.0 - threshold.0;
        if excess <= 0.0 {
            return Units::ONE;
        }
        match self {
            Overheat::ReducedYield { over } if over > 0.0 => {
                let remaining = (1.0 - excess / over).clamp(0.0, 1.0);
                Units::from_f64(remaining as f64)
            }
            // A zero-width falloff, a detonation and a ruin all leave nothing.
            _ => Units::ZERO,
        }
    }
}

/// Removes everything above its boiling point and returns it.
///
/// Plasma goes off at 323.15K, which means heating a beaker to run a hot
/// recipe can quietly cost you the catalyst that recipe needed. The caller
/// decides whether the returned gas becomes a smoke cloud or just vanishes.
pub fn boil_off(solution: &mut Solution, reagents: &ReagentRegistry) -> Solution {
    let mut gas = Solution::unbounded();
    gas.temperature = solution.temperature;

    let boiling: Vec<(crate::reagent::ReagentId, Units)> = solution
        .iter()
        .filter(|(id, _)| match reagents.get(*id).boils_at {
            Some(point) => solution.temperature >= point,
            None => false,
        })
        .collect();

    for (id, amount) in boiling {
        let removed = solution.remove(id, amount);
        let _ = gas.add(id, removed);
    }
    gas
}
