//! The one shared reading of "can this person work, and how dangerous is it?"
//!
//! Every department executor calls [`work_capacity`] and [`work_risk`]. Nothing
//! anywhere may special-case a reagent ID into a productivity bonus — that is
//! the rule the plan states as "department executors must not each invent a
//! separate rule that stimulant means faster", and
//! [`tests::capacity_reads_derived_physical_status_not_a_reagent_id`] is what
//! keeps it honest.
//!
//! The route from a chemical to a work effect is therefore always three steps:
//! the reagent enters a real [`chem_sim::Bloodstream`], ordinary metabolism
//! turns it into a [`chem_sim::StatusKind`], and *this* module reads the status.
//! A stimulant helps because it produced `Hastened`, not because it was a
//! stimulant. Anything that produces `Hastened` helps identically, and a
//! donated "stimulant" that metabolizes into nothing does nothing at all.
//!
//! Capacity and risk are deliberately **not** inverses. The interesting case —
//! the one the whole voluntary-aid system exists to create — is a worker who is
//! both faster *and* more dangerous, which only exists if the two readings can
//! rise together.

use chem_sim::{Bloodstream, StatusKind, Vitals};

/// Fatigue at or above this stops useful work on its own.
const EXHAUSTED: f32 = 0.95;

/// How much a full fatigue bar costs a worker's pace.
const FATIGUE_CAPACITY_COST: f32 = 0.45;

/// How much a full fatigue bar adds to the chance of a mistake. Larger than the
/// capacity cost: a tired worker slows a little and errs a lot.
const FATIGUE_RISK: f32 = 0.55;

/// The pace bonus a fully-`Hastened` worker gets, and the risk it carries.
/// The risk is the larger number on purpose — that is the trade the player is
/// being offered when they donate a stimulant to a strained department.
const HASTENED_CAPACITY: f32 = 0.25;
const HASTENED_RISK: f32 = 0.40;

/// Impairments that cost pace and add risk. Intensity-scaled, so a trace is a
/// nuisance and a heavy dose is disabling.
const SLUGGISH_CAPACITY_COST: f32 = 0.35;
const BLURRED_RISK: f32 = 0.30;
const UNSTEADY_RISK: f32 = 0.35;
const DRUNK_CAPACITY_COST: f32 = 0.40;
const DRUNK_RISK: f32 = 0.50;
const HALLUCINATING_RISK: f32 = 0.45;

/// How much accumulated damage costs at the point of collapse.
const DAMAGE_CAPACITY_COST: f32 = 0.5;

/// Statuses are unbounded in principle; clamp before scaling so an extreme dose
/// cannot invert a term's sign.
fn intensity(blood: &Bloodstream, kind: StatusKind) -> f32 {
    blood.status(kind).intensity.clamp(0.0, 1.0)
}

/// How much useful work this body can currently do, from `0.0` to `1.0`.
///
/// `1.0` is an unimpaired, rested worker. Zero means they should not be
/// working at all, and a caller seeing zero should not schedule them.
pub fn work_capacity(vitals: &Vitals, blood: &Bloodstream, fatigue: f32) -> f32 {
    if vitals.collapsed || blood.incapacitated() {
        return 0.0;
    }
    let fatigue = fatigue.clamp(0.0, 1.0);
    if fatigue >= EXHAUSTED {
        return 0.0;
    }

    let mut capacity = 1.0;
    capacity -= fatigue * FATIGUE_CAPACITY_COST;
    capacity -= intensity(blood, StatusKind::Sluggish) * SLUGGISH_CAPACITY_COST;
    capacity -= intensity(blood, StatusKind::Drunk) * DRUNK_CAPACITY_COST;
    capacity += intensity(blood, StatusKind::Hastened) * HASTENED_CAPACITY;

    // Damage short of collapse still slows someone down, scaled against the
    // threshold this particular body collapses at — so a stabilized patient
    // working through an injury is read against their own raised threshold
    // rather than a global constant.
    let threshold = blood.collapse_threshold().as_f32();
    if threshold > 0.0 {
        let hurt = (vitals.total().as_f32() / threshold).clamp(0.0, 1.0);
        capacity -= hurt * DAMAGE_CAPACITY_COST;
    }

    capacity.clamp(0.0, 1.0)
}

