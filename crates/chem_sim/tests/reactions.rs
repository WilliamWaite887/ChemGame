//! Tests run against the real `assets/data` chemistry, not fixtures, so a bad
//! edit to the data files fails here rather than in playtesting.

use std::collections::HashSet;

use chem_sim::{
    is_reacting, is_reacting_with_activation, resolve, resolve_step, resolve_step_with_activation,
    resolve_with_activation, Category, ChemData, Kelvin, ReactionEffect, ReactionProcess,
    ReagentId, Solution, Units,
};

const REAGENTS_RON: &str = include_str!("../../../assets/data/chem.reagents.ron");
const REACTIONS_RON: &str = include_str!("../../../assets/data/chem.reactions.ron");

/// The recipes the player starts the game knowing.
const STARTING_RECIPES: [&str; 3] = ["inaprovaline", "dylovene", "kelotane"];

/// The rest of the medicines authored into the ordinary order pool. These are
/// the staged half of the twelve-recipe service workflow.
const ADVANCED_ORDER_RECIPES: [&str; 9] = [
    "bicaridine",
    "hyronalin",
    "tricordrazine",
    "dermaline",
    "dexalin",
    "arithrazine",
    "potassium_iodide",
    "mannitol",
    "saline_glucose",
];

fn data() -> ChemData {
    ChemData::from_ron(REAGENTS_RON, REACTIONS_RON).expect("assets/data should load")
}

/// A beaker preloaded with whole units of each named reagent.
fn beaker(data: &ChemData, capacity: i32, contents: &[(&str, i32)]) -> Solution {
    let mut solution = Solution::new(Units::whole(capacity));
    for (name, amount) in contents {
        let overflow = solution.add(data.reagent(name), Units::whole(*amount));
        assert!(overflow.is_zero(), "{name} overflowed the test beaker");
    }
    solution
}

/// Combines two separately prepared sides exactly as a Mixing Chamber does:
/// capture provenance first, then transfer the complete first beaker into the
/// second. The activation has to survive alongside the resulting solution for
/// every timed resolver step.
fn agitated_batch(
    data: &ChemData,
    capacity: i32,
    first: &[(&str, i32)],
    second: &[(&str, i32)],
) -> (Solution, chem_sim::ReactionActivation) {
    let mut source = beaker(data, capacity, first);
    let mut destination = beaker(data, capacity, second);
    let activation = data.reactions.activate_agitation(&source, &destination);
    assert!(!activation.is_empty(), "prepared sides activated no recipe");
    let source_volume = source.total_volume();
    assert_eq!(
        source.transfer_to(&mut destination, source_volume),
        source_volume,
        "the destination needs room for the complete source beaker"
    );
    (destination, activation)
}

/// Asserts the solution holds exactly these reagents in these amounts.
fn assert_contents(data: &ChemData, solution: &Solution, expected: &[(&str, i32)]) {
    let actual: Vec<(String, Units)> = solution
        .iter()
        .map(|(id, qty)| (data.reagents.get(id).key.clone(), qty))
        .collect();
    let expected: Vec<(String, Units)> = expected
        .iter()
        .map(|(name, amount)| (name.to_string(), Units::whole(*amount)))
        .collect();

    let mut sorted_actual = actual.clone();
    sorted_actual.sort();
    let mut sorted_expected = expected.clone();
    sorted_expected.sort();
    assert_eq!(sorted_actual, sorted_expected, "solution contents differ");
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

#[test]
fn units_display_is_readable() {
    assert_eq!(Units::whole(15).to_string(), "15u");
    assert_eq!(Units::from_f64(0.5).to_string(), "0.5u");
    assert_eq!(Units::from_f64(15.25).to_string(), "15.25u");
    assert_eq!(Units::ZERO.to_string(), "0u");
    assert_eq!(Units::from_f64(-0.5).to_string(), "-0.5u");
}

#[test]
fn units_are_exact_where_floats_would_drift() {
    // A tenth of a unit, added ten times, is exactly one unit. The equivalent
    // f32 sum is not, which is the whole reason this type exists.
    let tenth = Units::from_f64(0.1);
    let sum: Units = (0..10).map(|_| tenth).sum();
    assert_eq!(sum, Units::ONE);
}

#[test]
fn quantities_survive_both_a_text_and_a_binary_round_trip() {
    // These need different encodings and it is not cosmetic. Data files are
    // hand-written, so `15` and `15.0` must both parse — which needs
    // `deserialize_any`. Binary formats are not self-describing and cannot
    // answer `deserialize_any` at all, so asking them to would fail at
    // runtime, silently, only once a solution crossed the network.
    let quantities = [
        Units::whole(15),
        Units::from_f64(0.5),
        Units::from_f64(15.25),
        Units::ZERO,
        Units::from_f64(-2.75),
    ];

    for quantity in quantities {
        let text = ron::to_string(&quantity).unwrap();
        let parsed: Units = ron::from_str(&text).unwrap();
        assert_eq!(parsed, quantity, "text round trip failed for {quantity}");

        let bytes = postcard::to_stdvec(&quantity).unwrap();
        let decoded: Units = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, quantity, "binary round trip failed for {quantity}");
    }

    // Whole numbers written without a decimal point must still parse.
    assert_eq!(ron::from_str::<Units>("15").unwrap(), Units::whole(15));
    assert_eq!(ron::from_str::<Units>("15.0").unwrap(), Units::whole(15));
}

#[test]
fn a_whole_solution_survives_a_binary_round_trip() {
    let data = data();
    let solution = beaker(&data, 100, &[("oxygen", 15), ("sugar", 30)]);

    let bytes = postcard::to_stdvec(&solution).unwrap();
    let decoded: Solution = postcard::from_bytes(&bytes).unwrap();

    assert_eq!(decoded, solution);
    assert_eq!(decoded.max_volume(), solution.max_volume());
}

// ---------------------------------------------------------------------------
// Solution
// ---------------------------------------------------------------------------

#[test]
fn add_respects_capacity_and_reports_overflow() {
    let data = data();
    let oxygen = data.reagent("oxygen");
    let mut solution = Solution::new(Units::whole(50));

    assert_eq!(solution.add(oxygen, Units::whole(30)), Units::ZERO);
    assert_eq!(solution.add(oxygen, Units::whole(30)), Units::whole(10));
    assert_eq!(solution.total_volume(), Units::whole(50));
    assert_eq!(solution.available_volume(), Units::ZERO);
}

#[test]
fn remove_takes_what_is_there_and_prunes_empties() {
    let data = data();
    let mut solution = beaker(&data, 100, &[("oxygen", 20)]);
    let oxygen = data.reagent("oxygen");

    assert_eq!(solution.remove(oxygen, Units::whole(5)), Units::whole(5));
    assert_eq!(solution.remove(oxygen, Units::whole(50)), Units::whole(15));
    assert!(solution.is_empty(), "emptied reagents should be pruned");
}

#[test]
fn transfer_draws_proportionally() {
    let data = data();
    // 3:1 mix. Pouring half must move half of each, not all of one.
    let mut source = beaker(&data, 100, &[("oxygen", 30), ("sugar", 10)]);
    let mut dest = Solution::new(Units::whole(100));

    let moved = source.transfer_to(&mut dest, Units::whole(20));

    assert_eq!(moved, Units::whole(20));
    assert_contents(&data, &dest, &[("oxygen", 15), ("sugar", 5)]);
    assert_contents(&data, &source, &[("oxygen", 15), ("sugar", 5)]);
}

#[test]
fn transfer_conserves_total_when_shares_do_not_divide_evenly() {
    let data = data();
    // Three reagents, an amount that cannot split three ways exactly.
    let mut source = beaker(&data, 100, &[("oxygen", 10), ("sugar", 10), ("carbon", 10)]);
    let mut dest = Solution::new(Units::whole(100));

    let moved = source.transfer_to(&mut dest, Units::from_f64(10.01));

    assert_eq!(moved, Units::from_f64(10.01), "asked-for amount must move");
    assert_eq!(
        source.total_volume() + dest.total_volume(),
        Units::whole(30),
        "rounding must not create or destroy reagent"
    );
}

#[test]
fn transfer_is_limited_by_destination_capacity() {
    let data = data();
    let mut source = beaker(&data, 100, &[("oxygen", 60)]);
    let mut dest = Solution::new(Units::whole(25));

    assert_eq!(
        source.transfer_to(&mut dest, Units::whole(60)),
        Units::whole(25)
    );
    assert_eq!(source.total_volume(), Units::whole(35));
    assert_eq!(dest.total_volume(), Units::whole(25));
}

#[test]
fn split_preserves_composition() {
    let data = data();
    let mut source = beaker(&data, 100, &[("oxygen", 30), ("sugar", 10)]);

    let portion = source.split(Units::whole(20));

    assert_contents(&data, &portion, &[("oxygen", 15), ("sugar", 5)]);
    assert_eq!(source.total_volume(), Units::whole(20));
}

// ---------------------------------------------------------------------------
// The seed recipes
// ---------------------------------------------------------------------------

#[test]
fn inaprovaline_from_base_reagents() {
    let data = data();
    let mut solution = beaker(&data, 100, &[("oxygen", 15), ("carbon", 15), ("sugar", 15)]);

    let report = resolve(&mut solution, &data.reactions);

    assert!(report.reacted());
    assert_contents(&data, &solution, &[("inaprovaline", 45)]);
}

#[test]
fn dylovene_from_base_reagents() {
    let data = data();
    let mut solution = beaker(
        &data,
        100,
        &[("silicon", 15), ("nitrogen", 15), ("potassium", 15)],
    );

    resolve(&mut solution, &data.reactions);

    assert_contents(&data, &solution, &[("dylovene", 45)]);
}

#[test]
fn kelotane_from_base_reagents() {
    let data = data();
    let mut solution = beaker(&data, 100, &[("silicon", 15), ("carbon", 15)]);

    resolve(&mut solution, &data.reactions);

    assert_contents(&data, &solution, &[("kelotane", 30)]);
}

#[test]
fn bicaridine_cannot_chain_through_inaprovaline_in_one_beaker() {
    let data = data();
    // The starter compound still forms, but no amount of waiting or clockless
    // resolution can invent the two-beaker provenance bicaridine requires.
    let mut solution = beaker(&data, 100, &[("oxygen", 15), ("sugar", 15), ("carbon", 60)]);

    let report = resolve(&mut solution, &data.reactions);

    assert_contents(&data, &solution, &[("inaprovaline", 45), ("carbon", 45)]);
    assert_eq!(
        report.fired_reactions().len(),
        1,
        "only the ambient starter recipe may fire"
    );
}

#[test]
fn tricordrazine_intermediates_do_not_self_mix() {
    let data = data();
    let mut solution = beaker(
        &data,
        200,
        &[
            ("carbon", 30),
            ("oxygen", 30),
            ("sugar", 30),
            ("silicon", 30),
            ("potassium", 30),
            ("nitrogen", 30),
        ],
    );

    resolve(&mut solution, &data.reactions);

    assert_contents(&data, &solution, &[("inaprovaline", 90), ("dylovene", 90)]);
}

