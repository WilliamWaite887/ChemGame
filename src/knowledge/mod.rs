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
/// Research points a hint costs. Every recipe's last hint is the literal
/// recipe, so this is what paces "deduction over a small candidate set" into
/// actually being deduction rather than a two-delivery unlock: a typical
/// 3-hint recipe now costs 6 points (≈6 deliveries, or three purchased
/// grants) to fully reveal instead of 2. `FREE_HINTS` stays untouched — the
/// one free hint is what bootstraps the very first reasoned guess. Raised
/// from 2 (M11) alongside the reagent unlock-cost tripling, so hints and
/// reagent unlocks compete meaningfully for the same research points instead
/// of hints being nearly free by comparison.
pub const HINT_COST: u32 = 3;
/// Points earned for a clean delivery of the weakest chemical that could
/// satisfy the order. See [`research_for_delivery`] for what a better one
/// pays.
pub const RESEARCH_PER_SUCCESS: u32 = 1;

/// Research a clean delivery of a reagent with this `potency` is worth.
///
/// Scaled by the same authored `potency` that already decides department
/// favor, on the same `saturating_sub(1)` convention `orders::reputation_delta`
/// uses: the weakest member of a category pays exactly what every delivery
/// used to, and a stronger answer pays more (arithrazine 3 → 3 points, against
/// hyronalin's 1). Research and standing agreeing on what "better" means is
/// the point — a chemist who reaches past the bare minimum should not have to
/// pick which currency it earns them. Anything with no potency at all — an
/// illicit reagent, a precursor — still pays the flat
/// [`RESEARCH_PER_SUCCESS`].
pub fn research_for_delivery(potency: u32) -> u32 {
    RESEARCH_PER_SUCCESS + potency.saturating_sub(1)
}

/// Research points to upgrade the dispenser from tier `N` to `N+1`, indexed
/// by `N` (index 0 = tier 0→1, etc.). Each entry is the sum of what the
/// reagents in that tier used to cost individually under the old
/// per-reagent unlock economy (M11) — the total to fully upgrade is still
/// 213, only the unit of purchase moved from one reagent to a whole tier.
/// See the tier comment block in `assets/data/chem.reagents.ron`.
pub const DISPENSER_TIER_COSTS: [u32; 7] = [24, 27, 48, 15, 54, 21, 24];

/// Distinct reagents a beaker can hold when it reacts and still count as a
/// focused experiment worth learning from — see
/// `chem_sim::ResolveReport::distinct_reagents` for why this works at all: a
/// legitimate build, however deep the chain, never has more present at once
/// than its widest single step's own reactant-plus-catalyst count (4, for
/// `smoke_powder`), because the resolver eagerly consumes intermediates as
/// they form. `5` gives that one unit of slack for a leftover catalyst from
/// an earlier step still sitting in the beaker, while comfortably refusing a
/// dump of most/all base reagents at once. Pinned by
/// `every_reactions_reactant_and_catalyst_count_fits_under_the_crowd_threshold`.
pub const CROWD_THRESHOLD: usize = 5;

use crate::saves::SaveSlot;

pub struct KnowledgePlugin;

