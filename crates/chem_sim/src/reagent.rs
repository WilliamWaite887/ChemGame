//! Reagent definitions and the registry that interns them.
//!
//! Data files refer to reagents by string name; everything at runtime uses
//! `ReagentId`, which is a plain index. The conversion happens once, at load.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::effect::{ReagentEffect, WorldEffect};
use crate::units::{Kelvin, Units};

fn neutral_ph() -> f32 {
    7.0
}

/// How an energetic reagent behaves when heated past its activation point.
///
/// Kept on the reagent rather than on a particular container so every future
/// delivery form (beaker, charge, foam payload) uses one authoritative profile.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ExplosiveProfile {
    /// Energy contributed by each unit present.
    pub strength: f32,
    /// Flat contribution from the most energetic compound in a mixture.
    #[serde(default)]
    pub modifier: f32,
    /// Temperature at which the stable compound initiates.
    pub activation_temp: Kelvin,
}

/// Units a body works through per metabolism tick when a reagent does not say
/// otherwise. /tg/station's default, and the rate every medicine is balanced
/// against.
pub const DEFAULT_METABOLISM: Units = Units::from_raw(40);

/// What a reagent is *for*.
///
/// The reference book is organised by this, and a reagent may sit under more
/// than one heading: tricordrazine genuinely does treat all four damage types,
/// and a book that files it under one of them is lying to the player.
///
/// Deliberately declared in the data rather than derived from `effects`.
/// Derivation looks tempting and breaks immediately — hyronalin's radiation
/// work is a `Counter` rather than a `Heal`, a precursor has no effects at all,
/// and by effect alone thermite is indistinguishable from a medicine that has
/// gone wrong.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Category {
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
    /// Recreational, and never legitimately requested. Distinct from
    /// `Poisons` on purpose: a raid's contraband check needs an unambiguous
    /// "is this illegal" test, and stacking that test onto a category that
    /// also holds real, legitimately-ordered medicine (hyperzine,
    /// synaptizine) would be simply wrong.
    Illicit,
}

impl Category {
    /// Every heading, in the order the book lists them: the four damage types
    /// first, then what a chemist reaches for less often, then the things that
    /// are not medicine at all.
    pub const ALL: [Category; 11] = [
        Category::Trauma,
        Category::Burns,
        Category::Antitoxins,
        Category::Airloss,
        Category::Radiation,
        Category::Stimulants,
        Category::Poisons,
        Category::Pyrotechnics,
        Category::Precursors,
        Category::Utility,
        Category::Illicit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Trauma => "Trauma",
            Category::Burns => "Burns",
            Category::Antitoxins => "Antitoxins",
            Category::Airloss => "Airloss",
            Category::Radiation => "Radiation",
            Category::Stimulants => "Stimulants & Sedatives",
            Category::Poisons => "Poisons",
            Category::Pyrotechnics => "Pyrotechnics",
            Category::Precursors => "Precursors",
            Category::Utility => "Utility",
            Category::Illicit => "Illicit",
        }
    }

    /// The line under the heading, telling a chemist what they are looking at.
    pub fn blurb(self) -> &'static str {
        match self {
            Category::Trauma => {
                "Physical injury. Bleeding, breaks, and what security leave behind."
            }
            Category::Burns => "Heat, plasma fires and welding accidents.",
            Category::Antitoxins => "Poisoning, and getting it back out again.",
            Category::Airloss => "Suffocation, breaches, and holding a crashing patient steady.",
            Category::Radiation => "Containment leaks. The one thing that keeps hurting you.",
            Category::Stimulants => "Nothing is healed. Everything is felt differently.",
            Category::Poisons => "Nobody will ever order these. Handle them anyway.",
            Category::Pyrotechnics => "Fire, smoke and pressure. Not for people.",
            Category::Precursors => "Nothing on its own. Everything downstream needs it.",
            Category::Utility => "Neither medicine nor weapon.",
            Category::Illicit => {
                "Nobody will ever legitimately order these. Somebody might anyway."
            }
        }
    }

    /// A noun phrase naming what a crew member wants without naming a
    /// chemical — how a legitimate order describes itself now that it asks
    /// for a kind of treatment rather than one exact answer. No leading
    /// article, so it drops cleanly into "hand over {amount}u {phrase}", "I
    /// asked for {phrase}" and "no {phrase} in this at all" alike.
    ///
    /// Only the six legitimately-orderable categories are ever actually shown
    /// this way — the rest exist so the match stays exhaustive, matching
    /// `.label()`/`.blurb()`.
    pub fn want_phrase(self) -> &'static str {
        match self {
            Category::Trauma => "trauma treatment",
            Category::Burns => "burn treatment",
            Category::Antitoxins => "antitoxin",
            Category::Airloss => "airloss treatment",
            Category::Radiation => "radiation treatment",
            Category::Stimulants => "something from the stimulant cabinet",
            Category::Poisons => "poison",
            Category::Pyrotechnics => "pyrotechnic",
            Category::Precursors => "precursor",
            Category::Utility => "utility chemical",
            Category::Illicit => "illicit substance",
        }
    }

    /// Whether legitimate crew could ever ask for this category. Only
    /// `Trauma`/`Burns`/`Antitoxins`/`Airloss`/`Radiation`/`Stimulants` —
    /// everything else is either not medicine (`Precursors`, `Utility`,
    /// `Pyrotechnics`) or medicine nobody admits to needing
    /// (`Poisons`, `Illicit`, the antagonist thread's exclusive domain).
    ///
    /// The one place this matters structurally rather than just for content
    /// authoring: it is how the order-queue HUD decides whether to show a
    /// want-phrase or a reagent's real name *without* ever querying whether
    /// an order is secretly an antagonist's — see `IllicitOrder` in
    /// `src/orders/mod.rs`. Every antagonist reagent is `Illicit`, so this
    /// test is equivalent in practice and needs no forbidden marker query.
    pub fn is_legitimately_orderable(self) -> bool {
        matches!(
            self,
            Category::Trauma
                | Category::Burns
                | Category::Antitoxins
                | Category::Airloss
                | Category::Radiation
                | Category::Stimulants
        )
    }
}

