//! Metabolism: what a chemical does to the person who takes it.
//!
//! These run against fixture data rather than `assets/data`, because the point
//! is the arithmetic, not the content. The guardrails at the bottom are the
//! exception — those read the real files, so a reagent written with a
//! meaningless effect fails here rather than in playtesting.

use chem_sim::body::{
    metabolise, Bloodstream, Vitals, CONTACT_REFERENCE_DOSE, DIGESTION_RATE, MAX_DAMAGE_PER_KIND,
    OXYGEN_RECOVERY, RECOVER,
};
use chem_sim::{
    ChemData, Damage, DamageKind, ReagentEffect, Route, Solution, StatusKind, Units, WorldEffect,
};

const REAGENTS_RON: &str = include_str!("../../../assets/data/chem.reagents.ron");
const REACTIONS_RON: &str = include_str!("../../../assets/data/chem.reactions.ron");

/// A pharmacy built to exercise the tick, not to be played.
const FIXTURE_REAGENTS: &str = r#"[
    (id: "inert",   name: "Inert",   color: (0.5, 0.5, 0.5), dispensable: true),
    (id: "quick",   name: "Quick",   color: (0.5, 0.5, 0.5), dispensable: true,
     metabolism: Some(2.0)),
    (id: "poison",  name: "Poison",  color: (0.2, 0.8, 0.2), dispensable: true,
     effects: [Harm(Toxin, 3)]),
    (id: "cure",    name: "Cure",    color: (0.2, 0.2, 0.8), dispensable: true,
     effects: [Heal(Toxin, 2)]),
    (id: "acid",    name: "Acid",    color: (0.9, 0.9, 0.2), dispensable: true,
     effects: [Contact(Burn, 2)]),
    (id: "booze",   name: "Booze",   color: (0.6, 0.4, 0.2), dispensable: true,
     metabolism: Some(0.2),
     effects: [Status(kind: Drunk, seconds: 4.0, intensity: 0.8)]),
    (id: "sober",   name: "Sober",   color: (0.5, 0.7, 0.9), dispensable: true,
     effects: [Counter(kind: Drunk, seconds: 2.0, intensity: 0.25)]),
    (id: "glow",    name: "Glow",    color: (0.4, 0.9, 0.4), dispensable: true,
     effects: [Status(kind: Irradiated, seconds: 6.0, intensity: 2.0)]),
    (id: "shield",  name: "Shield",  color: (0.4, 0.9, 0.4), dispensable: true,
     effects: [Status(kind: RadiationShield, seconds: 6.0, intensity: 1.0)]),
    (id: "detox",   name: "Detox",   color: (0.4, 0.9, 0.4), dispensable: true,
     effects: [Purge(1)]),
    (id: "crasher", name: "Crasher", color: (0.4, 0.9, 0.4), dispensable: true,
     metabolism: Some(2.0),
     effects: [Status(kind: Hastened, seconds: 4.0, intensity: 1.0)],
     after_effects: [Status(kind: Sluggish, seconds: 8.0, intensity: 1.5)]),
    (id: "tiered",  name: "Tiered",  color: (0.7, 0.3, 0.7), dispensable: true,
     overdose: Some(10), critical_overdose: Some(20),
     effects: [Heal(Brute, 1)],
     overdose_effects: [Harm(Toxin, 1)],
     critical_effects: [Harm(Toxin, 4)]),
    (id: "left",    name: "Left",    color: (0.5, 0.5, 0.5), dispensable: true),
    (id: "right",   name: "Right",   color: (0.5, 0.5, 0.5), dispensable: true),
    (id: "merged",  name: "Merged",  color: (0.5, 0.5, 0.5),
     effects: [Heal(Burn, 5)]),
]"#;

/// One reaction, so the "reagents react inside you" case has something to fire.
const FIXTURE_REACTIONS: &str = r#"[
    (id: "merge", reactants: [("left", 1), ("right", 1)], products: [("merged", 2)],
     hints: ["Two halves."]),
]"#;

fn fixture() -> ChemData {
    ChemData::from_ron(FIXTURE_REAGENTS, FIXTURE_REACTIONS).expect("fixture data should load")
}

fn real() -> ChemData {
    ChemData::from_ron(REAGENTS_RON, REACTIONS_RON).expect("assets/data should load")
}

/// A dose of one reagent, ready to hand to [`Bloodstream::receive`].
fn dose(data: &ChemData, key: &str, amount: i32) -> Solution {
    let mut solution = Solution::unbounded();
    let _ = solution.add(data.reagent(key), Units::whole(amount));
    solution
}

/// Injects a dose straight into a fresh body.
fn injected(data: &ChemData, key: &str, amount: i32) -> (Vitals, Bloodstream) {
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    let mut d = dose(data, key, amount);
    blood.receive(&mut d, Route::Injected, &mut vitals, data);
    (vitals, blood)
}

// ---------------------------------------------------------------------------
// Metabolism rate
// ---------------------------------------------------------------------------

#[test]
fn a_dose_drains_at_the_default_rate() {
    let data = fixture();
    let inert = data.reagent("inert");
    let (mut vitals, mut blood) = injected(&data, "inert", 10);

    // 10u at the default 0.4u per tick is exactly 25 ticks.
    for tick in 1..=24 {
        metabolise(&mut vitals, &mut blood, &data);
        assert!(
            blood.blood.volume_of(inert).is_positive(),
            "inert should still be present after {tick} ticks"
        );
    }
    metabolise(&mut vitals, &mut blood, &data);
    assert!(blood.blood.is_empty(), "10u should be gone after 25 ticks");
}

#[test]
fn a_declared_metabolism_rate_overrides_the_default() {
    let data = fixture();
    let (mut vitals, mut blood) = injected(&data, "quick", 10);

    // 2.0u per tick: five ticks, not twenty-five.
    for _ in 0..5 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    assert!(
        blood.blood.is_empty(),
        "a fast reagent should clear in five ticks"
    );
}

