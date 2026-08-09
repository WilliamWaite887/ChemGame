//! What the chemist currently knows how to make.
//!
//! M5 only reads this: the book renders it and order generation uses it to
//! stay within reach. M6 makes it change — discovery, hint purchases and
//! persistence.
//!
//! `chem_sim` deliberately knows nothing about any of this. The resolver
//! always simulates real chemistry; whether the player has written the recipe
//! down is a separate question, and conflating the two would mean a beaker
//! behaving differently depending on who is holding it.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use chem_sim::{ChemData, ReactionId, ReagentId};

use crate::chem_data::ChemDb;
use crate::machines::ReactionsFired;
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// The recipes a chemist starts the shift knowing.
///
/// These three are deliberate: every other seed recipe is these plus one or
/// two base reagents, so the first discovery is a short reasoned step rather
/// than a search through hundreds of combinations.
pub const STARTING_RECIPES: [&str; 3] = ["inaprovaline", "dylovene", "kelotane"];

/// Hints visible before any research is spent. One is enough to point at an
/// approach without giving the recipe away.
const FREE_HINTS: usize = 1;

pub struct KnowledgePlugin;

impl Plugin for KnowledgePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Playing), initialise_knowledge)
            .add_systems(
                Update,
                learn_from_experiments.run_if(in_state(AppState::Playing)),
            );
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    Known,
    Locked { hints_revealed: usize },
}

#[derive(Resource, Default)]
pub struct Knowledge {
    entries: HashMap<ReactionId, Entry>,
}

impl Knowledge {
    /// Starts a chemist off knowing [`STARTING_RECIPES`] and nothing else.
    pub fn new(data: &ChemData) -> Self {
        let mut entries = HashMap::new();
        for reaction in data.reactions.iter() {
            let known = STARTING_RECIPES.contains(&reaction.key.as_str());
            entries.insert(
                reaction.id,
                if known {
                    Entry::Known
                } else {
                    Entry::Locked {
                        hints_revealed: FREE_HINTS,
                    }
                },
            );
        }
        Knowledge { entries }
    }

    pub fn entry(&self, reaction: ReactionId) -> Entry {
        self.entries
            .get(&reaction)
            .copied()
            .unwrap_or(Entry::Locked { hints_revealed: 0 })
    }

    pub fn is_known(&self, reaction: ReactionId) -> bool {
        self.entry(reaction) == Entry::Known
    }

    pub fn known_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| **entry == Entry::Known)
            .count()
    }

    /// Marks a recipe as learned. Returns true if this was news.
    pub fn learn(&mut self, reaction: ReactionId) -> bool {
        let already = self.is_known(reaction);
        self.entries.insert(reaction, Entry::Known);
        !already
    }

    /// Reveals one more hint for a locked recipe.
    ///
    /// Nothing spends research points yet; M6 gives the player a way to buy
    /// these deliberately rather than only stumbling into recipes.
    #[allow(dead_code)]
    pub fn reveal_hint(&mut self, reaction: ReactionId) {
        if let Some(Entry::Locked { hints_revealed }) = self.entries.get_mut(&reaction) {
            *hints_revealed += 1;
        }
    }

    /// Every reagent the chemist can currently produce, plus the base reagents
    /// they can dispense.
    pub fn available_reagents(&self, data: &ChemData) -> HashSet<ReagentId> {
        let mut available: HashSet<ReagentId> =
            data.reagents.dispensable().map(|r| r.id).collect();

        // Known recipes can feed each other, so keep going until nothing new
        // appears rather than making a single pass.
        loop {
            let mut grew = false;
            for reaction in data.reactions.iter() {
                if !self.is_known(reaction.id) {
                    continue;
                }
                let inputs_ready = reaction
                    .reactants
                    .iter()
                    .chain(reaction.catalysts.iter())
                    .all(|(id, _)| available.contains(id));
                if inputs_ready {
                    for product in reaction.product_ids() {
                        grew |= available.insert(product);
                    }
                }
            }
            if !grew {
                break;
            }
        }
        available
    }

    /// Locked recipes whose ingredients the chemist can already obtain.
    ///
    /// This is the set worth asking for: reachable with one experiment, but
    /// not yet written down. Ordering follows the reaction list so generation
    /// stays reproducible.
    pub fn frontier(&self, data: &ChemData) -> Vec<ReactionId> {
        let available = self.available_reagents(data);
        data.reactions
            .iter()
            .filter(|reaction| !self.is_known(reaction.id))
            .filter(|reaction| {
                reaction
                    .reactants
                    .iter()
                    .chain(reaction.catalysts.iter())
                    .all(|(id, _)| available.contains(id))
            })
            .map(|reaction| reaction.id)
            .collect()
    }

    /// Hints unlocked so far for a locked recipe.
    pub fn visible_hints<'a>(&self, data: &'a ChemData, reaction: ReactionId) -> &'a [String] {
        let Entry::Locked { hints_revealed } = self.entry(reaction) else {
            return &[];
        };
        let hints = &data.reactions.get(reaction).hints;
        &hints[..hints_revealed.min(hints.len())]
    }
}

