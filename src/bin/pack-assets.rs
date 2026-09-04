//! Builds the deliberately small Steam asset tree from `release/assets.ron`.

#[path = "../asset_pack_format.rs"]
mod asset_pack_format;

use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

use asset_pack_format::{decode_key, write_pack, PackFile};
use serde::Deserialize;

const RUNTIME_EXTENSIONS: &[&str] = &["glb", "map", "ogg", "png", "ron"];

#[derive(Debug, Deserialize)]
struct Manifest {
    packs: Vec<PackSpec>,
    public: Vec<Rule>,
    ignored: Vec<Rule>,
    ignored_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PackSpec {
    file: String,
    include: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
struct Rule {
    root: String,
    extensions: Vec<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("asset packaging failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let mut manifest_path = PathBuf::from("release/assets.ron");
    let mut output = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--manifest") => {
                manifest_path = PathBuf::from(args.next().ok_or("--manifest needs a path")?);
            }
            Some("--output") => {
                output = Some(PathBuf::from(args.next().ok_or("--output needs a path")?));
            }
            Some("--help" | "-h") => {
                println!(
                    "Usage: cargo run --release --bin pack-assets -- --output <directory> [--manifest <file>]"
                );
                return Ok(());
            }
            Some(other) => return Err(format!("unknown argument: {other}")),
            None => return Err("arguments must be valid UTF-8".to_string()),
        }
    }
    let output = output.ok_or("missing required --output directory")?;
    let repo_root = env::current_dir().map_err(|error| error.to_string())?;
    let asset_root = repo_root.join("assets");
    if !asset_root.is_dir() {
        return Err(format!(
            "{} is not an assets directory; run the packer from the repository root",
            asset_root.display()
        ));
    }

    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("could not read {}: {error}", manifest_path.display()))?;
    let manifest: Manifest = ron::from_str(&manifest_text)
        .map_err(|error| format!("invalid {}: {error}", manifest_path.display()))?;
    let key = decode_key(
        &env::var("CHEMGAME_ASSET_KEY").map_err(|_| "CHEMGAME_ASSET_KEY is not set".to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let ignored_files: HashSet<_> = manifest
        .ignored_files
        .iter()
        .map(|path| normalize_manifest_path(path))
        .collect::<Result<_, _>>()?;
    let ignored_by_rule = collect_rules(&asset_root, &manifest.ignored)?;
    let mut ignored = ignored_files;
    ignored.extend(ignored_by_rule);

    let mut classification: HashMap<String, String> = HashMap::new();
    for path in &ignored {
        classification.insert(path.clone(), "ignored".to_string());
    }

    let mut public = collect_rules(&asset_root, &manifest.public)?;
    public.retain(|path| !ignored.contains(path));
    for path in &public {
        classify(&mut classification, path, "public")?;
        if Path::new(path).extension().and_then(|ext| ext.to_str()) != Some("ogg") {
            return Err(format!("public asset is not credited audio: {path}"));
        }
    }

    let mut pack_entries = Vec::with_capacity(manifest.packs.len());
    for pack in &manifest.packs {
        if Path::new(&pack.file)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(pack.file.as_str())
            || !pack.file.ends_with(".cgp")
        {
            return Err(format!("unsafe pack filename: {}", pack.file));
        }
        let mut paths = collect_rules(&asset_root, &pack.include)?;
        paths.retain(|path| !ignored.contains(path));
        for path in &paths {
            classify(&mut classification, path, &pack.file)?;
        }
        pack_entries.push((pack, paths));
    }

    let all_files = walk_files(&asset_root)?;
    let unclassified: Vec<_> = all_files
        .iter()
        .filter(|path| {
            let extension = Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            RUNTIME_EXTENSIONS.contains(&extension.as_str())
                && !classification.contains_key(path.as_str())
        })
        .cloned()
        .collect();
    if !unclassified.is_empty() {
        return Err(format!(
            "runtime-shaped assets are not classified in {}:\n  {}",
            manifest_path.display(),
            unclassified.join("\n  ")
        ));
    }

    let missing_ignored: Vec<_> = ignored
        .iter()
        .filter(|path| !asset_root.join(path.replace('/', "\\")).is_file())
        .cloned()
        .collect();
    if !missing_ignored.is_empty() {
        return Err(format!(
            "ignored_files contains paths that no longer exist:\n  {}",
            missing_ignored.join("\n  ")
        ));
    }

    let credits_path = repo_root.join("CREDITS.md");
    let credits = fs::read_to_string(&credits_path)
        .map_err(|error| format!("could not read {}: {error}", credits_path.display()))?;
    let count_marker = format!("{} `.ogg` files", public.len());
    if !credits.contains(&count_marker) {
        return Err(format!(
            "CREDITS.md does not declare the public audio count ({count_marker})"
        ));
    }

    fs::create_dir_all(&output)
        .map_err(|error| format!("could not create {}: {error}", output.display()))?;
    for (pack, paths) in pack_entries {
        let entries = paths
            .iter()
            .map(|path| {
                fs::read(asset_root.join(path.replace('/', "\\")))
                    .map(|bytes| (path.clone(), bytes))
                    .map_err(|error| format!("could not read {path}: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pack_path = output.join(&pack.file);
        if pack_path.exists() {
            fs::remove_file(&pack_path)
                .map_err(|error| format!("could not replace {}: {error}", pack_path.display()))?;
        }
        let count =
            write_pack(&pack_path, entries.clone(), key).map_err(|error| error.to_string())?;

        // Verify the completed file with the same reader the game uses. This
        // catches truncation, key mismatch and index mistakes before upload.
        let built = PackFile::open(&pack_path, key).map_err(|error| error.to_string())?;
        for (path, expected) in entries {
            let actual = built
                .read(Path::new(&path))
                .map_err(|error| error.to_string())?;
            if actual != expected {
                return Err(format!("post-build verification failed for {path}"));
            }
        }
        println!("built {} with {count} protected assets", pack.file);
    }

    for path in &public {
        let source = asset_root.join(path.replace('/', "\\"));
        let destination = output.join("assets").join(path.replace('/', "\\"));
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::copy(&source, &destination).map_err(|error| {
            format!(
                "could not copy public asset {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
    }
    fs::copy(&credits_path, output.join("CREDITS.md"))
        .map_err(|error| format!("could not stage CREDITS.md: {error}"))?;

    let source_only = all_files.len().saturating_sub(classification.len());
    println!(
        "staged {} public credited sounds and CREDITS.md; omitted {} authoring/source files",
        public.len(),
        source_only + ignored.len()
    );
    Ok(())
}

fn classify(
    classification: &mut HashMap<String, String>,
    path: &str,
    category: &str,
) -> Result<(), String> {
    if let Some(existing) = classification.insert(path.to_string(), category.to_string()) {
        return Err(format!(
            "asset {path} is classified twice ({existing} and {category})"
        ));
    }
    Ok(())
}

fn collect_rules(asset_root: &Path, rules: &[Rule]) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for rule in rules {
        let root = normalize_manifest_path(&rule.root)?;
        let full_root = asset_root.join(root.replace('/', "\\"));
        if !full_root.is_dir() {
            return Err(format!(
                "manifest root does not exist: {}",
                full_root.display()
            ));
        }
        let extensions: HashSet<_> = rule
            .extensions
            .iter()
            .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
            .collect();
        for path in walk_files(&full_root)? {
            let full = full_root.join(path.replace('/', "\\"));
            let extension = full
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if extensions.contains(&extension) {
                let relative = full
                    .strip_prefix(asset_root)
                    .map_err(|error| error.to_string())?;
                result.push(path_to_forward_slashes(relative)?);
            }
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}

fn walk_files(root: &Path) -> Result<Vec<String>, String> {
    fn recurse(root: &Path, current: &Path, out: &mut Vec<String>) -> Result<(), String> {
        let entries = fs::read_dir(current)
            .map_err(|error| format!("could not enumerate {}: {error}", current.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                recurse(root, &path, out)?;
            } else if kind.is_file() {
                out.push(path_to_forward_slashes(
                    path.strip_prefix(root).map_err(|error| error.to_string())?,
                )?);
            }
        }
        Ok(())
    }

    let mut result = Vec::new();
    recurse(root, root, &mut result)?;
    result.sort();
    Ok(result)
}

fn normalize_manifest_path(path: &str) -> Result<String, String> {
    let normalized = path.trim().replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "..")
    {
        return Err(format!("unsafe manifest path: {path}"));
    }
    Ok(normalized)
}

fn path_to_forward_slashes(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}