#[test]
fn a_remainder_smaller_than_the_rate_still_gets_one_full_tick() {
    let data = fixture();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    let mut d = Solution::unbounded();
    // A tenth of a unit, against a 0.4u rate.
    let _ = d.add(data.reagent("poison"), Units::from_f64(0.1));
    blood.receive(&mut d, Route::Injected, &mut vitals, &data);

    let report = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(
        report.harmed.toxin,
        Units::whole(3),
        "a remainder should do a full tick of damage before it goes"
    );
    assert!(blood.blood.is_empty(), "and then it should be gone");
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

#[test]
fn injecting_delivers_the_whole_dose_to_the_blood() {
    let data = fixture();
    let (_, blood) = injected(&data, "inert", 10);
    assert_eq!(
        blood.blood.volume_of(data.reagent("inert")),
        Units::whole(10)
    );
    assert!(blood.stomach.is_empty(), "injection bypasses the stomach");
}

#[test]
fn swallowing_delivers_less_and_delivers_it_slowly() {
    let data = fixture();
    let inert = data.reagent("inert");
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    let mut d = dose(&data, "inert", 10);
    blood.receive(&mut d, Route::Ingested, &mut vitals, &data);

    // 60% of it lands, and it lands in the stomach.
    assert_eq!(blood.stomach.volume_of(inert), Units::whole(6));
    assert!(blood.blood.is_empty(), "nothing is absorbed yet");

    metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(
        blood.stomach.volume_of(inert),
        Units::whole(6) - DIGESTION_RATE,
        "one tick moves one digestion step"
    );
}

#[test]
fn splashing_delivers_very_little() {
    let data = fixture();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    let mut d = dose(&data, "inert", 10);
    blood.receive(&mut d, Route::Touched, &mut vitals, &data);

    assert_eq!(
        blood.blood.volume_of(data.reagent("inert")),
        Units::from_f64(1.5),
        "touch is 15%"
    );
}

#[test]
fn contact_damage_scales_with_the_route() {
    let data = fixture();

    let measure = |route: Route| {
        let mut vitals = Vitals::default();
        let mut blood = Bloodstream::new();
        let mut d = dose(&data, "acid", 10);
        blood.receive(&mut d, route, &mut vitals, &data);
        vitals.damage.burn
    };

    let injected = measure(Route::Injected);
    let swallowed = measure(Route::Ingested);
    let splashed = measure(Route::Touched);

    // 10u of a `Contact(Burn, 2)` reagent is one reference dose, doubled by the
    // needle.
    assert_eq!(injected, Units::whole(4));
    assert!(
        injected > swallowed && swallowed > splashed,
        "a needle should hurt more than a mouthful, which should hurt more than a splash \
         (got {injected:?} / {swallowed:?} / {splashed:?})"
    );
    assert_eq!(CONTACT_REFERENCE_DOSE, Units::whole(10));
}

#[test]
fn a_patch_delivers_the_full_dose_and_its_topical_repair_bonus() {
    let data = real();
    let salve = data.reagent("miners_salve");
    let apply = |route| {
        let mut vitals = Vitals {
            damage: Damage {
                brute: Units::whole(10),
                burn: Units::whole(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut blood = Bloodstream::new();
        let mut d = dose(&data, "miners_salve", 10);
        blood.receive(&mut d, route, &mut vitals, &data);
        (vitals, blood)
    };

    let (patched_vitals, patched_blood) = apply(Route::Patched);
    assert_eq!(patched_vitals.damage.brute, Units::whole(6));
    assert_eq!(patched_vitals.damage.burn, Units::whole(6));
    assert_eq!(patched_blood.blood.volume_of(salve), Units::whole(10));

    let (injected_vitals, injected_blood) = apply(Route::Injected);
    assert_eq!(injected_vitals.damage.brute, Units::whole(10));
    assert_eq!(injected_vitals.damage.burn, Units::whole(10));
    assert_eq!(injected_blood.blood.volume_of(salve), Units::whole(10));
}

// ---------------------------------------------------------------------------
// Damage and collapse
// ---------------------------------------------------------------------------

#[test]
fn pyroxadone_heals_only_while_the_patient_is_burning() {
    let data = real();
    let treat = |burning: bool| {
        let mut vitals = Vitals {
            damage: Damage {
                brute: Units::whole(20),
                burn: Units::whole(20),
                toxin: Units::whole(20),
                oxygen: Units::whole(20),
            },
            ..Default::default()
        };
        let mut blood = Bloodstream::new();
        let mut d = dose(&data, "pyroxadone", 5);
        blood.receive(&mut d, Route::Injected, &mut vitals, &data);
        if burning {
            blood.add_status(StatusKind::Burning, 10.0, 2.0);
        }
        metabolise(&mut vitals, &mut blood, &data)
    };

    assert_eq!(treat(false).healed, Damage::default());
    assert_eq!(
        treat(true).healed,
        Damage {
            brute: Units::whole(2),
            burn: Units::whole(3),
            toxin: Units::whole(2),
            oxygen: Units::whole(4),
        }
    );
}

#[test]
fn regenerative_jelly_heals_all_four_damage_types() {
    let data = real();
    let mut vitals = Vitals {
        damage: Damage {
            brute: Units::whole(10),
            burn: Units::whole(10),
            toxin: Units::whole(10),
            oxygen: Units::whole(10),
        },
        ..Default::default()
    };
    let mut blood = Bloodstream::new();
    let mut d = dose(&data, "regenerative_jelly", 4);
    blood.receive(&mut d, Route::Injected, &mut vitals, &data);

    assert_eq!(
        metabolise(&mut vitals, &mut blood, &data).healed,
        Damage {
            brute: Units::from_f64(1.5),
            burn: Units::from_f64(1.5),
            toxin: Units::from_f64(1.5),
            oxygen: Units::from_f64(1.5),
        }
    );
}

#[test]
fn penthrite_only_repairs_a_critically_injured_patient() {
    let data = real();
    let treat = |damage_each: i32| {
        let mut vitals = Vitals {
            damage: Damage {
                brute: Units::whole(damage_each),
                burn: Units::whole(damage_each),
                toxin: Units::whole(damage_each),
                oxygen: Units::whole(damage_each),
            },
            ..Default::default()
        };
        let mut blood = Bloodstream::new();
        let mut d = dose(&data, "penthrite", 5);
        blood.receive(&mut d, Route::Injected, &mut vitals, &data);
        metabolise(&mut vitals, &mut blood, &data)
    };

    assert_eq!(treat(19).healed, Damage::default());
    assert_eq!(
        treat(20).healed,
        Damage {
            brute: Units::whole(2),
            burn: Units::whole(2),
            toxin: Units::whole(2),
            oxygen: Units::whole(6),
        }
    );
}

#[test]
fn psicodine_suppresses_three_readable_cognitive_impairments() {
    let data = real();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    blood.add_status(StatusKind::Hallucinating, 12.0, 2.0);
    blood.add_status(StatusKind::Paranoid, 12.0, 2.0);
    blood.add_status(StatusKind::Unsteady, 12.0, 2.0);
    let before = [
        blood.status(StatusKind::Hallucinating).intensity,
        blood.status(StatusKind::Paranoid).intensity,
        blood.status(StatusKind::Unsteady).intensity,
    ];

    let mut d = dose(&data, "psicodine", 5);
    blood.receive(&mut d, Route::Injected, &mut vitals, &data);
    metabolise(&mut vitals, &mut blood, &data);

    let after = [
        blood.status(StatusKind::Hallucinating).intensity,
        blood.status(StatusKind::Paranoid).intensity,
        blood.status(StatusKind::Unsteady).intensity,
    ];
    assert!(after
        .iter()
        .zip(before)
        .all(|(after, before)| *after < before));
    assert!(blood.status(StatusKind::Focused).intensity > 0.0);
}

#[test]
fn sulfonal_incapacitates_only_after_twenty_two_ticks_and_keeps_its_clock_on_save() {
    let data = real();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    let mut poison = dose(&data, "sulfonal", 2);
    blood.receive(&mut poison, Route::Injected, &mut vitals, &data);

    for _ in 0..10 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    let saved = ron::ser::to_string(&blood).unwrap();
    let mut blood: Bloodstream = ron::from_str(&saved).unwrap();
    for _ in 0..11 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    assert!(
        !blood.incapacitated(),
        "the delayed poison fired before tick 22"
    );

    metabolise(&mut vitals, &mut blood, &data);
    assert!(blood.incapacitated());
    assert_eq!(vitals.damage.toxin, Units::whole(11));
}

#[test]
fn anacea_purges_medicine_without_removing_an_unrelated_poison() {
    let data = real();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    for (reagent, amount) in [("bicaridine", 6), ("cyanide", 6), ("anacea", 2)] {
        let mut incoming = dose(&data, reagent, amount);
        blood.receive(&mut incoming, Route::Injected, &mut vitals, &data);
    }

    let report = metabolise(&mut vitals, &mut blood, &data);
    let bicaridine = data.reagent("bicaridine");
    let cyanide = data.reagent("cyanide");

    assert!(report.purged.contains(&(bicaridine, Units::whole(5))));
    assert!(!report.purged.iter().any(|(id, _)| *id == cyanide));
    assert!(blood.blood.volume_of(cyanide) > Units::whole(5));
}

#[test]
fn nicotine_is_a_mild_stimulant_until_its_fifteen_unit_overdose() {
    let data = real();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    blood.add_status(StatusKind::Sedated, 12.0, 2.0);

    let mut ordinary = dose(&data, "nicotine", 5);
    blood.receive(&mut ordinary, Route::Injected, &mut vitals, &data);
    let ordinary_report = metabolise(&mut vitals, &mut blood, &data);

    assert_eq!(ordinary_report.harmed, Damage::default());
    assert!(blood.status(StatusKind::Focused).intensity > 0.0);
    assert!(blood.status(StatusKind::Sedated).intensity < 2.0);

    let mut overdose_vitals = Vitals::default();
    let mut overdose_blood = Bloodstream::new();
    let mut overdose = dose(&data, "nicotine", 16);
    overdose_blood.receive(&mut overdose, Route::Injected, &mut overdose_vitals, &data);
    let overdose_report = metabolise(&mut overdose_vitals, &mut overdose_blood, &data);

    assert_eq!(overdose_report.harmed.oxygen, Units::from_f64(1.1));
    assert_eq!(overdose_report.harmed.toxin, Units::from_f64(0.1));
    assert!(overdose_report
        .overdosing
        .contains(&data.reagent("nicotine")));
}

#[test]
fn aranesp_trades_stimulation_for_damage_and_epoetin_escalates_over_time() {
    let data = real();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    let mut stimulant = dose(&data, "aranesp", 5);
    blood.receive(&mut stimulant, Route::Injected, &mut vitals, &data);

    let report = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(report.harmed.oxygen, Units::from_f64(0.5));
    assert_eq!(report.harmed.toxin, Units::from_f64(0.5));
    assert!(blood.status(StatusKind::Hastened).intensity > 0.0);
    assert!(blood.status(StatusKind::Focused).intensity > 0.0);

    let mut patient = Vitals::default();
    patient.damage.oxygen = Units::whole(20);
    let mut treatment = Bloodstream::new();
    let mut inverse = dose(&data, "epoetin_alfa", 12);
    treatment.receive(&mut inverse, Route::Injected, &mut patient, &data);
    for _ in 0..9 {
        metabolise(&mut patient, &mut treatment, &data);
    }
    assert_eq!(treatment.status(StatusKind::Blurred).intensity, 0.0);

    let tenth = metabolise(&mut patient, &mut treatment, &data);
    assert_eq!(tenth.healed.oxygen, Units::ONE);
    assert!(treatment.status(StatusKind::Blurred).intensity > 0.0);
    for _ in 0..21 {
        metabolise(&mut patient, &mut treatment, &data);
    }
    assert!(treatment.status(StatusKind::Unsteady).intensity > 0.0);
    assert!(treatment.status(StatusKind::Hallucinating).intensity > 0.0);
}

#[test]
fn happiness_changes_mood_while_sadness_strips_its_exact_counter_drugs() {
    let data = real();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    blood.add_status(StatusKind::Sadness, 10.0, 2.0);
    blood.add_status(StatusKind::Paranoid, 10.0, 2.0);
    blood.add_status(StatusKind::Unsteady, 10.0, 2.0);
    let mut happy = dose(&data, "happiness", 10);
    blood.receive(&mut happy, Route::Injected, &mut vitals, &data);

    let report = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(report.harmed.toxin, Units::from_f64(0.2));
    assert!(blood.status(StatusKind::Happiness).intensity > 0.0);
    assert!(blood.status(StatusKind::Sadness).intensity < 2.0);
    assert!(blood.status(StatusKind::Paranoid).intensity < 2.0);
    assert!(blood.status(StatusKind::Unsteady).intensity < 2.0);

    let mut target_vitals = Vitals::default();
    let mut target_blood = Bloodstream::new();
    for (key, amount) in [
        ("happiness", 6),
        ("psicodine", 6),
        ("cyanide", 6),
        ("sadness", 2),
    ] {
        let mut incoming = dose(&data, key, amount);
        target_blood.receive(&mut incoming, Route::Injected, &mut target_vitals, &data);
    }
    let purge = metabolise(&mut target_vitals, &mut target_blood, &data);
    for key in ["happiness", "psicodine"] {
        assert!(purge.purged.contains(&(data.reagent(key), Units::whole(5))));
    }
    assert!(!purge
        .purged
        .iter()
        .any(|(id, _)| *id == data.reagent("cyanide")));
}

#[test]
fn pump_up_resists_collapse_but_overdose_compounds_its_breathing_risk() {
    let data = real();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    let mut ordinary = dose(&data, "pump_up", 5);
    blood.receive(&mut ordinary, Route::Injected, &mut vitals, &data);

    let report = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(report.harmed.oxygen, Units::from_f64(0.15));
    assert_eq!(report.harmed.toxin, Units::ZERO);
    assert!(blood.status(StatusKind::Stabilized).intensity > 0.0);
    assert!(blood.status(StatusKind::Focused).intensity > 0.0);
    assert_eq!(blood.collapse_threshold(), Units::whole(125));

    let mut overdose_vitals = Vitals::default();
    let mut overdose_blood = Bloodstream::new();
    let mut overdose = dose(&data, "pump_up", 31);
    overdose_blood.receive(&mut overdose, Route::Injected, &mut overdose_vitals, &data);
    let overdose_report = metabolise(&mut overdose_vitals, &mut overdose_blood, &data);

    assert_eq!(overdose_report.harmed.oxygen, Units::from_f64(0.9));
    assert_eq!(overdose_report.harmed.toxin, Units::from_f64(0.3));
    assert!(overdose_report
        .overdosing
        .contains(&data.reagent("pump_up")));
    assert!(overdose_blood.status(StatusKind::Unsteady).intensity >= 1.2);
}

#[test]
fn mushroom_hallucinogen_is_slow_lived_and_overdose_deepens_disorientation() {
    let data = real();
    let reagent = data.reagent("mushroom_hallucinogen");
    let definition = data.reagents.get(reagent);
    assert_eq!(definition.rate(), Units::from_f64(0.08));

    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    let mut ordinary = dose(&data, "mushroom_hallucinogen", 5);
    blood.receive(&mut ordinary, Route::Injected, &mut vitals, &data);
    let report = metabolise(&mut vitals, &mut blood, &data);

    assert!(report.overdosing.is_empty());
    assert!(blood.status(StatusKind::Hallucinating).intensity > 0.0);
    assert!(blood.status(StatusKind::Drunk).intensity > 0.0);
    assert!(blood.status(StatusKind::Unsteady).intensity > 0.0);
    assert_eq!(blood.blood.volume_of(reagent), Units::from_f64(4.92));

    let mut overdose_vitals = Vitals::default();
    let mut overdose_blood = Bloodstream::new();
    let mut overdose = dose(&data, "mushroom_hallucinogen", 31);
    overdose_blood.receive(&mut overdose, Route::Injected, &mut overdose_vitals, &data);
    let overdose_report = metabolise(&mut overdose_vitals, &mut overdose_blood, &data);

    assert!(overdose_report.overdosing.contains(&reagent));
    assert!(overdose_blood.status(StatusKind::Paranoid).intensity > 0.0);
    assert!(overdose_blood.status(StatusKind::Blurred).intensity > 0.0);
    assert!(overdose_blood.status(StatusKind::Unsteady).intensity >= 1.0);
}

#[test]
fn maintenance_ladder_trades_harsher_refinement_for_distinct_survival_benefits() {
    let data = real();

    let tick = |key: &str, amount: i32| {
        let mut vitals = Vitals::default();
        let mut blood = Bloodstream::new();
        blood.add_status(StatusKind::Sedated, 10.0, 2.0);
        blood.add_status(StatusKind::Unsteady, 10.0, 2.0);
        let mut incoming = dose(&data, key, amount);
        blood.receive(&mut incoming, Route::Injected, &mut vitals, &data);
        let report = metabolise(&mut vitals, &mut blood, &data);
        (blood, report)
    };

    let (tar, tar_report) = tick("maintenance_tar", 5);
    assert_eq!(tar_report.harmed.toxin, Units::from_f64(1.5));
    assert!(tar.status(StatusKind::Stabilized).intensity > 0.0);
    assert!(tar.status(StatusKind::Sedated).intensity < 2.0);

    let (sludge, sludge_report) = tick("maintenance_sludge", 5);
    assert_eq!(sludge_report.harmed.toxin, Units::from_f64(0.5));
    assert!(sludge.status(StatusKind::Analgesic).intensity > 0.0);

    let (powder, powder_report) = tick("maintenance_powder", 5);
    assert_eq!(powder_report.harmed.toxin, Units::from_f64(0.1));
    assert!(powder.status(StatusKind::Focused).intensity > 0.0);
    assert!(powder.status(StatusKind::Unsteady).intensity < 2.0);

    for (key, amount, toxin) in [
        ("maintenance_tar", 31, 9.5),
        ("maintenance_sludge", 26, 2.0),
        ("maintenance_powder", 16, 3.1),
    ] {
        let (_, report) = tick(key, amount);
        assert!(report.overdosing.contains(&data.reagent(key)));
        assert_eq!(report.harmed.toxin, Units::from_f64(toxin), "{key}");
    }
}

#[test]
fn final_narcotics_have_distinct_speed_motor_and_concealment_identities() {
    let data = real();

    let tick = |key: &str, amount: i32| {
        let mut vitals = Vitals::default();
        let mut blood = Bloodstream::new();
        let mut incoming = dose(&data, key, amount);
        blood.receive(&mut incoming, Route::Injected, &mut vitals, &data);
        let report = metabolise(&mut vitals, &mut blood, &data);
        (blood, report)
    };

    let (kronkaine, kronkaine_report) = tick("kronkaine", 5);
    assert!(kronkaine_report.harmed.toxin.is_positive());
    assert!(kronkaine.status(StatusKind::Hastened).intensity >= 1.8);
    assert!(kronkaine.status(StatusKind::Focused).intensity >= 1.2);

    let (blastoff, blastoff_report) = tick("blastoff", 5);
    assert!(blastoff_report.harmed.oxygen.is_positive());
    assert!(blastoff.status(StatusKind::Hallucinating).intensity > 0.0);
    assert!(blastoff.motor_instability() > kronkaine.motor_instability());

    let (saturn_x, saturn_report) = tick("saturn_x", 5);
    assert!(saturn_report.harmed.toxin.is_positive());
    assert!(saturn_x.status(StatusKind::Obscured).intensity >= 1.5);
    assert!(saturn_x.concealment() >= 0.65);

    for (key, amount) in [("kronkaine", 21), ("blastoff", 31), ("saturn_x", 26)] {
        let (_, report) = tick(key, amount);
        assert!(report.overdosing.contains(&data.reagent(key)), "{key}");
    }
}

#[test]
fn damage_clamps_at_zero_and_at_the_ceiling() {
    let mut vitals = Vitals::default();

    vitals.apply(Damage::of(DamageKind::Brute, Units::whole(500)));
    assert_eq!(vitals.damage.brute, MAX_DAMAGE_PER_KIND);
    assert_eq!(vitals.fraction(DamageKind::Brute), 1.0);

    vitals.heal(Damage::of(DamageKind::Brute, Units::whole(500)));
    assert_eq!(vitals.damage.brute, Units::ZERO);
    assert_eq!(vitals.fraction(DamageKind::Brute), 0.0);
}

#[test]
fn collapse_trips_at_the_threshold_and_does_not_clear_until_recovery() {
    let mut vitals = Vitals::default();

    vitals.apply(Damage::of(DamageKind::Brute, Units::whole(99)));
    assert!(!vitals.collapsed, "99 is still standing");

    vitals.apply(Damage::of(DamageKind::Brute, Units::whole(1)));
    assert!(vitals.collapsed, "100 goes down");

    // The hysteresis is the point: healing back to just under the collapse
    // threshold must not stand you straight back up, or a chemist hovering at
    // the line flickers between states every tick.
    vitals.heal(Damage::of(DamageKind::Brute, Units::whole(15)));
    assert_eq!(vitals.total(), Units::whole(85));
    assert!(
        vitals.collapsed,
        "85 is above the recovery line, still down"
    );

    vitals.heal(Damage::of(DamageKind::Brute, Units::whole(10)));
    assert!(vitals.total() < RECOVER);
    assert!(!vitals.collapsed, "below the recovery line, back up");
}

#[test]
fn collapse_is_reported_as_an_edge_not_a_level() {
    let data = fixture();
    let (mut vitals, mut blood) = injected(&data, "poison", 200);

    let mut collapse_reports = 0;
    for _ in 0..60 {
        if metabolise(&mut vitals, &mut blood, &data).collapsed {
            collapse_reports += 1;
        }
    }
    assert!(vitals.collapsed, "200u of poison should put anyone down");
    assert_eq!(
        collapse_reports, 1,
        "the game layer needs one notification, not one per tick"
    );
}

#[test]
fn oxygen_debt_clears_on_its_own_and_brute_never_does() {
    let data = fixture();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    vitals.apply(Damage {
        brute: Units::whole(10),
        oxygen: Units::whole(10),
        ..Damage::default()
    });

    metabolise(&mut vitals, &mut blood, &data);

    assert_eq!(vitals.damage.oxygen, Units::whole(10) - OXYGEN_RECOVERY);
    assert_eq!(
        vitals.damage.brute,
        Units::whole(10),
        "brute needs bicaridine, not time"
    );
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

#[test]
fn healing_and_harming_in_the_same_tick_net_out() {
    let data = fixture();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    vitals.apply(Damage::of(DamageKind::Toxin, Units::whole(50)));

    let mut poison = dose(&data, "poison", 10);
    blood.receive(&mut poison, Route::Injected, &mut vitals, &data);
    let mut cure = dose(&data, "cure", 10);
    blood.receive(&mut cure, Route::Injected, &mut vitals, &data);

    // Poison does 3, cure repairs 2: a net 1 toxin per tick.
    let report = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(report.harmed.toxin, Units::whole(3));
    assert_eq!(report.healed.toxin, Units::whole(2));
    assert_eq!(vitals.damage.toxin, Units::whole(51));
}

#[test]
fn overdose_tiers_stack_rather_than_replace() {
    let data = fixture();

    let tick_toxin = |amount: i32| {
        let (mut vitals, mut blood) = injected(&data, "tiered", amount);
        let report = metabolise(&mut vitals, &mut blood, &data);
        (
            report.harmed.toxin,
            report.healed.brute,
            report.overdosing.len(),
        )
    };

    // Under the threshold: the medicine works and nothing else happens.
    assert_eq!(tick_toxin(5), (Units::ZERO, Units::whole(1), 0));
    // Past it: still working, now also hurting.
    assert_eq!(tick_toxin(15), (Units::whole(1), Units::whole(1), 1));
    // Past critical: both harmful tiers, on top of the healing.
    assert_eq!(tick_toxin(25), (Units::whole(5), Units::whole(1), 1));
}

#[test]
fn reagents_react_inside_a_bloodstream() {
    let data = fixture();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();

    let mut left = dose(&data, "left", 10);
    blood.receive(&mut left, Route::Injected, &mut vitals, &data);
    let mut right = dose(&data, "right", 10);
    let report = blood.receive(&mut right, Route::Injected, &mut vitals, &data);

    assert!(
        report.reactions.reacted(),
        "the second half should react with the first"
    );
    assert_eq!(
        blood.blood.volume_of(data.reagent("merged")),
        Units::whole(20),
        "injecting two halves of a recipe makes the product in you"
    );
}

// ---------------------------------------------------------------------------
// Statuses
// ---------------------------------------------------------------------------

#[test]
fn a_status_builds_while_its_reagent_is_present_and_decays_once_it_is_gone() {
    let data = fixture();
    let (mut vitals, mut blood) = injected(&data, "booze", 2);

    for _ in 0..5 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    let drunk = blood.status(StatusKind::Drunk);
    assert!(drunk.remaining > 0.0, "five ticks of booze should land");
    assert_eq!(drunk.intensity, 0.8);

    // Drink it dry, then wait it out.
    for _ in 0..200 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    assert!(blood.blood.is_empty());
    assert_eq!(
        blood.status(StatusKind::Drunk).remaining,
        0.0,
        "with nothing topping it up, it should decay away"
    );
}

#[test]
fn water_sobers_you_up_faster_than_waiting() {
    let data = fixture();
    let (mut vitals, mut blood) = injected(&data, "booze", 4);
    for _ in 0..10 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    let drunk_before = blood.status(StatusKind::Drunk);
    assert!(drunk_before.remaining > 0.0);

    let mut sober = dose(&data, "sober", 10);
    blood.receive(&mut sober, Route::Injected, &mut vitals, &data);
    for _ in 0..4 {
        metabolise(&mut vitals, &mut blood, &data);
    }

    let drunk_after = blood.status(StatusKind::Drunk);
    assert!(
        drunk_after.intensity < drunk_before.intensity,
        "a counter should cut the intensity, not just the clock"
    );
}

#[test]
fn radiation_deals_damage_through_its_status_tick() {
    let data = fixture();
    let (mut vitals, mut blood) = injected(&data, "glow", 5);

    let report = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(
        report.harmed.toxin,
        Units::whole(2),
        "irradiated deals toxin equal to its intensity"
    );
    assert!(blood.status(StatusKind::Irradiated).remaining > 0.0);
}

#[test]
fn radiation_shield_reduces_new_irradiation_without_erasing_old_exposure() {
    let mut blood = Bloodstream::new();
    blood.add_status(StatusKind::Irradiated, 8.0, 2.0);
    assert_eq!(blood.status(StatusKind::Irradiated).intensity, 2.0);

    blood.add_status(StatusKind::RadiationShield, 8.0, 1.0);
    blood.add_status(StatusKind::Irradiated, 8.0, 4.0);

    // One point of protection blocks 75% of the incoming intensity. The old
    // exposure remains stronger, because prophylaxis is not a cure.
    assert_eq!(blood.status(StatusKind::Irradiated).intensity, 2.0);
    assert_eq!(blood.radiation_resistance(), 0.75);
}

#[test]
fn sedation_incapacitation_and_apparent_death_are_distinct_from_damage() {
    let mut blood = Bloodstream::new();
    blood.add_status(StatusKind::Sedated, 10.0, 2.0);
    assert!(blood.incapacitated());
    assert!(!blood.appears_dead());

    blood.add_status(StatusKind::Sedated, 10.0, 4.0);
    assert!(blood.appears_dead());
    assert!(
        Vitals::default().damage.is_zero(),
        "sedation does not fake injury"
    );
}

#[test]
fn stabilization_and_analgesia_raise_collapse_without_healing() {
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    blood.add_status(StatusKind::Stabilized, 10.0, 1.0);
    blood.add_status(StatusKind::Analgesic, 10.0, 1.0);
    vitals.apply(Damage {
        brute: Units::whole(80),
        burn: Units::whole(60),
        ..Damage::default()
    });
    blood.reconcile_collapse(&mut vitals, false);

    assert_eq!(blood.collapse_threshold(), Units::whole(155));
    assert!(
        !vitals.collapsed,
        "masked damage remains below the raised threshold"
    );
    assert_eq!(vitals.total(), Units::whole(140), "no damage was healed");
}

#[test]
fn status_aggregates_provide_deterministic_gameplay_inputs() {
    let mut blood = Bloodstream::new();
    blood.add_status(StatusKind::Chilled, 8.0, 1.0);
    blood.add_status(StatusKind::Hallucinating, 8.0, 1.0);
    blood.add_status(StatusKind::Unsteady, 8.0, 1.0);
    blood.add_status(StatusKind::Obscured, 8.0, 1.0);
    blood.add_status(StatusKind::Muted, 8.0, 1.0);

    assert!(blood.movement_multiplier() < 1.0);
    assert!(blood.perception_distortion() > 0.0);
    assert!(blood.motor_instability() > 0.0);
    assert!(blood.concealment() > 0.0);
    assert!(blood.communication_suppression() > 0.0);
}

#[test]
fn every_status_has_a_mechanical_or_behavioral_signal() {
    for kind in StatusKind::ALL {
        let signalled = !kind.tick_damage(1.0).is_zero()
            || kind.movement_multiplier(1.0) != 1.0
            || kind.perception_distortion(1.0) != 0.0
            || kind.motor_instability(1.0) != 0.0
            || kind.concealment(1.0) != 0.0
            || kind.communication_suppression(1.0) != 0.0
            || matches!(
                kind,
                StatusKind::Stabilized
                    | StatusKind::Analgesic
                    | StatusKind::RadiationShield
                    | StatusKind::Pacified
            );
        assert!(
            signalled,
            "{} has no headless gameplay signal",
            kind.label()
        );
    }
}

#[test]
fn purge_accelerates_only_currently_harmful_reagents() {
    let data = fixture();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    for (key, amount) in [("poison", 5), ("cure", 5), ("detox", 5)] {
        let mut d = dose(&data, key, amount);
        blood.receive(&mut d, Route::Injected, &mut vitals, &data);
    }

    let report = metabolise(&mut vitals, &mut blood, &data);
    assert!(report
        .purged
        .iter()
        .any(|(id, amount)| *id == data.reagent("poison") && *amount == Units::whole(1)));
    assert_eq!(
        blood.blood.volume_of(data.reagent("poison")),
        Units::whole(5) - Units::whole(1) - chem_sim::DEFAULT_METABOLISM
    );
    assert_eq!(
        blood.blood.volume_of(data.reagent("cure")),
        Units::whole(5) - chem_sim::DEFAULT_METABOLISM,
        "therapeutic medicine is not a purge target"
    );
}

#[test]
fn naloxone_purges_morphine_and_reverses_its_acute_sedation() {
    let data = real();
    let morphine = data.reagent("morphine");
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    for (key, amount) in [("morphine", 10), ("naloxone", 5)] {
        let mut d = dose(&data, key, amount);
        blood.receive(&mut d, Route::Injected, &mut vitals, &data);
    }

    let report = metabolise(&mut vitals, &mut blood, &data);

    assert!(report
        .purged
        .iter()
        .any(|(id, amount)| *id == morphine && *amount == Units::whole(3)));
    assert!(
        blood.status(StatusKind::Sedated).intensity < 0.8,
        "the antagonist should reverse Morphine's ordinary sedative effect"
    );
}

#[test]
fn mute_toxin_exposes_a_communication_suppression_signal() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "mute_toxin", 2);

    metabolise(&mut vitals, &mut blood, &data);

    assert!(blood.status(StatusKind::Muted).intensity > 0.0);
    assert!(blood.communication_suppression() >= 0.5);
}

#[test]
fn heparin_only_worsens_an_existing_physical_injury() {
    let data = real();
    let (mut healthy_vitals, mut healthy_blood) = injected(&data, "heparin", 1);
    let healthy = metabolise(&mut healthy_vitals, &mut healthy_blood, &data);
    assert!(healthy.harmed.is_zero());

    let (mut injured_vitals, mut injured_blood) = injected(&data, "heparin", 1);
    injured_vitals.apply(Damage::of(DamageKind::Brute, Units::whole(5)));
    let injured = metabolise(&mut injured_vitals, &mut injured_blood, &data);

    assert_eq!(injured.harmed.brute, Units::whole(2));
    assert_eq!(injured.harmed.oxygen, Units::from_f64(0.5));
}

#[test]
fn amanitin_terminal_damage_scales_with_completed_exposure_ticks() {
    let data = real();
    let amanitin = data.reagent("amanitin");
    let (mut vitals, mut blood) = injected(&data, "amanitin", 1);

    for tick in 1..=4 {
        let report = metabolise(&mut vitals, &mut blood, &data);
        assert!(
            report.harmed.is_zero(),
            "Amanitin fired early on tick {tick}"
        );
        assert!(!report.after_effects.contains(&amanitin));
    }
    let terminal = metabolise(&mut vitals, &mut blood, &data);

    assert_eq!(terminal.harmed.toxin, Units::whole(15));
    assert_eq!(terminal.after_effects, vec![amanitin]);
}

#[test]
fn purging_amanitin_early_does_not_fake_its_terminal_metabolism_effect() {
    let data = real();
    let amanitin = data.reagent("amanitin");
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    for (key, amount) in [("amanitin", 1), ("pentetic_acid", 2)] {
        let mut d = dose(&data, key, amount);
        blood.receive(&mut d, Route::Injected, &mut vitals, &data);
    }

    let report = metabolise(&mut vitals, &mut blood, &data);

    assert!(report
        .purged
        .iter()
        .any(|(id, amount)| *id == amanitin && *amount == Units::whole(1)));
    assert!(!report.after_effects.contains(&amanitin));
    assert_eq!(report.harmed.toxin, Units::ZERO);
}

#[test]
fn curare_builds_steady_harm_before_delayed_paralysis() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "curare", 2);

    for tick in 1..=10 {
        let report = metabolise(&mut vitals, &mut blood, &data);
        assert_eq!(report.harmed.oxygen, Units::whole(1));
        assert_eq!(report.harmed.toxin, Units::whole(1));
        assert_eq!(
            blood.status(StatusKind::Sedated).intensity,
            0.0,
            "Curare paralyzed before its eleventh cycle on tick {tick}",
        );
    }
    metabolise(&mut vitals, &mut blood, &data);

    assert!(blood.status(StatusKind::Sedated).intensity >= 3.0);
    assert!(blood.incapacitated());
}

#[test]
fn epinephrine_is_lexorins_specific_fast_counteragent() {
    let data = real();
    let lexorin = data.reagent("lexorin");
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();
    for (key, amount) in [("lexorin", 5), ("epinephrine", 1)] {
        let mut d = dose(&data, key, amount);
        blood.receive(&mut d, Route::Injected, &mut vitals, &data);
    }

    let report = metabolise(&mut vitals, &mut blood, &data);

    assert!(report
        .purged
        .iter()
        .any(|(id, amount)| *id == lexorin && *amount == Units::whole(2)));
    assert_eq!(blood.blood.volume_of(lexorin), Units::from_f64(2.6));
    assert!(blood.status(StatusKind::Choking).intensity > 0.0);
}

#[test]
fn tirizene_and_tiring_solution_are_distinct_nonlethal_slowdowns() {
    let data = real();
    let (mut tirizene_vitals, mut tirizene_blood) = injected(&data, "tirizene", 2);
    let tirizene_report = metabolise(&mut tirizene_vitals, &mut tirizene_blood, &data);
    let (mut tiring_vitals, mut tiring_blood) = injected(&data, "tiring_solution", 2);
    let tiring_report = metabolise(&mut tiring_vitals, &mut tiring_blood, &data);

    assert!(tirizene_report.harmed.is_zero());
    assert!(tiring_report.harmed.is_zero());
    assert!(tirizene_blood.movement_multiplier() < 1.0);
    assert!(tiring_blood.movement_multiplier() < tirizene_blood.movement_multiplier());

    let before = tiring_blood.status(StatusKind::Sluggish).intensity;
    let mut antidote = dose(&data, "synaptizine", 1);
    tiring_blood.receive(&mut antidote, Route::Injected, &mut tiring_vitals, &data);
    let counter = metabolise(&mut tiring_vitals, &mut tiring_blood, &data);
    assert!(counter.purged.iter().any(|(id, amount)| {
        *id == data.reagent("tiring_solution") && *amount == Units::from_f64(1.4)
    }));
    metabolise(&mut tiring_vitals, &mut tiring_blood, &data);
    assert!(tiring_blood.status(StatusKind::Sluggish).intensity < before);
}

#[test]
fn tetrodotoxin_has_a_warning_phase_then_stacked_damage_thresholds() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "tetrodotoxin", 5);

    for tick in 1..=6 {
        let report = metabolise(&mut vitals, &mut blood, &data);
        assert!(
            report.harmed.is_zero(),
            "Tetrodotoxin harmed on tick {tick}"
        );
        assert_eq!(blood.status(StatusKind::Muted).intensity, 0.0);
    }
    let warning = metabolise(&mut vitals, &mut blood, &data);
    assert!(warning.harmed.is_zero());
    assert!(blood.status(StatusKind::Muted).intensity > 0.0);

    for _ in 8..=12 {
        assert!(metabolise(&mut vitals, &mut blood, &data).harmed.is_zero());
    }
    let paralysis = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(paralysis.harmed.oxygen, Units::whole(2));
    assert_eq!(paralysis.harmed.toxin, Units::whole(2));
    assert!(blood.incapacitated());

    for _ in 14..=20 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    let organ_phase = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(organ_phase.harmed.oxygen, Units::whole(4));
    assert_eq!(organ_phase.harmed.toxin, Units::whole(4));

    for _ in 22..=28 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    let lethal_phase = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(lethal_phase.harmed.oxygen, Units::whole(8));
    assert_eq!(lethal_phase.harmed.toxin, Units::whole(8));
}

