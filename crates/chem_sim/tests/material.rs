//! Tutorial scenarios and guard falsification for property-driven chemistry.
use chem_sim::*;

fn data() -> ChemData {
    ChemData::from_ron(
        include_str!("../../../assets/data/chem.reagents.ron"),
        include_str!("../../../assets/data/chem.reactions.ron"),
    )
    .unwrap()
}
fn mix(db: &ChemData, contents: &[(&str, f64)]) -> Solution {
    let mut s = Solution::unbounded();
    for (key, amount) in contents {
        let id = db.reagent(key);
        let _ = s.add_profiled(id, Units::from_f64(*amount), 1.0, db.reagents.get(id).ph);
    }
    s
}
fn energy(report: &ResolveReport) -> f32 {
    report.materials.iter().map(|e| e.energy).sum()
}

#[test]
fn sb01_synthetic_material_needs_no_pair_recipe_or_gameplay_branch() {
    let db = ChemData::from_ron(r#"[
        (id:"a", name:"A", color:(1.0,0.0,0.0), material:(water_reactive:Some((product:"c", activation_temp:250.0, rate:None, energy:2.0)))),
        (id:"b", name:"B", color:(0.0,0.0,1.0), material:(water:true)),
        (id:"c", name:"C", color:(0.5,0.5,0.5))]"#, "[]").unwrap();
    let mut s = mix(&db, &[("a", 4.0), ("b", 4.0)]);
    let r = resolve_step(&mut s, &db.reactions, 0.0);
    assert!(r.events.is_empty());
    assert_eq!(r.materials.len(), 1);
    assert_eq!(s.volume_of(db.reagent("c")), Units::whole(8));
    assert_eq!(energy(&r), 8.0);
    assert!(!resolve_step(&mut s, &db.reactions, 1.0).reacted());
}

#[test]
fn sb02_trace_mistakes_scale_and_dilution_moderates_heat() {
    let db = data();
    let run = |amount, dilution| {
        let mut s = mix(
            &db,
            &[
                ("potassium", amount),
                ("water", amount),
                ("nitrogen", dilution),
            ],
        );
        let r = resolve_step(&mut s, &db.reactions, 0.0);
        (s, r)
    };
    let (trace, small) = run(0.01, 0.0);
    let (strong, large) = run(10.0, 0.0);
    let (dilute, diluted) = run(10.0, 80.0);
    assert!(energy(&small) < 0.1);
    assert_eq!(energy(&large), energy(&diluted));
    assert!(dilute.temperature < strong.temperature);
    assert!(trace.temperature < strong.temperature);
    assert_eq!(
        explosion_damage(energy(&small), 0.0).total(),
        Units::from_f64(0.2)
    );
}

#[test]
fn sb03_neutralization_consumes_material_not_just_average_ph() {
    let db = data();
    let mut s = mix(&db, &[("sulphuric_acid", 2.0), ("lye", 2.0)]);
    assert!(!resolve_step(&mut s, &db.reactions, 0.0).reacted());
    let r = resolve_step(&mut s, &db.reactions, 1.0);
    assert_eq!(r.materials[0].family, MaterialFamily::Neutralization);
    assert_eq!(
        s.volume_of(db.reagent("neutralized_salts")),
        Units::whole(2)
    );
    assert_eq!(s.total_volume(), Units::whole(4));
    resolve_step(&mut s, &db.reactions, 1.0);
    assert_eq!(s.ph(), 7.0);
    assert!(!resolve_step(&mut s, &db.reactions, 1.0).reacted());
}

