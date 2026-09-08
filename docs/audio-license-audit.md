# ChemGame audio license audit

Audited 2026-09-08. Scope: the 68 audio files in `assets/sounds`, their source records, `CREDITS.md`, release configuration, packer, and existing Windows staging directory. This is an evidence review, not a legal clearance opinion.

## Result

No explicit NonCommercial, NoDerivatives, or proprietary license was found for a shipped file in the upstream records checked. All 68 files are byte-for-byte matches to their credited upstream copies. All 68 staged sounds also match the workspace files. Nevertheless, the current credits contain errors and an unresolved source conflict. Do not treat the present credits or the Discord answer as complete commercial clearance.

The Discord advice correctly identifies NonCommercial assets as unsuitable for a paid release without separate permission. Other licenses still impose conditions. Permission must come from the actual rights holder or a valid license, rather than a general assurance from a community member.

The existing credits classify the audio as follows. These are the document's classifications at audit time, not 68 independently cleared licenses:

| Recorded license | Files | Assessment |
|---|---:|---|
| CC BY-SA 3.0 | 48 | Commercial use is permitted subject to conditions; includes the incorrectly simplified `eject.ogg` row. |
| CC BY 4.0 | 3 | Two bottle sounds and `soft_thump.ogg`; attribution and other license conditions apply. |
| CC0 | 17 | Generally suitable for commercial use; includes the scanner source conflict and the reaction composite described below. |

## Findings to resolve before release

### 1. Missing license links and misleading compliance wording

`CREDITS.md` has no direct Creative Commons license URLs or license texts. Its statement that BY-SA conditions apply when assets are standalone but not when baked into the game is incorrect. Distribution obligations also apply to assets included in a collection. Its blanket assurance about the scope of ShareAlike is too broad.

Add direct links to [CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/), [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), and [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/). Preserve required creator credits, supplied titles, source notices and modification information. Say explicitly that third-party sounds retain their respective licenses, including when shipped with ChemGame. Do not impose conflicting restrictions on them through a game EULA or effective technological measures.