#[test]
fn pancuronium_is_silent_until_its_tenth_cycle_paralysis() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "pancuronium", 2);

    for tick in 1..=9 {
        let report = metabolise(&mut vitals, &mut blood, &data);
        assert!(report.harmed.is_zero(), "Pancuronium harmed on tick {tick}");
        assert!(!blood.incapacitated());
    }
    let onset = metabolise(&mut vitals, &mut blood, &data);

    assert_eq!(onset.harmed.oxygen, Units::whole(3));
    assert!(blood.incapacitated());
    assert!(blood.status(StatusKind::Choking).intensity > 0.0);
}

#[test]
fn sodium_thiopental_knocks_out_without_direct_damage_after_ten_cycles() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "sodium_thiopental", 4);

    for tick in 1..=9 {
        let report = metabolise(&mut vitals, &mut blood, &data);
        assert!(report.harmed.is_zero(), "Thiopental harmed on tick {tick}");
        assert!(!blood.incapacitated());
    }
    let onset = metabolise(&mut vitals, &mut blood, &data);

    assert!(onset.harmed.is_zero());
    assert!(blood.incapacitated());
}

#[test]
fn initropidril_escalates_from_toxin_damage_into_rapid_collapse() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "initropidril", 2);

    for _ in 1..=3 {
        let report = metabolise(&mut vitals, &mut blood, &data);
        assert_eq!(report.harmed.toxin, Units::from_f64(2.5));
        assert_eq!(report.harmed.oxygen, Units::ZERO);
    }
    let respiratory = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(respiratory.harmed.toxin, Units::from_f64(2.5));
    assert_eq!(respiratory.harmed.oxygen, Units::from_f64(7.5));

    for _ in 5..=7 {
        metabolise(&mut vitals, &mut blood, &data);
    }
    metabolise(&mut vitals, &mut blood, &data);
    assert!(blood.incapacitated());
}