#[test]
fn dermaline_does_not_chain_through_kelotane_without_agitation() {
    let data = data();
    let mut solution = beaker(
        &data,
        200,
        &[
            ("silicon", 15),
            ("carbon", 15),
            ("oxygen", 30),
            ("phosphorus", 30),
        ],
    );

    resolve(&mut solution, &data.reactions);

    assert_contents(
        &data,
        &solution,
        &[("kelotane", 30), ("oxygen", 30), ("phosphorus", 30)],
    );
}

#[test]
fn arithrazine_requires_two_distinct_agitation_stages() {
    let data = data();
    // Dylovene is ambient, but neither advanced stage can acquire provenance
    // merely because all of its eventual ingredients share one beaker.
    let mut solution = beaker(
        &data,
        300,
        &[
            ("silicon", 15),
            ("potassium", 15),
            ("nitrogen", 15),
            ("radium", 45),
            ("hydrogen", 90),
        ],
    );

    let report = resolve(&mut solution, &data.reactions);

    assert_contents(
        &data,
        &solution,
        &[("dylovene", 45), ("radium", 45), ("hydrogen", 90)],
    );
    assert_eq!(
        report.fired_reactions().len(),
        1,
        "only the ambient precursor forms"
    );
}

#[test]
fn dexalin_catalyst_is_required_but_not_consumed() {
    let data = data();
    let mut without = beaker(&data, 100, &[("oxygen", 60)]);
    resolve(&mut without, &data.reactions);
    assert_contents(&data, &without, &[("oxygen", 60)]);

    // Sharing a beaker is no longer enough, even with the catalyst present.
    let mut with = beaker(&data, 100, &[("oxygen", 60), ("plasma", 1)]);
    resolve(&mut with, &data.reactions);
    assert_contents(&data, &with, &[("oxygen", 60), ("plasma", 1)]);

    // One unit of plasma on the separately prepared catalyst side unlocks the
    // whole batch and survives intact.
    let (mut with, activation) = agitated_batch(&data, 100, &[("oxygen", 60)], &[("plasma", 1)]);
    resolve_with_activation(&mut with, &data.reactions, &activation);
    assert_contents(&data, &with, &[("dexalin", 30), ("plasma", 1)]);
}

#[test]
fn reactions_can_run_fractionally() {
    let data = data();
    let mut solution = Solution::new(Units::whole(10));
    for name in ["oxygen", "carbon", "sugar"] {
        let _ = solution.add(data.reagent(name), Units::from_f64(0.5));
    }

    resolve(&mut solution, &data.reactions);

    assert_eq!(
        solution.volume_of(data.reagent("inaprovaline")),
        Units::from_f64(1.5)
    );
}

#[test]
fn partial_ingredients_leave_the_remainder_behind() {
    let data = data();
    // Not enough potassium: only 5u worth of reaction can run, and nothing
    // downstream consumes the leftover silicon or nitrogen.
    let mut solution = beaker(
        &data,
        100,
        &[("silicon", 15), ("nitrogen", 15), ("potassium", 5)],
    );

    resolve(&mut solution, &data.reactions);

    assert_contents(
        &data,
        &solution,
        &[("dylovene", 15), ("silicon", 10), ("nitrogen", 10)],
    );
}

#[test]
fn leftover_reagents_cannot_activate_an_advanced_recipe() {
    let data = data();
    // Short on sugar, so only 5-of-ratio of inaprovaline forms. Carbon left in
    // the same beaker is contamination, not a substitute for the separately
    // prepared side bicaridine requires.
    let mut solution = beaker(&data, 100, &[("oxygen", 15), ("carbon", 15), ("sugar", 5)]);

    resolve(&mut solution, &data.reactions);

    assert_contents(
        &data,
        &solution,
        &[("inaprovaline", 15), ("carbon", 10), ("oxygen", 10)],
    );
}

#[test]
fn resolution_is_deterministic() {
    let data = data();
    let contents = &[
        ("carbon", 30),
        ("oxygen", 30),
        ("sugar", 30),
        ("silicon", 30),
        ("potassium", 30),
        ("nitrogen", 30),
    ];

    let mut first = beaker(&data, 200, contents);
    let mut second = beaker(&data, 200, contents);
    let first_report = resolve(&mut first, &data.reactions);
    let second_report = resolve(&mut second, &data.reactions);

    assert_eq!(first, second);
    assert_eq!(first_report, second_report);
}

// ---------------------------------------------------------------------------
// Resolver guards
// ---------------------------------------------------------------------------

#[test]
fn temperature_gates_reactions() {
    let reagents = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), dispensable: true),
        (id: "b", name: "B", color: (0.0, 1.0, 0.0)),
    ]"#;
    let reactions = r#"[
        (id: "hot", reactants: [("a", 1)], products: [("b", 1)], min_temp: Some((400.0))),
    ]"#;
    let data = ChemData::from_ron(reagents, reactions).unwrap();
    let (a, b) = (data.reagent("a"), data.reagent("b"));

    let mut cold = Solution::new(Units::whole(100));
    let _ = cold.add(a, Units::whole(10));
    resolve(&mut cold, &data.reactions);
    assert_eq!(cold.volume_of(a), Units::whole(10), "should not react cold");

    let mut hot = Solution::new(Units::whole(100));
    let _ = hot.add(a, Units::whole(10));
    hot.temperature = chem_sim::Kelvin(500.0);
    resolve(&mut hot, &data.reactions);
    assert_eq!(hot.volume_of(b), Units::whole(10), "should react when hot");
}

/// The overheat data used by the three tests below: a reaction that still runs
/// past 420K but wastes the batch doing it.
fn overheating_data(overheat: &str) -> ChemData {
    let reagents = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), dispensable: true),
        (id: "b", name: "B", color: (0.0, 1.0, 0.0)),
    ]"#;
    let reactions = format!(
        r#"[
            (id: "cook", reactants: [("a", 1)], products: [("b", 1)],
             overheat_temp: Some((420.0)), overheat: {overheat}, hints: ["Hot."]),
        ]"#
    );
    ChemData::from_ron(reagents, &reactions).unwrap()
}

#[test]
fn an_overheated_reaction_consumes_everything_and_yields_less() {
    let data = overheating_data("ReducedYield(over: 60.0)");
    let (a, b) = (data.reagent("a"), data.reagent("b"));

    let mut solution = Solution::new(Units::whole(100));
    let _ = solution.add(a, Units::whole(10));
    // Halfway through the falloff.
    solution.temperature = chem_sim::Kelvin(450.0);
    let report = resolve(&mut solution, &data.reactions);

    assert_eq!(
        solution.volume_of(a),
        Units::ZERO,
        "the reactants are gone regardless — that is what overheating costs"
    );
    assert_eq!(solution.volume_of(b), Units::whole(5), "at half yield");
    assert_eq!(
        report.overheated.len(),
        1,
        "and the chemist gets told, because they cannot see it in the beaker"
    );
}

#[test]
fn a_reaction_run_far_too_hot_yields_nothing_at_all() {
    let data = overheating_data("ReducedYield(over: 60.0)");
    let mut solution = Solution::new(Units::whole(100));
    let _ = solution.add(data.reagent("a"), Units::whole(10));
    solution.temperature = chem_sim::Kelvin(600.0);
    resolve(&mut solution, &data.reactions);

    assert!(solution.is_empty(), "everything in, nothing out");
}

#[test]
fn a_detonating_reaction_reports_a_blast_and_leaves_an_empty_beaker() {
    let data = overheating_data("Detonate(power: 3.0)");
    let mut solution = Solution::new(Units::whole(100));
    let _ = solution.add(data.reagent("a"), Units::whole(10));
    solution.temperature = chem_sim::Kelvin(450.0);
    let report = resolve(&mut solution, &data.reactions);

    assert!(
        report
            .effects
            .contains(&chem_sim::ReactionEffect::Explosion(3.0)),
        "the game layer learns about the blast through the report: {:?}",
        report.effects
    );
    assert!(solution.is_empty(), "and there is nothing left to salvage");
}

/// Makes phlogiston from `each` units of all three reactants, held just over
/// its 374K ignition point, and reports whether it went off.
fn phlogiston_batch(data: &ChemData, each: i32) -> (chem_sim::Kelvin, bool) {
    let mut beaker = beaker(
        data,
        100,
        &[
            ("plasma", each),
            ("sulphuric_acid", each),
            ("phosphorus", each),
        ],
    );
    beaker.temperature = chem_sim::Kelvin(374.5);
    let report = resolve(&mut beaker, &data.reactions);
    let detonated = report
        .effects
        .iter()
        .any(|effect| matches!(effect, chem_sim::ReactionEffect::Explosion(_)));
    (beaker.temperature, detonated)
}

/// The showpiece has to actually go off, and only when the batch is big.
///
/// This exists because it did not. The resolver runs a whole batch in one pass,
/// so the heat released is `Heat` × scale — and at the original 1.2 a *full*
/// 100u beaker peaked at 414K against a 420K threshold. The reaction could
/// never detonate at any fill, in any glassware, which made the whole overheat
/// rule dead data that read as though it worked.
#[test]
fn a_big_batch_of_phlogiston_cooks_itself_into_a_blast() {
    let data = data();

    let (small_temp, small_boom) = phlogiston_batch(&data, 15);
    assert!(
        !small_boom,
        "a modest batch has to be safe, or the recipe is unusable ({small_temp})"
    );
    assert!(small_temp.0 < 420.0);

    let (big_temp, big_boom) = phlogiston_batch(&data, 20);
    assert!(
        big_boom,
        "20u of each should run away; it only reached {big_temp}"
    );
}

/// A 50u beaker cannot hold enough to reach the threshold, whatever you do.
///
/// Worth pinning as a rule the player can rely on: if you want phlogiston and
/// not a blast, make it in a small beaker.
#[test]
fn a_small_beaker_of_phlogiston_can_never_detonate() {
    let data = data();
    // Sixteen of each is 48u — as much as a 50u beaker will take.
    let mut beaker = beaker(
        &data,
        50,
        &[("plasma", 16), ("sulphuric_acid", 16), ("phosphorus", 16)],
    );
    beaker.temperature = chem_sim::Kelvin(374.5);
    let report = resolve(&mut beaker, &data.reactions);

    assert!(
        !report
            .effects
            .iter()
            .any(|effect| matches!(effect, chem_sim::ReactionEffect::Explosion(_))),
        "a full small beaker reached {} and should not have gone off",
        beaker.temperature
    );
}

/// Makes chlorine trifluoride from `chlorine`-of-ratio (`chlorine` units of
/// chlorine, `3 * chlorine` of fluorine — the reactants are a 3:1 ratio, not
/// phlogiston's 1:1:1), held just over its 424K ignition point, and reports
/// whether it went off.
fn ctf_batch(data: &ChemData, chlorine: i32) -> (chem_sim::Kelvin, bool) {
    let mut beaker = beaker(
        data,
        100,
        &[("fluorine", chlorine * 3), ("chlorine", chlorine)],
    );
    beaker.temperature = chem_sim::Kelvin(424.5);
    let report = resolve(&mut beaker, &data.reactions);
    let detonated = report
        .effects
        .iter()
        .any(|effect| matches!(effect, chem_sim::ReactionEffect::Explosion(_)));
    (beaker.temperature, detonated)
}

