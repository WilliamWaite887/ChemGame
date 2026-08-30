//! Pure presentation coverage for one recipe-book detail page.
//!
//! Rendering code is intentionally separate from this inventory: tests can
//! prove that every authored mechanical field has a visible destination
//! without constructing a Bevy `World` or comparing screenshots.

use chem_sim::Reaction;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct RecipePresentation {
    pub(super) input_count: usize,
    pub(super) catalyst_count: usize,
    pub(super) temperature: String,
    pub(super) ph: Option<String>,
    pub(super) minimum_purity: Option<String>,
    pub(super) processing: String,
    pub(super) overheat: Option<String>,
    pub(super) profile: Option<ProfileCoverage>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ProfileCoverage {
    pub(super) ph: f32,
    pub(super) controlled: bool,
    pub(super) explosive: bool,
    pub(super) bodily_effects: usize,
    pub(super) targeted_purges: usize,
    pub(super) overdose_effects: usize,
    pub(super) critical_effects: usize,
    pub(super) after_effects: usize,
    pub(super) has_body_routes: bool,
    pub(super) world_effects: usize,
}

impl RecipePresentation {
    pub(super) fn new(reaction: &Reaction, product: Option<&chem_sim::Reagent>) -> Self {
        Self {
            input_count: reaction.reactants.len() + reaction.catalysts.len(),
            catalyst_count: reaction.catalysts.len(),
            temperature: super::temperature_value(reaction),
            ph: (reaction.min_ph.is_some() || reaction.max_ph.is_some())
                .then(|| super::ph_value(reaction)),
            minimum_purity: reaction
                .min_purity
                .map(|minimum| format!("≥{:.0}%", minimum * 100.0)),
            processing: super::processing_value(reaction),
            overheat: reaction
                .overheat_temp
                .map(|threshold| format!("{threshold}: {}", super::overheat_explanation(reaction))),
            profile: product.map(ProfileCoverage::new),
        }
    }
}

impl ProfileCoverage {
    pub(super) fn new(reagent: &chem_sim::Reagent) -> Self {
        Self {
            ph: reagent.ph,
            controlled: reagent.controlled,
            explosive: reagent.explosive.is_some(),
            bodily_effects: reagent.effects.len(),
            targeted_purges: reagent.targeted_purges.len(),
            overdose_effects: reagent.overdose_effects.len(),
            critical_effects: reagent.critical_effects.len(),
            after_effects: reagent.after_effects.len(),
            has_body_routes: !reagent.effects.is_empty() || !reagent.overdose_effects.is_empty(),
            world_effects: reagent.world_effects.len(),
        }
    }
}
