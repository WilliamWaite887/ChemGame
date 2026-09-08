//! Station contamination is a career fact, not a transient combat entity.
use super::{CultScript, CultVisual, CultVisualId, Script};
use crate::arc::{AntagId, Campaign, CampaignArc, CampaignId};
use crate::body::Body;
use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy};
use crate::interaction::{
    authority_segment_blocked, authority_target_in_reach, InteractRequested, Interactable, REACH,
};
use crate::lab::{CrisisSpots, MapReady, Solid};
use crate::player::Chemist;
use crate::radio::{RadioChannel, RadioEntry, RadioLog};
use crate::{net::is_authority, AppState};
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::Units;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cleanup {
    #[default]
    Cleaner,
    Dismantle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefacementDef {
    pub name: String,
    pub visual: CultVisualId,
    pub spot: String,
    /// One-based existing ritual wave. Zero is reserved for original remains.
    pub wave: usize,
    pub cleanup: Cleanup,
    pub amount: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidueRecord {
    pub site: DefacementDef,
    pub owner: CampaignId,
    pub cleared: bool,
    pub spent: bool,
}

/// Saved and carried by CampaignSync, including when another antagonist starts.
/// One record per physical location bounds the history; a later Cult campaign
/// can replace that location only when its own wave actually reaches it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CultAftermath {
    pub sites: Vec<ResidueRecord>,
    pub wave_owner: Option<CampaignId>,
    pub introduced_wave: usize,
    pub remnants_for: Option<CampaignId>,
}

impl CultAftermath {
    pub fn pending(&self) -> usize {
        self.sites.iter().filter(|site| !site.cleared).count()
    }

    pub fn spent_pending(&self) -> usize {
        self.sites
            .iter()
            .filter(|site| !site.cleared && site.spent)
            .count()
    }

    fn introduce(&mut self, site: DefacementDef, owner: CampaignId, spent: bool) {
        if let Some(record) = self
            .sites
            .iter_mut()
            .find(|record| record.site.spot == site.spot)
        {
            if record.owner != owner {
                *record = ResidueRecord {
                    site,
                    owner,
                    cleared: false,
                    spent,
                };
            }
        } else {
            self.sites.push(ResidueRecord {
                site,
                owner,
                cleared: false,
                spent,
            });
        }
    }

    pub(crate) fn advance(&mut self, campaign: &CampaignArc, script: &CultScript) {
        // A different antagonist must not erase the physical aftermath, nor
        // leave an old ritual's lights running after its campaign is replaced.
        for site in &mut self.sites {
            if site.owner != campaign.id || campaign.outcome.is_some() {
                site.spent = true;
            }
        }
        if campaign.antag != AntagId::Cult {
            return;
        }
        if self.wave_owner != Some(campaign.id) {
            self.wave_owner = Some(campaign.id);
            self.introduced_wave = 0;
        }
        // Incident slots are allocated atomically when a wave lands. Unlike
        // plot pressure, their high-water mark cannot retreat after treatment.
        let wave = script
            .stages
            .iter()
            .enumerate()
            .take_while(|(index, _)| {
                script.guard_ward_index(*index) < campaign.cult_incidents.len()
            })
            .count();
        for site in &script.defacements {
            if site.wave > self.introduced_wave && site.wave <= wave {
                self.introduce(site.clone(), campaign.id, campaign.outcome.is_some());
            }
        }
        self.introduced_wave = self.introduced_wave.max(wave);

        if campaign.outcome.is_none() || self.remnants_for == Some(campaign.id) {
            return;
        }
        // Showdown removes its combat entities. Reconstruct only unresolved
        // physical manifestations, using the same named map locations/models.
        let mut remains = Vec::new();
        if campaign.cult_incidents.first() != Some(&true) {
            remains.push(&script.altar);
        }
        for (stage, definition) in script.stages.iter().enumerate() {
            for (offset, incident) in definition.incidents.iter().enumerate() {
                if campaign
                    .cult_incidents
                    .get(script.stage_ward_base(stage) + offset)
                    == Some(&false)
                {
                    remains.push(incident);
                }
            }
        }
        for incident in remains {
            self.introduce(
                DefacementDef {
                    name: format!("Spent {}", incident.name.to_lowercase()),
                    visual: incident.visual,
                    spot: incident.spot.clone(),
                    wave: 0,
                    cleanup: if matches!(
                        incident.visual,
                        CultVisualId::BleedingOfferingBowl | CultVisualId::AirlessCandle
                    ) {
                        Cleanup::Dismantle
                    } else {
                        Cleanup::Cleaner
                    },
                    amount: 5,
                },
                campaign.id,
                true,
            );
        }
        self.remnants_for = Some(campaign.id);
    }
}

#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CultResidue {
    pub spot: String,
    pub owner: CampaignId,
    pub spent: bool,
}