impl Plugin for KnowledgePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<RecipeDiscovered>()
            .add_server_message::<KnowledgeSync>(Channel::Ordered)
            // Not `add_mapped_client_message` — it carries no `Entity` at
            // all (unlocking is career-wide, not tied to a machine), so
            // there is nothing for `MapEntities` to translate.
            .add_client_message::<UpgradeDispenserRequested>(Channel::Ordered)
            // Same reasoning as the line above: career-wide, carries no
            // `Entity`, so nothing for `MapEntities` to translate.
            .add_client_message::<BuyHintRequested>(Channel::Ordered)
            .add_systems(OnEnter(AppState::Playing), initialise_knowledge)
            .add_systems(
                Update,
                (
                    // The shared notebook belongs to the lab, so the server
                    // owns it and only the server writes the save file.
                    (
                        handle_dispenser_upgrade,
                        handle_hint_purchase,
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

/// A client asking to upgrade the dispenser to its next tier. Deliberately
/// not tied to a machine — the upgrade is a career-wide upgrade, not
/// something one specific dispenser owns, the same way [`RecipeDiscovered`]
/// and buying a hint aren't either. Carries no payload: there is only ever
/// one next tier. Like [`BuyHintRequested`], this crosses the network properly:
/// the server is the only one that ever calls
/// [`Knowledge::upgrade_dispenser`], via [`handle_dispenser_upgrade`], so a
/// joining client's purchase is never quietly overwritten by the next
/// [`KnowledgeSync`].
#[derive(Message, Serialize, Deserialize)]
pub struct UpgradeDispenserRequested;

fn handle_dispenser_upgrade(
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<UpgradeDispenserRequested>>,
    mut knowledge: ResMut<Knowledge>,
) {
    for _ in requests.read() {
        knowledge.upgrade_dispenser(&db);
    }
}

/// A client asking to spend [`HINT_COST`] on the next hint for one recipe.
///
/// Career-wide like [`UpgradeDispenserRequested`], and for the same reason: the
/// notebook belongs to the lab, not to whoever happens to be clicking. This
/// button used to mutate the local `Knowledge` directly, which worked on the
/// host and did nothing at all for a guest — their purchase was silently
/// overwritten by the very next [`KnowledgeSync`], costing them the hint and
/// visibly refunding points that had never actually been spent. Routing it
/// through the authority the way every other career-wide purchase already does
/// is the whole fix.
#[derive(Message, Serialize, Deserialize)]
pub struct BuyHintRequested {
    pub reaction: ReactionId,
}

fn handle_hint_purchase(
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<BuyHintRequested>>,
    mut knowledge: ResMut<Knowledge>,
) {
    for request in requests.read() {
        // `buy_hint` re-checks affordability and availability itself, so an
        // out-of-date or hand-crafted request buys nothing rather than going
        // into debt — the same trust boundary every other client message here
        // sits behind.
        knowledge.buy_hint(&db, request.reaction);
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
    /// How many dispenser upgrades have been bought. `0` means only tier-0
    /// (free) reagents are available; see [`Self::dispensable`].
    dispenser_tier: u32,
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
            dispenser_tier: 0,
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
    ///
    /// The bare form, used when loading a save. Anything that represents a
    /// chemist actually working a recipe out should call [`Self::discover`]
    /// instead, or the discovery pays nothing.
    pub fn learn(&mut self, reaction: ReactionId) -> bool {
        let already = self.is_known(reaction);
        self.entries.insert(reaction, Entry::Known);
        !already
    }

    /// Research handed back for working a recipe out instead of buying it.
    ///
    /// Exactly what was left unspent on that recipe: every hint still
    /// unrevealed, at what it would have cost to reveal. Deducing a recipe at
    /// the bench — or reverse-engineering a sample in the analyzer — is
    /// therefore worth precisely what not having to buy the rest of its hints
    /// saved, and a chemist who bought their way most of the way there gets
    /// correspondingly little back. Discovery used to be worth nothing in
    /// research terms at all, which left the first dispenser tier 24 clean
    /// deliveries away with no way to shorten it by being good at the job.
    pub fn discovery_payback(&self, data: &ChemData, reaction: ReactionId) -> u32 {
        match self.entry(reaction) {
            Entry::Known => 0,
            Entry::Locked { hints_revealed } => {
                let hints = data.reactions.get(reaction).hints.len();
                hints.saturating_sub(hints_revealed) as u32 * HINT_COST
            }
        }
    }

    /// Learns a recipe the chemist worked out, and pays the discovery back.
    ///
    /// Returns the points awarded, or `None` if the recipe was already known —
    /// the caller needs both facts, and a bare `bool` plus a second lookup
    /// would let the two disagree about which recipe was news.
    pub fn discover(&mut self, data: &ChemData, reaction: ReactionId) -> Option<u32> {
        let payback = self.discovery_payback(data, reaction);
        if !self.learn(reaction) {
            return None;
        }
        self.award_research(payback);
        Some(payback)
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

    /// Every dispensable reagent the chemist can actually draw from right
    /// now — everything at or below the current dispenser tier. Not
    /// necessarily all of them.
    pub fn dispensable(&self, data: &ChemData) -> HashSet<ReagentId> {
        data.reagents
            .dispensable()
            .filter(|r| self.is_reagent_unlocked(data, r.id))
            .map(|r| r.id)
            .collect()
    }

    /// Whether the dispenser will actually hand this over right now.
    pub fn is_reagent_unlocked(&self, data: &ChemData, reagent: ReagentId) -> bool {
        data.reagents.get(reagent).tier <= self.dispenser_tier
    }

    /// The dispenser's current upgrade tier.
    pub fn dispenser_tier(&self) -> u32 {
        self.dispenser_tier
    }

    /// Research points to upgrade the dispenser to its next tier, or `None`
    /// if it is already at the highest tier.
    pub fn next_upgrade_cost(&self) -> Option<u32> {
        DISPENSER_TIER_COSTS
            .get(self.dispenser_tier as usize)
            .copied()
    }

    /// Spends research points to upgrade the dispenser to its next tier,
    /// unlocking every reagent in that tier at once. Returns false if
    /// already at the highest tier or the chemist cannot afford it — never
    /// partially spends.
    pub fn upgrade_dispenser(&mut self, _data: &ChemData) -> bool {
        let Some(cost) = self.next_upgrade_cost() else {
            return false;
        };
        if self.research_points < cost {
            return false;
        }
        self.research_points -= cost;
        self.dispenser_tier += 1;
        true
    }

    /// Every reagent the chemist can currently produce, plus the base reagents
    /// they can dispense.
    pub fn available_reagents(&self, data: &ChemData) -> HashSet<ReagentId> {
        let mut available: HashSet<ReagentId> = self.dispensable(data);

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
            dispenser_tier: self.dispenser_tier,
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
        knowledge.dispenser_tier = save.dispenser_tier;
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
    #[serde(default)]
    dispenser_tier: u32,
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

/// Records any reaction the chemist manages to cause — but only from a
/// focused experiment, not a shotgun dump.
///
/// Making the thing *is* the discovery — there is no separate "confirm" step,
/// because a chemist who has just watched a beaker turn the right colour knows
/// perfectly well what they did. That reasoning breaks down once the beaker
/// holds more than a handful of unrelated reagents at once: nothing was
/// *understood*, something just happened to be in the mix. The chemistry
/// itself is never withheld — reactants still consume, products still form,
/// hazards still fire — only the "you now know this" credit is, when
/// [`CROWD_THRESHOLD`] is exceeded.
fn learn_from_experiments(
    db: Res<ChemDb>,
    mut knowledge: ResMut<Knowledge>,
    mut fired: MessageReader<ReactionsFired>,
    mut discovered: MessageWriter<RecipeDiscovered>,
    mut radio: ResMut<RadioLog>,
) {
    for event in fired.read() {
        if event.distinct_reagents > CROWD_THRESHOLD {
            // Only worth a line if something was actually missed — a crowded
            // beaker that only ever touches recipes already known costs
            // nothing, so nagging about it would just be noise.
            if event.reactions.iter().any(|reaction| !knowledge.is_known(*reaction)) {
                radio.push(RadioEntry {
                    channel: "LAB".to_string(),
                    text: "Too much going on in that beaker to tell what did what.".to_string(),
                    good: false,
                });
            }
            continue;
        }
        for reaction in &event.reactions {
            let Some(payback) = knowledge.discover(&db, *reaction) else {
                continue;
            };
            let name = product_name(&db, *reaction);
            info!("recipe discovered: {name} (+{payback} research)");
            radio.push(RadioEntry {
                channel: "LAB".to_string(),
                // The points are said out loud. A payback the player never
                // sees arrive is indistinguishable from no payback at all,
                // and this is the reward for the part of the job the game is
                // actually about.
                text: if payback > 0 {
                    format!(
                        "Method for {name} written up in the reference book. \
                         Research credits it at {payback} points."
                    )
                } else {
                    format!("Method for {name} written up in the reference book.")
                },
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

    // -- dispenser tiers -----------------------------------------------------

    #[test]
    fn every_starting_ingredients_reagent_is_unlocked_free() {
        // Every base reagent the 3 starting recipes need must be tier 0, or a
        // fresh chemist could not even make what they're supposed to already
        // know. Derived from the data rather than hardcoded, so a future edit
        // to a starting recipe's own ingredients can't silently strand it
        // behind a lock.
        let data = data();
        for key in STARTING_RECIPES {
            let reaction = data.reactions.find(key).unwrap();
            for &(reagent, _) in reaction.reactants.iter().chain(reaction.catalysts.iter()) {
                assert_eq!(
                    data.reagents.get(reagent).tier,
                    0,
                    "'{}', needed by starting recipe '{key}', is locked",
                    data.reagents.get(reagent).key
                );
            }
        }
    }

    #[test]
    fn a_fresh_chemist_can_only_dispense_the_starting_reagents() {
        let data = data();
        let knowledge = Knowledge::new(&data);
        let dispensable_count = data.reagents.dispensable().count();
        let unlocked_count = knowledge.dispensable(&data).len();
        assert!(
            unlocked_count < dispensable_count,
            "some dispensable reagents must start locked, or the feature does nothing"
        );
        for reagent in knowledge.dispensable(&data) {
            assert_eq!(data.reagents.get(reagent).tier, 0);
        }
    }

    #[test]
    fn upgrading_spends_research_and_advances_one_tier_at_a_time() {
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let hydrogen = data.reagent("hydrogen"); // tier 1
        let cost = DISPENSER_TIER_COSTS[0];

        assert!(
            !knowledge.upgrade_dispenser(&data),
            "cannot upgrade with no research"
        );
        assert!(!knowledge.is_reagent_unlocked(&data, hydrogen));

        knowledge.award_research(cost);
        assert!(knowledge.upgrade_dispenser(&data));
        assert_eq!(knowledge.research_points, 0);
        assert_eq!(knowledge.dispenser_tier(), 1);
        assert!(knowledge.is_reagent_unlocked(&data, hydrogen));

        // The next tier is a separate purchase — this one didn't come free.
        knowledge.award_research(cost);
        assert!(!knowledge.upgrade_dispenser(&data), "tier 2 costs more than tier 1 did");
        assert_eq!(knowledge.research_points, cost);
    }

    #[test]
    fn upgrading_past_the_top_tier_does_nothing() {
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        knowledge.award_research(1000);
        for _ in 0..DISPENSER_TIER_COSTS.len() {
            assert!(knowledge.upgrade_dispenser(&data));
        }
        assert_eq!(knowledge.next_upgrade_cost(), None);
        let leftover = knowledge.research_points;
        assert!(!knowledge.upgrade_dispenser(&data));
        assert_eq!(knowledge.research_points, leftover, "nothing should have been spent");
    }

    #[test]
    fn a_dispenser_upgrade_survives_a_save_round_trip() {
        let data = data();
        let mut original = Knowledge::new(&data);
        let hydrogen = data.reagent("hydrogen");
        original.award_research(DISPENSER_TIER_COSTS[0]);
        assert!(original.upgrade_dispenser(&data));

        let text = ron::ser::to_string(&original.to_save(&data)).unwrap();
        let restored = Knowledge::from_save(&data, ron::from_str(&text).unwrap());

        assert!(restored.is_reagent_unlocked(&data, hydrogen));
        assert_eq!(restored.dispenser_tier(), 1);
    }

    fn learn_app() -> App {
        let data = data();
        let mut app = App::new();
        app.insert_resource(Knowledge::new(&data))
            .insert_resource(ChemDb(data))
            .init_resource::<RadioLog>()
            .add_message::<ReactionsFired>()
            .add_message::<RecipeDiscovered>()
            .add_systems(Update, learn_from_experiments);
        app
    }

    #[test]
    fn a_crowded_experiment_teaches_nothing_but_a_focused_one_does() {
        let mut app = learn_app();
        let bicaridine = app.world().resource::<ChemDb>().reactions.find("bicaridine").unwrap().id;
        let dexalin = app.world().resource::<ChemDb>().reactions.find("dexalin").unwrap().id;

        app.world_mut().write_message(ReactionsFired {
            reactions: vec![bicaridine],
            container: Entity::PLACEHOLDER,
            effects: Vec::new(),
            distinct_reagents: CROWD_THRESHOLD + 1,
        });
        app.update();
        assert!(
            !app.world().resource::<Knowledge>().is_known(bicaridine),
            "a beaker crowded past the threshold should teach nothing"
        );

        app.world_mut().write_message(ReactionsFired {
            reactions: vec![dexalin],
            container: Entity::PLACEHOLDER,
            effects: Vec::new(),
            distinct_reagents: CROWD_THRESHOLD,
        });
        app.update();
        assert!(
            app.world().resource::<Knowledge>().is_known(dexalin),
            "exactly at the threshold should still count as focused"
        );
    }

    #[test]
    fn every_reactions_reactant_and_catalyst_count_fits_under_the_crowd_threshold() {
        // The property `CROWD_THRESHOLD` actually depends on: no single
        // reaction should ever need more distinct reagents than the
        // threshold allows, or a chemist could never legitimately reach it
        // in one focused experiment. If a future recipe needs more, this is
        // the test that should fail — raise the threshold deliberately
        // rather than let discovery quietly go dead for one recipe.
        let data = data();
        for reaction in data.reactions.iter() {
            let count = reaction.reactants.len() + reaction.catalysts.len();
            assert!(
                count <= CROWD_THRESHOLD,
                "'{}' needs {count} distinct reagents at once, over the crowd threshold of {CROWD_THRESHOLD}",
                reaction.key
            );
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

        // Only 6 of the 23 dispensable reagents are unlocked at career start
        // (oxygen, carbon, sugar, silicon, nitrogen, potassium — exactly what
        // the 3 starting recipes need), so the frontier this early is much
        // smaller than the recipe graph alone would suggest: bicaridine
        // (inaprovaline + carbon) and tricordrazine (inaprovaline + dylovene)
        // are the only locked recipes buildable from starting knowledge and
        // unlocked reagents alone. Everything else needs at least one locked
        // reagent — dermaline needs phosphorus, hyronalin needs radium, and
        // so on — regardless of how close it looks in the recipe graph.
        // `every_starting_ingredients_reagent_is_unlocked_free` pins the
        // other half of this: the 6 that must never end up locked.
        assert_eq!(frontier, vec!["bicaridine", "tricordrazine"]);
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

        // Knowing the recipe and being able to make it right now are
        // different things: `available_reagents` only counts hyronalin
        // itself as available once *its* ingredients (radium, tier 5) are
        // unlocked too, and arithrazine additionally needs hydrogen (tier 1,
        // already covered by reaching tier 5). Neither is what this test is
        // actually about (frontier growth on learning, not the dispenser
        // tier economy), so max out the dispenser directly.
        knowledge.award_research(DISPENSER_TIER_COSTS.iter().sum());
        for _ in 0..DISPENSER_TIER_COSTS.len() {
            assert!(knowledge.upgrade_dispenser(&data));
        }

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

        knowledge.award_research(HINT_COST);
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
    fn working_a_recipe_out_pays_back_every_hint_that_was_never_bought() {
        // The whole point of the payback: deduce it at the bench and you keep
        // what the notebook would have charged to spell it out for you.
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let bicaridine = data.reactions.find("bicaridine").unwrap().id;
        let hints = data.reactions.get(bicaridine).hints.len();

        assert_eq!(
            knowledge.discover(&data, bicaridine),
            Some((hints - FREE_HINTS) as u32 * HINT_COST)
        );
        assert_eq!(
            knowledge.research_points,
            (hints - FREE_HINTS) as u32 * HINT_COST
        );
        assert!(knowledge.is_known(bicaridine));
    }

    #[test]
    fn a_recipe_half_bought_pays_back_only_what_is_left() {
        // Buying hints is spending the payback in advance, not stacking with
        // it — otherwise the cheapest route to research would be to buy every
        // hint and then discover the recipe anyway.
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let bicaridine = data.reactions.find("bicaridine").unwrap().id;

        knowledge.award_research(HINT_COST);
        assert!(knowledge.buy_hint(&data, bicaridine));
        assert_eq!(knowledge.research_points, 0);

        let hints = data.reactions.get(bicaridine).hints.len();
        let left = (hints - FREE_HINTS - 1) as u32 * HINT_COST;
        assert_eq!(knowledge.discover(&data, bicaridine), Some(left));
        assert_eq!(knowledge.research_points, left);
    }

    #[test]
    fn rediscovering_a_known_recipe_pays_nothing() {
        // A beaker that makes inaprovaline again on every batch must not be a
        // research fountain.
        let data = data();
        let mut knowledge = Knowledge::new(&data);
        let inaprovaline = data.reactions.find("inaprovaline").unwrap().id;

        assert_eq!(knowledge.discover(&data, inaprovaline), None);
        assert_eq!(knowledge.research_points, 0);

        let bicaridine = data.reactions.find("bicaridine").unwrap().id;
        knowledge.discover(&data, bicaridine);
        let banked = knowledge.research_points;
        assert_eq!(knowledge.discover(&data, bicaridine), None);
        assert_eq!(knowledge.research_points, banked, "paid once, not per batch");
    }

    #[test]
    fn a_stronger_chemical_pays_more_research_than_the_bare_minimum() {
        let data = data();
        let hyronalin = data.reagents.get(data.reagent("hyronalin")).potency;
        let arithrazine = data.reagents.get(data.reagent("arithrazine")).potency;

        // The weakest answer in a category still pays exactly what every
        // delivery used to, so nothing regressed by adding the scale.
        assert_eq!(research_for_delivery(hyronalin), RESEARCH_PER_SUCCESS);
        assert!(
            research_for_delivery(arithrazine) > research_for_delivery(hyronalin),
            "the harder anti-rad has to be worth reaching for"
        );
        // Illicit reagents and precursors carry no potency at all and must
        // still pay something, or an antagonist delivery would be free.
        assert_eq!(research_for_delivery(0), RESEARCH_PER_SUCCESS);
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
        assert_eq!(restored.research_points, 4 - HINT_COST);
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
