//! Small authenticated asset-pack format shared by the release packer and the
//! shipping asset reader.
//!
//! The index contains keyed path hashes, byte ranges and sizes; it deliberately
//! contains no filenames. Each entry is independently encrypted so Bevy can
//! request one asset without decrypting the whole pack. This raises the effort
//! required to browse ChemGame's original data and art, but a key embedded in a
//! client executable can never make shipped assets impossible to extract.

use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    path::{Component, Path},
    sync::Arc,
};

use aes_gcm_siv::{
    aead::{Aead, KeyInit, Payload},
    Aes256GcmSiv, Nonce,
};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"CHGPK001";
const HEADER_LEN: usize = 12;
const RECORD_LEN: usize = 64;
const FLAG_ZSTD: u8 = 1;

#[derive(Debug, Clone)]
pub struct PackError(String);

impl PackError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PackError {}

impl From<std::io::Error> for PackError {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, Copy)]
struct Record {
    offset: usize,
    encrypted_len: usize,
    plain_len: usize,
    flags: u8,
}

/// An immutable pack loaded from disk. Pack bytes are shared cheaply if the
/// reader is cloned by Bevy.
#[derive(Clone)]
pub struct PackFile {
    bytes: Arc<[u8]>,
    records: HashMap<[u8; 32], Record>,
    master_key: [u8; 32],
}

impl PackFile {
    pub fn open(path: &Path, master_key: [u8; 32]) -> Result<Self, PackError> {
        let bytes = fs::read(path).map_err(|error| {
            PackError::new(format!("could not read {}: {error}", path.display()))
        })?;
        Self::from_bytes(bytes, master_key)
            .map_err(|error| PackError::new(format!("invalid pack {}: {error}", path.display())))
    }

    fn from_bytes(bytes: Vec<u8>, master_key: [u8; 32]) -> Result<Self, PackError> {
        if bytes.len() < HEADER_LEN || &bytes[..MAGIC.len()] != MAGIC {
            return Err(PackError::new("missing ChemGame pack signature"));
        }

        let count = read_u32(&bytes[8..12])? as usize;
        let index_len = count
            .checked_mul(RECORD_LEN)
            .and_then(|length| HEADER_LEN.checked_add(length))
            .ok_or_else(|| PackError::new("pack index length overflow"))?;
        if index_len > bytes.len() {
            return Err(PackError::new("truncated pack index"));
        }

        let mut records = HashMap::with_capacity(count);
        for number in 0..count {
            let start = HEADER_LEN + number * RECORD_LEN;
            let raw = &bytes[start..start + RECORD_LEN];
            let mut id = [0_u8; 32];
            id.copy_from_slice(&raw[..32]);
            let offset = usize::try_from(read_u64(&raw[32..40])?)
                .map_err(|_| PackError::new("entry offset does not fit this platform"))?;
            let encrypted_len = usize::try_from(read_u64(&raw[40..48])?)
                .map_err(|_| PackError::new("entry length does not fit this platform"))?;
            let plain_len = usize::try_from(read_u64(&raw[48..56])?)
                .map_err(|_| PackError::new("plain length does not fit this platform"))?;
            let flags = raw[56];
            if flags & !FLAG_ZSTD != 0 {
                return Err(PackError::new("entry uses unsupported flags"));
            }
            let end = offset
                .checked_add(encrypted_len)
                .ok_or_else(|| PackError::new("entry byte range overflow"))?;
            if offset < index_len || end > bytes.len() {
                return Err(PackError::new("entry points outside pack payload"));
            }
            if records
                .insert(
                    id,
                    Record {
                        offset,
                        encrypted_len,
                        plain_len,
                        flags,
                    },
                )
                .is_some()
            {
                return Err(PackError::new("duplicate keyed path in pack index"));
            }
        }

        Ok(Self {
            bytes: bytes.into(),
            records,
            master_key,
        })
    }

    #[allow(dead_code)] // Used by the game reader, not the packer binary.
    pub fn contains(&self, path: &Path) -> bool {
        normalize_asset_path(path)
            .map(|path| self.records.contains_key(&path_id(&self.master_key, &path)))
            .unwrap_or(false)
    }