/// What kind of substance a reagent physically is: metal, gas, halogen, and
/// so on. Separate from [`Category`] on purpose — `Category` answers "what
/// does this treat" (book-organising, multi-valued: tricordrazine really
/// does treat four things at once), while `ChemFamily` answers "what *is*
/// this" (a single fact, true even for raw elements that never get a book
/// entry at all).
///
/// Exists so the base ChemMaster 5000's ~30 stock chemicals can be grouped under a
/// heading instead of forced into one long alphabetised list. Only
/// `dispensable` reagents are expected to name one explicitly — everything
/// else defaults to `Unclassified` rather than demanding an editor tag all
/// the crafted/medicinal reagents before this compiles. See
/// `every_dispensable_reagent_names_a_chemical_family`, which enforces the
/// boundary that actually matters.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub enum ChemFamily {
    Metal,
    GasNonmetal,
    Halogen,
    Organic,
    AcidBaseBuffer,
    Radioactive,
    Industrial,
    #[default]
    Unclassified,
}

impl ChemFamily {
    /// Declared in the order the base stock grid lists it: chemically
    /// "simple" groups first, mixed/compound stock after, the rare and the
    /// unclassified last.
    pub const ALL: [ChemFamily; 8] = [
        ChemFamily::Metal,
        ChemFamily::GasNonmetal,
        ChemFamily::Halogen,
        ChemFamily::Organic,
        ChemFamily::AcidBaseBuffer,
        ChemFamily::Radioactive,
        ChemFamily::Industrial,
        ChemFamily::Unclassified,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ChemFamily::Metal => "METALS",
            ChemFamily::GasNonmetal => "GASES & NONMETALS",
            ChemFamily::Halogen => "HALOGENS",
            ChemFamily::Organic => "COMPOUNDS",
            ChemFamily::AcidBaseBuffer => "BUFFERS",
            ChemFamily::Radioactive => "RADIOACTIVE",
            ChemFamily::Industrial => "INDUSTRIAL",
            ChemFamily::Unclassified => "OTHER",
        }
    }
}

/// An interned reagent handle. Cheap to copy, compare and sort.
///
/// Serialisable because solutions cross the wire in co-op. The id is a
/// position in the loaded reagent list, so both ends must agree on the data
/// files. The game layer includes the catalog in its LAN/Steam compatibility
/// fingerprint.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ReagentId(pub u32);

