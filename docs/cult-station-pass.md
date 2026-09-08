# Cult station progression and aftermath

User request: more Cult assets, visible station changes as the campaign advances,
and physical cleanup after stopping the Cult. Continue the station kit's restrained
geometry, pixel textures and installed surface details. Follow release/assets.ron.

## Implementation scope

- Eight new authored models: pilgrim markings, etched wall panel, wax shrine,
  offering cache, bound ventilation grille, blood-root floor growth, processional
  banner and floor rift fracture. Twelve map-authored placements spread over the
  three existing ritual waves. Four new locations appear per wave.
- The marks sit on real station walls and floors. Doorways, route volumes,
  gameplay fixtures and their colliders remain usable.
- The existing investigation, ward threshold and chemistry-sealed finale stay
  authoritative. Clearing secondary contamination grants no extra wards.
- Victory extinguishes activity without resetting the station. Untreated original
  ritual manifestations also become spent remains. Cleaner removes chemical
  marks; empty hands dismantle spent physical deposits. Active deposits require
  cleaner. Consume only the required cleaner, retaining the container and surplus.
- Cleanup state belongs to the career, survives reload and subsequent antagonist
  arcs, and replicates to joining players. Cleared sites stay clear for that Cult
  campaign. A later Cult campaign can deface them again as its waves reach them.
- The standing board and radio report remaining contamination. A failed Cult arc
  also leaves persistent contamination; cleanup cannot retroactively change its
  recorded outcome.

## Ownership and gates

Root owns implementation; no parallel writers. Preserve the completed station
asset pass and unrelated clock/voice/local-tool work already present.

Files in flight: src/cult/aftermath.rs, src/cult/mod.rs, src/arc/mod.rs,
src/net/mod.rs, src/ui/mod.rs, src/shift/mod.rs, assets/data/station.cult.ron, assets/maps/lab.map,
src/lab/tb_map.rs, src/capture asset review support, release/assets.ron, and the
station kit's dedicated Cult expansion builder/source/manifest/exports/validator.

1. Implement persistent state, authority interaction and replicated presentation.
2. Author/export models serially, place and validate against map surfaces/routes.
3. Test wave progression, early cleaning, victory, original leftovers, repeated
   requests, exact chemical use, range/wall checks, save/wire round trips, new
   campaigns, reload, late visual loading and unrelated campaigns.
4. Run targeted checks, workspace suite and required asset/release classification.
5. Capture actual in-game early/late/spent/cleaned states in a disposable session
   with no career save writes. Record visual findings and remaining limitations.

## Progress

- Inspected the existing eight Cult props, three-wave campaign, sealing finale,
  asset release classification and career/replication contracts. Existing finale
  removes all ritual entities; persistent aftermath will reconstruct physical
  remains from stable content identifiers after that transition.
- The baseline station kit contains 148 editable assets / 154 GLBs. Its known
  Service bar depth validation issue predates this pass.
- Added all eight Cult models: 156 editable roots / 162 exported GLBs now.
  The dedicated Cult generator preserves the preceding room expansion. Catalog:
  `assets/3dassets/station_starter_kit/preview_expansion_cult.png`.
- Implemented career-owned site snapshots in `CampaignRoster`, replicated
  `CultResidue` entities, wave high-water tracking, spent original manifestations,
  private ash materials, extinguished activity lights and authority cleanup.
  Career rerolls carry the history. Progress saving runs after cleanup updates.
- Initial Cult suite passed 35 tests (11 new). Final `cargo test --workspace`
  passed all 1,842 tests, including 1,637 game tests. Save/wire round trips,
  old RON migration, exact cleaner consumption, early cleaning, blocked/far
  requests, repeated cleanup, reconstruction and campaign rerolls are covered.
  Evidence: `target/cult-pass/workspace-tests.log`.
- Actual exported bounds and map surfaces exposed four placement problems:
  Medical's doorway, Cargo's carved storage strips, the Chapel shrine's approach
  direction and Atmos's equipment strip. Corrected the authored markers. All 56
  map tests pass, including GLB parsing, route connectivity and the new backing
  and approach check. Evidence: `target/cult-pass/map-placement-tests.log`.
- Release classifier passes: content_00 27, content_01 173, content_02 122,
  public 68, ignored 61; zero unclassified files and zero missing exclusions.
  All eight new Cult GLBs belong to content_01. The preview is explicitly ignored.
  No keys accessed, encrypted packs built, staging changed or uploads performed.
- Full Blender source/export validation found only the existing
  `DEC_SvcBarCounter` depth/declared-footprint mismatch. No Cult failures.
  Evidence: `target/cult-pass/blender-validation.log`.
- `cargo clippy --workspace --all-targets` completed with existing warnings;
  no remaining warnings in the new Cult aftermath/review modules. Explicit
  allowances cover Bevy's compound query filter signatures. Touched-file
  rustfmt checks and `git diff --check` pass. Evidence:
  `target/cult-pass/clippy-final.log`.
- Three disposable in-game reviews completed with exit 0. The final run exercised
  4 early sites, 12 late sites, 17 spent sites, then all 17 successful authority
  cleanup requests including `cult.altar`. No live career save file sizes or
  modification times changed. Microphone and sandboxed app-settings/diagnostic
  write warnings were unrelated to the asset/campaign path.
- Visual review confirmed wall-facing panels/grilles/banners, floor contact,
  quiet-room cache clearance, and extinguished/duller spent variants. Moved
  the Chapel fracture onto exposed plating after seeing it intersect the runner.
  Corrected the maintenance review camera after its first pose hit a wall;
  its replacement image shows the fracture on the corridor plating, and all
  32 final views were saved. The site passed actual cleanup/reach checks.
- Standing-board cleanup includes the next outstanding site name. Old Cult
  residue can remain visible on the board during a new hidden antagonist arc
  without announcing or naming that new threat. Its regression test passes.
- Independent `.gitignore`, `.claude/` and `src/audio/mod.rs` changes were
  observed during this pass and preserved. They are outside this work's scope.

## Reproduce the in-game review

```powershell
$env:RUST_LOG = 'warn,chemgame::capture=info'
cargo run -- --solo --asset-tour target/cult-pass/final-review --cult-tour
```

Debug-only, disposable Trailer session through the asset-tour no-SaveSlot guard.
Stages actual campaign data into early (4 sites), late (12 sites), stopped
(five wards, with unresolved original manifestations) and cleaned states.
Captures 32 actual station views. Wave timing and finale outcome are staged;
cleanup sends real `InteractRequested` messages from legal, reachable approaches
against the loaded station's actual solids. A rejected cleanup or missing
approach exits with failure. This validates rendering and physical cleanup,
not a full naturally timed campaign playthrough or two-client playtest.

Final review images and per-site cleanup evidence live in
`target/cult-pass/final-review/` and `target/cult-pass/final-review.log`.
The older `target/cult-pass/review/` is retained as iteration evidence.

## Remaining acceptance boundaries

Implementation is complete. Natural campaign pacing and multiplayer visual
acceptance still need ordinary playtesting. Release classification is verified;
encrypted packaging, Steam upload and clean-install release smoke testing were
not part of this pass. The pre-existing Service bar source-envelope validation
failure remains outside the Cult work.