/// The same showpiece property phlogiston has, pinned the same way: a modest
/// batch has to be safe, and a large one has to actually go off.
///
/// This exists because at the original `Heat(3.0)` it did not. A maximally
/// full 100u beaker (25-of-ratio) peaked at 499.5K against a 500K threshold —
/// 0.5K short, forever — the same "could never detonate at any fill" bug
/// phlogiston's original `Heat(1.2)` had, just close enough to the line that
/// nobody noticed by eye.
#[test]
fn a_big_batch_of_chlorine_trifluoride_cooks_itself_into_a_blast() {
    let data = data();

    let (small_temp, small_boom) = ctf_batch(&data, 15); // 60u total
    assert!(
        !small_boom,
        "a modest batch has to be safe, or the recipe is unusable ({small_temp})"
    );
    assert!(small_temp.0 < 500.0);

    let (big_temp, big_boom) = ctf_batch(&data, 20); // 80u total
    assert!(
        big_boom,
        "20-of-ratio should run away; it only reached {big_temp}"
    );
}

/// A 50u beaker cannot hold enough to reach the threshold, whatever you do —
/// the same guarantee phlogiston makes.
#[test]
fn a_small_beaker_of_chlorine_trifluoride_can_never_detonate() {
    let data = data();
    // Twelve-of-ratio is 48u — as much as a 50u beaker will take.
    let mut beaker = beaker(&data, 50, &[("fluorine", 36), ("chlorine", 12)]);
    beaker.temperature = chem_sim::Kelvin(424.5);
    let report = resolve(&mut beaker, &data.reactions);

    assert!(
        !report
            .effects
            .iter()
            .any(|effect| matches!(effect, chem_sim::ReactionEffect::Explosion(_))),
        "a full small beaker reached {} and should not have gone off",
        beaker.temperature
    );
}

#[test]
fn a_recipe_that_never_names_an_overheat_is_unaffected_by_temperature() {
    // The guarantee that let this land without touching a line of the existing
    // reaction data.
    let data = data();
    let mut hot = beaker(&data, 100, &[("oxygen", 15), ("carbon", 15), ("sugar", 15)]);
    hot.temperature = chem_sim::Kelvin(900.0);
    resolve(&mut hot, &data.reactions);

    assert_contents(&data, &hot, &[("inaprovaline", 45)]);
}

#[test]
fn a_stabilised_batch_makes_powder_instead_of_a_cloud() {
    // Identical reactants; one unit of stabilizing agent decides which recipe
    // wins. This is the pair that teaches catalysts and priority at once, so
    // the two outcomes are pinned against each other rather than separately.
    let data = data();
    let ingredients = [("phosphorus", 10), ("potassium", 10), ("sugar", 10)];

    let mut loose = beaker(&data, 100, &ingredients);
    let report = resolve(&mut loose, &data.reactions);
    assert!(
        report
            .effects
            .iter()
            .any(|effect| matches!(effect, ReactionEffect::Smoke(_))),
        "unstabilised, it should vent into the room"
    );
    assert_contents(&data, &loose, &[("smoke", 30)]);

    let mut stabilised = beaker(&data, 100, &ingredients);
    // One unit, and it survives: a catalyst is not consumed and does not limit
    // the yield, so 30u of powder comes off a single unit of agent.
    let overflow = stabilised.add(data.reagent("stabilizing_agent"), Units::whole(1));
    assert!(overflow.is_zero());

    let report = resolve(&mut stabilised, &data.reactions);
    assert!(
        !report
            .effects
            .iter()
            .any(|effect| matches!(effect, ReactionEffect::Smoke(_))),
        "stabilised, the cloud should stay in the beaker"
    );
    assert_contents(
        &data,
        &stabilised,
        &[("smoke_powder", 30), ("stabilizing_agent", 1)],
    );
}

#[test]
fn a_cold_reaction_only_runs_when_the_chamber_is_cold() {
    // Every temperature-gated recipe before ice wanted heat, which left the
    // chamber's lowest preset doing nothing at all.
    let data = data();

    let mut warm = beaker(&data, 100, &[("water", 20)]);
    resolve(&mut warm, &data.reactions);
    assert_contents(&data, &warm, &[("water", 20)]);

    let mut chilled = beaker(&data, 100, &[("water", 20)]);
    chilled.temperature = Kelvin(270.0);
    resolve(&mut chilled, &data.reactions);
    assert_contents(&data, &chilled, &[("ice", 20)]);
}

#[test]
fn an_endothermic_reaction_cools_its_own_beaker() {
    // `Heat` is plain addition in the resolver, so a negative value cools. The
    // bound worth pinning is the one nothing clamps: a full beaker must not be
    // able to drive the temperature through absolute zero.
    let data = data();
    let mut beaker = beaker(&data, 100, &[("ice", 33), ("plasma", 33), ("nitrogen", 33)]);
    resolve(&mut beaker, &data.reactions);

    assert_contents(&data, &beaker, &[("cryostylane", 99)]);
    assert!(
        beaker.temperature < Kelvin::AMBIENT,
        "a batch should end colder than the room, got {}",
        beaker.temperature
    );
    assert!(
        beaker.temperature > Kelvin(0.0),
        "no batch may cool past absolute zero, got {}",
        beaker.temperature
    );
}

#[test]
fn cyclic_recipes_terminate_at_the_iteration_cap() {
    // A makes B, B makes A. Without the cap this hangs the game.
    let reagents = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), dispensable: true),
        (id: "b", name: "B", color: (0.0, 1.0, 0.0), dispensable: true),
    ]"#;
    let reactions = r#"[
        (id: "forward", reactants: [("a", 1)], products: [("b", 1)]),
        (id: "back",    reactants: [("b", 1)], products: [("a", 1)]),
    ]"#;
    let data = ChemData::from_ron(reagents, reactions).unwrap();

    let mut solution = Solution::new(Units::whole(100));
    let _ = solution.add(data.reagent("a"), Units::whole(10));
    let report = resolve(&mut solution, &data.reactions);

    assert!(report.hit_iteration_cap, "cycle should trip the cap");
    assert_eq!(report.events.len(), chem_sim::MAX_ITERATIONS);
}

#[test]
fn unknown_reagents_in_reactions_are_rejected() {
    let reagents = r#"[(id: "a", name: "A", color: (1.0, 0.0, 0.0))]"#;
    let reactions = r#"[(id: "bad", reactants: [("nonexistent", 1)], products: [("a", 1)])]"#;

    assert!(ChemData::from_ron(reagents, reactions).is_err());
}

#[test]
fn targeted_purges_must_name_a_real_reagent_with_a_positive_amount() {
    let unknown = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), targeted_purges: [("missing", 1)])
    ]"#;
    assert!(ChemData::from_ron(unknown, "[]").is_err());

    let zero = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), targeted_purges: [("b", 0)]),
        (id: "b", name: "B", color: (0.0, 1.0, 0.0))
    ]"#;
    assert!(ChemData::from_ron(zero, "[]").is_err());
}

#[test]
fn agitation_sides_must_exactly_partition_recipe_inputs_regardless_of_order() {
    let reagents = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), dispensable: true),
        (id: "b", name: "B", color: (0.0, 1.0, 0.0), dispensable: true),
        (id: "c", name: "C", color: (0.0, 0.0, 1.0), dispensable: true),
        (id: "d", name: "D", color: (1.0, 1.0, 1.0)),
    ]"#;
    // Side A deliberately lists B before A. Definition order has no bearing
    // on validation; the totals and ratios do.
    let valid = r#"[
        (id: "mix", reactants: [("a", 2), ("b", 1)], catalysts: [("c", 1)],
         products: [("d", 1)], process: Agitated(
             side_a: [("b", 1), ("a", 2)], side_b: [("c", 1)])),
    ]"#;
    assert!(ChemData::from_ron(reagents, valid).is_ok());

    let wrong_ratio = r#"[
        (id: "mix", reactants: [("a", 2), ("b", 1)], catalysts: [("c", 1)],
         products: [("d", 1)], process: Agitated(
             side_a: [("a", 1), ("b", 1)], side_b: [("c", 1)])),
    ]"#;
    assert!(ChemData::from_ron(reagents, wrong_ratio).is_err());
}

#[test]
fn duplicate_catalog_keys_are_rejected_instead_of_silently_ignored() {
    let duplicate_reagents = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), dispensable: true),
        (id: "a", name: "Other A", color: (0.0, 1.0, 0.0), dispensable: true),
    ]"#;
    assert!(ChemData::from_ron(duplicate_reagents, "[]")
        .unwrap_err()
        .to_string()
        .contains("defined more than once"));

    let reagents = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), dispensable: true),
        (id: "b", name: "B", color: (0.0, 1.0, 0.0)),
    ]"#;
    let reactions = r#"[
        (id: "mix", reactants: [("a", 1)], products: [("b", 1)]),
        (id: "mix", reactants: [("a", 1)], products: [("b", 1)]),
    ]"#;
    assert!(ChemData::from_ron(reagents, reactions)
        .unwrap_err()
        .to_string()
        .contains("defined more than once"));
}

#[test]
fn invalid_numeric_profiles_and_unreachable_cycles_fail_closed() {
    let bad_ph = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0), ph: 15.0, dispensable: true),
    ]"#;
    assert!(ChemData::from_ron(bad_ph, "[]").is_err());

    let cyclic_reagents = r#"[
        (id: "a", name: "A", color: (1.0, 0.0, 0.0)),
        (id: "b", name: "B", color: (0.0, 1.0, 0.0)),
    ]"#;
    let cyclic_reactions = r#"[
        (id: "make_a", reactants: [("b", 1)], products: [("a", 1)]),
        (id: "make_b", reactants: [("a", 1)], products: [("b", 1)]),
    ]"#;
    assert!(ChemData::from_ron(cyclic_reagents, cyclic_reactions)
        .unwrap_err()
        .to_string()
        .contains("circular dependency"));
}

#[test]
fn agitation_matching_is_ratio_aware_order_independent_and_reversible() {
    let data = data();
    let saline = data.reactions.find("saline_glucose").unwrap().id;
    // The Solution sorts by reagent id, so this insertion order intentionally
    // differs from the authored side. Matching uses identities and exact
    // fixed-point ratios rather than vector position.
    let side_a = beaker(&data, 100, &[("water", 5), ("saltwater", 10)]);
    let side_b = beaker(&data, 100, &[("sugar", 5)]);
    assert!(data
        .reactions
        .activate_agitation(&side_a, &side_b)
        .contains(saline));
    assert!(data
        .reactions
        .activate_agitation(&side_b, &side_a)
        .contains(saline));

    let wrong_ratio = beaker(&data, 100, &[("water", 6), ("saltwater", 10)]);
    assert!(!data
        .reactions
        .activate_agitation(&wrong_ratio, &side_b)
        .contains(saline));

    let everything = beaker(&data, 100, &[("water", 5), ("saltwater", 10), ("sugar", 5)]);
    assert!(data
        .reactions
        .activate_agitation(&everything, &Solution::unbounded())
        .is_empty());
}