impl ReagentId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A reagent as written in `assets/data/reagents.ron`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReagentDef {
    #[serde(default)]
    pub material: crate::material::MaterialProperties,
    /// Stable identifier used by reactions and save files.
    pub id: String,
    /// Name shown to the player.
    pub name: String,
    /// Liquid colour, used to tint the beaker contents.
    pub color: [f32; 3],
    /// Gameplay pH used for reaction control and analyzer readouts. This is a
    /// deliberately approachable model rather than a molarity simulation.
    #[serde(default = "neutral_ph")]
    pub ph: f32,
    /// Energetic profile, if heating this reagent can initiate a blast.
    #[serde(default)]
    pub explosive: Option<ExplosiveProfile>,
    /// Controlled material even when it is not filed as an illicit drug.
    #[serde(default)]
    pub controlled: bool,
    /// A plot material whose supply choice belongs to the player alone.
    ///
    /// Distinct from both `controlled` (Security cares) and `Category::Illicit`
    /// (never legitimately requested), because neither of those means what this
    /// does: **no NPC job, covert action, incident generator, shop, load hook,
    /// or campaign step may ever spawn this for NPC use.** The only way an NPC
    /// holds one is a physical batch a player handed over, and a covert action
    /// needing it scores zero when no such stock remains.
    ///
    /// Deliberately narrow. Dangerous chemicals that arise naturally from Botany
    /// or ordinary station processes stay available through those real sources;
    /// tagging them here would remove player agency rather than protect it.
    #[serde(default)]
    pub player_only: bool,
    /// Optional related inverse form produced by a reaction-quality branch.
    #[serde(default)]
    pub inverse: Option<String>,
    /// One-way HPLC recovery target. Only failed inverse products set this;
    /// the useful paired reagent may still name `inverse` for documentation.
    #[serde(default)]
    pub recovers_to: Option<String>,
    /// Dose above which this does harm rather than good. `None` means safe at
    /// any dose.
    #[serde(default)]
    pub overdose: Option<Units>,
    /// Whether the base ChemMaster 5000 can produce it directly.
    #[serde(default)]
    pub dispensable: bool,
    /// The dispenser upgrade tier this reagent requires, `0` meaning
    /// available from the start (or not `dispensable` at all, where this is
    /// simply unused). `chem_sim` has no opinion on what "unlocked" means —
    /// that state lives in `Knowledge` on the game side, the same split
    /// `Category`'s book-organising role already draws. See
    /// `Knowledge::upgrade_dispenser`.
    #[serde(default)]
    pub tier: u32,
    /// Whether it comes out of ground produce rather than the ChemMaster 5000.
    ///
    /// The sibling of `dispensable`: both mean "obtainable without a
    /// reaction", which is what stops the reachability guardrail flagging it
    /// as a dead end in the recipe graph. What does the grinding is the game's
    /// business, not this crate's.
    #[serde(default)]
    pub from_produce: bool,
    /// What this treats, shown in the reference book even while the recipe is
    /// still locked — a chemist knows what bicaridine is for even if they have
    /// forgotten how to make it.
    #[serde(default)]
    pub treats: Option<String>,
    /// Which headings this files under in the reference book. Empty for raw
    /// materials, which never get an entry; required for anything craftable,
    /// which `every_reaction_files_under_a_heading` enforces.
    #[serde(default)]
    pub categories: Vec<Category>,
    /// What this substance physically is, independent of what it treats —
    /// see [`ChemFamily`]. Only reagents that need to render grouped (today:
    /// the dispensable ~30) are expected to set this explicitly.
    #[serde(default)]
    pub family: ChemFamily,
    /// How good a treatment this is within its category, `0` for anything
    /// never meant to satisfy an order. Only meaningful for reagents in
    /// `Category::Trauma/Burns/Antitoxins/Airloss/Radiation/Stimulants` — the
    /// six categories a legitimate order can ever require — where a
    /// higher-potency delivery earns more department favor than the bare
    /// minimum. See `orders::reputation_delta`.
    #[serde(default)]
    pub potency: u32,

