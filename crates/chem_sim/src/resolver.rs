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
    /// Distinct reagents present the instant this call started, before
    /// anything reacted. `chem_sim` has no opinion on what this is *for* —
    /// it is just a fact about the input — but it is what the game layer
    /// uses to tell a focused experiment from a shotgun dump: a chain built
    /// one add at a time never has more present at once than the widest
    /// single reaction's own reactant count, because the resolver eagerly
    /// consumes intermediates as they form. Only a pile of reagents that
    /// *don't* react with each other can push this high.
    pub distinct_reagents: usize,
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
///
/// Ignores [`Reaction::rate`] entirely: this is the "no clock" resolution, and
/// it is what reagents reacting *inside a body* want — a bloodstream is not
/// somewhere a chemist can stand and watch a beaker. See [`resolve_step`] for
/// the timed form the lab's glassware uses.
pub fn resolve(solution: &mut Solution, reactions: &ReactionSet) -> ResolveReport {
    resolve_step(solution, reactions, f32::INFINITY)
}

/// Advances `solution` by `dt` seconds.
///
/// Identical to [`resolve`] except that a reaction naming a [`Reaction::rate`]
/// may only advance `rate * dt` reaction-units in this call — in total, however
/// many passes it takes — and is then set aside so the loop can get on with
/// whatever else the beaker can do. A reaction with no rate is unaffected and
/// still completes inside this one call, which is what keeps every existing
/// recipe, and every test pinning one, behaving exactly as it did.
///
/// `dt` of `0.0` therefore means "run the instant chemistry and leave the slow
/// chemistry where it is", which is precisely what a beaker being poured into
/// wants: the pour must not silently advance a batch by a frame's worth on the
/// strength of having been touched.
pub fn resolve_step(solution: &mut Solution, reactions: &ReactionSet, dt: f32) -> ResolveReport {
    let mut report = ResolveReport {
        distinct_reagents: solution.len(),
        ..Default::default()
    };
    // How much of each rated reaction's allowance this call has already used.
    // Without it a rate-capped reaction would simply be picked again on the
    // next pass and run its whole allowance up to `MAX_ITERATIONS` times over.
    let mut spent: Vec<(ReactionId, Units)> = Vec::new();

    for _ in 0..MAX_ITERATIONS {
        let Some((reaction, scale)) = best_reaction(solution, reactions, dt, &spent) else {
            return report;
        };
        match spent.iter_mut().find(|(id, _)| *id == reaction.id) {
            Some((_, total)) => *total += scale,
            None => spent.push((reaction.id, scale)),
        }
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
    report.hit_iteration_cap = best_reaction(solution, reactions, dt, &spent).is_some();
    report
}

/// Whether a rated reaction can still make progress in `solution`.
///
/// The question "is this batch finished?", answerable from the solution and
/// the chemistry alone. That matters in the game layer: a client holds both,
/// so it can tell a chemist their beaker is still working without the
/// authority replicating a per-frame flag to say so.
///
/// Only rated reactions count. An unrated one is never *part-way* through —
/// it completes inside the call that noticed it could happen — so a solution
/// where one can run is a solution nobody has resolved yet, which is a bug
/// elsewhere rather than a batch in progress.
pub fn is_reacting(solution: &Solution, reactions: &ReactionSet) -> bool {
    reactions
        .iter()
        .any(|reaction| reaction.rate.is_some() && reaction.max_scale(solution).is_some())
}

/// Highest-priority applicable reaction, ties broken by definition order so
/// the outcome is always deterministic.
///
/// A rated reaction that has already used its whole allowance for this step is
/// skipped rather than returned at zero scale — so a slow reaction cannot
/// block a fast one that happens to sit below it in priority and wants
/// entirely different reagents.
fn best_reaction<'a>(
    solution: &Solution,
    reactions: &'a ReactionSet,
    dt: f32,
    spent: &[(ReactionId, Units)],
) -> Option<(&'a Reaction, Units)> {
    let mut best: Option<(&Reaction, Units)> = None;
    for reaction in reactions.iter() {
        let Some(scale) = reaction.max_scale(solution) else {
            continue;
        };
        let scale = match reaction.step_limit(dt) {
            Some(limit) => {
                let used = spent
                    .iter()
                    .find(|(id, _)| *id == reaction.id)
                    .map(|(_, total)| *total)
                    .unwrap_or(Units::ZERO);
                scale.min((limit - used).clamp_non_negative())
            }
            None => scale,
        };
        if !scale.is_positive() {
            continue;
        }
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