#[test]
fn botanical_toxins_cover_direct_persistent_and_overdose_sensitive_harm() {
    let data = real();

    let (mut amatoxin_vitals, mut amatoxin_blood) = injected(&data, "amatoxin", 2);
    let amatoxin = metabolise(&mut amatoxin_vitals, &mut amatoxin_blood, &data);
    assert_eq!(amatoxin.harmed.toxin, Units::from_f64(2.5));

    let (mut coniine_vitals, mut coniine_blood) = injected(&data, "coniine", 1);
    let coniine = metabolise(&mut coniine_vitals, &mut coniine_blood, &data);
    assert_eq!(coniine.harmed.toxin, Units::from_f64(1.75));
    assert_eq!(coniine.harmed.oxygen, Units::ONE);
    assert!(coniine_blood.status(StatusKind::Choking).intensity > 0.0);
    assert!(
        coniine_blood.blood.volume_of(data.reagent("coniine")) > Units::from_f64(0.9),
        "Coniine should retain its exceptionally slow clearance"
    );

    let (mut ordinary_vitals, mut ordinary_blood) = injected(&data, "histamine", 20);
    let ordinary = metabolise(&mut ordinary_vitals, &mut ordinary_blood, &data);
    assert_eq!(ordinary.harmed.brute, Units::from_f64(0.4));
    assert_eq!(ordinary.harmed.toxin, Units::ZERO);
    assert_eq!(ordinary.harmed.oxygen, Units::ZERO);
    assert_eq!(ordinary_blood.status(StatusKind::Blurred).intensity, 0.4);

    let (mut overdose_vitals, mut overdose_blood) = injected(&data, "histamine", 31);
    let overdose = metabolise(&mut overdose_vitals, &mut overdose_blood, &data);
    assert!(overdose.overdosing.contains(&data.reagent("histamine")));
    assert_eq!(overdose.harmed.brute, Units::from_f64(2.4));
    assert_eq!(overdose.harmed.toxin, Units::whole(2));
    assert_eq!(overdose.harmed.oxygen, Units::whole(2));
    assert_eq!(overdose_blood.status(StatusKind::Blurred).intensity, 1.2);
}