// ---------------------------------------------------------------------------
// Data and progression guardrails
// ---------------------------------------------------------------------------

#[test]
fn seed_data_loads_completely() {
    let data = data();
    assert_eq!(data.reagents.dispensable().count(), 30);
    assert_eq!(data.reactions.len(), 162);
    for recipe in STARTING_RECIPES {
        assert!(data.reactions.find(recipe).is_some(), "missing {recipe}");
    }
}

#[test]
fn pump_up_concentrates_seven_units_of_external_inputs_into_five() {
    let data = data();
    let mut solution = beaker(&data, 50, &[("epinephrine", 4), ("coffee", 10)]);

    let report = resolve(&mut solution, &data.reactions);

    assert!(report.reacted());
    assert_contents(&data, &solution, &[("pump_up", 10)]);
    let reaction = data.reactions.find("pump_up").unwrap();
    assert_eq!(reaction.min_purity, Some(0.30));
    assert_eq!(reaction.min_ph, Some(5.0));
    assert_eq!(reaction.max_ph, Some(9.0));
}

#[test]
fn maintenance_drugs_form_a_three_stage_low_yield_refinement_ladder() {
    let data = data();

    let (mut slurry, activation) =
        agitated_batch(&data, 50, &[("plant_fibre", 3)], &[("welding_fuel", 3)]);
    let report = resolve_with_activation(&mut slurry, &data.reactions, &activation);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("organic_slurry").unwrap().id));
    assert_contents(&data, &slurry, &[("organic_slurry", 3)]);

    let mut tar = beaker(
        &data,
        50,
        &[("organic_slurry", 2), ("tea", 2), ("welding_fuel", 2)],
    );
    resolve(&mut tar, &data.reactions);
    assert_contents(
        &data,
        &tar,
        &[("maintenance_tar", 6), ("sulphuric_acid", 2)],
    );

    let mut sludge = beaker(
        &data,
        100,
        &[
            ("maintenance_tar", 18),
            ("fluorosulfuric_acid", 6),
            ("hydrogen_peroxide", 5),
        ],
    );
    resolve(&mut sludge, &data.reactions);
    assert_contents(
        &data,
        &sludge,
        &[("maintenance_sludge", 6), ("hydrogen_peroxide", 5)],
    );

    let mut powder = beaker(
        &data,
        100,
        &[
            ("maintenance_sludge", 6),
            ("nitric_acid", 1),
            ("universal_enzyme", 1),
            ("acetone_oxide", 5),
        ],
    );
    resolve(&mut powder, &data.reactions);
    assert_contents(
        &data,
        &powder,
        &[("maintenance_powder", 1), ("acetone_oxide", 5)],
    );
}

#[test]
fn final_narcotics_preserve_their_authored_ratios_and_source_depth() {
    let data = data();

    let mut kronkaine = beaker(
        &data,
        50,
        &[("kronkus_extract", 6), ("welding_fuel", 4), ("ammonia", 2)],
    );
    resolve(&mut kronkaine, &data.reactions);
    assert_contents(&data, &kronkaine, &[("kronkaine", 12)]);

    let mut blastoff = beaker(&data, 50, &[("cyanide", 4), ("silver", 4), ("lye", 2)]);
    resolve(&mut blastoff, &data.reactions);
    assert_contents(&data, &blastoff, &[("blastoff", 10)]);

    let mut saturn_x = beaker(
        &data,
        50,
        &[("lead", 2), ("water", 2), ("maintenance_tar", 4)],
    );
    resolve(&mut saturn_x, &data.reactions);
    assert_contents(&data, &saturn_x, &[("saturn_x", 8)]);

    for key in ["kronkaine", "blastoff", "saturn_x"] {
        let reaction = data.reactions.find(key).unwrap();
        assert_eq!(reaction.min_purity, Some(0.40), "{key}");
        assert!(reaction.rate.is_some(), "{key} should be a monitored batch");
    }
}

#[test]
fn specialist_toxins_preserve_their_ratios_and_operating_windows() {
    let data = data();

    let mut mute = beaker(&data, 50, &[("uranium", 4), ("water", 2), ("carbon", 2)]);
    resolve(&mut mute, &data.reactions);
    assert_contents(&data, &mute, &[("mute_toxin", 4)]);

    let mut heparin = beaker(
        &data,
        50,
        &[("formaldehyde", 3), ("sodium_chloride", 3), ("lithium", 3)],
    );
    resolve(&mut heparin, &data.reactions);
    assert_contents(&data, &heparin, &[("heparin", 9)]);

    let mut lexorin = beaker(
        &data,
        50,
        &[("salbutamol", 3), ("plasma", 3), ("hydrogen", 3)],
    );
    resolve(&mut lexorin, &data.reactions);
    assert_contents(&data, &lexorin, &[("lexorin", 9)]);

    let mute = data.reactions.find("mute_toxin").unwrap();
    assert_eq!(
        (mute.min_ph, mute.optimal_ph, mute.max_ph),
        (Some(6.0), Some(12.2), Some(14.0))
    );
    assert_eq!(mute.min_purity, Some(0.40));
    let heparin = data.reactions.find("heparin").unwrap();
    assert_eq!(
        (heparin.min_ph, heparin.optimal_ph, heparin.max_ph),
        (Some(5.0), Some(8.0), Some(9.5))
    );
    assert_eq!(heparin.min_purity, Some(0.60));
    let lexorin = data.reactions.find("lexorin").unwrap();
    assert_eq!(
        (lexorin.min_ph, lexorin.optimal_ph, lexorin.max_ph),
        (Some(1.8), Some(4.0), Some(7.0))
    );
    assert_eq!(lexorin.min_purity, Some(0.40));
    assert!(lexorin.ph_shift < 0.0);
}

#[test]
fn poison_kit_adaptations_add_two_dependency_steps_and_two_expert_syntheses() {
    let data = data();

    let mut tiring = beaker(&data, 50, &[("tirizene", 4), ("saline_glucose", 2)]);
    resolve(&mut tiring, &data.reactions);
    assert_contents(&data, &tiring, &[("tiring_solution", 6)]);

    let mut pancuronium = beaker(
        &data,
        50,
        &[("curare", 2), ("salbutamol", 2), ("sodium_chloride", 2)],
    );
    pancuronium.temperature = Kelvin(320.0);
    resolve(&mut pancuronium, &data.reactions);
    assert_contents(&data, &pancuronium, &[("pancuronium", 6)]);

    let mut thiopental = beaker(&data, 50, &[("sulfonal", 2), ("sodium", 2), ("ethanol", 2)]);
    thiopental.temperature = Kelvin(400.0);
    resolve(&mut thiopental, &data.reactions);
    assert_contents(&data, &thiopental, &[("sodium_thiopental", 6)]);

    let mut initropidril = beaker(
        &data,
        50,
        &[("cyanide", 2), ("nitric_acid", 2), ("plasma", 2)],
    );
    initropidril.temperature = Kelvin(450.0);
    resolve(&mut initropidril, &data.reactions);
    assert_contents(&data, &initropidril, &[("initropidril", 6)]);

    for (key, purity) in [
        ("tiring_solution", 0.30),
        ("pancuronium", 0.60),
        ("sodium_thiopental", 0.50),
        ("initropidril", 0.70),
    ] {
        let reaction = data.reactions.find(key).unwrap();
        assert_eq!(reaction.min_purity, Some(purity), "{key}");
        assert!(reaction.rate.is_some(), "{key} should be a monitored batch");
    }
}

#[test]
fn isotope_and_opiate_refinements_require_their_expert_operating_windows() {
    let data = data();

    let mut cold_isotope = beaker(&data, 50, &[("uranium", 2), ("radium", 2), ("chlorine", 2)]);
    cold_isotope.temperature = Kelvin(499.0);
    resolve(&mut cold_isotope, &data.reactions);
    assert_eq!(
        cold_isotope.volume_of(data.reagent("polonium")),
        Units::ZERO
    );

    let mut isotope = beaker(&data, 50, &[("uranium", 2), ("radium", 2), ("chlorine", 2)]);
    isotope.temperature = Kelvin(550.0);
    resolve(&mut isotope, &data.reactions);
    assert_contents(&data, &isotope, &[("polonium", 6)]);

    let mut cool_opiate = beaker(&data, 50, &[("space_drugs", 6)]);
    cool_opiate.temperature = Kelvin(673.0);
    resolve(&mut cool_opiate, &data.reactions);
    assert_contents(&data, &cool_opiate, &[("space_drugs", 6)]);

    let mut opiate = beaker(&data, 50, &[("space_drugs", 6)]);
    opiate.temperature = Kelvin(700.0);
    resolve(&mut opiate, &data.reactions);
    assert_contents(&data, &opiate, &[("fentanyl", 6)]);

    for (key, minimum, optimum, maximum, purity) in [
        ("polonium", 5.0, 7.0, 9.0, 0.70),
        ("fentanyl", 7.0, 9.0, 11.0, 0.50),
    ] {
        let reaction = data.reactions.find(key).unwrap();
        assert_eq!(
            (reaction.min_ph, reaction.optimal_ph, reaction.max_ph),
            (Some(minimum), Some(optimum), Some(maximum))
        );
        assert_eq!(reaction.min_purity, Some(purity));
        assert!(reaction.rate.is_some(), "{key} should be a monitored batch");
    }
}

#[test]
fn final_specialist_toxins_form_through_botany_components_and_expert_refinement() {
    let data = data();

    let mut lead = beaker(&data, 50, &[("lead", 2), ("acetone", 2), ("oxygen", 2)]);
    lead.temperature = Kelvin(400.0);
    resolve(&mut lead, &data.reactions);
    assert_contents(&data, &lead, &[("lead_acetate", 6)]);

    let mut irritant = beaker(
        &data,
        50,
        &[("multiver", 2), ("ammonia", 2), ("welding_fuel", 2)],
    );
    irritant.temperature = Kelvin(300.0);
    resolve(&mut irritant, &data.reactions);
    assert_contents(&data, &irritant, &[("itching_powder", 6)]);

    let mut cold_teslium = beaker(&data, 50, &[("gunpowder", 2), ("silver", 2), ("plasma", 2)]);
    cold_teslium.temperature = Kelvin(399.0);
    resolve(&mut cold_teslium, &data.reactions);
    assert_eq!(cold_teslium.volume_of(data.reagent("teslium")), Units::ZERO);

    let mut teslium = beaker(&data, 50, &[("gunpowder", 2), ("silver", 2), ("plasma", 2)]);
    teslium.temperature = Kelvin(420.0);
    resolve(&mut teslium, &data.reactions);
    assert_contents(&data, &teslium, &[("teslium", 6)]);

    let mut rotatium = beaker(
        &data,
        50,
        &[("teslium", 2), ("mindbreaker_toxin", 2), ("fentanyl", 2)],
    );
    resolve(&mut rotatium, &data.reactions);
    assert_contents(&data, &rotatium, &[("rotatium", 6)]);

    for (key, purity) in [
        ("lead_acetate", 0.50),
        ("itching_powder", 0.30),
        ("teslium", 0.60),
        ("rotatium", 0.60),
    ] {
        let reaction = data.reactions.find(key).unwrap();
        assert_eq!(reaction.min_purity, Some(purity), "{key}");
        assert!(reaction.rate.is_some(), "{key} should be a monitored batch");
    }
}

