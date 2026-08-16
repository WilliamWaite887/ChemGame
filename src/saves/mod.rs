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
use std::io::Write;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::arc::AntagId;

/// The directory below `%LOCALAPPDATA%` used by release builds.
///
/// Steam Auto-Cloud can map this exact root (`WinAppDataLocal/ChemGame/saves`)
/// without ever trying to write into the installed game directory. The
/// relative fallback keeps command-line/test builds usable on platforms which
/// do not expose `LOCALAPPDATA`.
const SAVES_DIR: &str = "saves";
const APP_DATA_DIR: &str = "ChemGame";
const KNOWLEDGE_FILE: &str = "save.ron";
const PROGRESS_FILE: &str = "progress.ron";
const ENVELOPE_MAGIC: &[u8; 8] = b"CHEMSV01";
const TAG_BYTES: usize = 32;
const INTEGRITY_KEY_FILE: &str = ".integrity-key";

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
        saves_root().join(&self.name)
    }

    pub fn knowledge_path(&self) -> PathBuf {
        self.dir().join(KNOWLEDGE_FILE)
    }

    pub fn progress_path(&self) -> PathBuf {
        self.dir().join(PROGRESS_FILE)
    }

    /// Writes the notebook as a signed, recoverable save envelope.
    pub fn write_knowledge(&self, text: &str) {
        self.write(&self.knowledge_path(), text);
    }

    /// Writes the career.
    pub fn write_progress(&self, text: &str) {
        self.write(&self.progress_path(), text);
    }

    /// Creates the directory on the way past, signs the payload with this
    /// machine's private integrity key, and preserves the prior complete save
    /// as `.bak`. The payload remains RON *inside* the envelope so format
    /// migrations stay pleasant to write, but changing a number in a text
    /// editor no longer produces a valid save.
    ///
    /// Here rather than when the slot is chosen so that picking "New save" and
    /// quitting before anything happens leaves nothing behind — and so that
    /// choosing a save is a decision about state, not a disk write.
    fn write(&self, path: &Path, text: &str) {
        write_slot_text(path, text);
    }
}

/// Writes a signed save payload. Shared by slot-local files and the small
/// cross-save campaign file, so neither becomes the weak editable exception.
pub fn write_slot_text(path: &Path, text: &str) {
    let Some(parent) = path.parent() else {
        warn!(
            "could not determine a parent directory for {}",
            path.display()
        );
        return;
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        warn!("could not create {}: {error}", parent.display());
        return;
    }

    let key = match integrity_key() {
        Ok(key) => key,
        Err(error) => {
            warn!("could not prepare save integrity key: {error}");
            return;
        }
    };
    let bytes = encode_envelope(text.as_bytes(), &key);
    if let Err(error) = write_recoverable(path, &bytes) {
        warn!("could not write {}: {error}", path.display());
    }
}

/// Where all mutable career data lives. Keeping this under Local AppData is
/// important for Steam Cloud: Steam owns the install directory, while this is
/// a documented per-user data root that Auto-Cloud can sync safely.
pub fn saves_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|root| root.join(APP_DATA_DIR).join(SAVES_DIR))
        .unwrap_or_else(|| PathBuf::from(SAVES_DIR))
}

/// Reads a slot file, rejecting modified/truncated signed files and falling
/// back to the last complete backup. Plain RON is accepted only as a legacy
/// import path; the next normal save upgrades it to the signed format.
pub fn read_slot_text(path: &Path) -> Option<String> {
    match read_slot_text_at(path) {
        Ok(Some(text)) => Some(text),
        Ok(None) => None,
        Err(error) => {
            warn!("ignoring invalid save {}: {error}", path.display());
            let backup = backup_path(path);
            match read_slot_text_at(&backup) {
                Ok(Some(text)) => {
                    warn!("restored {} from {}", path.display(), backup.display());
                    Some(text)
                }
                Ok(None) => None,
                Err(backup_error) => {
                    warn!(
                        "backup {} is also invalid: {backup_error}",
                        backup.display()
                    );
                    None
                }
            }
        }
    }
}

fn read_slot_text_at(path: &Path) -> Result<Option<String>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !bytes.starts_with(ENVELOPE_MAGIC) {
        return String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| "unrecognised legacy save encoding".to_string());
    }
    let key = integrity_key().map_err(|error| error.to_string())?;
    decode_envelope(&bytes, &key)
        .and_then(|payload| {
            String::from_utf8(payload).map_err(|_| "save payload is not UTF-8".to_string())
        })
        .map(Some)
}

fn integrity_key() -> std::io::Result<[u8; TAG_BYTES]> {
    let path = saves_root().join(INTEGRITY_KEY_FILE);
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() == TAG_BYTES => {
            let mut key = [0; TAG_BYTES];
            key.copy_from_slice(&bytes);
            Ok(key)
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "integrity key has the wrong length",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(saves_root())?;
            let mut key = [0; TAG_BYTES];
            rand::rng().fill_bytes(&mut key);
            write_atomic(&path, &key)?;
            Ok(key)
        }
        Err(error) => Err(error),
    }
}

