# Linux and Steam Deck release plan

Status: proposed, awaiting design approval before implementation.
Updated: 2026-09-14.

## Goal and decisions

Ship Windows and native x86_64 Linux builds through Steam, with a complete
controller experience on Steam Deck. Test the Windows build under Proton early
as a compatibility baseline and fallback option. A native build is a separate
deliverable; a successful Proton run does not validate it.

Keep builds and uploads local, including Linux builds in a local Steam Runtime
SDK container. No GitHub Actions. Use the existing candidate branch and encrypted
asset pipeline. The user confirmed testing hardware/environment is available;
identify the exact Linux/WSL2 setup and Deck model during setup.

Aim to meet Valve's Deck Verified criteria. Only Valve can award that rating.
Do not promise compatibility based on compilation or headless tests.

## Current evidence

Repository inspection, without running a Linux build or gameplay test:

| Area | Current state | Work needed |
| --- | --- | --- |
| Engine | Bevy 0.19, Steam transport, microphone capture and vendored Opus | Validate the entire dependency chain on Linux |
| Controller menus | `src/menu/mod.rs` reads gamepads for navigation, activation and scrolling | Reuse the foundation across gameplay panels and modals |
| Movement/look | `src/player/mod.rs` reads keyboard movement and mouse motion | Analog movement, stick look, shared input intent |
| Gameplay actions | `src/settings/mod.rs` bindings use `KeyCode`; interaction, labels and other systems read keys directly | Device-independent actions and controller bindings |
| Dense UI | Machine screens, book, crew panels, editable fields and drag controls | Complete focus navigation and alternatives to dragging/hover |
| Saves | `src/saves/mod.rs::saves_root` uses LOCALAPPDATA or relative `saves` | Stable Linux user-data path, legacy migration and Cloud mapping |
| Steam runtime library | `build.rs` copies only `steam_api64.dll` | Target-aware Linux `libsteam_api.so` handling |
| Packaging | `tools/release-steam.ps1`, `docs/steam-release.md` and Cloud documentation cover Windows | Linux staging, auditing, depot and launch configuration |

The older `LAUNCH_PLAN.md` contains stale release-pipeline/CI statements. This
proposal and `docs/steam-release.md` take precedence for this platform effort.
Update the older plan when implementation starts.

Existing uncommitted work in settings, networking and voice belongs to ongoing
work. Reinspect and coordinate before editing those modules; preserve it.

## Milestone 1: Platform and input feasibility

1. Record the source revision, Rust toolchain, SDK/container version and hardware.
   Build from a Linux filesystem with case-sensitive paths. WSL2 can host local
   build tools; it does not replace SteamOS hardware testing.
2. Build and test the workspace inside Valve's recommended Steam Runtime SDK.
   The current recommendation is Steam Runtime 4; pin a tested image/digest and
   select the matching runtime in Steamworks. Confirm dependency support before
   committing to the toolchain baseline.
3. Resolve target-specific Steam library copying/loading, the Windows SDK path
   default in `.cargo/config.toml`, and native build dependencies including audio,
   controller discovery, windowing, CMake and Opus.
4. Stage an initial native packed-assets build and launch it on Deck. Separately
   test the existing Windows release under Proton. Capture startup, rendering,
   audio, Steam initialization and initial frame-time evidence for both.
5. Prototype one controller path through movement, a machine panel and a text
   field. Validate Steam Input detection, glyphs and the on-screen keyboard.

Exit gate: native Linux starts from a staged install with assets and sound;
controller feasibility is demonstrated on Deck. Record remaining failures before
estimating the rest of the work. Do not optimize around an unmeasured platform.

## Milestone 2: Shared actions and one complete chemistry loop

Introduce a small input module, proposed `src/input/mod.rs`, that emits actions
such as Move, Look, Interact, Inspect, Drop, Drink, Apply, Label, OpenBook,
OpenCrew, Pause, Confirm, Back, Navigate, Adjust, Scroll, PushToTalk and RadioTalk.
Keep keyboard/mouse remapping and existing saved settings compatible.

Use a single frame's movement intent for client prediction and network sending.
Preserve fractional stick magnitude, cap combined movement at unit length, and
keep the existing periodic resend of movement state. Stick look uses elapsed
time; mouse displacement does not. Add dead zones, sensitivity, inversion,
hold/toggle options and controller reconnection handling.