#[test]
fn pyrosium_cools_on_synthesis_then_heats_by_consuming_oxygen() {
    let data = data();
    let mut solution = beaker(
        &data,
        100,
        &[("plasma", 5), ("radium", 5), ("phosphorus", 5)],
    );
    solution.temperature = Kelvin(300.0);

    resolve(&mut solution, &data.reactions);
    assert_contents(&data, &solution, &[("pyrosium", 15)]);
    assert_eq!(solution.temperature, Kelvin(290.0));

    let overflow = solution.add(data.reagent("oxygen"), Units::whole(5));
    assert!(overflow.is_zero());
    resolve(&mut solution, &data.reactions);
    assert_contents(
        &data,
        &solution,
        &[("pyrosium", 15), ("depleted_oxygen", 5)],
    );
    assert_eq!(solution.temperature, Kelvin(310.0));
}

#[test]
fn stabilizer_separates_portable_pulse_agents_from_immediate_failures() {
    let data = data();
    let cases = [
        (
            "sonic_powder",
            vec![("oxygen", 2), ("sugar", 2), ("phosphorus", 2)],
            chem_sim::PulseKind::Concuss,
        ),
        (
            "sorium",
            vec![
                ("carbon", 2),
                ("mercury", 2),
                ("nitrogen", 2),
                ("oxygen", 2),
            ],
            chem_sim::PulseKind::Push,
        ),
        (
            "liquid_dark_matter",
            vec![("carbon", 2), ("plasma", 2), ("radium", 2)],
            chem_sim::PulseKind::Pull,
        ),
    ];

    for (product, ingredients, kind) in cases {
        let mut unstable = beaker(&data, 100, &ingredients);
        let report = resolve(&mut unstable, &data.reactions);
        assert!(report.effects.iter().any(
            |effect| matches!(effect, ReactionEffect::Pulse { kind: actual, .. } if *actual == kind)
        ));

        let mut stable = beaker(&data, 100, &ingredients);
        assert!(stable
            .add(data.reagent("stabilizing_agent"), Units::ONE)
            .is_zero());
        let report = resolve(&mut stable, &data.reactions);
        assert!(
            !report
                .effects
                .iter()
                .any(|effect| matches!(effect, ReactionEffect::Pulse { .. })),
            "cold stabilized {product} should stay portable"
        );
        assert!(stable.volume_of(data.reagent(product)).is_positive());
        assert_eq!(
            stable.volume_of(data.reagent("stabilizing_agent")),
            Units::ONE,
            "the stabilizer must survive {product} synthesis"
        );
    }
}

#[test]
fn stabilized_pulse_agents_activate_only_at_their_authored_temperature() {
    let data = data();
    for (reagent, threshold, kind) in [
        ("sonic_powder", 374.0, chem_sim::PulseKind::Concuss),
        ("sorium", 474.0, chem_sim::PulseKind::Push),
        ("liquid_dark_matter", 474.0, chem_sim::PulseKind::Pull),
    ] {
        let mut cold = beaker(&data, 50, &[(reagent, 6)]);
        cold.temperature = Kelvin(threshold - 1.0);
        assert!(!resolve(&mut cold, &data.reactions).reacted());

        let mut hot = beaker(&data, 50, &[(reagent, 6)]);
        hot.temperature = Kelvin(threshold);
        let report = resolve(&mut hot, &data.reactions);
        let power = report.effects.iter().find_map(|effect| match effect {
            ReactionEffect::Pulse {
                kind: actual,
                power,
            } if *actual == kind => Some(*power),
            _ => None,
        });
        assert!(power.is_some_and(|power| power > 1.0));
        assert_contents(&data, &hot, &[("ash", 6)]);
    }
}

#[test]
fn order_medicines_have_the_authored_process_and_four_to_eight_second_batches() {
    let data = data();
    for starter in STARTING_RECIPES {
        let reaction = data.reactions.find(starter).unwrap();
        assert!(matches!(reaction.process, ReactionProcess::Ambient));
        assert_eq!(reaction.rate, None, "{starter} must remain instant");
    }

    // The smallest and largest deliverable quantities from station.orders.ron.
    // Testing both ends catches a clock that only happens to fit at one order
    // size, and exercises each recipe's actual yield ratio rather than
    // assuming every medicine is a 1:1 -> 2 reaction.
    let advanced = [
        ("bicaridine", 8.0, 14.0),
        ("hyronalin", 20.0, 30.0),
        ("tricordrazine", 30.0, 40.0),
        ("dermaline", 6.0, 9.0),
        ("dexalin", 12.0, 20.0),
        ("arithrazine", 20.0, 30.0),
        ("potassium_iodide", 20.0, 30.0),
        ("mannitol", 10.0, 15.0),
        ("saline_glucose", 40.0, 50.0),
    ];
    for (key, smallest_order, largest_order) in advanced {
        let reaction = data.reactions.find(key).unwrap();
        let ReactionProcess::Agitated { side_a, side_b } = &reaction.process else {
            panic!("{key} must require agitation");
        };
        assert!(!side_a.is_empty() && !side_b.is_empty());
        let rate = reaction.rate.expect("agitated order medicine needs a rate");
        let product_ratio = reaction
            .products
            .iter()
            .find_map(|(id, amount)| (data.reagents.get(*id).key == key).then_some(*amount))
            .expect("medicine reaction should make its namesake");
        for ordered_units in [smallest_order, largest_order] {
            let seconds = ordered_units / product_ratio.as_f64() / rate.as_f64();
            assert!(
                (4.0..=8.0).contains(&seconds),
                "{key}'s {ordered_units}u order batch takes {seconds:.2}s"
            );
        }
    }
}

