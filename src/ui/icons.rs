//! Semantic icon registry for the chemistry field manual.
//!
//! Runtime UI uses checked-in monochrome PNGs so Bevy can tint one coherent
//! line-art set for category, state, process and effect contexts. Editable SVG
//! source and the deterministic exporter live beside those images under
//! `assets/ui/chemistry_book`.

use std::collections::HashMap;

use bevy::prelude::*;
use chem_sim::Category;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BookIcon {
    All,
    Trauma,
    Burns,
    Antitoxins,
    Airloss,
    Radiation,
    Stimulants,
    Poisons,
    Pyrotechnics,
    Precursors,
    Utility,
    Illicit,
    Recorded,
    Ready,
    Frontier,
    Locked,
    ChemMaster,
    ReactionChamber,
    MixingChamber,
    DirectMix,
    Agitate,
    Heat,
    Catalyst,
    Temperature,
    Ph,
    Purity,
    Duration,
    Research,
    Inputs,
    Controlled,
    Explosive,
    Overdose,
    Critical,
    Aftereffect,
    Inject,
    Ingest,
    Patch,
    Spray,
    Contact,
    Smoke,
    Heal,
    Harm,
    Status,
    Purge,
    Clean,
    Corrode,
    Ignite,
    Slippery,
    Flammable,
    Chill,
    Flash,
    Foam,
    Extinguish,
    Dependency,
    RawReagent,
    Orders,
    Book,
    Key,
}

impl BookIcon {
    pub(crate) const ALL: [Self; 58] = [
        Self::All,
        Self::Trauma,
        Self::Burns,
        Self::Antitoxins,
        Self::Airloss,
        Self::Radiation,
        Self::Stimulants,
        Self::Poisons,
        Self::Pyrotechnics,
        Self::Precursors,
        Self::Utility,
        Self::Illicit,
        Self::Recorded,
        Self::Ready,
        Self::Frontier,
        Self::Locked,
        Self::ChemMaster,
        Self::ReactionChamber,
        Self::MixingChamber,
        Self::DirectMix,
        Self::Agitate,
        Self::Heat,
        Self::Catalyst,
        Self::Temperature,
        Self::Ph,
        Self::Purity,
        Self::Duration,
        Self::Research,
        Self::Inputs,
        Self::Controlled,
        Self::Explosive,
        Self::Overdose,
        Self::Critical,
        Self::Aftereffect,
        Self::Inject,
        Self::Ingest,
        Self::Patch,
        Self::Spray,
        Self::Contact,
        Self::Smoke,
        Self::Heal,
        Self::Harm,
        Self::Status,
        Self::Purge,
        Self::Clean,
        Self::Corrode,
        Self::Ignite,
        Self::Slippery,
        Self::Flammable,
        Self::Chill,
        Self::Flash,
        Self::Foam,
        Self::Extinguish,
        Self::Dependency,
        Self::RawReagent,
        Self::Orders,
        Self::Book,
        Self::Key,
    ];

    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Trauma => "trauma",
            Self::Burns => "burns",
            Self::Antitoxins => "antitoxins",
            Self::Airloss => "airloss",
            Self::Radiation => "radiation",
            Self::Stimulants => "stimulants",
            Self::Poisons => "poisons",
            Self::Pyrotechnics => "pyrotechnics",
            Self::Precursors => "precursors",
            Self::Utility => "utility",
            Self::Illicit => "illicit",
            Self::Recorded => "recorded",
            Self::Ready => "ready",
            Self::Frontier => "frontier",
            Self::Locked => "locked",
            Self::ChemMaster => "chemmaster",
            Self::ReactionChamber => "reaction-chamber",
            Self::MixingChamber => "mixing-chamber",
            Self::DirectMix => "direct-mix",
            Self::Agitate => "agitate",
            Self::Heat => "heat",
            Self::Catalyst => "catalyst",
            Self::Temperature => "temperature",
            Self::Ph => "ph",
            Self::Purity => "purity",
            Self::Duration => "duration",
            Self::Research => "research",
            Self::Inputs => "inputs",
            Self::Controlled => "controlled",
            Self::Explosive => "explosive",
            Self::Overdose => "overdose",
            Self::Critical => "critical",
            Self::Aftereffect => "aftereffect",
            Self::Inject => "inject",
            Self::Ingest => "ingest",
            Self::Patch => "patch",
            Self::Spray => "spray",
            Self::Contact => "contact",
            Self::Smoke => "smoke",
            Self::Heal => "heal",
            Self::Harm => "harm",
            Self::Status => "status",
            Self::Purge => "purge",
            Self::Clean => "clean",
            Self::Corrode => "corrode",
            Self::Ignite => "ignite",
            Self::Slippery => "slippery",
            Self::Flammable => "flammable",
            Self::Chill => "chill",
            Self::Flash => "flash",
            Self::Foam => "foam",
            Self::Extinguish => "extinguish",
            Self::Dependency => "dependency",
            Self::RawReagent => "raw-reagent",
            Self::Orders => "orders",
            Self::Book => "book",
            Self::Key => "key",
        }
    }

    pub(super) fn category(category: Option<Category>) -> Self {
        match category {
            None => Self::All,
            Some(Category::Trauma) => Self::Trauma,
            Some(Category::Burns) => Self::Burns,
            Some(Category::Antitoxins) => Self::Antitoxins,
            Some(Category::Airloss) => Self::Airloss,
            Some(Category::Radiation) => Self::Radiation,
            Some(Category::Stimulants) => Self::Stimulants,
            Some(Category::Poisons) => Self::Poisons,
            Some(Category::Pyrotechnics) => Self::Pyrotechnics,
            Some(Category::Precursors) => Self::Precursors,
            Some(Category::Utility) => Self::Utility,
            Some(Category::Illicit) => Self::Illicit,
        }
    }
}

#[derive(Resource)]
pub(crate) struct BookIconAssets(HashMap<BookIcon, Handle<Image>>);

impl FromWorld for BookIconAssets {
    fn from_world(world: &mut World) -> Self {
        let assets = world.resource::<AssetServer>();
        let mut handles = HashMap::new();
        for icon in BookIcon::ALL {
            let path = format!("ui/chemistry_book/icons/{}.png", icon.slug());
            handles.insert(icon, assets.load(path));
        }
        Self(handles)
    }
}

impl BookIconAssets {
    pub(crate) fn image(&self, icon: BookIcon) -> Handle<Image> {
        self.0
            .get(&icon)
            .cloned()
            .expect("every semantic book icon is loaded")
    }
}

pub(crate) fn icon_image(
    assets: &BookIconAssets,
    icon: BookIcon,
    size: f32,
    color: Color,
) -> impl Bundle {
    (
        ImageNode {
            image: assets.image(icon),
            color,
            ..default()
        },
        Node {
            width: px(size),
            height: px(size),
            flex_shrink: 0.0,
            ..default()
        },
        Pickable::IGNORE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_has_a_distinct_semantic_icon() {
        let mut icons = Category::ALL
            .map(|category| BookIcon::category(Some(category)).slug())
            .to_vec();
        icons.sort_unstable();
        icons.dedup();
        assert_eq!(icons.len(), Category::ALL.len());
        assert_eq!(BookIcon::category(None), BookIcon::All);
    }

    #[test]
    fn every_registry_entry_has_a_runtime_slug() {
        for icon in BookIcon::ALL {
            let slug = icon.slug();
            assert!(!slug.is_empty());
            assert!(slug.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
        }
    }
}
