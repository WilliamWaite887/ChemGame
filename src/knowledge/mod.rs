//! What the chemist knows how to make, and how they come to know it.
//!
//! `chem_sim` deliberately knows nothing about any of this. The resolver
//! always simulates real chemistry; whether the player has written the recipe
//! down is a separate question, and conflating the two would mean a beaker
//! behaving differently depending on who is holding it.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{Category, ChemData, ReactionId, ReagentId};
use serde::{Deserialize, Serialize};

use crate::chem_data::ChemDb;
use crate::machines::ReactionsFired;
use crate::net::is_authority;
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// The recipes a chemist starts the shift knowing.
///
/// These three are deliberate: every other seed recipe is these plus one or
/// two base reagents, so the first discovery is a short reasoned step rather
/// than a search through hundreds of combinations.
pub const STARTING_RECIPES: [&str; 3] = ["inaprovaline", "dylovene", "kelotane"];

/// Hints visible before any research is spent.
const FREE_HINTS: usize = 1;
/// Research points a hint costs.
pub const HINT_COST: u32 = 1;
/// Points earned for a clean delivery.
pub const RESEARCH_PER_SUCCESS: u32 = 1;

use crate::saves::SaveSlot;

pub struct KnowledgePlugin;

impl Plugin for KnowledgePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<RecipeDiscovered>()
            .add_server_message::<KnowledgeSync>(Channel::Ordered)
            .add_systems(OnEnter(AppState::Playing), initialise_knowledge)
            .add_systems(
                Update,
                (
                    // The shared notebook belongs to the lab, so the server
                    // owns it and only the server writes the save file.
                    (
                        learn_from_experiments,
                        persist_knowledge,
                        broadcast_knowledge,
                    )
                        .chain()
                        .run_if(is_authority),
                    apply_knowledge.run_if(in_state(ClientState::Connected)),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    Known,
    Locked { hints_revealed: usize },
}

/// Announced when a recipe is worked out, however it happened.
#[derive(Message)]
pub struct RecipeDiscovered {
    pub name: String,
}

#[derive(Resource, Default)]
pub struct Knowledge {
    entries: HashMap<ReactionId, Entry>,
    pub research_points: u32,
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
        Knowledge {
            entries,
            research_points: 0,
        }
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

    pub fn award_research(&mut self, points: u32) {
        self.research_points += points;
    }

    /// Whether another hint exists to buy for this recipe.
    pub fn hint_available(&self, data: &ChemData, reaction: ReactionId) -> bool {
        match self.entry(reaction) {
            Entry::Known => false,
            Entry::Locked { hints_revealed } => {
                hints_revealed < data.reactions.get(reaction).hints.len()
            }
        }
    }

    /// Spends a point to reveal one more hint. Returns false if the chemist
    /// cannot afford it or there is nothing left to reveal.
    pub fn buy_hint(&mut self, data: &ChemData, reaction: ReactionId) -> bool {
        if self.research_points < HINT_COST || !self.hint_available(data, reaction) {
            return false;
        }
        if let Some(Entry::Locked { hints_revealed }) = self.entries.get_mut(&reaction) {
            *hints_revealed += 1;
            self.research_points -= HINT_COST;
            return true;
        }
        false
    }

    /// Every reagent the chemist can currently produce, plus the base reagents
    /// they can dispense.
    pub fn available_reagents(&self, data: &ChemData) -> HashSet<ReagentId> {
        let mut available: HashSet<ReagentId> = data.reagents.dispensable().map(|r| r.id).collect();

        // Known recipes feed each other, so keep going until nothing new
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
    /// not yet written down.
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

    // -- persistence --------------------------------------------------------

    /// Reaction *keys* rather than ids, because ids are positions in the data
    /// file: inserting a recipe would otherwise silently rewrite what a save
    /// says the chemist knows.
    fn to_save(&self, data: &ChemData) -> SaveData {
        let mut known = Vec::new();
        let mut hints = HashMap::new();
        for reaction in data.reactions.iter() {
            match self.entry(reaction.id) {
                Entry::Known => known.push(reaction.key.clone()),
                Entry::Locked { hints_revealed } if hints_revealed != FREE_HINTS => {
                    hints.insert(reaction.key.clone(), hints_revealed);
                }
                Entry::Locked { .. } => {}
            }
        }
        known.sort();
        SaveData {
            known,
            hints,
            research_points: self.research_points,
        }
    }

    fn from_save(data: &ChemData, save: SaveData) -> Self {
        let mut knowledge = Knowledge::new(data);
        knowledge.research_points = save.research_points;
        for key in &save.known {
            if let Some(reaction) = data.reactions.find(key) {
                knowledge.learn(reaction.id);
            }
        }
        for (key, revealed) in &save.hints {
            if let Some(reaction) = data.reactions.find(key) {
                if let Some(Entry::Locked { hints_revealed }) =
                    knowledge.entries.get_mut(&reaction.id)
                {
                    *hints_revealed = *revealed;
                }
            }
        }
        knowledge
    }
}

/// A complete snapshot of what the lab knows.
///
/// Doubles as the save format and the co-op sync payload — they want exactly
/// the same thing, and having one representation means a save and a joining
/// client can never disagree about what "known" means.
#[derive(Serialize, Deserialize, Default, Clone)]
struct SaveData {
    known: Vec<String>,
    #[serde(default)]
    hints: HashMap<String, usize>,
    #[serde(default)]
    research_points: u32,
}

fn initialise_knowledge(mut commands: Commands, db: Res<ChemDb>, slot: Option<Res<SaveSlot>>) {
    // No slot is a guest: their notebook arrives from the host, and reading a
    // local save first would show them recipes this lab has not discovered.
    let loaded = slot.and_then(|slot| load_save(&db, &slot.knowledge_path()));
    let knowledge = match loaded {
        Some(loaded) => {
            info!(
                "resumed: {} of {} recipes, {} research",
                loaded.known_count(),
                db.reactions.len(),
                loaded.research_points
            );
            loaded
        }
        None => {
            let fresh = Knowledge::new(&db);
            info!(
                "new chemist: knows {} of {} recipes",
                fresh.known_count(),
                db.reactions.len()
            );
            fresh
        }
    };
    commands.insert_resource(knowledge);
}

fn load_save(db: &ChemDb, path: &Path) -> Option<Knowledge> {
    read_save(path).map(|save| Knowledge::from_save(db, save))
}

/// The notebook on disk, or `None` if there is not a readable one.
fn read_save(path: &Path) -> Option<SaveData> {
    if !path.exists() {
        return None;
    }
    // A corrupt save should cost the player their progress, not the session.
    match std::fs::read_to_string(path).map(|text| ron::from_str::<SaveData>(&text)) {
        Ok(Ok(save)) => Some(save),
        Ok(Err(error)) => {
            warn!("ignoring unreadable {}: {error}", path.display());
            None
        }
        Err(error) => {
            warn!("could not read {}: {error}", path.display());
            None
        }
    }
}

/// How many recipes a save holds, for the menu's load list.
///
/// Reads the file rather than the live resource because the menu is listing
/// saves it has not opened — and it goes through the owning module so the
/// format stays in one place.
pub fn known_count_in(path: &Path) -> Option<usize> {
    Some(read_save(path)?.known.len())
}

fn persist_knowledge(db: Res<ChemDb>, knowledge: Res<Knowledge>, slot: Option<Res<SaveSlot>>) {
    if !knowledge.is_changed() {
        return;
    }
    let Some(slot) = slot else {
        return;
    };
    let save = knowledge.to_save(&db);
    let Ok(text) = ron::ser::to_string_pretty(&save, default_ron_config()) else {
        return;
    };
    slot.write_knowledge(&text);
}

fn default_ron_config() -> ron::ser::PrettyConfig {
    ron::ser::PrettyConfig::default()
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
    mut discovered: MessageWriter<RecipeDiscovered>,
    mut radio: ResMut<RadioLog>,
) {
    for event in fired.read() {
        for reaction in &event.reactions {
            if !knowledge.learn(*reaction) {
                continue;
            }
            let name = product_name(&db, *reaction);
            info!("recipe discovered: {name}");
            radio.push(RadioEntry {
                channel: "LAB".to_string(),
                text: format!("Method for {name} written up in the reference book."),
                good: true,
            });
            discovered.write(RecipeDiscovered { name });
        }
    }
}

/// The whole notebook, pushed to clients whenever it changes.
///
/// Sent wholesale rather than as deltas: it is a few dozen short strings, and
/// a snapshot cannot drift out of step the way an incremental stream can if a
/// client joins mid-shift or misses an update.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct KnowledgeSync(SaveData);

fn broadcast_knowledge(
    db: Res<ChemDb>,
    knowledge: Res<Knowledge>,
    mut outgoing: MessageWriter<ToClients<KnowledgeSync>>,
) {
    if !knowledge.is_changed() {
        return;
    }
    outgoing.write(ToClients {
        // The host already holds the authoritative copy; echoing to it would
        // just overwrite the original with a copy of itself.
        targets: SendTargets::CLIENTS_ONLY,
        message: KnowledgeSync(knowledge.to_save(&db)),
    });
}

fn apply_knowledge(
    db: Res<ChemDb>,
    mut knowledge: ResMut<Knowledge>,
    mut incoming: MessageReader<KnowledgeSync>,
) {
    for sync in incoming.read() {
        *knowledge = Knowledge::from_save(&db, sync.0.clone());
    }
}

/// A recipe is named after what it makes — that is how the crew ask for it.
pub fn product_name(db: &ChemDb, reaction: ReactionId) -> String {
    let recipe = db.reactions.get(reaction);
    recipe
        .products
        .first()
        .map(|(id, _)| db.reagents.get(*id).name.clone())
        .unwrap_or_else(|| recipe.key.clone())
}

/// The headings a recipe files under in the reference book.
///
/// Same rule as [`product_name`]: a recipe *is* the thing it makes, so it is
/// filed wherever that reagent says it belongs. A reagent may name several —
/// tricordrazine appears under all four medical headings — and
/// `every_reaction_files_under_a_heading` guarantees this is never empty.
pub fn reaction_categories(db: &ChemDb, reaction: ReactionId) -> &[Category] {
    db.reactions
        .get(reaction)
        .products
        .first()
        .map(|(id, _)| db.reagents.get(*id).categories.as_slice())
        .unwrap_or_default()
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

        // Arithrazine is two steps out and must NOT appear until hyronalin is
        // learned. Phlogiston likewise, because it needs sulphuric acid first —
        // which is the check that matters most here, since it is the one recipe
        // that can hurt you. Of the recipes added since, `smoke_powder` is the
        // one to watch: it is one catalyst away from `smoke` and must wait for
        // stabilizing agent. `krokodil`/`methamphetamine`/`bath_salts` are
        // deliberately absent too — each needs a compound (`oil`, `ammonia`,
        // `space_drugs`) first, unlike the other three Illicit reagents below,
        // which are one base-reagent mix away like anything else this early.
        assert_eq!(
            frontier,
            vec![
                "ammonia",
                "bicaridine",
                "chloral_hydrate",
                "chlorine_trifluoride",
                "dermaline",
                "dexalin",
                "flash_powder",
                "hooch",
                "hydrogen_peroxide",
                "hyperzine",
                "hyronalin",
                "ice",
                "mannitol",
                "mindbreaker_toxin",
                "oil",
                "potassium_iodide",
                "smoke",
                "sodium_chloride",
                "space_drugs",
                "stabilizing_agent",
                "sulphuric_acid",
                "synaptizine",
                "thermite",
                "tricordrazine",
                "unstable_mutagen",
                "zombie_powder",
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
    fn hints_must_be_paid_for() {
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let bicaridine = data.reactions.find("bicaridine").unwrap().id;

        assert_eq!(knowledge.visible_hints(&data, bicaridine).len(), FREE_HINTS);
        assert!(
            !knowledge.buy_hint(&data, bicaridine),
            "cannot buy a hint with no research"
        );

        knowledge.award_research(1);
        assert!(knowledge.buy_hint(&data, bicaridine));
        assert_eq!(knowledge.research_points, 0);
        assert_eq!(
            knowledge.visible_hints(&data, bicaridine).len(),
            FREE_HINTS + 1
        );
    }

    #[test]
    fn buying_past_the_last_hint_costs_nothing() {
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let bicaridine = data.reactions.find("bicaridine").unwrap().id;
        knowledge.award_research(50);

        while knowledge.hint_available(&data, bicaridine) {
            assert!(knowledge.buy_hint(&data, bicaridine));
        }
        let spent = 50 - knowledge.research_points;

        // The button must go dead rather than quietly draining points.
        assert!(!knowledge.buy_hint(&data, bicaridine));
        assert_eq!(50 - knowledge.research_points, spent);
        let all = &data.reactions.get(bicaridine).hints;
        assert_eq!(knowledge.visible_hints(&data, bicaridine).len(), all.len());
    }

    #[test]
    fn known_recipes_expose_no_hints_and_cannot_be_researched() {
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let inaprovaline = data.reactions.find("inaprovaline").unwrap().id;
        knowledge.award_research(5);

        assert!(knowledge.visible_hints(&data, inaprovaline).is_empty());
        assert!(!knowledge.hint_available(&data, inaprovaline));
        assert!(!knowledge.buy_hint(&data, inaprovaline));
        assert_eq!(knowledge.research_points, 5);
    }

    #[test]
    fn a_save_round_trips_through_reaction_keys() {
        let data = data();
        let mut original = Knowledge::new(&data);
        let bicaridine = data.reactions.find("bicaridine").unwrap().id;
        let dermaline = data.reactions.find("dermaline").unwrap().id;

        original.learn(bicaridine);
        original.award_research(4);
        assert!(original.buy_hint(&data, dermaline));

        let text = ron::ser::to_string(&original.to_save(&data)).unwrap();
        let restored = Knowledge::from_save(&data, ron::from_str(&text).unwrap());

        assert!(restored.is_known(bicaridine));
        assert_eq!(restored.known_count(), original.known_count());
        assert_eq!(restored.research_points, 3);
        assert_eq!(
            restored.visible_hints(&data, dermaline).len(),
            FREE_HINTS + 1
        );
    }

    #[test]
    fn a_save_survives_recipes_being_added_to_the_data_file() {
        // Saves store keys, not indices, so shifting the reaction list must
        // not turn "knows bicaridine" into "knows whatever is at slot 3".
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        knowledge.learn(data.reactions.find("tricordrazine").unwrap().id);
        let save = knowledge.to_save(&data);

        assert!(save.known.contains(&"tricordrazine".to_string()));
        assert!(!save.known.iter().any(|key| key.parse::<u32>().is_ok()));
    }
}