#[test]
fn every_authored_agitated_recipe_is_accounted_for() {
    let data = data();
    let mut actual: Vec<&str> = data
        .reactions
        .iter()
        .filter(|reaction| matches!(reaction.process, ReactionProcess::Agitated { .. }))
        .map(|reaction| reaction.key.as_str())
        .collect();
    let mut expected = ADVANCED_ORDER_RECIPES.to_vec();
    expected.extend([
        "acetone_oxide",
        "epinephrine",
        "glycerol",
        "organic_slurry",
        "pentetic_acid",
    ]);
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[test]
fn ordinary_direct_pouring_cannot_activate_any_advanced_order_recipe() {
    let data = data();

    for key in ADVANCED_ORDER_RECIPES {
        let reaction = data.reactions.find(key).unwrap();
        let ReactionProcess::Agitated { side_a, side_b } = &reaction.process else {
            panic!("{key} must be agitated");
        };
        let mut combined = Solution::new(Units::whole(300));
        for &(reagent, amount) in side_a.iter().chain(side_b) {
            let overflow = combined.add(reagent, amount * 5);
            assert!(overflow.is_zero());
        }

        let report = resolve(&mut combined, &data.reactions);
        let product = reaction.products[0].0;
        assert_eq!(
            combined.volume_of(product),
            Units::ZERO,
            "putting both prepared sides directly into one beaker formed {key}"
        );
        assert!(
            !report.fired_reactions().contains(&reaction.id),
            "ordinary resolution forged provenance for {key}"
        );
    }
}

#[test]
fn staged_order_recipes_preserve_yield_catalysts_contamination_and_report_discovery() {
    let data = data();
    let contaminant = data.reagent("plant_fibre");

    for key in ADVANCED_ORDER_RECIPES {
        let reaction = data.reactions.find(key).unwrap();
        let ReactionProcess::Agitated { side_a, side_b } = &reaction.process else {
            panic!("{key} must be agitated");
        };
        let mut source = Solution::new(Units::whole(300));
        let mut destination = Solution::new(Units::whole(300));
        for &(reagent, amount) in side_a {
            assert!(source.add(reagent, amount * 5).is_zero());
        }
        for &(reagent, amount) in side_b {
            assert!(destination.add(reagent, amount * 5).is_zero());
        }
        assert!(source.add(contaminant, Units::ONE).is_zero());

        let activation = data.reactions.activate_agitation(&source, &destination);
        assert!(activation.contains(reaction.id), "{key} did not activate");
        let source_volume = source.total_volume();
        assert_eq!(
            source.transfer_to(&mut destination, source_volume),
            source_volume
        );
        let report = resolve_with_activation(&mut destination, &data.reactions, &activation);

        assert!(
            report.fired_reactions().contains(&reaction.id),
            "{key} did not report its reaction id for recipe discovery"
        );
        for &(reagent, _) in &reaction.reactants {
            assert_eq!(
                destination.volume_of(reagent),
                Units::ZERO,
                "{key} left a limiting reactant behind"
            );
        }
        for &(catalyst, required) in &reaction.catalysts {
            assert_eq!(
                destination.volume_of(catalyst),
                required * 5,
                "{key} consumed its catalyst"
            );
        }
        for &(product, amount) in &reaction.products {
            assert_eq!(
                destination.volume_of(product),
                amount * 5,
                "{key} changed its authored yield"
            );
        }
        assert_eq!(
            destination.volume_of(contaminant),
            Units::ONE,
            "{key} silently removed contamination"
        );
    }
}

#[test]
fn all_nine_order_medicines_declare_the_expected_preparation_sides() {
    let data = data();
    let expected = vec![
        ("bicaridine", vec![("inaprovaline", 1)], vec![("carbon", 1)]),
        ("hyronalin", vec![("dylovene", 1)], vec![("radium", 1)]),
        (
            "tricordrazine",
            vec![("inaprovaline", 1)],
            vec![("dylovene", 1)],
        ),
        (
            "dermaline",
            vec![("kelotane", 1)],
            vec![("oxygen", 1), ("phosphorus", 1)],
        ),
        ("dexalin", vec![("oxygen", 2)], vec![("plasma", 1)]),
        ("arithrazine", vec![("hyronalin", 1)], vec![("hydrogen", 1)]),
        (
            "potassium_iodide",
            vec![("potassium", 1)],
            vec![("iodine", 1)],
        ),
        (
            "mannitol",
            vec![("hydrogen", 2), ("water", 2)],
            vec![("sugar", 2)],
        ),
        (
            "saline_glucose",
            vec![("saltwater", 2), ("water", 1)],
            vec![("sugar", 1)],
        ),
    ];

    for (key, expected_a, expected_b) in expected {
        let reaction = data.reactions.find(key).unwrap();
        let ReactionProcess::Agitated { side_a, side_b } = &reaction.process else {
            panic!("{key} must be agitated");
        };
        let named = |side: &[(ReagentId, Units)]| {
            let mut values: Vec<(String, Units)> = side
                .iter()
                .map(|(id, amount)| (data.reagents.get(*id).key.clone(), *amount))
                .collect();
            values.sort();
            values
        };
        let expected_named = |side: Vec<(&str, i32)>| {
            let mut values: Vec<(String, Units)> = side
                .into_iter()
                .map(|(name, amount)| (name.to_string(), Units::whole(amount)))
                .collect();
            values.sort();
            values
        };
        assert_eq!(named(side_a), expected_named(expected_a), "{key} side A");
        assert_eq!(named(side_b), expected_named(expected_b), "{key} side B");
    }
}

#[test]
fn every_medicine_is_reachable_from_the_dispenser() {
    // A recipe whose ingredients can never all be obtained is a dead end the
    // player can never solve. Cheaper to catch here than in playtesting.
    //
    // Seeded from `raw()` rather than `dispensable()`: grind-only reagents are
    // roots of the graph too. They come out of produce instead of the
    // dispenser, but they are still obtainable without a reaction.
    let data = data();
    let mut reachable: HashSet<ReagentId> = data.reagents.raw().map(|r| r.id).collect();

    loop {
        let mut grew = false;
        for reaction in data.reactions.iter() {
            let inputs_available = reaction
                .reactants
                .iter()
                .chain(reaction.catalysts.iter())
                .all(|(id, _)| reachable.contains(id));
            if inputs_available {
                for product in reaction.product_ids() {
                    grew |= reachable.insert(product);
                }
            }
        }
        if !grew {
            break;
        }
    }

    for reagent in data.reagents.iter() {
        assert!(
            reachable.contains(&reagent.id),
            "'{}' is neither obtainable raw nor produced by any reaction — \
             dead end in the recipe graph",
            reagent.key
        );
    }
}

#[test]
fn locked_recipes_stay_within_reach_of_what_the_player_knows() {
    // Discovery has to be deduction, not brute force. Every locked recipe must
    // be at most one unknown compound away from something already known, so
    // the search space stays small enough to reason about.
    let data = data();
    let known_products: HashSet<ReagentId> = STARTING_RECIPES
        .iter()
        .flat_map(|key| {
            data.reactions
                .find(key)
                .expect("starting recipe")
                .product_ids()
        })
        .collect();
    let base: HashSet<ReagentId> = data.reagents.raw().map(|r| r.id).collect();

    let mut available: HashSet<ReagentId> = base.union(&known_products).copied().collect();
    let mut depth = 0;
    let mut undiscovered: Vec<&str> = data
        .reactions
        .iter()
        .filter(|r| !STARTING_RECIPES.contains(&r.key.as_str()))
        .map(|r| r.key.as_str())
        .collect();

    while !undiscovered.is_empty() {
        depth += 1;
        assert!(
            depth <= 8,
            "recipes still unreachable after 8 steps: {undiscovered:?}"
        );

        let mut newly_available = Vec::new();
        undiscovered.retain(|key| {
            let reaction = data.reactions.find(key).unwrap();
            let makeable = reaction
                .reactants
                .iter()
                .chain(reaction.catalysts.iter())
                .all(|(id, _)| available.contains(id));
            if makeable {
                newly_available.extend(reaction.product_ids());
            }
            !makeable
        });
        assert!(
            !newly_available.is_empty(),
            "no progress possible; stuck with {undiscovered:?}"
        );
        available.extend(newly_available);
    }

    // Every recipe reachable, and each hint list actually helps.
    for reaction in data.reactions.iter() {
        assert!(
            !reaction.hints.is_empty(),
            "recipe '{}' has no hints, so it can only be brute-forced",
            reaction.key
        );
    }
}

#[test]
fn medicines_carry_book_entries_and_base_reagents_do_not() {
    let data = data();
    for reagent in data.reagents.iter() {
        // Raw materials explain themselves: nobody needs telling what oxygen
        // is for, or that plant fibre is the part of the plant you did not
        // want. The entry is owed by anything a chemist has to *make*.
        if reagent.dispensable || reagent.from_produce {
            continue;
        }
        assert!(
            reagent.treats.is_some(),
            "'{}' is craftable but has no book entry explaining what it treats",
            reagent.key
        );
        assert!(
            !reagent.categories.is_empty(),
            "'{}' is craftable but names no category, so the book has nowhere \
             to file it",
            reagent.key
        );
    }
}

#[test]
fn every_reaction_files_under_a_heading() {
    // The book groups reactions by the categories of what they make. A recipe
    // whose product names none is invisible in every tab but "All" — which is
    // exactly the failure a reader would never notice, so it is checked here.
    let data = data();
    for reaction in data.reactions.iter() {
        let (product, _) = reaction.products.first().expect("reactions have products");
        let reagent = data.reagents.get(*product);
        assert!(
            !reagent.categories.is_empty(),
            "reaction '{}' makes '{}', which names no category",
            reaction.key,
            reagent.key
        );
    }
}

#[test]
fn every_category_has_at_least_one_recipe() {
    // A heading with nothing under it is a dead tab in the sidebar.
    let data = data();
    let used: HashSet<Category> = data
        .reactions
        .iter()
        .filter_map(|reaction| reaction.products.first())
        .flat_map(|(product, _)| data.reagents.get(*product).categories.clone())
        .collect();

    for category in Category::ALL {
        assert!(
            used.contains(&category),
            "nothing in the book files under '{}'",
            category.label()
        );
    }
}

#[test]
fn solution_colour_blends_by_volume() {
    let data = data();
    // Equal parts of a near-white and a near-black reagent land in between.
    let solution = beaker(&data, 100, &[("sugar", 10), ("carbon", 10)]);
    let [r, g, b] = solution.color(&data.reagents);

    for channel in [r, g, b] {
        assert!(
            (0.2..0.8).contains(&channel),
            "expected a mid-tone blend, got {r},{g},{b}"
        );
    }
}

// ---------------------------------------------------------------------------
// Reverse lookup (recipe tree)
// ---------------------------------------------------------------------------

#[test]
fn detox_and_neurology_branch_has_working_stoichiometry_and_surviving_catalysts() {
    let data = data();
    type RecipeCase<'a> = (&'a str, &'a [(&'a str, i32)], f32, &'a [(&'a str, i32)]);
    let cases: &[RecipeCase<'_>] = &[
        (
            "calomel",
            &[("mercury", 2), ("chlorine", 2)],
            400.0,
            &[("calomel", 4)],
        ),
        (
            "ammoniated_mercury",
            &[("calomel", 2), ("ammonia", 4)],
            293.0,
            &[("ammoniated_mercury", 6)],
        ),
        (
            "granibitaluri",
            &[
                ("sodium_chloride", 2),
                ("carbon", 2),
                ("sulphuric_acid", 2),
                ("iron", 5),
            ],
            293.0,
            &[("granibitaluri", 6), ("iron", 5)],
        ),
        (
            "seiver",
            &[("aluminium", 4), ("nitrogen", 4), ("potassium", 4)],
            330.0,
            &[("seiver", 6)],
        ),
        (
            "neurine",
            &[("acetone", 4), ("mannitol", 4), ("oxygen", 4)],
            293.0,
            &[("neurine", 8)],
        ),
        (
            "diphenhydramine",
            &[
                ("diethylamine", 2),
                ("oil", 2),
                ("bromine", 2),
                ("carbon", 2),
                ("ethanol", 2),
            ],
            293.0,
            &[("diphenhydramine", 8)],
        ),
        (
            "oculine",
            &[("multiver", 3), ("carbon", 3), ("hydrogen", 3)],
            293.0,
            &[("oculine", 9)],
        ),
    ];

    for (recipe, ingredients, temperature, expected) in cases {
        let mut solution = beaker(&data, 100, ingredients);
        solution.temperature = Kelvin(*temperature);
        let report = resolve(&mut solution, &data.reactions);
        let reaction = data.reactions.find(recipe).unwrap();
        assert!(
            report.fired_reactions().contains(&reaction.id),
            "{recipe} never fired"
        );
        assert_contents(&data, &solution, expected);
    }
}

#[test]
fn koibean_branch_builds_a_toxic_intermediate_into_rezadone() {
    let data = data();

    let mut intermediate = beaker(&data, 100, &[("oxygen", 3), ("potassium", 3), ("sugar", 3)]);
    let report = resolve(&mut intermediate, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("cryptobiolin").unwrap().id));
    assert_contents(&data, &intermediate, &[("cryptobiolin", 9)]);

    let mut medicine = beaker(
        &data,
        100,
        &[("carpotoxin", 4), ("cryptobiolin", 4), ("copper", 4)],
    );
    let report = resolve(&mut medicine, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("rezadone").unwrap().id));
    assert_contents(&data, &medicine, &[("rezadone", 12)]);
}

#[test]
fn antihol_refines_multiver_without_leaving_alcohol_behind() {
    let data = data();
    let mut solution = beaker(
        &data,
        100,
        &[("multiver", 6), ("copper", 6), ("ethanol", 6)],
    );

    let report = resolve(&mut solution, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("antihol").unwrap().id));
    assert_contents(&data, &solution, &[("antihol", 18)]);
}

#[test]
fn modafinil_joins_existing_component_branches_over_a_surviving_catalyst() {
    let data = data();
    let mut solution = beaker(
        &data,
        100,
        &[
            ("acetone", 3),
            ("diethylamine", 3),
            ("phenol", 3),
            ("sulphuric_acid", 3),
            ("bromine", 1),
        ],
    );

    let report = resolve(&mut solution, &data.reactions);
    let reaction = data.reactions.find("modafinil").unwrap();
    assert!(report.fired_reactions().contains(&reaction.id));
    assert_contents(&data, &solution, &[("bromine", 1), ("modafinil", 12)]);
    assert!(
        reaction.ph_shift > 0.0,
        "the source recipe consumes acid as it progresses"
    );
}

#[test]
fn naloxone_turns_an_existing_opioid_into_a_purge_medicine() {
    let data = data();
    let mut solution = beaker(
        &data,
        100,
        &[
            ("morphine", 3),
            ("hydrogen_peroxide", 3),
            ("bromine", 3),
            ("ethanol", 3),
        ],
    );

    let report = resolve(&mut solution, &data.reactions);
    let reaction = data.reactions.find("naloxone").unwrap();
    assert!(report.fired_reactions().contains(&reaction.id));
    assert_contents(&data, &solution, &[("naloxone", 12)]);
    assert!(reaction.ph_shift > 0.0);
}

#[test]
fn hercuri_requires_an_actively_cooled_cryostylane_batch() {
    let data = data();
    let prepare = |temperature| {
        let mut solution = beaker(
            &data,
            100,
            &[("cryostylane", 3), ("lye", 1), ("bromine", 1)],
        );
        solution.temperature = Kelvin(temperature);
        solution
    };

    let mut warm = prepare(293.0);
    assert!(resolve(&mut warm, &data.reactions)
        .fired_reactions()
        .is_empty());

    let mut cooled = prepare(240.0);
    let report = resolve(&mut cooled, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("hercuri").unwrap().id));
    assert_contents(&data, &cooled, &[("hercuri", 5)]);
    assert!(cooled.temperature.0 < 240.0, "the synthesis is endothermic");
}

