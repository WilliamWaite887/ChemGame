//! Heating, cooling, overheating and blasts.

use chem_sim::body::{blast_radius, explosion_damage};
use chem_sim::thermal::{approach, boil_off, Overheat, SETTLE};
use chem_sim::{ChemData, Kelvin, Solution, Units};

const REAGENTS: &str = r#"[
    (id: "stable",   name: "Stable",   color: (0.5, 0.5, 0.5), dispensable: true),
    (id: "volatile", name: "Volatile", color: (0.8, 0.2, 0.8), dispensable: true,
     boils_at: Some((323.15))),
]"#;

fn data() -> ChemData {
    ChemData::from_ron(REAGENTS, "[]").expect("fixture should load")
}

// ---------------------------------------------------------------------------
// approach
// ---------------------------------------------------------------------------

#[test]
fn heating_closes_the_gap_and_slows_as_it_arrives() {
    let target = Kelvin(400.0);
    let mut current = Kelvin(300.0);

    let first = approach(current, target, 0.5, 1.0);
    let first_step = first.0 - current.0;
    current = first;
    let second = approach(current, target, 0.5, 1.0);
    let second_step = second.0 - current.0;

    assert!(first_step > 0.0, "it should be heating");
    assert!(
        second_step < first_step,
        "each step should be smaller than the last: {first_step} then {second_step}"
    );
    assert!(second.0 < target.0, "and never overshoot");
}

#[test]
fn cooling_is_the_same_arithmetic_in_reverse() {
    let cooled = approach(Kelvin(500.0), Kelvin(293.15), 0.5, 1.0);
    assert!(cooled.0 < 500.0 && cooled.0 > 293.15);
    assert_eq!(cooled.0, 500.0 - (500.0 - 293.15) * 0.5);
}

#[test]
fn the_result_does_not_depend_on_the_frame_rate() {
    // This is the whole reason `approach` is exponential rather than linear. A
    // machine that heats faster on a better GPU is a bug nobody would find.
    let one_big_step = approach(Kelvin(300.0), Kelvin(500.0), 0.5, 1.0);

    let mut stepped = Kelvin(300.0);
    for _ in 0..10 {
        stepped = approach(stepped, Kelvin(500.0), 0.5, 0.1);
    }

    assert!(
        (one_big_step.0 - stepped.0).abs() < 0.01,
        "one 1.0s step gave {}, ten 0.1s steps gave {}",
        one_big_step.0,
        stepped.0
    );
}

#[test]
fn it_settles_exactly_on_the_target_rather_than_creeping_at_it() {
    // Without the snap this is an asymptote, and a chamber dialled to exactly
    // a recipe's `min_temp` would never fire it.
    let mut current = Kelvin(300.0);
    for _ in 0..200 {
        current = approach(current, Kelvin(400.0), 0.5, 0.1);
    }
    assert_eq!(current, Kelvin(400.0));

    let nearly = approach(Kelvin(400.0 - SETTLE / 2.0), Kelvin(400.0), 0.5, 0.1);
    assert_eq!(nearly, Kelvin(400.0), "inside the settle band it snaps");
}

#[test]
fn a_zero_step_or_a_dead_heater_changes_nothing() {
    let current = Kelvin(310.0);
    assert_eq!(approach(current, Kelvin(500.0), 0.5, 0.0), current);
    assert_eq!(approach(current, Kelvin(500.0), 0.0, 1.0), current);
}

// ---------------------------------------------------------------------------
// Overheat
// ---------------------------------------------------------------------------

#[test]
fn a_reaction_below_its_threshold_yields_in_full() {
    let overheat = Overheat::ReducedYield { over: 60.0 };
    assert_eq!(
        overheat.yield_factor(Kelvin(420.0), Kelvin(400.0)),
        Units::ONE
    );
    assert_eq!(
        overheat.yield_factor(Kelvin(420.0), Kelvin(420.0)),
        Units::ONE,
        "exactly at the threshold is still fine"
    );
}

#[test]
fn yield_falls_to_nothing_across_the_falloff_and_never_below() {
    let overheat = Overheat::ReducedYield { over: 60.0 };
    let threshold = Kelvin(420.0);

    let half = overheat.yield_factor(threshold, Kelvin(450.0));
    assert_eq!(half, Units::from_f64(0.5), "halfway through the falloff");

    assert_eq!(overheat.yield_factor(threshold, Kelvin(480.0)), Units::ZERO);
    assert_eq!(
        overheat.yield_factor(threshold, Kelvin(2000.0)),
        Units::ZERO,
        "and it never goes negative no matter how hot it gets"
    );
}

#[test]
fn detonating_and_ruining_leave_no_yield_at_all() {
    let threshold = Kelvin(420.0);
    assert_eq!(
        Overheat::Detonate { power: 3.0 }.yield_factor(threshold, Kelvin(421.0)),
        Units::ZERO
    );
    assert_eq!(
        Overheat::Ruin.yield_factor(threshold, Kelvin(421.0)),
        Units::ZERO
    );
}

// ---------------------------------------------------------------------------
// Boiling
// ---------------------------------------------------------------------------

#[test]
fn only_reagents_past_their_boiling_point_leave_the_beaker() {
    let data = data();
    let mut beaker = Solution::new(Units::whole(100));
    let _ = beaker.add(data.reagent("stable"), Units::whole(20));
    let _ = beaker.add(data.reagent("volatile"), Units::whole(10));

    beaker.temperature = Kelvin(300.0);
    let cold = boil_off(&mut beaker, &data.reagents);
    assert!(cold.is_empty(), "nothing boils at room temperature");

    beaker.temperature = Kelvin(350.0);
    let gas = boil_off(&mut beaker, &data.reagents);
    assert_eq!(gas.volume_of(data.reagent("volatile")), Units::whole(10));
    assert_eq!(beaker.volume_of(data.reagent("volatile")), Units::ZERO);
    assert_eq!(
        beaker.volume_of(data.reagent("stable")),
        Units::whole(20),
        "the rest of the batch is untouched"
    );
}

// ---------------------------------------------------------------------------
// Blasts
// ---------------------------------------------------------------------------

#[test]
fn a_blast_falls_off_with_distance_and_stops_at_its_radius() {
    let power = 3.0;
    let radius = blast_radius(power);

    let point_blank = explosion_damage(power, 0.0);
    let halfway = explosion_damage(power, radius / 2.0);
    let edge = explosion_damage(power, radius);
    let outside = explosion_damage(power, radius + 1.0);

    assert!(point_blank.total() > halfway.total());
    assert!(halfway.total() > edge.total());
    assert_eq!(edge.total(), Units::ZERO, "the radius is where it stops");
    assert_eq!(outside.total(), Units::ZERO);

    assert!(
        point_blank.brute.is_positive() && point_blank.burn.is_positive(),
        "a blast should do both kinds of damage"
    );
    assert_eq!(
        point_blank.toxin,
        Units::ZERO,
        "and neither of the other two"
    );
}

#[test]
fn a_bigger_blast_hurts_more_and_reaches_further() {
    assert!(blast_radius(6.0) > blast_radius(3.0));
    assert!(explosion_damage(6.0, 1.0).total() > explosion_damage(3.0, 1.0).total());
}

#[test]
fn a_blast_with_no_power_does_nothing_and_does_not_panic() {
    assert_eq!(explosion_damage(0.0, 0.0).total(), Units::ZERO);
    assert_eq!(explosion_damage(-1.0, 0.0).total(), Units::ZERO);
    assert_eq!(explosion_damage(3.0, -1.0), explosion_damage(3.0, 0.0));
}