    // ---- Body effects -----------------------------------------------------
    //
    // Every field below is `#[serde(default)]`, which is what let this land
    // without touching a line of `chem.reagents.ron`: a reagent that says
    // nothing about bodies simply passes through one unchanged.
    /// Units a body works through per tick. `None` means
    /// [`DEFAULT_METABOLISM`]. Sugar burns off at 2.0, ethanol lingers at 0.2.
    #[serde(default)]
    pub metabolism: Option<Units>,
    /// What it does each tick while it is in a bloodstream.
    #[serde(default)]
    pub effects: Vec<ReagentEffect>,
    /// Specific reagents removed from the bloodstream each tick. Unlike the
    /// broad harmful/medicine purge effects, this records authored antidote
    /// and counter-drug relationships without deleting unrelated chemistry.
    #[serde(default)]
    pub targeted_purges: Vec<(String, Units)>,
    /// Applied **in addition to** `effects` once the dose passes `overdose`.
    ///
    /// Stacking rather than replacing is what makes an overdose read the way it
    /// should: the medicine is still working, it is just also hurting you now.
    #[serde(default)]
    pub overdose_effects: Vec<ReagentEffect>,
    /// A second, worse overdose tier, stacking on top of the first.
    #[serde(default)]
    pub critical_overdose: Option<Units>,
    #[serde(default)]
    pub critical_effects: Vec<ReagentEffect>,
    /// Effects applied once, when the last active or digesting amount of this
    /// reagent leaves the body. Used for stimulant crashes and comedowns.
    #[serde(default)]
    pub after_effects: Vec<ReagentEffect>,
    /// Declarative effects produced when this reagent is released into the
    /// station. The engine layer owns spatial queries and puddle entities;
    /// `chem_sim` owns the portable, hot-reloadable profile.
    #[serde(default)]
    pub world_effects: Vec<WorldEffect>,
    /// Explicitly records that an otherwise effect-free crafted reagent is
    /// inert on purpose rather than unfinished content.
    #[serde(default)]
    pub intentionally_inert: bool,
    /// Above this it leaves the solution as gas. Plasma at 323.15K.
    ///
    /// Note for data files: RON will not coerce an integer into this `f32`.
    /// Write `Some((323.15))`, never `Some((323))`.
    #[serde(default)]
    pub boils_at: Option<Kelvin>,
    /// How readily repeated doses build a habit, `0.0` for anything that never
    /// does. Read by the station layer (`src/addiction`), never by this crate:
    /// addiction is not something that happens on a metabolism tick, it is a
    /// fact about a *person aboard the station*, tracked across visits long
    /// after any particular body has left the lab. `chem_sim` only carries the
    /// number so it lives next to the reagent it describes.
    #[serde(default)]
    pub addictive: f64,
}

/// A loaded reagent.
#[derive(Clone, Debug)]
pub struct Reagent {
    pub material: crate::material::MaterialProperties,
    pub id: ReagentId,
    pub key: String,
    pub name: String,
    pub color: [f32; 3],
    pub ph: f32,
    pub explosive: Option<ExplosiveProfile>,
    pub controlled: bool,
    /// See [`ReagentDef::player_only`]. Only a player-supplied physical batch
    /// can ever put this in NPC hands.
    pub player_only: bool,
    pub inverse: Option<String>,
    pub recovers_to: Option<String>,
    pub overdose: Option<Units>,
    pub dispensable: bool,
    pub tier: u32,
    pub from_produce: bool,
    pub treats: Option<String>,
    pub categories: Vec<Category>,
    pub family: ChemFamily,
    pub potency: u32,
    pub metabolism: Option<Units>,
    pub effects: Vec<ReagentEffect>,
    pub targeted_purges: Vec<(String, Units)>,
    pub overdose_effects: Vec<ReagentEffect>,
    pub critical_overdose: Option<Units>,
    pub critical_effects: Vec<ReagentEffect>,
    pub after_effects: Vec<ReagentEffect>,
    pub world_effects: Vec<WorldEffect>,
    pub intentionally_inert: bool,
    pub boils_at: Option<Kelvin>,
    /// See [`ReagentDef::addictive`].
    pub addictive: f64,
}

impl Reagent {
    /// How fast a body works through this, with the default filled in.
    pub fn rate(&self) -> Units {
        self.metabolism.unwrap_or(DEFAULT_METABOLISM)
    }

    /// Whether taking this can hurt you at any dose.
    ///
    /// True for anything with a harmful effect *or* an overdose threshold — a
    /// medicine you can overdose on is not safe to hand someone unasked.
    pub fn is_harmful(&self) -> bool {
        self.material.water_reactive.is_some()
            || self.overdose.is_some()
            || self
                .effects
                .iter()
                .chain(&self.overdose_effects)
                .chain(&self.critical_effects)
                .chain(&self.after_effects)
                .any(|effect| effect.is_harmful())
            || !self.targeted_purges.is_empty()
            || self.world_effects.iter().any(|effect| effect.is_harmful())
    }