#[test]
fn polonium_creates_a_persistent_radiological_threat_with_existing_counterplay() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "polonium", 2);

    let first = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(first.harmed.toxin, Units::whole(4));
    assert_eq!(blood.status(StatusKind::Irradiated).intensity, 4.0);
    assert!(
        blood.blood.volume_of(data.reagent("polonium")) > Units::from_f64(1.9),
        "the isotope should clear much more slowly than ordinary chemistry"
    );

    let mut chelator = dose(&data, "pentetic_acid", 3);
    blood.receive(&mut chelator, Route::Injected, &mut vitals, &data);
    metabolise(&mut vitals, &mut blood, &data);
    // Polonium refreshes Irradiated on the treatment tick even as Pentetic
    // Acid purges the last isotope. The following tick demonstrates that the
    // chelator then wins once there is no active source to refresh the status.
    metabolise(&mut vitals, &mut blood, &data);
    assert!(
        blood.status(StatusKind::Irradiated).intensity < 4.0,
        "Pentetic Acid should lower the isotope's irradiation intensity"
    );
}

#[test]
fn fentanyl_impairs_immediately_and_knocks_out_after_eighteen_cycles() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "fentanyl", 4);

    for tick in 1..=17 {
        let report = metabolise(&mut vitals, &mut blood, &data);
        assert_eq!(report.harmed.toxin, Units::from_f64(2.5));
        assert!(
            !blood.incapacitated(),
            "Fentanyl collapsed early on tick {tick}"
        );
        assert!(blood.status(StatusKind::Unsteady).intensity > 0.0);
    }

    metabolise(&mut vitals, &mut blood, &data);
    assert!(blood.incapacitated());
}

