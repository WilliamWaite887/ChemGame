//! Reagent definitions and the registry that interns them.
//!
//! Data files refer to reagents by string name; everything at runtime uses
//! `ReagentId`, which is a plain index. The conversion happens once, at load.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::effect::{ReagentEffect, WorldEffect};
use crate::units::{Kelvin, Units};

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

/// An interned reagent handle. Cheap to copy, compare and sort.
///
/// Serialisable because solutions cross the wire in co-op. The id is a
/// position in the loaded reagent list, so both ends must agree on the data
/// files — which is enforced by replicon's protocol hash, not by us.
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
    /// Stable identifier used by reactions and save files.
    pub id: String,
    /// Name shown to the player.
    pub name: String,
    /// Liquid colour, used to tint the beaker contents.
    pub color: [f32; 3],
    /// Dose above which this does harm rather than good. `None` means safe at
    /// any dose.
    #[serde(default)]
    pub overdose: Option<Units>,
    /// Whether the base chemical dispenser can produce it directly.
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
    /// Whether it comes out of ground produce rather than the dispenser.
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
    pub id: ReagentId,
    pub key: String,
    pub name: String,
    pub color: [f32; 3],
    pub overdose: Option<Units>,
    pub dispensable: bool,
    pub tier: u32,
    pub from_produce: bool,
    pub treats: Option<String>,
    pub categories: Vec<Category>,
    pub potency: u32,
    pub metabolism: Option<Units>,
    pub effects: Vec<ReagentEffect>,
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
        self.overdose.is_some()
            || self
                .effects
                .iter()
                .chain(&self.overdose_effects)
                .chain(&self.critical_effects)
                .chain(&self.after_effects)
                .any(|effect| effect.is_harmful())
            || self.world_effects.iter().any(|effect| effect.is_harmful())
    }

    /// Whether this reagent's body effects are harmful at the supplied active
    /// volume. Unlike [`Self::is_harmful`], a medicine's mere ability to
    /// overdose does not make a therapeutic dose a purge target.
    pub fn is_harmful_at(&self, volume: Units) -> bool {
        self.effects.iter().any(|effect| effect.is_harmful())
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
            id,
            key: def.id,
            name: def.name,
            color: def.color,
            overdose: def.overdose,
            dispensable: def.dispensable,
            tier: def.tier,
            from_produce: def.from_produce,
            treats: def.treats,
            categories: def.categories,
            potency: def.potency,
            metabolism: def.metabolism,
            effects: def.effects,
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

    /// Everything the base dispenser can produce.
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