#[test]
fn nitrous_oxide_is_a_hot_volatile_intermediate() {
    let data = data();
    let prepare = |temperature| {
        let mut solution = beaker(
            &data,
            100,
            &[("ammonia", 4), ("oxygen", 4), ("nitrogen", 2)],
        );
        solution.temperature = Kelvin(temperature);
        solution
    };

    let mut controlled = prepare(530.0);
    let report = resolve(&mut controlled, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("nitrous_oxide").unwrap().id));
    assert_contents(&data, &controlled, &[("nitrous_oxide", 10)]);
    assert!(controlled.temperature.0 > 530.0);

    let mut overheated = prepare(576.0);
    let report = resolve(&mut overheated, &data.reactions);
    assert!(report.effects.iter().any(
        |effect| matches!(effect, ReactionEffect::Explosion(power) if (*power - 2.0).abs() < 0.001)
    ));
    assert_eq!(
        overheated.volume_of(data.reagent("nitrous_oxide")),
        Units::ZERO
    );
}

#[test]
fn syriniver_refines_the_volatile_branch_into_a_dilution_medicine() {
    let data = data();
    let mut solution = beaker(
        &data,
        100,
        &[
            ("nitrous_oxide", 4),
            ("mindbreaker_toxin", 2),
            ("fluorine", 2),
            ("sulfur", 2),
        ],
    );
    solution.temperature = Kelvin(300.0);

    let report = resolve(&mut solution, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("syriniver").unwrap().id));
    assert_contents(&data, &solution, &[("syriniver", 10)]);
    assert!(solution.temperature.0 < 300.0);
}

#[test]
fn miners_salve_is_a_simple_oil_intermediate_medicine() {
    let data = data();
    let mut solution = beaker(&data, 100, &[("oil", 4), ("iron", 4), ("water", 4)]);

    let report = resolve(&mut solution, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("miners_salve").unwrap().id));
    assert_contents(&data, &solution, &[("miners_salve", 12)]);
}

#[test]
fn pyroxadone_joins_cold_and_fire_branches_in_a_narrow_hot_window() {
    let data = data();
    let mut solution = beaker(
        &data,
        100,
        &[("cryoxadone", 3), ("plasma", 3), ("phlogiston", 3)],
    );
    solution.temperature = Kelvin(390.0);

    let report = resolve(&mut solution, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("pyroxadone").unwrap().id));
    assert_contents(&data, &solution, &[("pyroxadone", 9)]);
    assert!(solution.temperature.0 > 390.0);
}

#[test]
fn regenerative_jelly_combines_two_purified_botanical_extracts() {
    let data = data();
    let mut solution = beaker(&data, 100, &[("omnizine", 6), ("slime_jelly", 6)]);

    let report = resolve(&mut solution, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("regenerative_jelly").unwrap().id));
    assert_contents(&data, &solution, &[("regenerative_jelly", 12)]);
}

#[test]
fn penthrite_chain_builds_two_expert_intermediates_over_a_surviving_stabilizer() {
    let data = data();

    let mut aldehyde = beaker(
        &data,
        100,
        &[("acetone", 3), ("formaldehyde", 3), ("water", 3)],
    );
    aldehyde.temperature = Kelvin(450.0);
    let report = resolve(&mut aldehyde, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("acetaldehyde").unwrap().id));
    assert_contents(&data, &aldehyde, &[("acetaldehyde", 9)]);

    let mut scaffold = beaker(
        &data,
        100,
        &[("acetaldehyde", 3), ("formaldehyde", 9), ("lye", 3)],
    );
    let report = resolve(&mut scaffold, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("pentaerythritol").unwrap().id));
    assert_contents(&data, &scaffold, &[("pentaerythritol", 6)]);

    let mut medicine = beaker(
        &data,
        100,
        &[
            ("pentaerythritol", 3),
            ("nitric_acid", 3),
            ("acetone", 3),
            ("stabilizing_agent", 1),
        ],
    );
    medicine.temperature = Kelvin(300.0);
    let report = resolve(&mut medicine, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("penthrite").unwrap().id));
    assert_contents(
        &data,
        &medicine,
        &[("penthrite", 9), ("stabilizing_agent", 1)],
    );
}

#[test]
fn penthrite_and_epinephrine_are_an_immediate_explosive_incompatibility() {
    let data = data();
    let mut solution = beaker(&data, 100, &[("penthrite", 4), ("epinephrine", 4)]);

    let report = resolve(&mut solution, &data.reactions);

    assert!(report.effects.iter().any(
        |effect| matches!(effect, ReactionEffect::Explosion(power) if (*power - 2.3).abs() < 0.001)
    ));
    assert_contents(&data, &solution, &[("ash", 4)]);
}

#[test]
fn exotic_stabilizer_separates_portable_tatp_from_an_immediate_failure() {
    let data = data();
    let ingredients = [
        ("acetone_oxide", 3),
        ("nitric_acid", 3),
        ("pentaerythritol", 3),
    ];

    let mut stable = Solution::new(Units::whole(100));
    for (key, amount) in ingredients
        .iter()
        .copied()
        .chain([("exotic_stabilizer", 1)])
    {
        let id = data.reagent(key);
        let definition = data.reagents.get(id);
        assert!(stable
            .add_profiled(id, Units::whole(amount), 1.0, definition.ph)
            .is_zero());
    }
    stable.temperature = Kelvin(450.0);
    let report = resolve(&mut stable, &data.reactions);
    assert!(
        report
            .effects
            .iter()
            .all(|effect| !matches!(effect, ReactionEffect::Explosion(_))),
        "stabilized batch unexpectedly failed: {report:?}"
    );
    assert_contents(&data, &stable, &[("tatp", 3), ("exotic_stabilizer", 1)]);

    let mut unstable = beaker(&data, 100, &ingredients);
    unstable.temperature = Kelvin(450.0);
    let report = resolve(&mut unstable, &data.reactions);
    assert!(report.effects.iter().any(
        |effect| matches!(effect, ReactionEffect::Explosion(power) if (*power - 3.9).abs() < 0.001)
    ));
    assert_contents(&data, &unstable, &[("ash", 3)]);
}

#[test]
fn stabilized_tatp_has_a_clear_550k_activation_threshold() {
    let data = data();
    let mut below = beaker(&data, 20, &[("tatp", 2)]);
    below.temperature = Kelvin(549.0);
    let report = resolve(&mut below, &data.reactions);
    assert!(!report.reacted());
    assert_contents(&data, &below, &[("tatp", 2)]);

    below.temperature = Kelvin(550.0);
    let report = resolve(&mut below, &data.reactions);
    assert!(report.effects.iter().any(
        |effect| matches!(effect, ReactionEffect::Explosion(power) if (*power - 3.6).abs() < 0.001)
    ));
    assert_contents(&data, &below, &[("ash", 2)]);
}

#[test]
fn emp_and_both_teslium_triggers_scale_their_electrical_pulses() {
    let data = data();
    let mut emp = beaker(&data, 50, &[("iron", 3), ("uranium", 3), ("aluminium", 3)]);
    let report = resolve(&mut emp, &data.reactions);
    assert!(report.effects.iter().any(
        |effect| matches!(effect, ReactionEffect::Emp(power) if (*power - 2.16).abs() < 0.001)
    ));
    assert_contents(&data, &emp, &[("emp_residue", 3)]);

    let mut wet = beaker(&data, 20, &[("teslium", 4), ("water", 4)]);
    let report = resolve(&mut wet, &data.reactions);
    assert!(report.effects.iter().any(
        |effect| matches!(effect, ReactionEffect::Electric(power) if (*power - 2.5).abs() < 0.001)
    ));

    let mut hot = beaker(&data, 20, &[("teslium", 4)]);
    hot.temperature = Kelvin(473.0);
    assert!(!resolve(&mut hot, &data.reactions).reacted());
    hot.temperature = Kelvin(474.0);
    let report = resolve(&mut hot, &data.reactions);
    assert!(report.effects.iter().any(
        |effect| matches!(effect, ReactionEffect::Electric(power) if (*power - 2.5).abs() < 0.001)
    ));
}

#[test]
fn carbon_dioxide_and_pax_complete_the_atmosphere_utility_branch() {
    let data = data();
    let mut carbon_dioxide = beaker(&data, 50, &[("carbon", 3), ("oxygen", 6)]);
    carbon_dioxide.temperature = Kelvin(776.0);
    assert!(!resolve(&mut carbon_dioxide, &data.reactions).reacted());
    carbon_dioxide.temperature = Kelvin(777.0);
    let report = resolve(&mut carbon_dioxide, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("carbon_dioxide").unwrap().id));
    assert_contents(&data, &carbon_dioxide, &[("carbon_dioxide", 9)]);

    let mut pax = beaker(
        &data,
        50,
        &[
            ("mindbreaker_toxin", 2),
            ("multiver", 2),
            ("sodium_chloride", 4),
            ("sodium", 2),
        ],
    );
    let report = resolve(&mut pax, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("pax").unwrap().id));
    assert_contents(&data, &pax, &[("pax", 10)]);
    assert!(data.reagents.get(data.reagent("pax")).controlled);
}

#[test]
fn psicodine_refines_an_impairing_narcotic_with_mannitol() {
    let data = data();
    let mut precursor = beaker(&data, 100, &[("mercury", 3), ("oxygen", 3), ("sugar", 3)]);
    let report = resolve(&mut precursor, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("impedrezene").unwrap().id));
    assert_contents(&data, &precursor, &[("impedrezene", 6)]);

    let mut medicine = beaker(
        &data,
        100,
        &[("mannitol", 4), ("impedrezene", 2), ("water", 4)],
    );
    let report = resolve(&mut medicine, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("psicodine").unwrap().id));
    assert_contents(&data, &medicine, &[("psicodine", 10)]);
}

#[test]
fn sulfonal_and_anacea_form_from_their_authored_advanced_branches() {
    let data = data();
    let mut sulfonal = beaker(
        &data,
        100,
        &[("acetone", 4), ("diethylamine", 4), ("sulfur", 4)],
    );
    let report = resolve(&mut sulfonal, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("sulfonal").unwrap().id));
    assert_contents(&data, &sulfonal, &[("sulfonal", 12)]);

    let mut anacea = beaker(
        &data,
        100,
        &[("haloperidol", 3), ("impedrezene", 3), ("radium", 3)],
    );
    let report = resolve(&mut anacea, &data.reactions);
    assert!(report
        .fired_reactions()
        .contains(&data.reactions.find("anacea").unwrap().id));
    assert_contents(&data, &anacea, &[("anacea", 9)]);
}

