use super::*;
use crate::arc::{ArcOutcome, Mode};
use crate::containers::ContainerKind;

fn script() -> CultScript {
    ron::from_str(include_str!("../../../assets/data/station.cult.ron")).unwrap()
}

fn wave(campaign: &mut Campaign, script: &CultScript, count: usize) {
    let slots = if count == 0 {
        1
    } else {
        script.guard_ward_index(count - 1) + 1
    };
    campaign.cult_incidents.resize(slots, false);
    let arc = campaign.active[0].clone();
    campaign.cult_aftermath.advance(&arc, script);
}

#[test]
fn waves_leave_four_new_sites_without_recreating_cleaned_ones() {
    let script = script();
    let mut campaign = Campaign::new(AntagId::Cult, Mode::Chemist, 4);
    wave(&mut campaign, &script, 0);
    assert_eq!(campaign.cult_aftermath.pending(), 0);
    for level in 1..=3 {
        wave(&mut campaign, &script, level);
        assert_eq!(campaign.cult_aftermath.sites.len(), level * 4);
    }
    campaign.cult_aftermath.sites[0].cleared = true;
    campaign.plot = 0;
    wave(&mut campaign, &script, 3);
    assert_eq!(campaign.cult_aftermath.pending(), 11);
    assert_eq!(campaign.cult_aftermath.introduced_wave, 3);
}

#[test]
fn stopping_preserves_spent_sites_and_only_unresolved_original_manifestations() {
    let script = script();
    let mut campaign = Campaign::new(AntagId::Cult, Mode::Chemist, 4);
    wave(&mut campaign, &script, 2);
    campaign.cult_aftermath.sites[0].cleared = true;
    campaign.cult_incidents[0] = true;
    campaign.cult_incidents[script.stage_ward_base(0)] = true;
    campaign.outcome = Some(ArcOutcome::StoppedDirectly);
    wave(&mut campaign, &script, 2);
    assert_eq!(campaign.cult_aftermath.pending(), 10); // 7 deposits + 3 original anchors
    assert_eq!(campaign.cult_aftermath.spent_pending(), 10);
    assert!(!campaign
        .cult_aftermath
        .sites
        .iter()
        .any(|r| r.site.spot == script.altar.spot));
    assert!(!campaign
        .cult_aftermath
        .sites
        .iter()
        .any(|r| r.site.spot == script.stages[0].incidents[0].spot));
    for site in &mut campaign.cult_aftermath.sites {
        site.cleared = true;
    }
    wave(&mut campaign, &script, 2);
    assert_eq!(campaign.cult_aftermath.pending(), 0);
    assert_eq!(campaign.outcome, Some(ArcOutcome::StoppedDirectly));
}

#[test]
fn aftermath_survives_other_antagonists_and_new_cult_only_reaches_its_current_wave() {
    let script = script();
    let mut campaign = Campaign::new(AntagId::Cult, Mode::Chemist, 4);
    wave(&mut campaign, &script, 3);
    let mut later = Campaign::new(AntagId::Spy, Mode::Chemist, 4);
    later.id = CampaignId(2);
    later.cult_aftermath = campaign.cult_aftermath.clone();
    wave(&mut later, &script, 0);
    assert_eq!(later.cult_aftermath.spent_pending(), 12);
    later.antag = AntagId::Cult;
    later.id = CampaignId(3);
    wave(&mut later, &script, 1);
    assert_eq!(later.cult_aftermath.pending(), 12);
    assert_eq!(later.cult_aftermath.spent_pending(), 8);
    assert_eq!(
        later
            .cult_aftermath
            .sites
            .iter()
            .filter(|r| r.owner == CampaignId(3))
            .count(),
        4
    );
    let mut innocent = Campaign::new(AntagId::Blob, Mode::Chemist, 4);
    wave(&mut innocent, &script, 3);
    assert_eq!(innocent.cult_aftermath.pending(), 0);
}

#[test]
fn aftermath_round_trips_through_save_and_late_join_wire_with_legacy_defaults() {
    let script = script();
    let mut campaign = Campaign::new(AntagId::Cult, Mode::Chemist, 4);
    wave(&mut campaign, &script, 3);
    campaign.cult_aftermath.sites[2].cleared = true;
    campaign.outcome = Some(ArcOutcome::StoppedDirectly);
    wave(&mut campaign, &script, 3);
    let text = ron::ser::to_string(&campaign).unwrap();
    assert_eq!(ron::from_str::<Campaign>(&text).unwrap(), campaign);
    let bytes = postcard::to_allocvec(&campaign).unwrap();
    assert_eq!(postcard::from_bytes::<Campaign>(&bytes).unwrap(), campaign);
    let old: Campaign = ron::from_str("(antag: Cult, mode: Chemist)").unwrap();
    assert_eq!(old.cult_aftermath, CultAftermath::default());
    let old_roster = format!(
        "(active: [{}], max_active: 1)",
        ron::ser::to_string(&campaign.active[0]).unwrap()
    );
    assert_eq!(
        ron::from_str::<Campaign>(&old_roster)
            .unwrap()
            .cult_aftermath
            .pending(),
        0
    );
}