#[derive(SystemSet, Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct AftermathUpdate;

pub(super) fn register(app: &mut App) {
    app.init_resource::<SpentMaterials>()
        .add_systems(
            Update,
            (
                update_state,
                remove_ended_ritual,
                reconcile_entities,
                clean_residue,
            )
                .chain()
                .in_set(AftermathUpdate)
                .after(super::remember_started_finale)
                .after(crate::threat::PromoteScripts)
                .run_if(is_authority)
                .run_if(resource_exists::<MapReady>)
                .run_if(in_state(AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (ashen_spent_meshes, add_activity_lights, pulse_activity)
                .chain()
                .run_if(in_state(AppState::Playing)),
        );
}

fn update_state(
    campaign: Option<ResMut<Campaign>>,
    script: Option<Res<Script>>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(mut campaign), Some(script)) = (campaign, script) else {
        return;
    };
    if !campaign.is_changed() && !script.is_changed() {
        return;
    }
    let Some(arc) = campaign.active.first() else {
        return;
    };
    let mut next = campaign.cult_aftermath.clone();
    next.advance(arc, &script);
    if next == campaign.cult_aftermath {
        return;
    }
    let new_wave = next.wave_owner != campaign.cult_aftermath.wave_owner
        || next.introduced_wave > campaign.cult_aftermath.introduced_wave;
    let ended = next.remnants_for != campaign.cult_aftermath.remnants_for;
    if ended && next.pending() > 0 {
        radio.push(RadioEntry::new(RadioChannel::Lab, format!(
            "The ritual activity has stopped, but {} contaminated sites remain. Space Cleaner removes marks and growths; empty hands dismantle spent shrines, caches and banners. The standing board tracks cleanup.", next.pending()
        )).urgent());
    } else if new_wave && next.introduced_wave > 0 {
        radio.push(RadioEntry::new(RadioChannel::Lab,
            "New markings and ritual deposits are spreading onto station surfaces. Clearing these sites contains the mess; the original wards still lead to the source."
        ).negative());
    }
    campaign.cult_aftermath = next;
}

fn prompt(record: &ResidueRecord) -> String {
    if record.spent && record.site.cleanup == Cleanup::Dismantle {
        format!("{} - dismantle with empty hands", record.site.name)
    } else {
        format!(
            "{} - clean with {}u Space Cleaner",
            record.site.name, record.site.amount
        )
    }
}

#[allow(clippy::type_complexity)]
fn remove_ended_ritual(
    mut commands: Commands,
    campaign: Option<Res<Campaign>>,
    entities: Query<
        Entity,
        Or<(
            With<super::RitualAnchor>,
            With<super::RitualFocus>,
            With<super::Cultist>,
            With<super::CultHerald>,
        )>,
    >,
) {
    if campaign
        .is_some_and(|campaign| campaign.antag == AntagId::Cult && campaign.outcome.is_some())
    {
        for entity in &entities {
            commands.entity(entity).despawn();
        }
    }
}

pub(crate) fn reconcile_entities(
    mut commands: Commands,
    campaign: Option<Res<Campaign>>,
    spots: Res<CrisisSpots>,
    existing: Query<(Entity, &CultResidue)>,
) {
    let Some(campaign) = campaign else { return };
    for (entity, residue) in &existing {
        if !campaign.cult_aftermath.sites.iter().any(|record| {
            record.site.spot == residue.spot && record.owner == residue.owner && !record.cleared
        }) {
            commands.entity(entity).despawn();
        }
    }
    for record in campaign
        .cult_aftermath
        .sites
        .iter()
        .filter(|record| !record.cleared)
    {
        let residue = CultResidue {
            spot: record.site.spot.clone(),
            owner: record.owner,
            spent: record.spent,
        };
        if let Some((entity, previous)) = existing
            .iter()
            .find(|(_, previous)| previous.spot == residue.spot && previous.owner == residue.owner)
        {
            if previous != &residue {
                commands
                    .entity(entity)
                    .insert((residue, Interactable::new(prompt(record))));
            }
            continue;
        }
        let Some(transform) = spots.get(&record.site.spot) else {
            continue;
        };
        commands.spawn((
            residue,
            CultVisual(record.site.visual),
            transform,
            Visibility::default(),
            Interactable::new(prompt(record)),
            Replicated,
            crate::until_we_leave_the_lab(),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn clean_residue(
    mut commands: Commands,
    campaign: Option<ResMut<Campaign>>,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    targets: Query<(&CultResidue, &Transform)>,
    chemists: Query<(Entity, &Chemist, &Transform, Option<&Body>)>,
    mut containers: Query<(&mut Container, &HeldBy)>,
    solids: Query<(&Transform, &Solid)>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(mut campaign) = campaign else {
        requests.clear();
        return;
    };
    for request in requests.read() {
        let Ok((target, target_transform)) = targets.get(request.target) else {
            continue;
        };
        let Some((player, _, actor_transform, body)) = chemists
            .iter()
            .find(|(_, chemist, _, _)| chemist.client == request.client_id)
        else {
            continue;
        };
        if body.is_some_and(|body| body.0.collapsed)
            || !authority_target_in_reach(
                actor_transform.translation,
                target_transform.translation,
                REACH,
            )
            || solids.iter().any(|(at, solid)| {
                authority_segment_blocked(
                    actor_transform.translation,
                    target_transform.translation,
                    at.translation,
                    solid.half_extents,
                )
            })
        {
            continue;
        }
        let Some(index) = campaign.cult_aftermath.sites.iter().position(|record| {
            record.site.spot == target.spot && record.owner == target.owner && !record.cleared
        }) else {
            continue;
        };
        let record = &campaign.cult_aftermath.sites[index];
        let held = containers.iter_mut().find(|(_, held)| held.0 == player);
        if record.spent && record.site.cleanup == Cleanup::Dismantle {
            if held.is_some() {
                radio.push(RadioEntry::new(
                    RadioChannel::Lab,
                    "Set the container down first; these spent remains can be dismantled by hand.",
                ));
                continue;
            }
        } else {
            let Some((mut container, _)) = held else {
                radio.push(RadioEntry::new(
                    RadioChannel::Lab,
                    format!(
                        "{} needs {}u Space Cleaner. {}",
                        record.site.name,
                        record.site.amount,
                        if record.spent {
                            "The ritual is gone; the residue is still here."
                        } else {
                            "Secondary contamination grants no ward credit."
                        }
                    ),
                ));
                continue;
            };
            let Some(cleaner) = db.reagents.id_of("space_cleaner") else {
                continue;
            };
            let needed = Units::whole(record.site.amount as i32);
            if !container.solution.contains_at_least(cleaner, needed) {
                radio.push(RadioEntry::new(
                    RadioChannel::Lab,
                    format!(
                        "Bring at least {}u Space Cleaner for the {}. Nothing was consumed.",
                        record.site.amount, record.site.name
                    ),
                ));
                continue;
            }
            // Keep the vessel, surplus cleaner and unrelated reagents intact.
            container.solution.remove(cleaner, needed);
        }
        let name = record.site.name.clone();
        campaign.cult_aftermath.sites[index].cleared = true;
        commands.entity(request.target).despawn();
        let remaining = campaign.cult_aftermath.pending();
        radio.push(
            RadioEntry::new(
                RadioChannel::Lab,
                if remaining == 0 {
                    format!(
                        "{name} cleared. All currently known Cult contamination has been removed."
                    )
                } else {
                    format!("{name} cleared. {remaining} contaminated sites remain on the station.")
                },
            )
            .positive(),
        );
    }
}

#[derive(Resource, Default)]
struct SpentMaterials(HashMap<AssetId<StandardMaterial>, Handle<StandardMaterial>>);

#[derive(Component)]
struct Ashened;

fn ashen_spent_meshes(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<SpentMaterials>,
    children: Query<&Children>,
    residues: Query<(Entity, &CultResidue)>,
    mut meshes: Query<(Entity, &mut MeshMaterial3d<StandardMaterial>), Without<Ashened>>,
) {
    let candidates = residues
        .iter()
        .filter(|(_, residue)| residue.spent)
        .flat_map(|(entity, _)| children.iter_descendants(entity));
    for entity in candidates {
        let Ok((_, mut material)) = meshes.get_mut(entity) else {
            continue;
        };
        let original_id = material.0.id();
        let replacement = if let Some(cached) = cache.0.get(&original_id) {
            cached.clone()
        } else {
            let Some(original) = materials.get(&material.0) else {
                continue;
            };
            let mut ash = original.clone();
            let color = ash.base_color.to_linear();
            let grey = (color.red + color.green + color.blue) / 3.0;
            ash.base_color = Color::linear_rgba(
                color.red * 0.35 + grey * 0.35,
                color.green * 0.35 + grey * 0.33,
                color.blue * 0.35 + grey * 0.31,
                color.alpha,
            );
            ash.emissive = LinearRgba::BLACK;
            ash.perceptual_roughness = ash.perceptual_roughness.max(0.86);
            let handle = materials.add(ash);
            cache.0.insert(original_id, handle.clone());
            handle
        };
        material.0 = replacement;
        commands.entity(entity).insert(Ashened);
    }
}

#[derive(Component)]
struct ActivityDressed;
#[derive(Component)]
struct ActivityLight(Entity);

#[allow(clippy::type_complexity)]
fn add_activity_lights(
    mut commands: Commands,
    roots: Query<(Entity, &CultVisual), (With<CultResidue>, Without<ActivityDressed>)>,
) {
    for (entity, visual) in &roots {
        commands.entity(entity).insert(ActivityDressed);
        if !matches!(
            visual.0,
            CultVisualId::BoundVent | CultVisualId::RiftFracture
        ) {
            continue;
        }
        commands.spawn((
            ActivityLight(entity),
            ChildOf(entity),
            PointLight {
                color: Color::srgb(0.65, 0.03, 0.09),
                intensity: 900.0,
                range: 2.2,
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_xyz(0.0, -0.3, 0.2),
            Visibility::default(),
        ));
    }
}

fn pulse_activity(
    time: Res<Time>,
    roots: Query<&CultResidue>,
    mut lights: Query<(&ActivityLight, &mut PointLight)>,
) {
    for (source, mut light) in &mut lights {
        light.intensity = if roots.get(source.0).is_ok_and(|residue| !residue.spent) {
            900.0 * (0.72 + 0.28 * (time.elapsed_secs() * 1.5).sin())
        } else {
            0.0
        };
    }
}

#[cfg(test)]
mod tests;
