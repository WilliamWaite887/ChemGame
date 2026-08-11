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
use chem_sim::{ChemData, Damage, DamageKind, ReagentEffect, Route, Solution, StatusKind, Units};

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
    assert!(blood.blood.is_empty(), "a fast reagent should clear in five ticks");
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
    assert_eq!(blood.blood.volume_of(data.reagent("inert")), Units::whole(10));
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

// ---------------------------------------------------------------------------
// Damage and collapse
// ---------------------------------------------------------------------------

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
    assert!(vitals.collapsed, "85 is above the recovery line, still down");

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
        (report.harmed.toxin, report.healed.brute, report.overdosing.len())
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
fn radiation_is_the_one_status_that_hurts_you() {
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
        {
            assert!(
                effect.magnitude() > 0.0,
                "'{}' has an effect with a zero or negative magnitude: {effect:?}",
                reagent.key
            );
        }
    }
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
         ],
         overdose_effects: [Harm(Toxin, 1)],
         critical_effects: [Harm(Toxin, 4)]),
        (id: "other", name: "Other", color: (0.0, 0.0, 0.0), dispensable: true),
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
    assert_eq!(pinned.effects.len(), 5);
    assert_eq!(
        pinned.effects[3],
        ReagentEffect::Status {
            kind: StatusKind::Drunk,
            seconds: 4.0,
            intensity: 0.65,
        }
    );
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