fn cleanup_app() -> (App, Entity, Entity, Entity) {
    let mut app = App::new();
    let data = chem_sim::ChemData::from_ron(
        include_str!("../../../assets/data/chem.reagents.ron"),
        include_str!("../../../assets/data/chem.reactions.ron"),
    )
    .unwrap();
    let cleaner = data.reagents.id_of("space_cleaner").unwrap();
    let water = data.reagents.id_of("water").unwrap();
    let mut campaign = Campaign::new(AntagId::Cult, Mode::Chemist, 4);
    wave(&mut campaign, &script(), 1);
    let record = campaign.cult_aftermath.sites[0].clone();
    let residue = CultResidue {
        spot: record.site.spot,
        owner: record.owner,
        spent: false,
    };
    app.insert_resource(campaign)
        .insert_resource(ChemDb(data))
        .init_resource::<RadioLog>()
        .add_message::<FromClient<InteractRequested>>()
        .add_systems(Update, clean_residue);
    let target = app
        .world_mut()
        .spawn((residue, Transform::from_xyz(0.0, 1.0, 0.0)))
        .id();
    let player = app
        .world_mut()
        .spawn((
            Chemist {
                client: ClientId::Server,
            },
            Transform::from_xyz(0.0, 1.0, 1.0),
        ))
        .id();
    let mut container = Container::new(ContainerKind::Bottle);
    let _ = container.solution.add(cleaner, Units::whole(10));
    let _ = container.solution.add(water, Units::whole(4));
    let bottle = app.world_mut().spawn((container, HeldBy(player))).id();
    (app, target, player, bottle)
}

fn request(app: &mut App, target: Entity) {
    app.world_mut().write_message(FromClient {
        client_id: ClientId::Server,
        message: InteractRequested { target },
    });
}

#[test]
fn cleanup_uses_exact_cleaner_preserves_vessel_and_never_grants_wards() {
    let (mut app, target, _, bottle) = cleanup_app();
    let wards = app.world().resource::<Campaign>().cult_incidents.clone();
    request(&mut app, target);
    request(&mut app, target);
    app.update();
    let db = app.world().resource::<ChemDb>();
    let contents = &app.world().get::<Container>(bottle).unwrap().solution;
    assert_eq!(
        contents.volume_of(db.reagents.id_of("space_cleaner").unwrap()),
        Units::whole(7)
    );
    assert_eq!(
        contents.volume_of(db.reagents.id_of("water").unwrap()),
        Units::whole(4)
    );
    assert_eq!(app.world().resource::<Campaign>().cult_incidents, wards);
    assert!(app.world().resource::<Campaign>().outcome.is_none());
    assert_eq!(
        app.world().resource::<Campaign>().cult_aftermath.pending(),
        3
    );
    assert!(app.world().get_entity(target).is_err());
}

#[test]
fn remote_through_wall_and_another_players_container_cannot_clean() {
    for failure in 0..3 {
        let (mut app, target, player, bottle) = cleanup_app();
        match failure {
            0 => {
                app.world_mut()
                    .get_mut::<Transform>(player)
                    .unwrap()
                    .translation
                    .z = 20.0
            }
            1 => {
                app.world_mut().spawn((
                    Transform::from_xyz(0.0, 1.0, 0.5),
                    Solid {
                        half_extents: Vec3::new(0.4, 1.0, 0.08),
                    },
                ));
            }
            _ => {
                let other = app.world_mut().spawn_empty().id();
                app.world_mut().entity_mut(bottle).insert(HeldBy(other));
            }
        }
        request(&mut app, target);
        app.update();
        assert_eq!(
            app.world().resource::<Campaign>().cult_aftermath.pending(),
            4
        );
        assert_eq!(
            app.world()
                .get::<Container>(bottle)
                .unwrap()
                .solution
                .total_volume(),
            Units::whole(14)
        );
        assert!(app.world().get_entity(target).is_ok());
    }
}

#[test]
fn wrong_or_insufficient_cleaner_is_not_consumed() {
    let (mut app, target, _, bottle) = cleanup_app();
    let cleaner = app
        .world()
        .resource::<ChemDb>()
        .reagents
        .id_of("space_cleaner")
        .unwrap();
    app.world_mut()
        .get_mut::<Container>(bottle)
        .unwrap()
        .solution
        .remove(cleaner, Units::whole(9));
    request(&mut app, target);
    app.update();
    assert_eq!(
        app.world().resource::<Campaign>().cult_aftermath.pending(),
        4
    );
    assert_eq!(
        app.world()
            .get::<Container>(bottle)
            .unwrap()
            .solution
            .total_volume(),
        Units::whole(5)
    );
}