fn encode_envelope(payload: &[u8], key: &[u8; TAG_BYTES]) -> Vec<u8> {
    let mut body = Vec::with_capacity(ENVELOPE_MAGIC.len() + 8 + payload.len());
    body.extend_from_slice(ENVELOPE_MAGIC);
    body.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    body.extend_from_slice(payload);
    let tag = hmac_sha256(key, &body);
    body.extend_from_slice(&tag);
    body
}

fn decode_envelope(bytes: &[u8], key: &[u8; TAG_BYTES]) -> Result<Vec<u8>, String> {
    if bytes.len() < ENVELOPE_MAGIC.len() + 8 + TAG_BYTES {
        return Err("save envelope is truncated".to_string());
    }
    let signed_len = bytes.len() - TAG_BYTES;
    let expected = hmac_sha256(key, &bytes[..signed_len]);
    if !constant_time_eq(&expected, &bytes[signed_len..]) {
        return Err("save integrity check failed (the file was changed or damaged)".to_string());
    }
    let length_at = ENVELOPE_MAGIC.len();
    let mut length = [0; 8];
    length.copy_from_slice(&bytes[length_at..length_at + 8]);
    let payload_len = u64::from_le_bytes(length) as usize;
    let payload_start = length_at + 8;
    if payload_start.checked_add(payload_len) != Some(signed_len) {
        return Err("save envelope has an invalid payload length".to_string());
    }
    Ok(bytes[payload_start..signed_len].to_vec())
}

fn hmac_sha256(key: &[u8; TAG_BYTES], message: &[u8]) -> [u8; TAG_BYTES] {
    let mut inner_pad = [0x36; 64];
    let mut outer_pad = [0x5c; 64];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn backup_path(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".bak");
    PathBuf::from(backup)
}

fn write_recoverable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.exists() {
        std::fs::copy(path, backup_path(path))?;
    }
    write_atomic(path, bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)
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
    let Ok(entries) = std::fs::read_dir(saves_root()) else {
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

    found.sort_by(|(a_time, a), (b_time, b)| b_time.cmp(a_time).then_with(|| a.name.cmp(&b.name)));
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
    saves_root().join(CAMPAIGN_FILE)
}

/// Every antagonist beaten on this machine, in no particular order.
///
/// Unknown keys are dropped rather than treated as an error: the file is
/// hand-editable like every other save in this game, and a typo should cost
/// one unlock, not the launch.
pub fn thwarted_antags() -> Vec<AntagId> {
    let Some(text) = read_slot_text(&campaign_path()) else {
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

    let unlocks = CampaignUnlocks {
        thwarted: thwarted.iter().map(|id| id.key().to_string()).collect(),
    };
    let Ok(text) = ron::ser::to_string_pretty(&unlocks, default()) else {
        return;
    };
    write_slot_text(&campaign_path(), &text);
    info!("{} thwarted — antagonist runs unlocked", antag.label());
}

/// Where the last address joined is remembered.
///
/// Alongside the saves rather than in a slot: it belongs to this machine, not
/// to a career, and the whole point is that it survives starting a new game.
fn last_host_path() -> PathBuf {
    saves_root().join("last_host.txt")
}

/// Remembers an address so rejoining the same lab is one keystroke.
///
/// Typing an IP with a mouse-locked game either side of it is the worst part
/// of joining, and on a home network it is the same address every time.
pub fn remember_host(address: &str) {
    if let Err(error) = std::fs::create_dir_all(saves_root()) {
        warn!("could not create {}: {error}", saves_root().display());
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
    let destination = saves_root();
    if destination.exists() {
        return;
    }
    // Slots used to live below the working directory. Copy the whole tree on
    // the first launch after this move: it preserves every named career, and
    // leaving the source intact makes an interrupted migration harmless.
    let legacy_root = Path::new(SAVES_DIR);
    if legacy_root.exists() {
        if let Err(error) = copy_directory(legacy_root, &destination) {
            warn!(
                "could not copy existing saves from {} to {}: {error}",
                legacy_root.display(),
                destination.display()
            );
        } else {
            info!(
                "copied existing saves from {} to {}",
                legacy_root.display(),
                destination.display()
            );
        }
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
            warn!(
                "could not move {file} into {}: {error}",
                slot.dir().display()
            );
            continue;
        }
        if let Err(error) = std::fs::remove_file(file) {
            warn!("copied {file} into the save but could not remove it: {error}");
        }
    }
    info!("moved your existing save into '{}'", slot.name());
}

fn copy_directory(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
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
    fn signed_save_rejects_a_changed_payload() {
        let key = [7; TAG_BYTES];
        let mut envelope = encode_envelope(b"(research_points: 3)", &key);
        let payload_start = ENVELOPE_MAGIC.len() + 8;
        envelope[payload_start] ^= 1;
        assert!(decode_envelope(&envelope, &key).is_err());
    }

    #[test]
    fn signed_save_round_trips_its_payload() {
        let key = [42; TAG_BYTES];
        let payload = b"(known: [\"epinephrine\"])";
        assert_eq!(
            decode_envelope(&encode_envelope(payload, &key), &key).unwrap(),
            payload
        );
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