#[test]
fn sb04_storage_is_stable_and_oxidizer_accelerates_ignited_fuel() {
    let db = data();
    let original = mix(&db, &[("oil", 10.0), ("oxygen", 10.0)]);
    let mut stable = original.clone();
    assert!(!resolve_step(&mut stable, &db.reactions, 20.0).reacted());
    assert_eq!(stable, original);
    let mut explicit = original;
    explicit.temperature = Kelvin(550.0);
    let mut ambient = mix(&db, &[("oil", 10.0)]);
    ambient.temperature = Kelvin(550.0);
    let fast = resolve_step(&mut explicit, &db.reactions, 1.0);
    let slow = resolve_step(&mut ambient, &db.reactions, 1.0);
    assert!(
        energy(&fast) > energy(&slow),
        "fast {fast:?}, slow {slow:?}"
    );
    assert_eq!(slow.materials[0].environmental_units, Units::from_f64(0.5));
    assert!(ambient.volume_of(db.reagent("oxygen")).is_zero());
    assert_eq!(explicit.total_volume(), Units::whole(20));
    assert_eq!(ambient.total_volume(), Units::whole(10));
    for _ in 0..40 {
        resolve_step(&mut ambient, &db.reactions, 1.0);
    }
    assert!(!resolve_step(&mut ambient, &db.reactions, 1.0).reacted());
}

#[test]
fn sb05_swallowed_reactions_wait_and_body_water_is_one_shared_budget() {
    let db = data();
    let mut blood = Bloodstream::default();
    let mut vitals = Vitals::default();
    blood.receive(
        &mut mix(&db, &[("potassium", 10.0)]),
        Route::Ingested,
        &mut vitals,
        &db,
    );
    assert_eq!(blood.drain_reactions().count(), 0);
    blood.blood = mix(&db, &[("potassium", 3.0)]);
    metabolise(&mut vitals, &mut blood, &db);
    let reports: Vec<_> = blood.drain_reactions().collect();
    let used: Units = reports
        .iter()
        .flat_map(|(_, r)| &r.materials)
        .map(|e| e.environmental_units)
        .sum();
    assert_eq!(used, Units::ONE);
    assert!(reports
        .iter()
        .any(|(c, _)| *c == chem_sim::body::BodyCompartment::Stomach));
    let mut explicit = Bloodstream::default();
    explicit.receive(
        &mut mix(&db, &[("potassium", 20.0)]),
        Route::Ingested,
        &mut vitals,
        &db,
    );
    explicit.receive(
        &mut mix(&db, &[("water", 20.0)]),
        Route::Ingested,
        &mut vitals,
        &db,
    );
    metabolise(&mut vitals, &mut explicit, &db);
    let burst: f32 = explicit.drain_reactions().map(|(_, r)| energy(&r)).sum();
    assert!(burst >= 20.0);
}

#[test]
fn sb06_background_supplies_cannot_synthesize_or_be_extracted() {
    let db = data();
    let mut s = mix(&db, &[("sugar", 5.0)]);
    let initial = s.clone();
    let mut env = ReactionEnvironment::body(2.0);
    assert!(!resolve_in_environment(&mut s, &db.reactions, 2.0, None, &mut env).reacted());
    assert_eq!(s, initial);
    assert_eq!(env.water, Units::ONE);
}

#[test]
fn sb07_material_progress_is_stable_across_step_sizes() {
    let db = data();
    let mut one = mix(&db, &[("oil", 10.0), ("oxygen", 20.0)]);
    one.temperature = Kelvin(550.0);
    let mut ten = one.clone();
    let a = energy(&resolve_step(&mut one, &db.reactions, 1.0));
    let b: f32 = (0..10)
        .map(|_| energy(&resolve_step(&mut ten, &db.reactions, 0.1)))
        .sum();
    assert!((a - b).abs() < 0.001);
    assert_eq!(
        one.iter().collect::<Vec<_>>(),
        ten.iter().collect::<Vec<_>>()
    );
    assert!((one.temperature.0 - ten.temperature.0).abs() < 0.01);
}

