# Windows Steam release

ChemGame ships one Windows depot. Original data, maps, models, and textures are
stored in authenticated encrypted packs so they are not casually browsable.
The 68 third-party sounds remain ordinary `.ogg` files, and `CREDITS.md` is an
ordinary file in the install root, because their licenses and attribution must
remain easy to inspect.

This is deterrence, not DRM. The game client must contain the decryption key to
load its own assets, so a determined reverse engineer can eventually extract
them. Steam DRM may be enabled separately, but it does not change that fact.

The packer protects the Steam download only. It cannot protect files that are
also available in a public source repository or its Git history. Before treating
the packs as meaningful launch protection, make the development repository
private or move the protected original assets to a private repository and purge
their public history. This release automation intentionally does not change
repository visibility or rewrite Git history.

## One-time setup

1. Confirm the editable `$WindowsDepotId` line near the top of
   `tools/release-steam.ps1`. It is currently set to `5103231`; change that one
   line if Steamworks assigns a different Windows depot. App ID `5103230` is
   fixed separately in the script and game.
2. Install the Steamworks SDK and set `STEAM_SDK_LOCATION` to its root, either
   in the release environment or the existing local Cargo configuration.
3. Install SteamCMD. The script finds the copy inside `STEAM_SDK_LOCATION`
   automatically; otherwise put `steamcmd.exe` on `PATH` or set
   `STEAMCMD_PATH` to its full path.
4. Generate one stable 32-byte key, keep it in the release environment as
   `CHEMGAME_ASSET_KEY`, and back it up outside the repository. It must be 64
   hexadecimal characters. Changing it forces a full pack and executable
   update.

   ```powershell
   $Bytes = New-Object byte[] 32
   $Rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
   $Rng.GetBytes($Bytes)
   $Key = -join ($Bytes | ForEach-Object { $_.ToString("x2") })
   $Rng.Dispose()
   $Key
   ```
5. Set `STEAM_USERNAME` or let the local script ask for the Steam build account
   name. SteamCMD handles passwords and Steam Guard interactively/cached; never
   add them to this repository.
6. In Steamworks, create the passworded `candidate` branch once. The script
   assigns successful uploads to that branch; moving a build to the public
   default branch remains a deliberate manual action.

Releases are intentionally local-only. Run the commands below from a trusted
Windows development machine. No GitHub Actions workflow, pushed tag, or other
remote automation builds or uploads the Steam depot.

## Release commands

Build, test, audit, and upload to `candidate`:

```powershell
.\tools\release-steam.ps1
```

Build and inspect the exact depot folder without uploading:

```powershell
.\tools\release-steam.ps1 -PackageOnly
```

Tests run by default. `-SkipTests` exists for an already-tested rebuild, but
should not be used for the release that is promoted publicly.

## Depot contents

The staging folder is `target/steam/windows-content` and contains only:

```text
chemgame.exe
steam_api64.dll
CREDITS.md
content_00.cgp             encrypted data and maps
content_01.cgp             encrypted models
content_02.cgp             encrypted textures and UI images
assets/sounds/**/*.ogg     readable, credited originals
```

No `.py`, `.pyc`, `.blend`, `.blend1`, source Markdown, JSON, SVG, preview
render, loose RON, loose map, loose GLB, or loose PNG is accepted by the stage
audit. `steam_appid.txt` is also absent.

`release/assets.ron` is the source of truth for packaging. It classifies each
runtime-shaped asset as protected, public, or deliberately ignored. Adding a
new GLB, PNG, RON, map, or OGG without classifying it makes packaging fail,
which prevents accidental inclusion of new authoring output.

Before promoting a candidate build, install it through Steam on a clean Windows
account, open `CREDITS.md` from the install folder, confirm the sounds are
readable, and play through startup, a normal shift, save/reload, and Steam
co-op. Do not test only from the repository: development builds intentionally
use loose assets and exercise a different reader.