#[test]
fn bungotoxin_and_lead_acetate_adapt_unsupported_organs_into_readable_harm() {
    let data = real();
    let (mut bungo_vitals, mut bungo_blood) = injected(&data, "bungotoxin", 3);

    for tick in 1..=11 {
        let report = metabolise(&mut bungo_vitals, &mut bungo_blood, &data);
        assert_eq!(report.harmed.toxin, Units::whole(2));
        assert_eq!(report.harmed.oxygen, Units::ONE);
        assert!(
            !bungo_blood.incapacitated(),
            "Bungotoxin fainted early on tick {tick}"
        );
    }
    metabolise(&mut bungo_vitals, &mut bungo_blood, &data);
    assert!(bungo_blood.incapacitated());
    assert!(bungo_blood.status(StatusKind::Choking).intensity > 0.0);

    let (mut lead_vitals, mut lead_blood) = injected(&data, "lead_acetate", 2);
    let lead = metabolise(&mut lead_vitals, &mut lead_blood, &data);
    assert_eq!(lead.harmed.brute, Units::ONE);
    assert_eq!(lead.harmed.toxin, Units::ONE);
    assert!(lead_blood.status(StatusKind::Blurred).intensity > 0.0);
}

#[test]
fn venom_damage_falls_with_every_unit_removed_from_the_bloodstream() {
    let data = real();
    let (mut vitals, mut blood) = injected(&data, "venom", 10);

    let first = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(first.harmed.toxin, Units::ONE);
    assert_eq!(first.harmed.brute, Units::whole(3));

    let second = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(second.harmed.toxin, Units::from_f64(0.99));
    assert_eq!(second.harmed.brute, Units::from_f64(2.97));
}

#[test]
fn itching_powder_works_through_touch_and_rotatium_waits_twenty_cycles() {
    let data = real();
    let mut touch_vitals = Vitals::default();
    let mut touch_blood = Bloodstream::new();
    let mut irritant = dose(&data, "itching_powder", 10);
    let exposure = touch_blood.receive(&mut irritant, Route::Touched, &mut touch_vitals, &data);
    assert_eq!(exposure.absorbed, Units::from_f64(1.5));
    let touch = metabolise(&mut touch_vitals, &mut touch_blood, &data);
    assert_eq!(touch.harmed.brute, Units::from_f64(0.2));
    assert!(touch_blood.status(StatusKind::Unsteady).intensity > 0.0);

    let (mut rotatium_vitals, mut rotatium_blood) = injected(&data, "rotatium", 5);
    for tick in 1..=19 {
        let report = metabolise(&mut rotatium_vitals, &mut rotatium_blood, &data);
        assert_eq!(report.harmed.toxin, Units::from_f64(0.5));
        assert_eq!(
            rotatium_blood.status(StatusKind::Blurred).intensity,
            0.0,
            "Rotatium distorted vision early on tick {tick}"
        );
    }
    metabolise(&mut rotatium_vitals, &mut rotatium_blood, &data);
    assert!(rotatium_blood.status(StatusKind::Blurred).intensity > 0.0);
    assert!(rotatium_blood.status(StatusKind::Unsteady).intensity >= 2.0);
}