    pub fn read(&self, path: &Path) -> Result<Vec<u8>, PackError> {
        let normalized = normalize_asset_path(path)?;
        let id = path_id(&self.master_key, &normalized);
        let record = self
            .records
            .get(&id)
            .ok_or_else(|| PackError::new(format!("asset not present: {normalized}")))?;
        let encrypted = &self.bytes[record.offset..record.offset + record.encrypted_len];
        let key = entry_key(&self.master_key, &normalized);
        let cipher = Aes256GcmSiv::new_from_slice(&key)
            .map_err(|_| PackError::new("invalid AES key length"))?;
        let nonce_bytes = entry_nonce(&self.master_key, &normalized);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let aad = entry_aad(&id, record.flags, record.plain_len)?;
        let stored = cipher
            .decrypt(
                nonce,
                Payload {
                    msg: encrypted,
                    aad: &aad,
                },
            )
            .map_err(|_| PackError::new(format!("authentication failed for {normalized}")))?;

        let plain = if record.flags & FLAG_ZSTD != 0 {
            zstd::bulk::decompress(&stored, record.plain_len)
                .map_err(|error| PackError::new(format!("zstd decode failed: {error}")))?
        } else {
            stored
        };
        if plain.len() != record.plain_len {
            return Err(PackError::new(format!(
                "decoded length mismatch for {normalized}: expected {}, got {}",
                record.plain_len,
                plain.len()
            )));
        }
        Ok(plain)
    }
}

/// Writes one pack atomically enough for a staging directory: all bytes are
/// constructed first, then the finished pack replaces the destination file.
#[allow(dead_code)] // Used by the packer binary, not the game binary.
pub fn write_pack(
    output: &Path,
    entries: impl IntoIterator<Item = (String, Vec<u8>)>,
    master_key: [u8; 32],
) -> Result<usize, PackError> {
    let mut entries: Vec<_> = entries.into_iter().collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut seen = HashSet::with_capacity(entries.len());
    let mut encoded = Vec::with_capacity(entries.len());

    for (path, bytes) in entries {
        let normalized = normalize_asset_path(Path::new(&path))?;
        if !seen.insert(normalized.clone()) {
            return Err(PackError::new(format!(
                "duplicate asset path: {normalized}"
            )));
        }

        let compressed = zstd::bulk::compress(&bytes, 10)
            .map_err(|error| PackError::new(format!("zstd encode failed: {error}")))?;
        let (stored, flags) = if compressed.len() < bytes.len() {
            (compressed, FLAG_ZSTD)
        } else {
            (bytes.clone(), 0)
        };
        let id = path_id(&master_key, &normalized);
        let key = entry_key(&master_key, &normalized);
        let cipher = Aes256GcmSiv::new_from_slice(&key)
            .map_err(|_| PackError::new("invalid AES key length"))?;
        let nonce_bytes = entry_nonce(&master_key, &normalized);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let aad = entry_aad(&id, flags, bytes.len())?;
        let encrypted = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: &stored,
                    aad: &aad,
                },
            )
            .map_err(|_| PackError::new(format!("encryption failed for {normalized}")))?;
        encoded.push((id, bytes.len(), flags, encrypted));
    }

    let payload_start = HEADER_LEN
        .checked_add(
            encoded
                .len()
                .checked_mul(RECORD_LEN)
                .ok_or_else(|| PackError::new("pack index length overflow"))?,
        )
        .ok_or_else(|| PackError::new("pack index length overflow"))?;
    let mut next_offset = payload_start;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(
        &u32::try_from(encoded.len())
            .map_err(|_| PackError::new("too many entries"))?
            .to_le_bytes(),
    );
    for (id, plain_len, flags, encrypted) in &encoded {
        out.extend_from_slice(id);
        out.extend_from_slice(&(next_offset as u64).to_le_bytes());
        out.extend_from_slice(&(encrypted.len() as u64).to_le_bytes());
        out.extend_from_slice(&(*plain_len as u64).to_le_bytes());
        out.push(*flags);
        out.extend_from_slice(&[0_u8; 7]);
        next_offset = next_offset
            .checked_add(encrypted.len())
            .ok_or_else(|| PackError::new("pack payload length overflow"))?;
    }
    for (_, _, _, encrypted) in &encoded {
        out.extend_from_slice(encrypted);
    }

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = output.with_extension("cgp.tmp");
    fs::write(&temporary, out)?;
    fs::rename(&temporary, output)?;
    Ok(encoded.len())
}