#[test]
fn aranesp_quality_selects_the_stimulant_or_its_useful_inverse() {
    let data = data();
    let batch = |purity: f32| {
        let mut solution = Solution::new(Units::whole(100));
        for key in [
            "epinephrine",
            "diethylamine",
            "phenol",
            "atropine",
            "morphine",
        ] {
            let reagent = data.reagent(key);
            let definition = data.reagents.get(reagent);
            let overflow = solution.add_profiled(reagent, Units::whole(2), purity, definition.ph);
            assert!(overflow.is_zero());
        }
        solution
    };

    let mut clean = batch(0.80);
    let clean_report = resolve(&mut clean, &data.reactions);
    assert!(clean_report
        .fired_reactions()
        .contains(&data.reactions.find("aranesp").unwrap().id));
    assert_contents(&data, &clean, &[("aranesp", 10)]);

    let mut impure = batch(0.40);
    let impure_report = resolve(&mut impure, &data.reactions);
    assert!(impure_report
        .fired_reactions()
        .contains(&data.reactions.find("epoetin_alfa").unwrap().id));
    assert_contents(&data, &impure, &[("epoetin_alfa", 10)]);
}

#[test]
fn happiness_quality_selects_the_mood_drug_or_sadness_inverse() {
    let data = data();
    let batch = |purity: f32| {
        let mut solution = Solution::new(Units::whole(100));
        for (key, amount) in [
            ("nitrous_oxide", 4),
            ("epinephrine", 2),
            ("ethanol", 2),
            ("plasma", 5),
        ] {
            let reagent = data.reagent(key);
            let definition = data.reagents.get(reagent);
            let overflow =
                solution.add_profiled(reagent, Units::whole(amount), purity, definition.ph);
            assert!(overflow.is_zero());
        }
        solution
    };

    let mut clean = batch(0.80);
    let clean_report = resolve(&mut clean, &data.reactions);
    assert!(clean_report
        .fired_reactions()
        .contains(&data.reactions.find("happiness").unwrap().id));
    assert_contents(&data, &clean, &[("happiness", 8), ("plasma", 5)]);

    let mut impure = batch(0.30);
    let impure_report = resolve(&mut impure, &data.reactions);
    assert!(impure_report
        .fired_reactions()
        .contains(&data.reactions.find("sadness").unwrap().id));
    assert_contents(&data, &impure, &[("sadness", 8), ("plasma", 5)]);
}

#[test]
fn producer_of_finds_the_reaction_that_makes_a_reagent() {
    let data = data();
    let bicaridine = data.reagent("bicaridine");
    let producer = data
        .reactions
        .producer_of(bicaridine)
        .expect("bicaridine should be producible");
    assert_eq!(producer.key, "bicaridine");
}

#[test]
fn producer_of_returns_none_for_a_raw_reagent() {
    let data = data();
    assert!(data.reactions.producer_of(data.reagent("carbon")).is_none());
}

#[test]
fn a_coproduct_never_overrides_its_primary_synthesis_path() {
    let data = data();
    let acid = data.reagent("sulphuric_acid");
    let primary = data.reactions.producer_of(acid).unwrap();

    assert_eq!(primary.key, "sulphuric_acid");
    let tar = data.reactions.find("maintenance_tar").unwrap();
    assert_eq!(tar.products[1], (acid, Units::ONE));
}

#[test]
fn every_reagent_has_at_most_one_primary_synthesis() {
    // Coproducts may overlap a standalone synthesis, but recipe-tree and
    // batch-sizing callers must always see one unambiguous primary route.
    let data = data();
    let mut seen = HashSet::new();
    for reaction in data.reactions.iter() {
        let Some(&(reagent, _)) = reaction.products.first() else {
            continue;
        };
        if !seen.insert(reagent) {
            let definition = data.reagents.get(reagent);
            let consumed_downstream = data
                .reactions
                .iter()
                .any(|candidate| candidate.reactants.iter().any(|(id, _)| *id == reagent));
            assert!(
                definition.key == "ash" || (definition.intentionally_inert && !consumed_downstream),
                "only terminal inert waste may share primary event routes; '{}' is ambiguous",
                definition.key
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Reactions that take time
// ---------------------------------------------------------------------------
//
// `resolve` is `resolve_step(.., f32::INFINITY)`, so every test above this line
// is also a test that an unrated reaction is unchanged. These pin the new half.

#[test]
fn a_recipe_with_no_rate_finishes_inside_a_single_step() {
    // The compatibility guarantee the whole design rests on: a recipe that
    // names no rate must behave under `resolve_step` exactly as it always did
    // under `resolve`, at any `dt` including zero.
    let data = data();
    for dt in [0.0, 1.0 / 240.0, 1.0, f32::INFINITY] {
        let mut solution = beaker(&data, 50, &[("oxygen", 5), ("carbon", 5), ("sugar", 5)]);
        let report = resolve_step(&mut solution, &data.reactions, dt);
        assert!(report.reacted(), "inaprovaline should have run at dt {dt}");
        assert_contents(&data, &solution, &[("inaprovaline", 15)]);
    }
}

#[test]
fn a_rated_recipe_takes_its_authored_time() {
    // Bicaridine is `rate: 1`, so the smallest ordered batch — 4u of each
    // reactant, producing 8u — takes four seconds.
    let data = data();
    let (mut solution, activation) =
        agitated_batch(&data, 50, &[("inaprovaline", 4)], &[("carbon", 4)]);

    let first = resolve_step_with_activation(&mut solution, &data.reactions, 0.5, &activation);
    assert!(first.reacted());
    assert_eq!(
        solution.volume_of(data.reagent("bicaridine")),
        Units::whole(1),
        "half a second at 1/s is a scale of 0.5, which is 1u of product"
    );
    assert!(
        solution.volume_of(data.reagent("carbon")).is_positive(),
        "the batch is not finished, so there is still carbon left"
    );

    // Seven more half-seconds finish it, and a ninth finds nothing to do.
    for _ in 0..7 {
        assert!(
            resolve_step_with_activation(&mut solution, &data.reactions, 0.5, &activation)
                .reacted()
        );
    }
    assert_contents(&data, &solution, &[("bicaridine", 8)]);
    assert!(
        !resolve_step_with_activation(&mut solution, &data.reactions, 0.5, &activation).reacted()
    );
}

#[test]
fn a_step_of_zero_runs_the_instant_chemistry_and_leaves_the_slow_alone() {
    // What `Container::mutate` relies on. Pouring into a beaker resolves
    // everything instant — grading and the panel both read it the same frame —
    // without secretly advancing a batch already running in it.
    let data = data();
    let mut solution = beaker(&data, 100, &[("oxygen", 5), ("carbon", 15), ("sugar", 5)]);

    let report = resolve_step(&mut solution, &data.reactions, 0.0);
    assert!(
        report.reacted(),
        "inaprovaline is instant and must still run"
    );
    // Inaprovaline formed; the leftover carbon cannot make bicaridine because
    // no separate preparation sides were agitated.
    assert_contents(&data, &solution, &[("inaprovaline", 15), ("carbon", 10)]);
}

#[test]
fn a_rated_reaction_spends_its_allowance_once_per_step_not_once_per_pass() {
    // The resolver loops up to `MAX_ITERATIONS` times per call. Without
    // tracking what each rated reaction has already used, a capped reaction
    // would simply be picked again next pass and run its whole per-step
    // allowance over a hundred times — a rate that silently does nothing.
    let data = data();
    let (mut solution, activation) =
        agitated_batch(&data, 100, &[("inaprovaline", 30)], &[("carbon", 30)]);

    resolve_step_with_activation(&mut solution, &data.reactions, 0.1, &activation);
    assert_eq!(
        solution.volume_of(data.reagent("bicaridine")),
        Units::from_f64(0.2),
        "a tenth of a second at 1/s is a scale of 0.1, so 0.2u of product"
    );
}

#[test]
fn a_rated_reaction_always_creeps_forward_however_short_the_frame() {
    // Rounded honestly, a scale of 5/s over a 1/1000s frame is zero — and a
    // reaction that advances by zero never finishes, on fast machines only.
    let data = data();
    let (mut solution, activation) =
        agitated_batch(&data, 50, &[("inaprovaline", 10)], &[("carbon", 10)]);

    let report = resolve_step_with_activation(&mut solution, &data.reactions, 0.001, &activation);
    assert!(report.reacted());
    assert!(solution.volume_of(data.reagent("bicaridine")).is_positive());
}

#[test]
fn activated_resolve_is_activated_step_with_no_clock() {
    // Headless callers can still opt into an activated recipe without
    // modelling wall-clock time. The ordinary `resolve` intentionally cannot.
    let data = data();

    let (mut instant, activation) =
        agitated_batch(&data, 50, &[("inaprovaline", 10)], &[("carbon", 10)]);
    resolve_with_activation(&mut instant, &data.reactions, &activation);
    assert_contents(&data, &instant, &[("bicaridine", 20)]);

    let (mut stepped, activation) =
        agitated_batch(&data, 50, &[("inaprovaline", 10)], &[("carbon", 10)]);
    resolve_step_with_activation(&mut stepped, &data.reactions, f32::INFINITY, &activation);
    assert_contents(&data, &stepped, &[("bicaridine", 20)]);
}

#[test]
fn nothing_that_releases_heat_is_rated() {
    // Phlogiston and chlorine trifluoride are tuned to the kelvin against the
    // resolver running a whole batch in one pass. Rate one of them and the
    // reaction chamber's own thermal exchange starts competing with the heat
    // it releases, which quietly moves the batch size at which it detonates —
    // and both tuning blocks in `chem.reactions.ron` become fiction.
    //
    // Not a rule of the engine, which is happy either way. A rule of the
    // *data*, and a reminder to redo the tuning before breaking it.
    let data = data();
    for reaction in data.reactions.iter() {
        let heats = reaction
            .effects
            .iter()
            .any(|effect| matches!(effect, ReactionEffect::Heat(delta) if *delta > 0.0));
        assert!(
            !(heats && reaction.rate.is_some()),
            "'{}' both releases heat and has a rate — retune it first",
            reaction.key
        );
    }
}

#[test]
fn a_finished_batch_stops_reading_as_a_running_one() {
    // The Mixing Chamber keeps the activation token until its paired
    // completion query goes false.
    let data = data();
    let (mut solution, activation) =
        agitated_batch(&data, 50, &[("inaprovaline", 10)], &[("carbon", 10)]);
    assert!(!is_reacting(&solution, &data.reactions));
    assert!(is_reacting_with_activation(
        &solution,
        &data.reactions,
        &activation
    ));

    while resolve_step_with_activation(&mut solution, &data.reactions, 0.5, &activation).reacted() {
    }
    assert!(!is_reacting_with_activation(
        &solution,
        &data.reactions,
        &activation
    ));
    assert_contents(&data, &solution, &[("bicaridine", 20)]);
}

#[test]
fn an_instant_recipe_never_reads_as_a_running_batch() {
    // Nothing unrated is ever *part-way* through: it finishes inside the pour
    // that made it possible. A beaker of the base recipe's ingredients that
    // has been resolved holds only product, and one that has not is a bug
    // somewhere else — either way this must not hold a delivery back.
    let data = data();
    let mut solution = beaker(&data, 50, &[("oxygen", 5), ("carbon", 5), ("sugar", 5)]);
    resolve_step(&mut solution, &data.reactions, 0.0);
    assert!(!is_reacting(&solution, &data.reactions));
}
