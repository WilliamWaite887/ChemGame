//! Named save slots.
//!
//! One directory per save under `saves/`, each holding the two files the game
//! already wrote to the project root: `save.ron` (the notebook) and
//! `progress.ron` (the career). Keeping the *file* names is deliberate — it
//! makes moving an existing single-save game into a slot a file move rather
//! than a format change, and each module still owns its own format.
//!
//! The slot is chosen in the menu, before the lab is built, and never changes
//! during a session. A joining client has no slot at all: the host's notebook
//! and career are replicated, so writing them locally would hand the guest a
//! save file describing someone else's lab.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::arc::AntagId;

const SAVES_DIR: &str = "saves";
const KNOWLEDGE_FILE: &str = "save.ron";
const PROGRESS_FILE: &str = "progress.ron";

/// Where a pre-menu save ends up, and the slot the command-line flags use.
const DEFAULT_SLOT: &str = "Chemist";

/// The save this session reads and writes.
///
/// Absent means "do not touch the disk", which covers both a joining client and
/// the headless tests.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct SaveSlot {
    name: String,
}

impl SaveSlot {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// The slot used when the command line skips the menu.
    pub fn default_slot() -> Self {
        Self::new(DEFAULT_SLOT)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn dir(&self) -> PathBuf {
        Path::new(SAVES_DIR).join(&self.name)
    }

    pub fn knowledge_path(&self) -> PathBuf {
        self.dir().join(KNOWLEDGE_FILE)
    }

    pub fn progress_path(&self) -> PathBuf {
        self.dir().join(PROGRESS_FILE)
    }

    /// Writes the notebook.
    pub fn write_knowledge(&self, text: &str) {
        self.write(&self.knowledge_path(), text);
    }

    /// Writes the career.
    pub fn write_progress(&self, text: &str) {
        self.write(&self.progress_path(), text);
    }

    /// Creates the directory on the way past.
    ///
    /// Here rather than when the slot is chosen so that picking "New save" and
    /// quitting before anything happens leaves nothing behind — and so that
    /// choosing a save is a decision about state, not a disk write.
    fn write(&self, path: &Path, text: &str) {
        if let Err(error) = std::fs::create_dir_all(self.dir()) {
            warn!("could not create {}: {error}", self.dir().display());
            return;
        }
        // Fire and forget by design: a failed save warns and lets the shift
        // carry on rather than interrupting it.
        if let Err(error) = std::fs::write(path, text) {
            warn!("could not write {}: {error}", path.display());
        }
    }
}

/// What the menu shows about a save without loading it.
pub struct SlotSummary {
    pub name: String,
    pub delivered: u32,
    pub botched: u32,
    /// Recipes in the notebook. `None` if the notebook is missing or unreadable.
    pub known: Option<usize>,
    /// What this save's arc has come to, if it is safe to say. `None` for a
    /// save with no campaign *and* for one whose antagonist the player has not
    /// worked out yet — see [`ArcStanding`].
    pub arc: Option<ArcStanding>,
}

/// What the load list is allowed to say about a save's campaign.
///
/// Deliberately narrower than the campaign itself. A save whose antagonist is
/// still [`crate::arc::Reveal::Hidden`] produces `None` here: naming it in the
/// menu would hand the player, for free and before they have even loaded the
/// game, the exact answer the whole arc is built to make them work out.
pub struct ArcStanding {
    pub antag: AntagId,
    /// `None` while the arc is still live.
    pub won: Option<bool>,
}

impl ArcStanding {
    fn phrase(&self) -> String {
        match self.won {
            Some(true) => format!("{} thwarted", self.antag.label()),
            Some(false) => format!("{} won", self.antag.label()),
            None => format!("hunting {}", self.antag.label()),
        }
    }
}

impl SlotSummary {
    /// The line under the save's name in the load list.
    pub fn detail(&self) -> String {
        let recipes = match self.known {
            Some(1) => " · 1 recipe".to_string(),
            Some(count) => format!(" · {count} recipes"),
            None => String::new(),
        };
        let arc = match &self.arc {
            Some(standing) => format!(" · {}", standing.phrase()),
            None => String::new(),
        };
        format!(
            "{} delivered, {} botched{recipes}{arc}",
            self.delivered, self.botched
        )
    }
}

/// Every save on disk, most recently played first.
///
/// Ordered by modification time rather than name so the game you are actually
/// in the middle of is the top button, and because "Save 10" sorts before
/// "Save 2" every other way.
pub fn list_slots() -> Vec<SlotSummary> {
    let Ok(entries) = std::fs::read_dir(SAVES_DIR) else {
        // No directory yet is the normal first-run reading, not a problem.
        return Vec::new();
    };

    let mut found: Vec<(Option<std::time::SystemTime>, SlotSummary)> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .map(|name| {
            let slot = SaveSlot::new(name);
            let (delivered, botched) =
                crate::shift::progress_summary(&slot.progress_path()).unwrap_or((0, 0));
            let touched = [slot.progress_path(), slot.knowledge_path()]
                .iter()
                .filter_map(|path| std::fs::metadata(path).ok()?.modified().ok())
                .max();
            (
                touched,
                SlotSummary {
                    known: crate::knowledge::known_count_in(&slot.knowledge_path()),
                    arc: crate::shift::arc_standing(&slot.progress_path()),
                    name: slot.name,
                    delivered,
                    botched,
                },
            )
        })
        .collect();

    found.sort_by(|(a_time, a), (b_time, b)| {
        b_time.cmp(a_time).then_with(|| a.name.cmp(&b.name))
    });
    found.into_iter().map(|(_, summary)| summary).collect()
}

/// A name no existing save is using.
pub fn next_slot_name() -> String {
    let taken: HashSet<String> = list_slots().into_iter().map(|slot| slot.name).collect();
    if !taken.contains(DEFAULT_SLOT) {
        return DEFAULT_SLOT.to_string();
    }
    (2..)
        .map(|n| format!("Chemist {n}"))
        .find(|name| !taken.contains(name))
        .expect("an unused name exists in an unbounded sequence")
}

// ---------------------------------------------------------------------------
// Cross-save unlocks
// ---------------------------------------------------------------------------

/// Which main antagonists this machine has beaten.
///
/// Alongside the saves rather than inside one, for exactly the reason
/// [`last_host_path`] gives for the address file: this belongs to the player,
/// not to a career. Beating the Cult in one save is what unlocks playing *as*
/// the Cult in the next one, so a per-slot file would make the unlock
/// unreachable by design.
const CAMPAIGN_FILE: &str = "campaign.ron";

#[derive(Serialize, Deserialize, Default)]
struct CampaignUnlocks {
    #[serde(default)]
    thwarted: Vec<String>,
}

fn campaign_path() -> PathBuf {
    Path::new(SAVES_DIR).join(CAMPAIGN_FILE)
}

/// Every antagonist beaten on this machine, in no particular order.
///
/// Unknown keys are dropped rather than treated as an error: the file is
/// hand-editable like every other save in this game, and a typo should cost
/// one unlock, not the launch.
pub fn thwarted_antags() -> Vec<AntagId> {
    let Ok(text) = std::fs::read_to_string(campaign_path()) else {
        // No file yet is the normal first-run reading, not a problem.
        return Vec::new();
    };
    match ron::from_str::<CampaignUnlocks>(&text) {
        Ok(unlocks) => unlocks
            .thwarted
            .iter()
            .filter_map(|key| AntagId::from_key(key))
            .collect(),
        Err(error) => {
            warn!("ignoring unreadable {}: {error}", campaign_path().display());
            Vec::new()
        }
    }
}

/// Records an antagonist as beaten, if it is not already.
///
/// Re-reads the file rather than writing a cached list: a second copy of the
/// game, or the same player finishing two saves in one sitting, must not
/// silently drop the other's unlock.
pub fn record_thwarted(antag: AntagId) {
    let mut thwarted = thwarted_antags();
    if thwarted.contains(&antag) {
        return;
    }
    thwarted.push(antag);

    if let Err(error) = std::fs::create_dir_all(SAVES_DIR) {
        warn!("could not create {SAVES_DIR}: {error}");
        return;
    }
    let unlocks = CampaignUnlocks {
        thwarted: thwarted.iter().map(|id| id.key().to_string()).collect(),
    };
    let Ok(text) = ron::ser::to_string_pretty(&unlocks, default()) else {
        return;
    };
    if let Err(error) = std::fs::write(campaign_path(), text) {
        warn!("could not record the unlock: {error}");
        return;
    }
    info!("{} thwarted — antagonist runs unlocked", antag.label());
}

/// Where the last address joined is remembered.
///
/// Alongside the saves rather than in a slot: it belongs to this machine, not
/// to a career, and the whole point is that it survives starting a new game.
fn last_host_path() -> PathBuf {
    Path::new(SAVES_DIR).join("last_host.txt")
}

/// Remembers an address so rejoining the same lab is one keystroke.
///
/// Typing an IP with a mouse-locked game either side of it is the worst part
/// of joining, and on a home network it is the same address every time.
pub fn remember_host(address: &str) {
    if let Err(error) = std::fs::create_dir_all(SAVES_DIR) {
        warn!("could not create {SAVES_DIR}: {error}");
        return;
    }
    if let Err(error) = std::fs::write(last_host_path(), address) {
        warn!("could not remember the address: {error}");
    }
}

/// The last address joined, if there is one.
pub fn remembered_host() -> Option<String> {
    let text = std::fs::read_to_string(last_host_path()).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Moves a save written before there were slots into one.
///
/// Only runs when there is no `saves/` directory at all, so it cannot touch a
/// player who already has slots, and it cannot run twice. Without it the first
/// launch after this change looks like the career was wiped: the old files sit
/// in the project root where nothing reads them any more.
pub fn migrate_legacy_saves() {
    if Path::new(SAVES_DIR).exists() {
        return;
    }
    let legacy: Vec<&str> = [KNOWLEDGE_FILE, PROGRESS_FILE]
        .into_iter()
        .filter(|file| Path::new(file).exists())
        .collect();
    if legacy.is_empty() {
        return;
    }

    let slot = SaveSlot::default_slot();
    if let Err(error) = std::fs::create_dir_all(slot.dir()) {
        warn!("could not create {}: {error}", slot.dir().display());
        return;
    }
    for file in legacy {
        let to = slot.dir().join(file);
        // Copy-then-remove rather than `rename`, which fails across volumes —
        // and a save that is copied but not cleaned up still loads.
        if let Err(error) = std::fs::copy(file, &to) {
            warn!("could not move {file} into {}: {error}", slot.dir().display());
            continue;
        }
        if let Err(error) = std::fs::remove_file(file) {
            warn!("copied {file} into the save but could not remove it: {error}");
        }
    }
    info!("moved your existing save into '{}'", slot.name());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_keeps_its_two_files_together() {
        let slot = SaveSlot::new("Chemist 4");
        assert_eq!(
            slot.knowledge_path().parent(),
            slot.progress_path().parent(),
            "both files belong to the same save"
        );
        assert!(slot.knowledge_path().ends_with("Chemist 4/save.ron"));
        assert!(slot.progress_path().ends_with("Chemist 4/progress.ron"));
    }

    #[test]
    fn the_summary_line_reads_as_a_sentence() {
        let summary = SlotSummary {
            name: "Chemist".into(),
            delivered: 9,
            botched: 2,
            known: Some(9),
            arc: None,
        };
        assert_eq!(summary.detail(), "9 delivered, 2 botched · 9 recipes");

        // An unreadable or missing notebook says nothing rather than "0".
        let fresh = SlotSummary {
            name: "Chemist 2".into(),
            delivered: 0,
            botched: 0,
            known: None,
            arc: None,
        };
        assert_eq!(fresh.detail(), "0 delivered, 0 botched");

        // Singular, because "1 recipes" in a menu looks like a bug.
        let one = SlotSummary {
            name: "Chemist 3".into(),
            delivered: 5,
            botched: 1,
            known: Some(1),
            arc: None,
        };
        assert_eq!(one.detail(), "5 delivered, 1 botched · 1 recipe");
    }

    #[test]
    fn a_resolved_arc_is_named_in_the_load_list() {
        let won = SlotSummary {
            name: "Chemist".into(),
            delivered: 40,
            botched: 3,
            known: Some(12),
            arc: Some(ArcStanding {
                antag: AntagId::Cult,
                won: Some(true),
            }),
        };
        assert_eq!(
            won.detail(),
            "40 delivered, 3 botched · 12 recipes · the Cult thwarted"
        );

        let lost = SlotSummary {
            name: "Chemist 2".into(),
            delivered: 12,
            botched: 9,
            known: Some(4),
            arc: Some(ArcStanding {
                antag: AntagId::Blob,
                won: Some(false),
            }),
        };
        assert_eq!(
            lost.detail(),
            "12 delivered, 9 botched · 4 recipes · the Blob won"
        );
    }

    #[test]
    fn a_live_arc_reads_as_a_hunt_rather_than_a_result() {
        let live = SlotSummary {
            name: "Chemist".into(),
            delivered: 20,
            botched: 1,
            known: Some(8),
            arc: Some(ArcStanding {
                antag: AntagId::Spy,
                won: None,
            }),
        };
        assert_eq!(
            live.detail(),
            "20 delivered, 1 botched · 8 recipes · hunting the Syndicate"
        );
    }

    #[test]
    fn an_unknown_antagonist_key_costs_one_unlock_not_the_launch() {
        // The file is hand-editable like every other save in this game.
        let unlocks: CampaignUnlocks =
            ron::from_str(r#"(thwarted: ["Cult", "Nobody", "Blob"])"#).unwrap();
        let ids: Vec<AntagId> = unlocks
            .thwarted
            .iter()
            .filter_map(|key| AntagId::from_key(key))
            .collect();
        assert_eq!(ids, vec![AntagId::Cult, AntagId::Blob]);
    }
}
