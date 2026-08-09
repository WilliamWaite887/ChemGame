//! Tests run against the real `assets/data` chemistry, not fixtures, so a bad
//! edit to the data files fails here rather than in playtesting.

use std::collections::HashSet;

use chem_sim::{resolve, ChemData, ReagentId, Solution, Units};

const REAGENTS_RON: &str = include_str!("../../../assets/data/chem.reagents.ron");
const REACTIONS_RON: &str = include_str!("../../../assets/data/chem.reactions.ron");

/// The recipes the player starts the game knowing.
const STARTING_RECIPES: [&str; 3] = ["inaprovaline", "dylovene", "kelotane"];

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

    assert_eq!(source.transfer_to(&mut dest, Units::whole(60)), Units::whole(25));
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
fn bicaridine_chains_through_inaprovaline_in_one_beaker() {
    let data = data();
    // The wiki writes this as two steps only because a beaker holds 100u. The
    // chemistry itself works in a single pot, and the resolver must handle it:
    // inaprovaline forms first, then consumes the leftover carbon.
    let mut solution = beaker(&data, 100, &[("oxygen", 15), ("sugar", 15), ("carbon", 60)]);

    let report = resolve(&mut solution, &data.reactions);

    assert_contents(&data, &solution, &[("bicaridine", 90)]);
    assert_eq!(
        report.fired_reactions().len(),
        2,
        "should have fired inaprovaline then bicaridine"
    );
}

#[test]
fn tricordrazine_needs_both_intermediates() {
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

    // Kelotane also wants silicon and carbon; its lower priority is what keeps
    // it from eating the ingredients before the real recipe runs.
    assert_contents(&data, &solution, &[("tricordrazine", 180)]);
}

#[test]
fn dermaline_chains_through_kelotane() {
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

    assert_contents(&data, &solution, &[("dermaline", 90)]);
}

#[test]
fn arithrazine_resolves_a_three_step_chain() {
    let data = data();
    // dylovene -> hyronalin -> arithrazine, all in one pass.
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

    assert_contents(&data, &solution, &[("arithrazine", 180)]);
    assert_eq!(report.fired_reactions().len(), 3, "three distinct reactions");
}

#[test]
fn dexalin_catalyst_is_required_but_not_consumed() {
    let data = data();
    let mut without = beaker(&data, 100, &[("oxygen", 60)]);
    resolve(&mut without, &data.reactions);
    assert_contents(&data, &without, &[("oxygen", 60)]);

    // One unit of plasma catalyses the whole beaker and survives intact.
    let mut with = beaker(&data, 100, &[("oxygen", 60), ("plasma", 1)]);
    resolve(&mut with, &data.reactions);
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
fn leftover_reagents_keep_reacting_and_contaminate_the_result() {
    let data = data();
    // Short on sugar, so only 5u of inaprovaline forms — and then the 10u of
    // carbon still sitting in the beaker converts most of it onward into
    // bicaridine. The chemist asked for one thing and has made three.
    //
    // This is the mess the order system grades as a partial delivery, so it is
    // worth pinning down: sloppy ratios produce contaminated output rather
    // than simply less output.
    let mut solution = beaker(&data, 100, &[("oxygen", 15), ("carbon", 15), ("sugar", 5)]);

    resolve(&mut solution, &data.reactions);

    assert_contents(
        &data,
        &solution,
        &[("bicaridine", 20), ("inaprovaline", 5), ("oxygen", 10)],
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

// ---------------------------------------------------------------------------
// Data and progression guardrails
// ---------------------------------------------------------------------------

#[test]
fn seed_data_loads_completely() {
    let data = data();
    assert_eq!(data.reagents.dispensable().count(), 18);
    assert_eq!(data.reactions.len(), 9);
    for recipe in STARTING_RECIPES {
        assert!(data.reactions.find(recipe).is_some(), "missing {recipe}");
    }
}

#[test]
fn every_medicine_is_reachable_from_the_dispenser() {
    // A recipe whose ingredients can never all be obtained is a dead end the
    // player can never solve. Cheaper to catch here than in playtesting.
    let data = data();
    let mut reachable: HashSet<ReagentId> =
        data.reagents.dispensable().map(|r| r.id).collect();

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
            "'{}' can never be produced — dead end in the recipe graph",
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
        .flat_map(|key| data.reactions.find(key).expect("starting recipe").product_ids())
        .collect();
    let base: HashSet<ReagentId> = data.reagents.dispensable().map(|r| r.id).collect();

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
        assert!(depth <= 3, "recipes still unreachable after 3 steps: {undiscovered:?}");

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
        if reagent.dispensable {
            continue;
        }
        assert!(
            reagent.treats.is_some(),
            "'{}' is craftable but has no book entry explaining what it treats",
            reagent.key
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