/// How likely this body is to make the task go wrong, from `0.0` to `1.0`.
///
/// Read by department executors that already roll for a bad outcome. It is not
/// a probability by itself — an executor scales its own authored base rate by
/// this — because a clumsy worker filing paperwork is not a hazard and a
/// clumsy worker on a reactor is.
pub fn work_risk(vitals: &Vitals, blood: &Bloodstream, fatigue: f32) -> f32 {
    let fatigue = fatigue.clamp(0.0, 1.0);
    let mut risk = fatigue * FATIGUE_RISK;

    // Every one of these is a *derived physical status*. None of them names a
    // chemical.
    risk += intensity(blood, StatusKind::Hastened) * HASTENED_RISK;
    risk += intensity(blood, StatusKind::Blurred) * BLURRED_RISK;
    risk += intensity(blood, StatusKind::Unsteady) * UNSTEADY_RISK;
    risk += intensity(blood, StatusKind::Drunk) * DRUNK_RISK;
    risk += intensity(blood, StatusKind::Hallucinating) * HALLUCINATING_RISK;

    if vitals.collapsed {
        return 1.0;
    }
    risk.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Vitals {
        Vitals::default()
    }

    fn clean() -> Bloodstream {
        Bloodstream::default()
    }

    fn with_status(kind: StatusKind, intensity: f32) -> Bloodstream {
        let mut blood = Bloodstream::default();
        blood.add_status(kind, 100.0, intensity);
        blood
    }

    #[test]
    fn a_rested_healthy_worker_is_at_full_capacity_and_no_risk() {
        assert_eq!(work_capacity(&healthy(), &clean(), 0.0), 1.0);
        assert_eq!(work_risk(&healthy(), &clean(), 0.0), 0.0);
    }

    #[test]
    fn a_collapsed_body_can_do_no_work_at_all() {
        let down = Vitals {
            collapsed: true,
            ..Default::default()
        };
        assert_eq!(work_capacity(&down, &clean(), 0.0), 0.0);
        assert_eq!(work_risk(&down, &clean(), 0.0), 1.0);
    }

    #[test]
    fn sedation_stops_work_without_any_damage() {
        // The distinction that makes sedatives a real tactic rather than a
        // slow poison: nothing is hurt and the shift still stops.
        let sedated = with_status(StatusKind::Sedated, 3.0);
        assert!(sedated.incapacitated());
        assert_eq!(work_capacity(&healthy(), &sedated, 0.0), 0.0);
        assert_eq!(healthy().total(), chem_sim::Health::ZERO);
    }

    #[test]
    fn a_stimulant_raises_pace_and_raises_risk_more() {
        // The trade the aid system is built to offer. If these ever move in
        // the same direction, donating stimulants becomes free and the whole
        // decision collapses.
        let fast = with_status(StatusKind::Hastened, 1.0);
        let base_capacity = work_capacity(&healthy(), &clean(), 0.3);
        let base_risk = work_risk(&healthy(), &clean(), 0.3);

        assert!(work_capacity(&healthy(), &fast, 0.3) > base_capacity);
        assert!(work_risk(&healthy(), &fast, 0.3) > base_risk);
        assert!(
            work_risk(&healthy(), &fast, 0.3) - base_risk
                > work_capacity(&healthy(), &fast, 0.3) - base_capacity,
            "the risk a stimulant adds must exceed the pace it buys",
        );
    }

    #[test]
    fn capacity_and_risk_are_not_inverses() {
        // Both rise together under `Hastened`. A design that derived one from
        // the other could not express that, and this is the case the plan
        // specifically wants.
        let fast = with_status(StatusKind::Hastened, 1.0);
        let capacity = work_capacity(&healthy(), &fast, 0.0);
        let risk = work_risk(&healthy(), &fast, 0.0);
        assert!(capacity > 0.9, "still fast: {capacity}");
        assert!(risk > 0.3, "and still dangerous: {risk}");
    }

    #[test]
    fn exhaustion_stops_work_before_it_becomes_negative() {
        assert_eq!(work_capacity(&healthy(), &clean(), 1.0), 0.0);
        assert!(work_capacity(&healthy(), &clean(), 0.5) > 0.0);
    }

    #[test]
    fn impairment_costs_pace_and_adds_risk() {
        let drunk = with_status(StatusKind::Drunk, 1.0);
        assert!(work_capacity(&healthy(), &drunk, 0.0) < work_capacity(&healthy(), &clean(), 0.0));
        assert!(work_risk(&healthy(), &drunk, 0.0) > work_risk(&healthy(), &clean(), 0.0));
    }

    #[test]
    fn capacity_reads_derived_physical_status_not_a_reagent_id() {
        // Two bloodstreams holding *different* reagents that produced the same
        // status must read identically. This is the guard against a future
        // executor growing a `if reagent == STIMULANT` branch: there is no
        // reagent identity in scope here at all.
        let mut from_one = Bloodstream::default();
        let _ = from_one
            .blood
            .add(chem_sim::ReagentId(1), chem_sim::Units::whole(5));
        from_one.add_status(StatusKind::Hastened, 100.0, 1.0);

        let mut from_another = Bloodstream::default();
        let _ = from_another
            .blood
            .add(chem_sim::ReagentId(2), chem_sim::Units::whole(50));
        from_another.add_status(StatusKind::Hastened, 100.0, 1.0);

        assert_eq!(
            work_capacity(&healthy(), &from_one, 0.2),
            work_capacity(&healthy(), &from_another, 0.2),
        );
        assert_eq!(
            work_risk(&healthy(), &from_one, 0.2),
            work_risk(&healthy(), &from_another, 0.2),
        );
    }

    #[test]
    fn a_chemical_that_metabolizes_to_nothing_changes_no_work_reading() {
        // The other half of the same guarantee: a donated "stimulant" that
        // produces no status is inert, however it was labelled.
        let mut inert = Bloodstream::default();
        let _ = inert
            .blood
            .add(chem_sim::ReagentId(9), chem_sim::Units::whole(40));
        assert_eq!(
            work_capacity(&healthy(), &inert, 0.4),
            work_capacity(&healthy(), &clean(), 0.4),
        );
    }

    #[test]
    fn readings_stay_bounded_under_extreme_doses() {
        let overdosed = with_status(StatusKind::Hastened, 40.0);
        let capacity = work_capacity(&healthy(), &overdosed, 0.0);
        let risk = work_risk(&healthy(), &overdosed, 0.0);
        assert!((0.0..=1.0).contains(&capacity), "{capacity}");
        assert!((0.0..=1.0).contains(&risk), "{risk}");
    }
}
