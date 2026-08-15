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

**Compliance notes:**
- CC0 files need no credit and carry no restriction — this table lists them anyway for traceability.
- CC BY-SA 3.0 files require attribution (this table serves as that) and, if redistributed as a standalone asset (not just baked into the compiled game), must remain under CC BY-SA 3.0 themselves. It does not affect ChemGame's own code license.
- Excluded during sourcing: anything under CC-BY-NC (noncommercial-only — incompatible with a commercial ChemGame release), the handful of tgstation sounds licensed through third-party proprietary EULAs (Zapsplat, Uppbeat.io, SoundFishing) since those need their own separate compliance steps, and one candidate (`sound/effects/chemistry/bufferadd.ogg`) whose only license reference was a Freesound page that has since been deleted and so couldn't be verified.