pub fn decode_key(raw: &str) -> Result<[u8; 32], PackError> {
    let raw = raw.trim();
    if raw.len() != 64 {
        return Err(PackError::new(
            "CHEMGAME_ASSET_KEY must contain exactly 64 hexadecimal characters",
        ));
    }
    let mut key = [0_u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16)
            .map_err(|_| PackError::new("CHEMGAME_ASSET_KEY contains a non-hex character"))?;
    }
    Ok(key)
}

fn normalize_asset_path(path: &Path) -> Result<String, PackError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| PackError::new("asset path is not valid UTF-8"))?,
            ),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PackError::new("asset path may not escape the asset root"));
            }
        }
    }
    if parts.is_empty() {
        return Err(PackError::new("asset path is empty"));
    }
    Ok(parts.join("/"))
}

fn path_id(master_key: &[u8; 32], path: &str) -> [u8; 32] {
    hash_parts(&[master_key, b"chemgame/path-id/v1", path.as_bytes()])
}

fn entry_key(master_key: &[u8; 32], path: &str) -> [u8; 32] {
    hash_parts(&[master_key, b"chemgame/entry-key/v1", path.as_bytes()])
}

fn entry_nonce(master_key: &[u8; 32], path: &str) -> [u8; 12] {
    let hash = hash_parts(&[master_key, b"chemgame/entry-nonce/v1", path.as_bytes()]);
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(&hash[..12]);
    nonce
}

fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn entry_aad(id: &[u8; 32], flags: u8, plain_len: usize) -> Result<Vec<u8>, PackError> {
    let mut aad = Vec::with_capacity(MAGIC.len() + 32 + 1 + 8);
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(id);
    aad.push(flags);
    aad.extend_from_slice(
        &u64::try_from(plain_len)
            .map_err(|_| PackError::new("plain length does not fit in pack format"))?
            .to_le_bytes(),
    );
    Ok(aad)
}

fn read_u32(bytes: &[u8]) -> Result<u32, PackError> {
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| PackError::new("truncated 32-bit pack field"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(bytes: &[u8]) -> Result<u64, PackError> {
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| PackError::new("truncated 64-bit pack field"))?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [0x5a; 32];

    #[test]
    fn encrypted_pack_round_trips_compressible_and_binary_assets() {
        let temporary = std::env::temp_dir().join(format!(
            "chemgame-pack-roundtrip-test-{}.cgp",
            std::process::id()
        ));
        let compressible = vec![b'x'; 4096];
        let binary: Vec<u8> = (0..=255).collect();
        write_pack(
            &temporary,
            [
                ("data/test.ron".to_string(), compressible.clone()),
                ("textures/test.png".to_string(), binary.clone()),
            ],
            KEY,
        )
        .unwrap();

        let pack = PackFile::open(&temporary, KEY).unwrap();
        assert_eq!(pack.read(Path::new("data/test.ron")).unwrap(), compressible);
        assert_eq!(pack.read(Path::new("textures/test.png")).unwrap(), binary);
        assert!(!pack.contains(Path::new("sounds/public.ogg")));
        let _ = fs::remove_file(temporary);
    }

    #[test]
    fn wrong_key_cannot_decrypt_an_entry() {
        let temporary = std::env::temp_dir().join(format!(
            "chemgame-pack-wrong-key-test-{}.cgp",
            std::process::id()
        ));
        write_pack(
            &temporary,
            [("data/private.ron".to_string(), b"secret".to_vec())],
            KEY,
        )
        .unwrap();
        let pack = PackFile::open(&temporary, [0x33; 32]).unwrap();
        assert!(pack.read(Path::new("data/private.ron")).is_err());
        let _ = fs::remove_file(temporary);
    }

    #[test]
    fn rejects_parent_directory_paths() {
        assert!(normalize_asset_path(Path::new("../outside.ron")).is_err());
    }
}
