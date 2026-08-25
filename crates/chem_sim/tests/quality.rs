use chem_sim::{resolve, resolve_step, ChemData, Kelvin, ReactionEffect, Solution, Units};

const REAGENTS_RON: &str = include_str!("../../../assets/data/chem.reagents.ron");
const REACTIONS_RON: &str = include_str!("../../../assets/data/chem.reactions.ron");

fn data() -> ChemData {
    ChemData::from_ron(REAGENTS_RON, REACTIONS_RON).expect("chemistry data should load")
}

fn add(data: &ChemData, solution: &mut Solution, key: &str, amount: i32, purity: f32) {
    let id = data.reagent(key);
    let reagent = data.reagents.get(id);
    assert!(solution
        .add_profiled(id, Units::whole(amount), purity, reagent.ph)
        .is_zero());
}

#[test]
fn purity_and_ph_survive_proportional_transfer() {
    let data = data();
    let salt = data.reagent("sodium_chloride");
    let ash = data.reagent("ash");
    let mut source = Solution::new(Units::whole(100));
    add(&data, &mut source, "sodium_chloride", 20, 1.0);
    add(&data, &mut source, "ash", 20, 0.5);

    assert!((source.average_purity() - 0.75).abs() < 0.001);
    assert!((source.ph() - 8.75).abs() < 0.001);

    let mut destination = Solution::new(Units::whole(100));
    source.transfer_to(&mut destination, Units::whole(10));
    assert!((destination.purity_of(salt) - 1.0).abs() < 0.001);
    assert!((destination.purity_of(ash) - 0.5).abs() < 0.001);
    assert!((destination.ph() - 8.75).abs() < 0.001);
}

#[test]
fn ph_control_turns_input_quality_into_recoverable_product_quality() {
    let data = data();
    let multiver = data.reagent("multiver");
    let mut solution = Solution::new(Units::whole(100));
    add(&data, &mut solution, "sodium_chloride", 20, 1.0);
    add(&data, &mut solution, "ash", 20, 0.5);
    solution.temperature = Kelvin(390.0);

    let report = resolve(&mut solution, &data.reactions);
    assert!(report.reacted());
    let purity = solution.purity_of(multiver);
    assert!(
        (0.5..0.75).contains(&purity),
        "an imperfect but controlled batch should remain useful: {purity}"
    );
}

#[test]
fn unrelated_pure_filler_cannot_launder_low_quality_reactants() {
    let data = data();
    let reaction = data.reactions.find("pump_up").unwrap();
    let mut solution = Solution::new(Units::whole(200));
    for (key, amount) in [("epinephrine", 2), ("coffee", 5)] {
        add(&data, &mut solution, key, amount, 0.10);
    }
    add(&data, &mut solution, "water", 99, 1.0);

    assert!(solution.average_purity() > 0.90);
    assert!((reaction.input_purity(&solution) - 0.10).abs() < 0.001);
    let report = resolve(&mut solution, &data.reactions);

    assert!(!report.fired_reactions().contains(&reaction.id));
    assert_eq!(solution.volume_of(data.reagent("pump_up")), Units::ZERO);
}

#[test]
fn a_stabilizer_selects_product_instead_of_the_explosive_failure() {
    let data = data();
    let mut stable = Solution::new(Units::whole(100));
    for key in [
        "glycerol",
        "sulphuric_acid",
        "nitric_acid",
        "stabilizing_agent",
    ] {
        add(&data, &mut stable, key, 10, 1.0);
    }
    let report = resolve(&mut stable, &data.reactions);
    assert!(report
        .effects
        .iter()
        .all(|effect| !matches!(effect, ReactionEffect::Explosion(_))));
    assert_eq!(
        stable.volume_of(data.reagent("nitroglycerin")),
        Units::whole(20)
    );
    assert_eq!(
        stable.volume_of(data.reagent("stabilizing_agent")),
        Units::whole(10),
        "the stabilizer is present, not consumed"
    );

    let mut unstable = Solution::new(Units::whole(100));
    for key in ["glycerol", "sulphuric_acid", "nitric_acid"] {
        add(&data, &mut unstable, key, 10, 1.0);
    }
    let report = resolve(&mut unstable, &data.reactions);
    assert!(report
        .effects
        .iter()
        .any(|effect| matches!(effect, ReactionEffect::Explosion(power) if *power >= 5.0)));
    assert_eq!(
        unstable.volume_of(data.reagent("nitroglycerin")),
        Units::ZERO
    );
}

#[test]
fn hot_explosive_energy_scales_with_batch_size() {
    let data = data();
    let mut solution = Solution::new(Units::whole(100));
    add(&data, &mut solution, "gunpowder", 10, 1.0);
    solution.temperature = Kelvin(474.0);

    let report = resolve(&mut solution, &data.reactions);
    assert!(report.effects.iter().any(
        |effect| matches!(effect, ReactionEffect::Explosion(power) if (*power - 6.0).abs() < 0.001)
    ));
}

#[test]
fn libital_has_a_safe_ph_product_and_an_acidic_recoverable_inverse() {
    let data = data();
    let prepare = |acidic: bool| {
        let mut solution = Solution::new(Units::whole(50));
        add(&data, &mut solution, "phenol", 2, 1.0);
        add(&data, &mut solution, "nitrogen", 2, 1.0);
        add(&data, &mut solution, "oxygen", 2, 1.0);
        add(&data, &mut solution, "sodium", 1, 1.0);
        if acidic {
            solution.shift_ph(-3.0);
        }
        solution.temperature = Kelvin(300.0);
        solution
    };

    let mut controlled = prepare(false);
    resolve_step(&mut controlled, &data.reactions, 10.0);
    assert_eq!(
        controlled.volume_of(data.reagent("libital")),
        Units::whole(4)
    );
    assert_eq!(controlled.volume_of(data.reagent("libitoil")), Units::ZERO);

    let mut acidic = prepare(true);
    resolve_step(&mut acidic, &data.reactions, 10.0);
    assert_eq!(acidic.volume_of(data.reagent("libital")), Units::ZERO);
    assert_eq!(acidic.volume_of(data.reagent("libitoil")), Units::whole(3));
}

#[test]
fn hercuri_has_a_safe_ph_product_and_an_acidic_recoverable_inverse() {
    let data = data();
    let prepare = |acidic: bool| {
        let mut solution = Solution::new(Units::whole(50));
        add(&data, &mut solution, "cryostylane", 3, 1.0);
        add(&data, &mut solution, "lye", 1, 1.0);
        add(&data, &mut solution, "bromine", 1, 1.0);
        if acidic {
            solution.shift_ph(-3.5);
        }
        solution.temperature = Kelvin(240.0);
        solution
    };

    let mut controlled = prepare(false);
    resolve_step(&mut controlled, &data.reactions, 10.0);
    assert_eq!(
        controlled.volume_of(data.reagent("hercuri")),
        Units::whole(5)
    );
    assert_eq!(controlled.volume_of(data.reagent("herignis")), Units::ZERO);

    let mut acidic = prepare(true);
    resolve_step(&mut acidic, &data.reactions, 10.0);
    assert_eq!(acidic.volume_of(data.reagent("hercuri")), Units::ZERO);
    assert_eq!(acidic.volume_of(data.reagent("herignis")), Units::whole(5));
}