#[test]
fn sb08_body_effect_outbox_is_not_replayed_by_binary_or_text_restore() {
    let db = data();
    let mut blood = Bloodstream::default();
    let mut vitals = Vitals::default();
    blood.receive(
        &mut mix(&db, &[("potassium", 2.0), ("water", 2.0)]),
        Route::Injected,
        &mut vitals,
        &db,
    );
    let encoded = postcard::to_allocvec(&blood).unwrap();
    let mut restored: Bloodstream = postcard::from_bytes(&encoded).unwrap();
    assert_eq!(restored.drain_reactions().count(), 0);
    assert_eq!(restored.blood, blood.blood);
    let mut text: Bloodstream = ron::from_str(&ron::to_string(&blood).unwrap()).unwrap();
    assert_eq!(text.drain_reactions().count(), 0);
    assert_eq!(blood.drain_reactions().count(), 1);
}

#[test]
fn sb09_normal_medicine_and_buffered_methods_remain_reliable() {
    let db = data();
    for (contents, product, units) in [
        (
            vec![("silicon", 5.0), ("nitrogen", 5.0), ("potassium", 5.0)],
            "dylovene",
            15,
        ),
        (
            vec![("oxygen", 5.0), ("carbon", 5.0), ("sugar", 5.0)],
            "inaprovaline",
            15,
        ),
        (vec![("sodium", 5.0), ("water", 5.0)], "lye", 10),
    ] {
        let mut s = mix(&db, &contents);
        let r = resolve_step(&mut s, &db.reactions, 0.0);
        assert!(r.materials.is_empty(), "{product}");
        assert_eq!(s.volume_of(db.reagent(product)), Units::whole(units));
    }
}

#[test]
fn sb10_missing_or_self_producing_material_products_are_rejected() {
    for product in ["missing", "a"] {
        let ron = format!(
            r#"[(id:"a",name:"A",color:(1.0,0.0,0.0),material:(water_reactive:Some((product:"{product}",activation_temp:250.0,rate:None,energy:1.0))))]"#
        );
        assert!(ChemData::from_ron(&ron, "[]").is_err());
    }
}

#[test]
fn sb17_material_products_chain_without_replenishing_the_rate_budget() {
    let db = ChemData::from_ron(r#"[
        (id:"a",name:"A",color:(1.0,0.0,0.0),material:(water_reactive:Some((product:"c",activation_temp:250.0,rate:Some(1.0),energy:1.0)))),
        (id:"b",name:"B",color:(0.0,0.0,1.0),material:(water:true)),
        (id:"c",name:"C",color:(0.5,0.5,0.5),material:(water_reactive:Some((product:"d",activation_temp:300.0,rate:None,energy:1.0)))),
        (id:"d",name:"D",color:(0.1,0.1,0.1))]"#,"[]").unwrap();
    let mut s = mix(&db, &[("a", 10.0), ("b", 10.0)]);
    s.temperature = Kelvin(299.0);
    let r = resolve_step(&mut s, &db.reactions, 1.0);
    assert_eq!(s.volume_of(db.reagent("a")), Units::whole(9));
    assert_eq!(s.volume_of(db.reagent("d")), Units::whole(4));
    assert_eq!(s.total_volume(), Units::whole(20));
    assert_eq!(r.materials.len(), 2);
    assert!(!r.hit_iteration_cap);
    assert!(r
        .materials
        .iter()
        .all(|event| event.consumed.iter().map(|v| v.1).sum::<Units>()
            == event.products.iter().map(|v| v.1).sum::<Units>()));
}

#[test]
fn sb19_shipped_water_reaction_heat_ignites_fuel_with_one_budget() {
    let db = data();
    let mut s = mix(
        &db,
        &[
            ("potassium", 10.0),
            ("water", 10.0),
            ("oil", 1.0),
            ("oxygen", 1.0),
        ],
    );
    s.temperature = Kelvin(470.0);
    let r = resolve_step(&mut s, &db.reactions, 0.1);
    assert_eq!(
        r.materials.iter().map(|e| e.family).collect::<Vec<_>>(),
        vec![MaterialFamily::WaterReactive, MaterialFamily::Combustion]
    );
    assert_eq!(s.volume_of(db.reagent("oil")), Units::from_f64(0.6));
    assert_eq!(s.total_volume(), Units::whole(22));
    assert!((energy(&r) - 21.2).abs() < 0.001);
}