#[test]
fn after_effects_fire_once_when_the_whole_dose_clears() {
    let data = fixture();
    let (mut vitals, mut blood) = injected(&data, "crasher", 4);
    let crasher = data.reagent("crasher");

    let first = metabolise(&mut vitals, &mut blood, &data);
    assert!(first.after_effects.is_empty());
    let cleared = metabolise(&mut vitals, &mut blood, &data);
    assert_eq!(cleared.after_effects, vec![crasher]);
    assert_eq!(blood.status(StatusKind::Sluggish).intensity, 1.5);
    let later = metabolise(&mut vitals, &mut blood, &data);
    assert!(
        later.after_effects.is_empty(),
        "a comedown is a one-shot edge"
    );
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

#[test]
fn two_bodies_given_the_same_doses_end_up_identical() {
    let data = fixture();

    let run = || {
        let mut vitals = Vitals::default();
        let mut blood = Bloodstream::new();
        for key in ["poison", "cure", "booze", "left", "right", "tiered"] {
            let mut d = dose(&data, key, 12);
            blood.receive(&mut d, Route::Ingested, &mut vitals, &data);
        }
        for _ in 0..40 {
            metabolise(&mut vitals, &mut blood, &data);
        }
        (vitals, blood)
    };

    let (first_vitals, first_blood) = run();
    let (second_vitals, second_blood) = run();
    assert_eq!(first_vitals, second_vitals);
    assert_eq!(first_blood, second_blood);
}

#[test]
fn an_untouched_body_reports_itself_empty() {
    let blood = Bloodstream::new();
    assert!(blood.is_empty(), "so the game can skip ticking it at all");
}

#[test]
fn contents_merge_blood_and_stomach_largest_first() {
    let data = fixture();
    let mut vitals = Vitals::default();
    let mut blood = Bloodstream::new();

    let mut swallowed = dose(&data, "inert", 20);
    blood.receive(&mut swallowed, Route::Ingested, &mut vitals, &data);
    let mut jabbed = dose(&data, "poison", 5);
    blood.receive(&mut jabbed, Route::Injected, &mut vitals, &data);

    let contents = blood.contents();
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0], (data.reagent("inert"), Units::whole(12)));
    assert_eq!(contents[1], (data.reagent("poison"), Units::whole(5)));
}

// ---------------------------------------------------------------------------
// Data guardrails, against the real files
// ---------------------------------------------------------------------------

#[test]
fn an_overdose_threshold_always_comes_with_something_to_happen_at_it() {
    let data = real();
    for reagent in data.reagents.iter() {
        if reagent.overdose.is_some() {
            assert!(
                !reagent.overdose_effects.is_empty(),
                "'{}' declares an overdose threshold but nothing happens past it, \
                 so the number is a lie",
                reagent.key
            );
        }
        if reagent.critical_overdose.is_some() {
            assert!(
                !reagent.critical_effects.is_empty(),
                "'{}' declares a critical overdose with no critical effects",
                reagent.key
            );
            assert!(
                reagent.overdose.is_some(),
                "'{}' has a critical overdose but no ordinary one to escalate from",
                reagent.key
            );
        }
    }
}

#[test]
fn no_effect_is_written_with_a_meaningless_magnitude() {
    let data = real();
    for reagent in data.reagents.iter() {
        for effect in reagent
            .effects
            .iter()
            .chain(&reagent.overdose_effects)
            .chain(&reagent.critical_effects)
            .chain(&reagent.after_effects)
        {
            assert!(
                effect.magnitude() > 0.0,
                "'{}' has an effect with a zero or negative magnitude: {effect:?}",
                reagent.key
            );
        }
        for effect in &reagent.world_effects {
            assert!(
                effect.magnitude() > 0.0,
                "'{}' has a world effect with a zero or negative magnitude: {effect:?}",
                reagent.key
            );
        }
        for (target, amount) in &reagent.targeted_purges {
            assert!(
                amount.is_positive(),
                "'{}' targets '{target}' with a meaningless purge amount",
                reagent.key
            );
            assert!(data.reagents.id_of(target).is_some());
        }
    }
}

#[test]
fn every_crafted_compound_has_an_explicit_identity() {
    let data = real();
    let mut products = std::collections::BTreeSet::new();
    for reaction in data.reactions.iter() {
        products.extend(reaction.product_ids());
    }

    assert_eq!(
        products.len(),
        145,
        "the audit should cover every shipped product"
    );
    for id in products {
        let reagent = data.reagents.get(id);
        assert!(
            reagent
                .treats
                .as_deref()
                .is_some_and(|description| !description.trim().is_empty()),
            "crafted compound '{}' has no player-facing identity text",
            reagent.key
        );
        assert!(
            !reagent.effects.is_empty()
                || !reagent.after_effects.is_empty()
                || !reagent.world_effects.is_empty()
                || reagent.intentionally_inert,
            "crafted compound '{}' has no body, after-, world, or documented inert identity",
            reagent.key
        );
    }
}

#[test]
fn drug_like_compounds_do_not_share_a_complete_effect_signature() {
    let data = real();
    let keys = [
        "chloral_hydrate",
        "hyperzine",
        "synaptizine",
        "hooch",
        "space_drugs",
        "bath_salts",
        "krokodil",
        "methamphetamine",
        "mindbreaker_toxin",
        "zombie_powder",
        "aranesp",
        "happiness",
        "sadness",
        "pump_up",
        "mushroom_hallucinogen",
        "maintenance_tar",
        "maintenance_sludge",
        "maintenance_powder",
        "kronkaine",
        "blastoff",
        "saturn_x",
        "mute_toxin",
        "heparin",
        "amanitin",
        "curare",
        "lexorin",
        "initropidril",
        "tirizene",
        "tiring_solution",
        "tetrodotoxin",
        "pancuronium",
        "sodium_thiopental",
        "amatoxin",
        "coniine",
        "histamine",
        "polonium",
        "fentanyl",
        "bungotoxin",
        "lead_acetate",
        "venom",
        "itching_powder",
        "rotatium",
    ];
    let mut seen = std::collections::HashMap::<String, &str>::new();
    for key in keys {
        let reagent = data.reagents.get(data.reagent(key));
        let signature = format!(
            "{:?}|{:?}|{:?}|{:?}",
            reagent.effects,
            reagent.overdose_effects,
            reagent.critical_effects,
            reagent.after_effects
        );
        if let Some(other) = seen.insert(signature, key) {
            panic!("'{key}' and '{other}' have identical complete drug effects");
        }
    }
}