The relevant BY-SA terms are sections 1(a), 4(a), 4(b), and 4(c) of the [legal code](https://creativecommons.org/licenses/by-sa/3.0/legalcode.en). Collection treatment does not automatically license independent game code under CC. However, synchronization with moving images and adaptations need separate analysis. For this reason, obtain advice on the actual game/trailer use if retaining BY-SA audio, or replace it with original, properly licensed commissioned, or well-documented CC0 audio. A loose OGG copy alone does not settle adaptation questions.

### 2. `eject.ogg`: original creator and license omitted

The credits currently list only tgstation contributors and the project's BY-SA default. The [tgstation attribution list](https://github.com/tgstation/tgstation/blob/8840d67cb0760f4bfa87cd1c87432bf3319caeb7/sound/attributions.txt) identifies **magedu** and Freesound 267832. The [original sound page](https://freesound.org/s/267832/) identifies `video_recorder_eject_cassette.wav` under **CC BY 4.0**.

Commercial use is allowed by that source license, but the original attribution must be carried forward. Credit magedu, identify the source and tgstation adaptation, and record the original CC BY 4.0 license separately from any license applying to tgstation's added contribution. Do not simply treat a project's default license as replacing the original license.

### 3. `ss14/scan_finish.ogg`: conflicting upstream provenance

The [pinned Machines manifest](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Machines/attributions.yml) lists **pan14**, **CC0**, and a link to **steaq**'s Freesound 509249. That [linked page](https://freesound.org/people/steaq/sounds/509249/) is titled *Sci Fi Drone Engine Loop* and is CC0. The author identities disagree; confirming that the linked page is CC0 does not prove it is this recording's source.

The local file matches SS14, and its file history points to introduction commit `273e0968e4cd5b7dd049cf6fe83669f124b292ef` (PR 12204). The conflict remains unresolved. Recover the correct source from the contributor or replace this sound before commercial release. This is a provenance hold, not evidence that it is NonCommercial.

### 4. `reaction_occurred.ogg`: incomplete composite-source credit

This matches tgstation `sound/effects/chemistry/catalyst.ogg`. The folder's [SoundSources.txt](https://github.com/tgstation/tgstation/blob/8840d67cb0760f4bfa87cd1c87432bf3319caeb7/sound/effects/chemistry/SoundSources.txt) identifies two inputs, but ChemGame credits only one:

- [Melthurian, Bubbling beaker.wav, 319384](https://freesound.org/people/Melthurian/sounds/319384/): CC0 confirmed on the source page.
- [The_Chemical_Workshop, Test Tube explosion and breaking glass 10x slower, 408137](https://freesound.org/people/The_Chemical_Workshop/sounds/408137/): CC0 confirmed on the source page.

Add the second source and identify the upstream composite/edit. Both inputs allow commercial reuse. The manifest's final license sentence is imprecise, so the existing assertion that the entire edited composite is CC0 is not fully supported merely by the input licenses. Preserve any license applicable to creative contributions made by the upstream editor, or obtain clarification/use a directly licensed replacement.

### 5. Older sounds have weaker source evidence

Forty-four files are currently credited using project-default licensing rather than explicit per-file license metadata; one is `eject.ogg`, which has the additional source identified above. This leaves 43 other files credited on the default basis. This is an upstream licensing statement, but weaker provenance than a named creator's original recording and license. A repository cannot authorize rights it does not own. Missing per-file metadata is not itself evidence of infringement.

This group includes the ten tgstation ambience files, many machine sounds, the twelve radiation pulses, `radiation.ogg`, `shuttlecalled.ogg`, and `announce_syndi.ogg`. The inventory identifies each such row. Trace original sources or replace these if the desired release standard is documented original-author provenance for every sound. No claim is made that any particular sound was taken from a film, television show, or commercial game.

`redalert.ogg` has an explicit SS14 BY-SA entry pointing to historical Skyrat/Citadel commit `2d4f2d1b489590b559e4073f41b126cef56f4c50`. That historical README also states the BY-SA asset default. This supports the recorded license but does not independently establish original authorship; the history reviewed did not resolve the ultimate recording source.

## Confirmed positive evidence

- All 68 local files matched upstream bytes, not just filenames. Git blob and SHA-256 hashes are preserved in the inventory.
- The three Starlight files each have explicit CC0 attribution entries at the credited revision. The fork's separate code licensing does not replace these per-asset entries.
- Both `bottle_clunk` files have explicit SS14 CC BY 4.0 entries, and the original [volivieri recording](https://freesound.org/people/volivieri/sounds/37190/) confirms that license. Existing credits name the upstream editors.
- `soft_thump.ogg` has an explicit SS14 CC BY 4.0 entry naming CheChoDj and FairlySadPanda. The original Freesound page could not be retrieved in this audit, so that original-page check remains unverified.
- `release/assets.ron` places sounds in the public file group. `src/bin/pack-assets.rs` copies public files and credits; `tools/release-steam.ps1` checks their presence/count.
- All 68 files under `target/steam/windows-content/assets/sounds` match current workspace bytes. The staged `CREDITS.md` also matches, including the identified documentation issues. This is an inspection of existing staging, not an upload or a fresh Steam installation test.

## Evidence and limits

[Inventory](audio-license-inventory.json) records every filename, claimed license, credit, source URL, revision, hashes, upstream match, and staged match. [Upstream evidence](audio-license-upstream-evidence.json) preserves the fetched README/attribution documents and retrieval results, including missing candidate manifests. No audio was replaced and no release was uploaded during this audit; the production credits have not yet been corrected.

The checks fetched conventional attribution/license filenames in the selected source directories and their ancestors: `attributions.yml` for SS14 and Starlight; `attributions.txt`, `attribution.txt`, `license.txt`, and `SoundSources.txt` for tgstation, plus READMEs. They did not exhaustively inspect every differently named repository document or every historical revision. Older tgstation rows were compared against live `master`; the head observed during this run was `8840d67cb0760f4bfa87cd1c87432bf3319caeb7`. The fetched documents are preserved rather than relying solely on mutable links. SS14 and Starlight rows used the revisions recorded in the inventory.

Original source pages were checked selectively, including all sources implicated in the discovered credit errors. The audit did not independently verify every contributor's ownership, compare decoded audio against every original recording, conduct an audio fingerprint search, or assess fonts, images, models, code licenses, a final EULA, or trailers. A successful byte match proves identity with upstream, not ownership of copyright.

For a low-uncertainty commercial release: correct the credits, resolve or replace the scanner and reaction composite, decide whether to retain BY-SA audio after reviewing its actual use, and preserve evidence with the release. Prefer directly traceable CC0/original replacements when a source chain cannot be established. Obtain an IP lawyer's review for the remaining ShareAlike/application questions rather than treating this audit as a guarantee against claims.
