# Third-party asset credits

## Sound effects — sourced from tgstation

The files below in [assets/sounds/](assets/sounds/) are sourced from the
[/tg/station 13](https://github.com/tgstation/tgstation) game codebase
(`sound/` directory), unmodified unless noted.

tgstation's assets are **not** uniformly licensed — see their
[README](https://github.com/tgstation/tgstation/blob/master/README.md) and
[sound/attributions.txt](https://github.com/tgstation/tgstation/blob/master/sound/attributions.txt).
Their project-wide default is *"all assets including icons and sound are
under a Creative Commons 3.0 BY-SA license unless otherwise indicated."*
Every file below was checked against that override list (and any per-folder
`license.txt`/`attribution.txt`/`SoundSources.txt`) before inclusion; files
that turned out to be CC-BY-NC, proprietary-EULA, or unverifiable (a source
Freesound page that had since been deleted) were excluded rather than guessed at.

| File | tgstation source | License | Original author | Source |
|---|---|---|---|---|
| `dispense_pour.ogg` | `sound/effects/liquid_pour/liquid_pour1.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/effects/liquid_pour/liquid_pour1.ogg |
| `eject.ogg` | `sound/machines/eject.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/machines/eject.ogg |
| `buffer_transfer.ogg` | `sound/effects/bubbles/bubbles.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/effects/bubbles/bubbles.ogg |
| `reaction_occurred.ogg` | `sound/effects/chemistry/catalyst.ogg` | **CC0** | Melthurian ("Bubbling beaker.wav") | https://freesound.org/people/Melthurian/sounds/319384/ |
| `hazard_smoke.ogg` | `sound/effects/gas_hissing.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/effects/gas_hissing.ogg |
| `hazard_explosion.ogg` | `sound/effects/explosion/explosion1.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/effects/explosion/explosion1.ogg |
| `major_alarm.ogg` | `sound/machines/fire_alarm/fire_alarm1.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/machines/fire_alarm/fire_alarm1.ogg |
| `fall.ogg` | `sound/effects/bodyfall/bodyfall1.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/effects/bodyfall/bodyfall1.ogg |
| `grinder.ogg` | `sound/machines/blender.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/machines/blender.ogg |
| `door_open.ogg` | `sound/machines/airlock/airlock.ogg` | CC BY-SA 3.0 (tgstation project default — not listed in that folder's `attributions.txt`) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/machines/airlock/airlock.ogg |
| `door_closed.ogg` | `sound/machines/airlock/airlockclose.ogg` | CC BY-SA 3.0 (tgstation project default — not listed in that folder's `attributions.txt`) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/machines/airlock/airlockclose.ogg |
| `order_success.ogg` | `sound/effects/achievement/beeps_jingle.ogg` | **CC0** | Eponn | https://github.com/tgstation/tgstation/blob/master/sound/effects/achievement/beeps_jingle.ogg |
| `radio_blip.ogg` | `sound/effects/achievement/glockenspiel_ping.ogg` | **CC0** | FunWithSound | https://github.com/tgstation/tgstation/blob/master/sound/effects/achievement/glockenspiel_ping.ogg |
| `requisition_confirm.ogg` | `sound/machines/coindrop.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/machines/coindrop.ogg |
| `ui_click.ogg` | `sound/machines/click.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/machines/click.ogg |
| `ui_refused.ogg` | `sound/machines/buzz/buzz-sigh.ogg` | CC BY-SA 3.0 (tgstation project default) | tgstation contributors | https://github.com/tgstation/tgstation/blob/master/sound/machines/buzz/buzz-sigh.ogg |
| `ambience/ambigen1.ogg` – `ambigen4.ogg`, `ambigen9.ogg`, `ambigen10.ogg`–`ambigen13.ogg`, `shipambience.ogg` | `sound/ambience/general/` (same filenames) | CC BY-SA 3.0 (tgstation project default — no attribution file in that folder) | tgstation contributors | https://github.com/tgstation/tgstation/tree/master/sound/ambience/general |

The radio channel-ident palette reuses the credited one-shots above: Medical uses the scanner finish, Security the refused buzz, Engineering the buffer bubbles, Cargo the requisition coin, Service the package pop, Bridge the UI click, and station-wide priority traffic the major alarm. No additional uncredited audio asset is introduced by that palette.

**Compliance notes:**
- CC0 files need no credit and carry no restriction — this table lists them anyway for traceability.
- CC BY-SA 3.0 files require attribution (this table serves as that) and, if redistributed as a standalone asset (not just baked into the compiled game), must remain under CC BY-SA 3.0 themselves. It does not affect ChemGame's own code license.
- Excluded during sourcing: anything under CC-BY-NC (noncommercial-only — incompatible with a commercial ChemGame release), the handful of tgstation sounds licensed through third-party proprietary EULAs (Zapsplat, Uppbeat.io, SoundFishing) since those need their own separate compliance steps, and one candidate (`sound/effects/chemistry/bufferadd.ogg`) whose only license reference was a Freesound page that has since been deleted and so couldn't be verified.

## Sound effects — sourced from Space Station 14

These are unmodified copies from [Space Station 14](https://github.com/space-wizards/space-station-14) revision [`f76827c45504f263fc540bc11a1ec7cde6c16977`](https://github.com/space-wizards/space-station-14/tree/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio). SS14's default for assets not named by an attribution manifest is CC BY-SA 3.0; the license-basis column distinguishes that default from explicit per-file metadata. CC0 files are included for traceability.

| File | SS14 source | License | License basis | Original author / credit | Original source |
|---|---|---|---|---|---|
| `ss14/bubbles.ogg` | `Resources/Audio/Effects/Chemistry/bubbles.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned SS14 file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/Chemistry/bubbles.ogg) |
| `ss14/glug.ogg` | `Resources/Audio/Effects/Fluids/glug.ogg` | CC0 1.0 | Explicit `Effects/Fluids/attributions.yml` | brittmosel | [Freesound 529300](https://freesound.org/people/brittmosel/sounds/529300/) |
| `ss14/splash.ogg` | `Resources/Audio/Effects/Fluids/splash.ogg` | CC0 1.0 | Explicit `Effects/Fluids/attributions.yml` | deadrobotmusic | [Freesound 609953](https://freesound.org/people/deadrobotmusic/sounds/609953/) |
| `ss14/drink.ogg` | `Resources/Audio/Items/drink.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned SS14 file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Items/drink.ogg) |
| `ss14/hypospray.ogg` | `Resources/Audio/Items/hypospray.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned SS14 file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Items/hypospray.ogg) |
| `ss14/buzz_loop.ogg` | `Resources/Audio/Machines/buzz_loop.ogg` | CC0 1.0 | Explicit `Machines/attributions.yml` | Duasun; converted by EmoGarbage404 | [Freesound 712127](https://freesound.org/people/Duasun/sounds/712127/) |
| `ss14/scan_finish.ogg` | `Resources/Audio/Machines/scan_finish.ogg` | CC0 1.0 | Explicit `Machines/attributions.yml` | pan14 | [SS14-listed source](https://freesound.org/people/steaq/sounds/509249/) |
| `ss14/pop.ogg` | `Resources/Audio/Effects/pop.ogg` | CC0 1.0 | Explicit `Effects/attributions.yml` | mirrorcult | [Pinned original revision](https://github.com/space-wizards/space-station-14/blob/9168fc629c555b8c395d695d291faea1eeda1db6/Resources/Audio/Effects/pop.ogg) |
| `ss14/bottle_clunk.ogg` | `Resources/Audio/Items/bottle_clunk.ogg` | CC BY 4.0 | Explicit `Items/attributions.yml` | volivieri; modified by Velcroboy; mono by perryprog | [Freesound 37190](https://freesound.org/people/volivieri/sounds/37190/) |
| `ss14/bottle_clunk_2.ogg` | `Resources/Audio/Items/bottle_clunk_2.ogg` | CC BY 4.0 | Explicit `Items/attributions.yml` | volivieri; modified by Velcroboy; trimmed/mono by perryprog | [Freesound 37190](https://freesound.org/people/volivieri/sounds/37190/) |
| `ss14/sizzle.ogg` | `Resources/Audio/Effects/sizzle.ogg` | CC BY-SA 3.0 | Explicit `Effects/attributions.yml` | Recorded by deltanedas for SS14 | [SS14 commit](https://github.com/space-wizards/space-station-14/commit/0a4c16ca21e266c24243119d944cbff8084829dd) |
| `ss14/fire.ogg` | `Resources/Audio/Effects/fire.ogg` | CC0 1.0 | Explicit `Effects/attributions.yml` | raremess; edited for SS14 | [Freesound 222557](https://freesound.org/people/raremess/sounds/222557/) |
| `ss14/flash_bang.ogg` | `Resources/Audio/Effects/flash_bang.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned SS14 file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/flash_bang.ogg) |
| `ss14/slip.ogg` | `Resources/Audio/Effects/slip.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned SS14 file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/slip.ogg) |
| `ss14/radpulse1.ogg` | `Resources/Audio/Effects/radpulse1.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse1.ogg) |
| `ss14/radpulse2.ogg` | `Resources/Audio/Effects/radpulse2.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse2.ogg) |
| `ss14/radpulse3.ogg` | `Resources/Audio/Effects/radpulse3.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse3.ogg) |
| `ss14/radpulse4.ogg` | `Resources/Audio/Effects/radpulse4.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse4.ogg) |
| `ss14/radpulse5.ogg` | `Resources/Audio/Effects/radpulse5.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse5.ogg) |
| `ss14/radpulse6.ogg` | `Resources/Audio/Effects/radpulse6.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse6.ogg) |
| `ss14/radpulse7.ogg` | `Resources/Audio/Effects/radpulse7.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse7.ogg) |
| `ss14/radpulse8.ogg` | `Resources/Audio/Effects/radpulse8.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse8.ogg) |
| `ss14/radpulse9.ogg` | `Resources/Audio/Effects/radpulse9.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse9.ogg) |
| `ss14/radpulse10.ogg` | `Resources/Audio/Effects/radpulse10.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse10.ogg) |
| `ss14/radpulse11.ogg` | `Resources/Audio/Effects/radpulse11.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse11.ogg) |
| `ss14/radpulse12.ogg` | `Resources/Audio/Effects/radpulse12.ogg` | CC BY-SA 3.0 | SS14 project default | Space Station 14 contributors | [Pinned file](https://github.com/space-wizards/space-station-14/blob/f76827c45504f263fc540bc11a1ec7cde6c16977/Resources/Audio/Effects/radpulse12.ogg) |
| `ss14/female_cough_1.ogg` | `Resources/Audio/Voice/Human/female_cough_1.ogg` | CC0 1.0 | Explicit `Voice/Human/attributions.yml` | Ashe Kirk | [Freesound 151213](https://freesound.org/people/OwlStorm/sounds/151213/) |
| `ss14/female_cough_2.ogg` | `Resources/Audio/Voice/Human/female_cough_2.ogg` | CC0 1.0 | Explicit `Voice/Human/attributions.yml` | thatkellytrna | [Freesound 425777](https://freesound.org/people/thatkellytrna/sounds/425777/) |
| `ss14/male_cough_1.ogg` | `Resources/Audio/Voice/Human/male_cough_1.ogg` | CC0 1.0 | Explicit `Voice/Human/attributions.yml` | dastudiospr | [Freesound 537150](https://freesound.org/people/dastudiospr/sounds/537150/) |
| `ss14/male_cough_2.ogg` | `Resources/Audio/Voice/Human/male_cough_2.ogg` | CC0 1.0 | Explicit `Voice/Human/attributions.yml` | qubodup | [Freesound 743360](https://freesound.org/people/qubodup/sounds/743360/) |
| `ss14/soft_thump.ogg` | `Resources/Audio/Effects/soft_thump.ogg` | CC BY 4.0 | Explicit `Effects/attributions.yml` | CheChoDj; clipped by FairlySadPanda | [Freesound 609353](https://freesound.org/people/CheChoDj/sounds/609353/) |

**Compliance notes:**
- No CC-BY-NC, custom-license, or attribution-conflicted SS14 candidate was copied. In particular, `alien_spitacid.ogg`, `pill_insert.ogg`, `pill_remove.ogg`, `ice_crit.ogg`, and `jet_injector.ogg` were excluded.
- CC BY-SA files remain available as their original, unmodified `.ogg` assets under CC BY-SA 3.0. This share-alike requirement applies to those assets, not to ChemGame's Rust source code.