#[test]
fn spent_debris_needs_empty_hands_and_cleaning_keeps_victory() {
    let (mut app, target, _, bottle) = cleanup_app();
    {
        let mut campaign = app.world_mut().resource_mut::<Campaign>();
        campaign.outcome = Some(ArcOutcome::StoppedDirectly);
        campaign.cult_aftermath.sites[0].spent = true;
        campaign.cult_aftermath.sites[0].site.cleanup = Cleanup::Dismantle;
    }
    request(&mut app, target);
    app.update();
    assert_eq!(
        app.world().resource::<Campaign>().cult_aftermath.pending(),
        4
    );
    app.world_mut().entity_mut(bottle).remove::<HeldBy>();
    request(&mut app, target);
    app.update();
    assert_eq!(
        app.world().resource::<Campaign>().cult_aftermath.pending(),
        3
    );
    assert_eq!(
        app.world().resource::<Campaign>().outcome,
        Some(ArcOutcome::StoppedDirectly)
    );
}

#[test]
fn restore_waits_for_spots_and_recreates_only_uncleared_sites_once() {
    let mut app = App::new();
    let mut campaign = Campaign::new(AntagId::Cult, Mode::Chemist, 4);
    wave(&mut campaign, &script(), 1);
    campaign.cult_aftermath.sites[0].cleared = true;
    let records = campaign.cult_aftermath.sites.clone();
    app.insert_resource(campaign)
        .init_resource::<CrisisSpots>()
        .add_systems(Update, reconcile_entities);
    app.update();
    assert_eq!(
        app.world_mut()
            .query::<&CultResidue>()
            .iter(app.world())
            .count(),
        0
    );
    for record in records {
        app.world_mut()
            .resource_mut::<CrisisSpots>()
            .insert(record.site.spot, Transform::from_xyz(0.0, 1.0, 0.0));
    }
    app.update();
    app.update();
    assert_eq!(
        app.world_mut()
            .query::<&CultResidue>()
            .iter(app.world())
            .count(),
        3
    );
    let entities: Vec<_> = app
        .world_mut()
        .query_filtered::<Entity, With<CultResidue>>()
        .iter(app.world())
        .collect();
    for entity in entities {
        app.world_mut().despawn(entity);
    }
    app.update();
    assert_eq!(
        app.world_mut()
            .query::<&CultResidue>()
            .iter(app.world())
            .count(),
        3
    );
}

#[test]
fn spent_materials_handle_late_meshes_without_changing_shared_station_materials() {
    let mut app = App::new();
    app.init_resource::<Assets<StandardMaterial>>()
        .init_resource::<SpentMaterials>()
        .add_systems(Update, ashen_spent_meshes);
    let material = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            emissive: LinearRgba::rgb(2.0, 1.0, 1.0),
            ..default()
        });
    let root = app
        .world_mut()
        .spawn(CultResidue {
            spot: "test".into(),
            owner: CampaignId(1),
            spent: true,
        })
        .id();
    app.update(); // scene has not loaded yet
    let mesh = app
        .world_mut()
        .spawn((ChildOf(root), MeshMaterial3d(material.clone())))
        .id();
    let ordinary = app.world_mut().spawn(MeshMaterial3d(material.clone())).id();
    app.update();
    let ash = &app
        .world()
        .get::<MeshMaterial3d<StandardMaterial>>(mesh)
        .unwrap()
        .0;
    assert_ne!(ash, &material);
    assert_eq!(
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .get(ash)
            .unwrap()
            .emissive,
        LinearRgba::BLACK
    );
    assert_eq!(
        &app.world()
            .get::<MeshMaterial3d<StandardMaterial>>(ordinary)
            .unwrap()
            .0,
        &material
    );
    assert_eq!(
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .get(&material)
            .unwrap()
            .emissive
            .red,
        2.0
    );
}

#[test]
fn authored_sites_cover_every_new_asset_and_three_waves() {
    let script = script();
    assert_eq!(script.defacements.len(), 12);
    let mut spots = std::collections::HashSet::new();
    let mut visuals = std::collections::HashSet::new();
    for site in &script.defacements {
        assert!(spots.insert(&site.spot));
        assert!((1..=script.stages.len()).contains(&site.wave));
        assert!((1..=5).contains(&site.amount));
        visuals.insert(site.visual as usize);
    }
    assert_eq!(visuals.len(), 8);
    for wave in 1..=3 {
        assert_eq!(
            script.defacements.iter().filter(|s| s.wave == wave).count(),
            4
        );
    }
}
