//! Runs reactions to completion in a solution.

use crate::reaction::{Reaction, ReactionEffect, ReactionId, ReactionSet};
use crate::solution::Solution;
use crate::thermal::Overheat;
use crate::units::{Kelvin, Units};

/// Recipes can form cycles (A makes B, B makes A). Without a cap the resolver
/// would spin forever and take the game with it.
pub const MAX_ITERATIONS: usize = 128;

/// One reaction firing.
#[derive(Clone, Debug, PartialEq)]
pub struct ReactionEvent {
    pub reaction: ReactionId,
    /// The multiplier it ran at. A 1:1:1 recipe firing at 15 consumed 15u of
    /// each reactant.
    pub scale: Units,
}

/// What happened during a call to [`resolve`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolveReport {
    pub events: Vec<ReactionEvent>,
    /// True if the resolver stopped at [`MAX_ITERATIONS`] with reactions still
    /// pending — almost always a cycle in the data.
    pub hit_iteration_cap: bool,
    pub effects: Vec<ReactionEffect>,
    /// Reactions that ran hot enough to lose yield. The chemist has already
    /// paid for these in reactants; the machine panel is how they find out.
    pub overheated: Vec<ReactionId>,
}

impl ResolveReport {
    pub fn reacted(&self) -> bool {
        !self.events.is_empty()
    }

    /// Distinct reactions that fired, in order of first firing. This is the
    /// hook recipe discovery hangs off.
    pub fn fired_reactions(&self) -> Vec<ReactionId> {
        let mut seen = Vec::new();
        for event in &self.events {
            if !seen.contains(&event.reaction) {
                seen.push(event.reaction);
            }
        }
        seen
    }
}

/// Reacts `solution` until nothing more can happen.
///
/// Each pass picks the single best applicable reaction — highest priority,
/// ties broken by definition order — and runs it as far as it will go. Looping
/// afterwards is what makes chains work: producing inaprovaline in one pass
/// lets bicaridine fire in the next, so a chemist can build a multi-step
/// recipe in a single beaker.
pub fn resolve(solution: &mut Solution, reactions: &ReactionSet) -> ResolveReport {
    let mut report = ResolveReport::default();

    for _ in 0..MAX_ITERATIONS {
        let Some((reaction, scale)) = best_reaction(solution, reactions) else {
            return report;
        };
        let overheated = reaction.is_overheated(solution.temperature);
        apply(
            solution,
            reaction,
            scale,
            reaction.yield_factor(solution.temperature),
        );
        report.events.push(ReactionEvent {
            reaction: reaction.id,
            scale,
        });
        if overheated && !report.overheated.contains(&reaction.id) {
            report.overheated.push(reaction.id);
        }
        for effect in &reaction.effects {
            report.effects.push(effect.clone());
            if let ReactionEffect::Heat(kelvin_per_unit) = effect {
                solution.temperature =
                    Kelvin(solution.temperature.0 + kelvin_per_unit * scale.as_f32());
            }
        }

        // A runaway exothermic reaction is the one failure that does not wait
        // for the chemist to notice. Handled after the heat above, so a
        // reaction can cook itself into its own detonation in a single pass.
        if reaction.is_overheated(solution.temperature) {
            match reaction.overheat {
                Overheat::Detonate { power } => {
                    report.effects.push(ReactionEffect::Explosion(power));
                    solution.clear();
                    return report;
                }
                Overheat::Ruin => {
                    solution.clear();
                    return report;
                }
                Overheat::ReducedYield { .. } => {}
            }
        }
    }

    // Fell out of the loop still having work to do.
    report.hit_iteration_cap = best_reaction(solution, reactions).is_some();
    report
}

/// Highest-priority applicable reaction, ties broken by definition order so
/// the outcome is always deterministic.
fn best_reaction<'a>(
    solution: &Solution,
    reactions: &'a ReactionSet,
) -> Option<(&'a Reaction, Units)> {
    let mut best: Option<(&Reaction, Units)> = None;
    for reaction in reactions.iter() {
        let Some(scale) = reaction.max_scale(solution) else {
            continue;
        };
        let better = match best {
            Some((current, _)) => reaction.priority > current.priority,
            None => true,
        };
        if better {
            best = Some((reaction, scale));
        }
    }
    best
}

/// Consumes reactants and adds products. Catalysts are untouched by design.
///
/// `yield_factor` scales the **products only**. Reactants are always consumed
/// in full: an overheated reaction wastes what it was given, which is the whole
/// point of letting one overheat rather than simply stopping it.
fn apply(solution: &mut Solution, reaction: &Reaction, scale: Units, yield_factor: Units) {
    for &(id, required) in &reaction.reactants {
        let consumed = required.scaled(scale, Units::ONE);
        let removed = solution.remove(id, consumed);
        debug_assert_eq!(
            removed, consumed,
            "reaction '{}' consumed less than its scale allowed",
            reaction.key
        );
    }
    // Reactants are removed first, so products are never rejected for want of
    // space the reaction itself just freed.
    for &(id, produced) in &reaction.products {
        let amount = produced
            .scaled(scale, Units::ONE)
            .scaled(yield_factor, Units::ONE);
        let _overflow = solution.add(id, amount);
    }
}