Use explicit input contexts for roaming, machine UI, books/crew, pause, text
entry and rebinding. The top context owns input: Confirm cannot also activate
the world, Back closes one layer, and text entry suppresses gameplay shortcuts.
Menu input/repeat must work while singleplayer virtual time is paused.

Proposed Steam integration: Steam Input actions for Steam builds, with Bevy
keyboard/mouse and a standard gamepad fallback behind the same action layer.
Milestone 1 must confirm the available Rust bindings and SDK APIs. If an API
bridge is disproportionate, gamepad emulation remains an option, provided it
passes the same glyph, default-layout and complete-access tests. Select one
controller event source per device to avoid duplicate input.

Initial layout to test, with Xbox/Deck face-button names:

| Input | Roaming | Panel/menu |
| --- | --- | --- |
| Left stick | Move | Navigate |
| Right stick | Look | Scroll or inspect, according to panel |
| A | Interact | Confirm |
| B | Back/cancel | Back one layer |
| X | Inspect | Context action when shown |
| Y | Open action wheel | Context action when shown |
| Left stick click | Sprint | No required action |
| View | Reference book | Book/back to parent |
| Menu | Pause | Pause/back as appropriate |
| D-pad | Documented shortcuts | Navigate or adjust |
| Shoulders/triggers | Voice actions or shortcuts after ergonomic testing | Tabs and value steps |

The action wheel provides crew, labeling, drop, drink and apply access. Give
drink/apply clear labels and deliberate activation to prevent mistakes. Reserve
usable hold bindings for proximity and handset speech, with explicit priority
if both are held. Every action must work on an ordinary gamepad; rear buttons,
gyro and trackpads can be optional conveniences. Finalize the full action map
in the prototype, including simultaneous movement and voice use.

Exit gate: from a fresh launch, use only a controller to start a save, walk to a
machine, dispense, combine, inspect, label, deliver an order, save, quit and
reload. Keyboard/mouse and network movement behavior remain correct.

## Milestone 3: Complete controller UI and text entry

Inventory every player-facing screen and action, including tutorial, save-slot
creation/deletion, multiplayer lobby and join, machines, recipe/textbook pages,
bookmarks, search, inventory/containers, dialogue, crew orders, shops, settings,
controls, inspection, error dialogs and endings. Each gets a tested coverage row.

Extend the existing menu focus foundation with visible focus, stable focus
after rebuilds, automatic scrolling into view, disabled-control skipping and
modal focus restoration. Provide value stepping/numeric entry and explicit
item selection/transfer wherever mouse dragging is currently required. Reveal
help on focus wherever hover reveals it today. A virtual cursor may supplement
these controls, but the normal chemistry workflow should be efficient with focus.

Create a shared text-entry adapter for labels, mixture names, searches and save
names. Invoke Steam's keyboard automatically, handle submit/cancel and Unicode,
and retain normal typing. Prevent the keyboard from obscuring the active field.
Provide controller text entry when Steam keyboard services are unavailable if
that launch mode is supported.

Prompts and tutorial instructions derive from action bindings and the active
device. Steam Input remaps must show the correct glyphs. Trackpad/gyro use must
not flash keyboard prompts when the player is still using the Deck layout.
Support live switching between mouse and controller without losing focus.

Exit gate: every coverage row passes on Deck and an ordinary gamepad, including
remapping, controller disconnect/reconnect, nested Back behavior and text cancel.

## Milestone 4: Linux persistence and release packaging

- Use `$XDG_DATA_HOME/ChemGame/saves`, falling back to
  `$HOME/.local/share/ChemGame/saves`, on Linux. Retain the Windows path. Define a
  recoverable migration for legacy relative saves, avoiding silent overwrites.
  Route settings and diagnostics through intentional writable user paths too.
- Verify case-sensitive asset references, pack paths, executable permissions,
  working-directory independence and read-only install-directory behavior.
- Add a local Linux packaging entry point, proposed `tools/release-steam-linux.sh`,
  preserving the Windows script. Reuse `release/assets.ron` and the packer.
  Stage the ELF executable, `libsteam_api.so`, packs, public sounds and credits;
  audit dependencies inside the selected runtime and reject development files.
- Keep CHEMGAME_ASSET_KEY in the trusted release environment. Never put it in
  source, command arguments or logs. Build both platforms with compatible packs.
- Configure a real Linux depot ID, Linux launch option, runtime and package
  entitlements in Steamworks. Do not invent IDs. Prefer separate platform depots
  initially; shared asset depot changes are optional later work.
