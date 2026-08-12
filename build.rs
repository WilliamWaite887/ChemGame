use std::env;
use std::fs;
use std::path::PathBuf;

/// Copies `steam_api64.dll` from the Steamworks SDK next to the built
/// executable, so `steamworks-sys`'s dynamically-loaded API can actually be
/// found at runtime (see `crates/bevy_replicon_renet2_patched/PATCH_NOTES.md`
/// for why the Steam transport needs the real SDK at all).
///
/// Reads `STEAM_SDK_LOCATION` (set in `.cargo/config.toml`) and no-ops
/// quietly if it isn't set, so this stays harmless on a machine without the
/// SDK — only the Steam transport needs it; the LAN/dev path does not.
fn main() {
    let Ok(sdk) = env::var("STEAM_SDK_LOCATION") else {
        return;
    };

    let dll = PathBuf::from(&sdk)
        .join("redistributable_bin")
        .join("win64")
        .join("steam_api64.dll");
    if !dll.exists() {
        println!(
            "cargo:warning=STEAM_SDK_LOCATION is set but {} was not found \
             — Steam transport builds will fail to run until it exists",
            dll.display()
        );
        return;
    }

    // OUT_DIR is target/<profile>/build/<crate>-<hash>/out; the built
    // executable lives three directories up, at target/<profile>/.
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is always set by cargo"));
    let target_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR should be nested three levels under target/<profile>/")
        .to_path_buf();

    let dest = target_dir.join("steam_api64.dll");
    if let Err(e) = fs::copy(&dll, &dest) {
        println!(
            "cargo:warning=failed to copy steam_api64.dll to {}: {e}",
            dest.display()
        );
    }

    println!("cargo:rerun-if-env-changed=STEAM_SDK_LOCATION");
}