/// Records any reaction the chemist manages to cause.
///
/// Making the thing *is* the discovery — there is no separate "confirm" step,
/// because a chemist who has just watched a beaker turn the right colour knows
/// perfectly well what they did.
fn learn_from_experiments(
    db: Res<ChemDb>,
    mut knowledge: ResMut<Knowledge>,
    mut fired: MessageReader<ReactionsFired>,
    mut radio: ResMut<RadioLog>,
) {
    for event in fired.read() {
        for reaction in &event.reactions {
            if !knowledge.learn(*reaction) {
                continue;
            }
            let recipe = db.reactions.get(*reaction);
            let name = recipe
                .products
                .first()
                .map(|(id, _)| db.reagents.get(*id).name.clone())
                .unwrap_or_else(|| recipe.key.clone());

            info!("recipe discovered: {name}");
            radio.push(RadioEntry {
                channel: "LAB".to_string(),
                text: format!("Method for {name} written up in the reference book."),
                good: true,
            });
        }
    }
}

fn initialise_knowledge(mut commands: Commands, db: Res<ChemDb>) {
    let knowledge = Knowledge::new(&db);
    info!(
        "chemist knows {} of {} recipes",
        knowledge.known_count(),
        db.reactions.len()
    );
    commands.insert_resource(knowledge);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> ChemData {
        ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn reaction_key(data: &ChemData, id: ReactionId) -> &str {
        &data.reactions.get(id).key
    }

    #[test]
    fn a_fresh_chemist_knows_only_the_starting_recipes() {
        let data = data();
        let knowledge = Knowledge::new(&data);
        assert_eq!(knowledge.known_count(), STARTING_RECIPES.len());
        for key in STARTING_RECIPES {
            let reaction = data.reactions.find(key).unwrap();
            assert!(knowledge.is_known(reaction.id), "{key} should be known");
        }
    }

    #[test]
    fn the_frontier_is_exactly_what_one_experiment_could_reach() {
        let data = data();
        let knowledge = Knowledge::new(&data);
        let mut frontier: Vec<&str> = knowledge
            .frontier(&data)
            .into_iter()
            .map(|id| reaction_key(&data, id))
            .collect();
        frontier.sort();

        // Bicaridine and hyronalin build on inaprovaline and dylovene;
        // tricordrazine needs both; dermaline builds on kelotane; dexalin only
        // needs base reagents and a catalyst. Arithrazine is two steps out and
        // must NOT appear until hyronalin is learned.
        assert_eq!(
            frontier,
            vec![
                "bicaridine",
                "dermaline",
                "dexalin",
                "hyronalin",
                "tricordrazine"
            ]
        );
    }

    #[test]
    fn learning_a_recipe_opens_the_next_one_up() {
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let hyronalin = data.reactions.find("hyronalin").unwrap().id;

        assert!(
            !knowledge
                .frontier(&data)
                .iter()
                .any(|id| reaction_key(&data, *id) == "arithrazine"),
            "arithrazine is out of reach before hyronalin"
        );

        assert!(knowledge.learn(hyronalin), "learning it should be news");
        assert!(!knowledge.learn(hyronalin), "learning it twice should not");

        assert!(
            knowledge
                .frontier(&data)
                .iter()
                .any(|id| reaction_key(&data, *id) == "arithrazine"),
            "arithrazine becomes reachable once hyronalin is known"
        );
    }

    #[test]
    fn hints_are_revealed_one_at_a_time() {
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let bicaridine = data.reactions.find("bicaridine").unwrap().id;

        assert_eq!(knowledge.visible_hints(&data, bicaridine).len(), FREE_HINTS);
        knowledge.reveal_hint(bicaridine);
        assert_eq!(
            knowledge.visible_hints(&data, bicaridine).len(),
            FREE_HINTS + 1
        );

        // Revealing past the end must clamp rather than panic on the slice.
        for _ in 0..10 {
            knowledge.reveal_hint(bicaridine);
        }
        let all = &data.reactions.get(bicaridine).hints;
        assert_eq!(knowledge.visible_hints(&data, bicaridine).len(), all.len());
    }

    #[test]
    fn known_recipes_expose_no_hints() {
        let data = data();
        let knowledge = Knowledge::new(&data);
        let inaprovaline = data.reactions.find("inaprovaline").unwrap().id;
        assert!(knowledge.visible_hints(&data, inaprovaline).is_empty());
    }
}