- Match source/content versions across Windows and Linux candidates. Record
  manifests/checksums and audit results. Preserve package-only operation and
  deliberate promotion to the public branch.
- Extend Auto-Cloud with Linux root overrides mapping the same relative save
  files between OSes. Include `.integrity-key` and recovery backups. Test a save
  going Windows -> Linux/Deck -> Windows. Verify conflicts and offline recovery.
  Keep hardware-specific display/audio preferences from degrading another device
  when settings are synchronized; decide their separation before changing rules.

Exit gate: a clean Steam install works on Linux and Deck without the repository,
SDK, shell commands or dependency installation, and cross-platform saves survive.

## Milestone 5: Deck presentation, performance and release acceptance

Use 1280x800 as the primary layout and test 1280x720 plus docked displays. Review
actual rendered chemistry text, long names, tables, popups and touch targets at
handheld distance. Add UI/text scaling and ensure enlarged layouts remain usable.

Start with a stable 30 fps minimum target at 800p on Deck. Pursue 40/60 fps only
after measuring. Profile frame times and memory during a busy station, spills,
NPC activity and long sessions, with the Deck hosting and joining maximum
supported co-op. Address CPU simulation and rendering bottlenecks separately.
Choose playable defaults automatically while respecting explicit user settings.

Test suspend/resume, Steam overlay, focus loss, controller reconnect, docking,
audio output changes, microphone failure/recovery and network interruptions.
Suspending a co-op host need not preserve a live connection, but it must recover
cleanly without stuck input, lost save data or unusable menus.

| Environment | Required release evidence |
| --- | --- |
| Windows, keyboard/mouse | Existing gameplay and release regression pass |
| Windows, gamepad | Full controller workflow and remapping |
| Linux desktop, native | Clean Steam install, Vulkan rendering, sound, saves, co-op |
| Deck Gaming Mode, native | Complete controller coverage, keyboard, legibility, performance, resume |
| Deck, Windows via Proton | Baseline comparison and any advertised fallback behavior |
| Windows + Linux/Deck co-op | Each OS hosts/joins; voice/radio, save ownership and reconnect |
| Windows <-> Linux/Deck Cloud | Round-trip saves, integrity verification and conflict recovery |

Run targeted regression tests for input consumption, analog intent, focus,
text entry and path migration; run workspace tests on both OSes and normal
format/lint gates. Use rendered tests and manual playthroughs for layout and
controller ergonomics. Log build ID, hardware, settings, scenario, result and
remaining issues. Passing tests does not substitute for manual acceptance.

Request Valve compatibility review when available after the candidate passes;
resolve review findings and publish only supported store claims.

## Execution boundaries and progress ledger

No implementation has been started by this planning task. Work sequentially
unless parallel work is explicitly requested. If delegated later, give one owner
to the shared input contract and coordinate settings/UI/net changes first.

| Workstream | Primary files | Status / evidence |
| --- | --- | --- |
| Feasibility | Cargo.toml, build.rs, .cargo/config.toml, release tools | Proposed; no Linux runtime test yet |
| Input | proposed src/input, player, interaction, body, labels, voice | Proposed; keyboard/gamepad call sites inspected |
| UI | menu, ui, settings, inspection, tutorial | Proposed; existing menu foundation inspected |
| Persistence/release | saves, build.rs, tools, release/assets.ron, Steam docs | Proposed; Windows assumptions confirmed |
| Device acceptance | all shipped flows, evidence log | Pending hardware runs |

Next handoff: approve the target architecture and execute Milestone 1. Record
exact setup commands and evidence here before expanding implementation. Update
the ledger, file ownership and blockers after each milestone. Platform readiness,
automated checks and human gameplay/visual acceptance must remain separate.

## Sources checked 2026-09-14

- [Valve compatibility requirements](https://partner.steamgames.com/doc/steamhardware/compat):
  full default controller access, matching glyphs, controller text entry, playable
  defaults and Deck readability. Valve specifies 30 fps at 800p and a minimum
  rendered character height of 9 pixels, recommending 12 where possible.
- [Valve Steam Runtime](https://github.com/ValveSoftware/steam-runtime): current
  native Linux recommendation is Steam Runtime 4 with a matching SDK build and
  Steamworks runtime selection.
- [Developing for SteamOS and Linux](https://partner.steamgames.com/doc/store/application/platforms/linux).