    /// Whether this is meant to be used *on the station* rather than on a
    /// person — a cleaner, a solvent, a foam.
    ///
    /// True when the reagent does something to the world and nothing good to a
    /// body. Space cleaner is the archetype: it scrubs a spill, and the only
    /// thing it does to whoever swallows it is a toxin tick.
    ///
    /// This exists because a delivery has to choose a route, and "drink it"
    /// is the wrong answer for a bottle of cleaner no matter who asked for it.
    /// Deliberately structural rather than a category check — `Utility` is a
    /// reference-book heading an author picks, while this is a fact about the
    /// effects the reagent actually carries.
    pub fn is_for_the_station_not_a_body(&self) -> bool {
        !self.world_effects.is_empty()
            && !self
                .effects
                .iter()
                .chain(&self.overdose_effects)
                .chain(&self.critical_effects)
                .chain(&self.after_effects)
                .any(|effect| !effect.is_harmful())
    }

    /// Whether this reagent's body effects are harmful at the supplied active
    /// volume. Unlike [`Self::is_harmful`], a medicine's mere ability to
    /// overdose does not make a therapeutic dose a purge target.
    pub fn is_harmful_at(&self, volume: Units) -> bool {
        self.material.water_reactive.is_some()
            || self.effects.iter().any(|effect| effect.is_harmful())
            || self.after_effects.iter().any(|effect| effect.is_harmful())
            || !self.targeted_purges.is_empty()
            || matches!(self.overdose, Some(threshold) if volume > threshold)
                && self
                    .overdose_effects
                    .iter()
                    .any(|effect| effect.is_harmful())
            || matches!(self.critical_overdose, Some(threshold) if volume > threshold)
                && self
                    .critical_effects
                    .iter()
                    .any(|effect| effect.is_harmful())
    }
}

/// Owns every known reagent and maps names to ids.
#[derive(Clone, Debug, Default)]
pub struct ReagentRegistry {
    reagents: Vec<Reagent>,
    by_key: HashMap<String, ReagentId>,
}

impl ReagentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a reagent, or returns the existing id if the key is already
    /// known (in which case the later definition is ignored).
    pub fn insert(&mut self, def: ReagentDef) -> ReagentId {
        if let Some(&existing) = self.by_key.get(&def.id) {
            return existing;
        }
        let id = ReagentId(self.reagents.len() as u32);
        self.by_key.insert(def.id.clone(), id);
        self.reagents.push(Reagent {
            material: def.material,
            id,
            key: def.id,
            name: def.name,
            color: def.color,
            ph: def.ph.clamp(0.0, 14.0),
            explosive: def.explosive,
            controlled: def.controlled,
            player_only: def.player_only,
            inverse: def.inverse,
            recovers_to: def.recovers_to,
            overdose: def.overdose,
            dispensable: def.dispensable,
            tier: def.tier,
            from_produce: def.from_produce,
            treats: def.treats,
            categories: def.categories,
            family: def.family,
            potency: def.potency,
            metabolism: def.metabolism,
            effects: def.effects,
            targeted_purges: def.targeted_purges,
            overdose_effects: def.overdose_effects,
            critical_overdose: def.critical_overdose,
            critical_effects: def.critical_effects,
            after_effects: def.after_effects,
            world_effects: def.world_effects,
            intentionally_inert: def.intentionally_inert,
            boils_at: def.boils_at,
            addictive: def.addictive,
        });
        id
    }

    pub fn id_of(&self, key: &str) -> Option<ReagentId> {
        self.by_key.get(key).copied()
    }

    pub fn get(&self, id: ReagentId) -> &Reagent {
        &self.reagents[id.index()]
    }

    pub fn len(&self) -> usize {
        self.reagents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reagents.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Reagent> {
        self.reagents.iter()
    }

    /// Everything the base ChemMaster 5000 can produce.
    pub fn dispensable(&self) -> impl Iterator<Item = &Reagent> {
        self.reagents.iter().filter(|r| r.dispensable)
    }

    /// Everything obtainable without running a reaction — the roots of the
    /// recipe graph. Anything outside this set and not produced by some
    /// reaction is unreachable.
    pub fn raw(&self) -> impl Iterator<Item = &Reagent> {
        self.reagents
            .iter()
            .filter(|r| r.dispensable || r.from_produce)
    }
}