#[test]
fn flagship_compounds_expose_their_new_systemic_profiles() {
    let data = real();
    let has_status =
        |key: &str, expected: StatusKind| {
            data.reagents.get(data.reagent(key)).effects.iter().any(
                |effect| matches!(effect, ReagentEffect::Status { kind, .. } if *kind == expected),
            )
        };
    let has_counter = |key: &str, expected: StatusKind| {
        data.reagents.get(data.reagent(key)).effects.iter().any(
            |effect| matches!(effect, ReagentEffect::Counter { kind, .. } if *kind == expected),
        )
    };
    assert!(has_status("inaprovaline", StatusKind::Stabilized));
    assert!(has_status("chloral_hydrate", StatusKind::Sedated));
    assert!(has_status("space_drugs", StatusKind::Euphoric));
    assert!(has_status("space_drugs", StatusKind::Hallucinating));
    assert!(has_status("bath_salts", StatusKind::Paranoid));
    assert!(has_status("krokodil", StatusKind::Analgesic));
    assert!(data
        .reagents
        .get(data.reagent("krokodil"))
        .effects
        .iter()
        .any(|effect| matches!(effect, ReagentEffect::Harm(DamageKind::Burn, _))));
    assert!(has_status("phlogiston", StatusKind::Burning));
    assert!(has_status("cryostylane", StatusKind::Chilled));
    assert!(has_status("potassium_iodide", StatusKind::RadiationShield));
    assert!(has_status("cyanide", StatusKind::Choking));
    assert!(has_status("unstable_mutagen", StatusKind::Mutating));
    assert!(has_status("synaptizine", StatusKind::Focused));
    assert!(has_status("seiver", StatusKind::RadiationShield));
    assert!(has_status("neurine", StatusKind::Focused));
    assert!(has_counter("seiver", StatusKind::Irradiated));
    assert!(has_counter("neurine", StatusKind::Unsteady));
    assert!(has_counter("neurine", StatusKind::Blurred));
    assert!(has_counter("diphenhydramine", StatusKind::Hastened));
    assert!(has_counter("oculine", StatusKind::Blurred));
    assert!(has_status("cryptobiolin", StatusKind::Unsteady));
    assert!(has_status("cryptobiolin", StatusKind::Blurred));
    assert!(has_counter("antihol", StatusKind::Drunk));
    assert!(has_counter("antihol", StatusKind::Unsteady));
    assert!(has_counter("modafinil", StatusKind::Sedated));
    assert!(has_counter("modafinil", StatusKind::Unsteady));
    assert!(has_counter("modafinil", StatusKind::Chilled));
    assert!(has_status("modafinil", StatusKind::Focused));
    assert!(has_counter("naloxone", StatusKind::Sedated));
    assert!(has_counter("naloxone", StatusKind::Choking));
    assert!(has_counter("hercuri", StatusKind::Burning));
    assert!(has_status("hercuri", StatusKind::Chilled));
    assert!(has_status("herignis", StatusKind::Burning));
    assert!(has_status("nitrous_oxide", StatusKind::Sedated));
    assert!(has_status("nitrous_oxide", StatusKind::Hallucinating));
    assert!(has_status("miners_salve", StatusKind::Analgesic));
    let hercuri = data.reagents.get(data.reagent("hercuri"));
    assert!(hercuri.effects.iter().any(
        |effect| matches!(effect, ReagentEffect::Heal(DamageKind::Burn, amount) if *amount == Units::whole(3))
    ));
    assert!(hercuri.world_effects.iter().any(
        |effect| matches!(effect, WorldEffect::Chill { kelvin_per_unit } if (*kelvin_per_unit - 2.0).abs() < 0.001)
    ));
    let syriniver = data.reagents.get(data.reagent("syriniver"));
    assert_eq!(syriniver.overdose, Some(Units::whole(6)));
    assert!(syriniver.effects.iter().any(
        |effect| matches!(effect, ReagentEffect::Heal(DamageKind::Toxin, amount) if *amount == Units::whole(3))
    ));
    assert!(syriniver.effects.iter().any(
        |effect| matches!(effect, ReagentEffect::Purge(amount) if *amount == Units::whole(2))
    ));
    let naloxone = data.reagents.get(data.reagent("naloxone"));
    assert!(naloxone.effects.iter().any(
        |effect| matches!(effect, ReagentEffect::Purge(amount) if *amount == Units::whole(3))
    ));
    assert!(naloxone.effects.iter().any(
        |effect| matches!(effect, ReagentEffect::Heal(DamageKind::Oxygen, amount) if *amount == Units::ONE)
    ));

    let modafinil = data.reagents.get(data.reagent("modafinil"));
    assert!(modafinil.overdose_effects.iter().any(|effect| matches!(
        effect,
        ReagentEffect::Status {
            kind: StatusKind::Choking,
            ..
        }
    )));
    assert!(modafinil
        .critical_effects
        .iter()
        .any(|effect| matches!(effect, ReagentEffect::Harm(DamageKind::Oxygen, _))));

    let rezadone = data.reagents.get(data.reagent("rezadone"));
    for damage in [DamageKind::Brute, DamageKind::Burn] {
        assert!(rezadone
            .effects
            .iter()
            .any(|effect| matches!(effect, ReagentEffect::Heal(kind, _) if *kind == damage)));
    }
    assert!(rezadone
        .overdose_effects
        .iter()
        .any(|effect| matches!(effect, ReagentEffect::Harm(DamageKind::Toxin, _))));

    assert!(data
        .reagents
        .get(data.reagent("dylovene"))
        .effects
        .iter()
        .any(|effect| matches!(effect, ReagentEffect::Purge(_))));
    for key in ["calomel", "ammoniated_mercury"] {
        assert!(data
            .reagents
            .get(data.reagent(key))
            .effects
            .iter()
            .any(|effect| matches!(effect, ReagentEffect::Purge(_))));
    }
    assert!(data
        .reagents
        .get(data.reagent("space_cleaner"))
        .world_effects
        .iter()
        .any(|effect| matches!(effect, WorldEffect::Clean { .. })));
    assert!(data
        .reagents
        .get(data.reagent("thermite"))
        .world_effects
        .iter()
        .any(|effect| matches!(effect, WorldEffect::Corrode { .. })));
}

/// Pins the RON spelling of every field added for bodies.
///
/// The trap this exists for: RON will not coerce an integer into `Kelvin`'s
/// `f32`. `Some((323.15))` parses and `Some((323))` does not, and the failure
/// surfaces as an asset-load panic at startup rather than anywhere useful.
#[test]
fn every_new_field_round_trips_through_ron() {
    let reagents = r#"[
        (id: "pinned", name: "Pinned", color: (0.1, 0.2, 0.3), dispensable: true,
         overdose: Some(15), critical_overdose: Some(30),
         metabolism: Some(0.2),
         boils_at: Some((323.15)),
         effects: [
             Heal(Brute, 2), Harm(Toxin, 1), Contact(Burn, 0.5),
             Status(kind: Drunk, seconds: 4.0, intensity: 0.65),
             Counter(kind: Irradiated, seconds: 2.0, intensity: 1.0),
             Purge(0.5),
         ],
         targeted_purges: [("other", 1)],
         recovers_to: Some("other"),
         overdose_effects: [Harm(Toxin, 1)],
         critical_effects: [Harm(Toxin, 4)],
         after_effects: [Status(kind: Sluggish, seconds: 8.0, intensity: 1.0)],
         world_effects: [
             Clean(strength: 1.0), Corrode(strength: 2.0),
             Ignite(intensity: 1.0, seconds: 4.0),
             ReleaseSmoke(radius: 3.0, seconds: 5.0),
             Slippery(seconds: 6.0), Flammable(intensity: 1.0, seconds: 8.0),
             Chill(kelvin_per_unit: 2.0), Flash(radius: 4.0, seconds: 3.0),
         ]),
        (id: "other", name: "Other", color: (0.0, 0.0, 0.0), dispensable: true,
         intentionally_inert: true),
    ]"#;
    let reactions = r#"[
        (id: "hot", reactants: [("pinned", 1)], products: [("other", 1)],
         min_temp: Some((374.0)), max_temp: Some((600.0)),
         overheat_temp: Some((420.0)), overheat: Detonate(power: 3.0),
         effects: [Heat(1.2)], hints: ["Warm."]),
        (id: "spoils", reactants: [("other", 1)], products: [("pinned", 1)],
         overheat_temp: Some((500.0)), overheat: ReducedYield(over: 60.0),
         hints: ["Cool."]),
        (id: "wasted", reactants: [("other", 2)], products: [("pinned", 2)],
         overheat_temp: Some((500.0)), overheat: Ruin, hints: ["Careful."]),
    ]"#;

    let data = ChemData::from_ron(reagents, reactions).expect("every new field should parse");
    let pinned = data.reagents.get(data.reagent("pinned"));

    assert_eq!(pinned.metabolism, Some(Units::from_f64(0.2)));
    assert_eq!(pinned.rate(), Units::from_f64(0.2));
    assert_eq!(pinned.boils_at.map(|k| k.0), Some(323.15));
    assert_eq!(pinned.effects.len(), 6);
    assert_eq!(
        pinned.targeted_purges,
        vec![("other".to_string(), Units::ONE)]
    );
    assert_eq!(pinned.recovers_to.as_deref(), Some("other"));
    assert_eq!(
        pinned.effects[3],
        ReagentEffect::Status {
            kind: StatusKind::Drunk,
            seconds: 4.0,
            intensity: 0.65,
        }
    );
    assert_eq!(pinned.after_effects.len(), 1);
    assert_eq!(pinned.world_effects.len(), 8);
    assert!(data.reagents.get(data.reagent("other")).intentionally_inert);
    assert!(pinned.is_harmful());

    let hot = data.reactions.find("hot").expect("reaction should load");
    assert_eq!(hot.overheat_temp.map(|k| k.0), Some(420.0));
    assert_eq!(hot.overheat, chem_sim::Overheat::Detonate { power: 3.0 });
    assert_eq!(
        data.reactions.find("spoils").unwrap().overheat,
        chem_sim::Overheat::ReducedYield { over: 60.0 }
    );
    assert_eq!(
        data.reactions.find("wasted").unwrap().overheat,
        chem_sim::Overheat::Ruin
    );
}
