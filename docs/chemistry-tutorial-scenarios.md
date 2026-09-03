# Chemistry tutorial scenarios

These are authoring fixtures for a future tutorial, not a claim that an
interactive tutorial has been implemented. Use an isolated save. For hazardous
lessons use a test subject or a disposable test scene, away from ordinary crew.

Read [simulation rules](chemistry-simulation.md) and the
[handbook](chemistry-player-guide.md). Verification IDs refer to named automated
tests. Amounts below are in-game units, not real-world instructions.

## Lesson 1 — Reliable preparation and inspection

- **Prerequisites:** ChemMaster 5000, beaker and analyzer; ordinary starting stock.
- **Setup/actions:** Add 5u each Silicon, Nitrogen and Potassium. Inspect the
  finished mixture. Prepare Inaprovaline separately from 5u each Oxygen, Carbon
  and Sugar.
- **Observation/success:** 15u Dylovene and 15u Inaprovaline, without a material
  hazard. Completed medicine does not retain elemental potassium's reactivity.
- **Mistake/recovery:** Unrelated extras are contamination; separate or discard
  them using existing equipment. Do not add water to leftover raw potassium.
- **Reset:** Empty containers and start again.
- **Verification:** `sb09`; existing synthesis and agitation suites.

## Lesson 2 — A small mistake versus a substantial mixture

- **Prerequisites:** Safe test scene, ChemMaster 5000, analyzer and two beakers.
- **Setup/actions:** Compare 1u Potassium + 1u Water against 10u of each. In a
  separate 100u Large Beaker add 80u inert Nitrogen before repeating the latter
  mixture (100u total).
- **Observation/success:** Water reactivity consumes both explicit ingredients,
  produces Ash and releases energy. The small mix has a smaller effect. Dilution
  lowers heating, but does not remove the reactive material or its total energy.
- **Mistake/recovery:** Do not teach dilution as making an explosive dose safe.
  Move others away before testing. Discard or clean remaining material and use
  established trauma/burn treatment after injury.
- **Reset:** Replace damaged glassware; reset the test scene after the large mix.
- **Verification:** `sb01`, `sb02`; trace-size checks use the engine fixture.

## Lesson 3 — Chemistry on the floor

- **Prerequisites:** Two separately prepared beakers and a clear floor area.
- **Setup/actions:** Pour 5u Potassium and 5u Water into touching pools. Repeat
  with the pools separated by a solid wall or placed on distinct floor levels.
- **Observation/success:** Touching pools combine, react once and retain 10u Ash.
  Blocked or vertically separated pools remain independent. The reaction does
  not require a workstation.
- **Mistake/recovery:** Pool radius matters. Keep unplanned spills outside the
  experiment; use existing cleaner and replace the setup if they touch.
- **Reset:** Clean all pools before the next attempt.
- **Verification:** `sb11`, `sb12`, `sb13`.

## Lesson 4 — Neutralization is a material change

- **Prerequisites:** Prepared Lye, Sulphuric Acid, beaker and analyzer.
- **Setup/actions:** Combine 2u of each. Observe for two seconds and inspect.
- **Observation/success:** Acid and base are consumed into 4u Neutralized Salts.
  At completion the salts have neutral pH. Neither original reagent remains.
- **Mistake/recovery:** Unequal amounts leave excess reactive material. Add the
  missing counterpart cautiously, or discard the mixture. A neutral-looking pH
  during a mixture's progress does not prove the original substances are gone.
- **Reset:** Discard the salts; they do not replace a named recipe ingredient.
- **Verification:** `sb03`. Analyzer feedback is part of rendered review.

## Lesson 5 — Ignition and oxidizers

- **Prerequisites:** Heater, Oil, Oxygen, separate large beakers, clear workspace.
- **Setup/actions:** Leave 10u Oil + 10u Oxygen at room temperature. Separately
  compare 10u Oil alone and the oxygenated mixture at 550K.
- **Observation/success:** The room-temperature mixture is stable. Heated fuel
  burns to Ash; explicit oxygen accelerates consumption compared with room air.
  Burning ends when compatible fuel is exhausted. An extinguisher also cools affected floor mixtures, so
  residual heat alone does not immediately reignite them.
- **Mistake/recovery:** Keep distance from activated mixtures. Cool/remove the
  heat source before preparing another batch. For floor fires, use the existing
  extinguisher chemistry rather than assuming every liquid extinguishes fire.
- **Reset:** Cool equipment, clean spills and refill fresh containers.
- **Verification:** `sb04`, `sb07`; inspect fire and audio in the rendered scene.

## Lesson 6 — The body is another reaction location

- **Prerequisites:** Isolated test subject, separately packaged potassium/water
  doses, established trauma and burn treatments.
- **Setup/actions:** Compare a swallowed potassium dose alone with separate
  potassium and water doses given before the next digestion beat. Use the
  engine fixture for exact 20u pill comparisons; retain actual route absorption.
- **Observation/success:** Swallowed reactions wait for a metabolism beat.
  Potassium alone encounters limited body water; explicit water can cause a
  stronger burst. A strong internal explosion injures the subject and nearby
  bodies while leaving the subject entity available for recovery.
- **Mistake/recovery:** The delay is not a guaranteed two-second rescue window.
  Stop further dosing, clear bystanders, then use existing medical recovery.
  There is no stomach-purge action in this feature.
- **Reset:** Restore the test subject between comparisons.
- **Verification:** `sb05`, `sb06`, `sb08`, `sb14`.

## Lesson 7 — Products, chains and transport

- **Prerequisites:** Knowledge of the preceding lessons and the existing smoke
  and foam tools.
- **Setup/actions:** Prepare 2u Ash + 2u Sodium Chloride and heat to 390K for
  Multiver. For an exact chain, the `sb19` engine fixture starts with 10u
  Potassium + 10u Water + 1u Oil + 1u Oxygen at 470K and advances 0.1 seconds.
  This preloaded fixture avoids preparation timing differences. Separately,
  `sb15` triggers a smoke report from a 20u Water puddle and inspects the
  remaining pool and carried payload. These two setups are test fixtures,
  not buttons that exist in the game UI.
- **Observation/success:** Ash is reusable chemistry, not a new recipe card for
  every way it was produced; the first setup yields 4u Multiver. In the chain,
  water reactivity heats the mixture past Oil's activation temperature; 0.4u Oil
  burns in the same step, leaving 0.6u. Total mixture volume stays 22u. Smoke
  transfers a 10u payload, leaving 10u behind; total volume stays 20u.
- **Mistake/recovery:** Smoke carries the real mixture, including contaminants.
  Keep others away and let it disperse before resetting.
- **Reset:** Clear the test scene and refill fresh reagents.
- **Verification:** `sb15`, `sb17`, `sb19`, existing recipe-chain tests and `sb16` book checks.

## Verification ledger

Validation date: 2026-09-02.

- `cargo test --workspace --offline`: 1,316 passed, zero failed or ignored.
  This includes 196 simulation tests, 1,115 game tests and five doctests. All
  `sb01`�`sb19` guards pass, alongside existing medicine, staged processing,
  orders, progression, recovery and serialization coverage.
- `cargo clippy --workspace --all-targets --offline`: completed successfully.
  Warnings remain, chiefly ECS query complexity and argument counts; this is
  not a warning-free workspace. The material simulation has no clippy warnings.
- Detailed automated output: `target/sandbox-workspace-tests-final.txt` and
  `target/sandbox-clippy-final.txt` (local generated artifacts).

Automated checks establish quantities, timing, boundaries, conservation and
serialization. They do not establish whether an unfamiliar player understands
the sound, can read a changing pool, or finds a lesson enjoyable.

`cargo build --offline` passed. The debug-only rendered harness can be rerun with
`./tools/run-chemistry-playtest.ps1 -Mode solo` or `-Mode coop`; it uses isolated
application-data directories and writes evidence under `target/chemistry-playtest`.
The co-op script waits for the host's sandbox result before launching the client.

- **Host/client with late join: PASS.** Both reports verify a merged 10u Ash
  spill and an intact collapsed body with the exact damage from one internal
  blast. The workstation scenario also verifies all three agitation directions,
  packaging without duplication, shared analysis reports and truthful contents
  beneath a misleading label. Final reports were written at 15:45 local time.
- **Solo: PASS.** The same sandbox, preparation, packaging and report checks
  passed on the final executable at 15:47 local time. No hierarchy warnings or
  game panic were recorded in the solo run. Evidence: `solo/result.txt` and the
  screenshots beside it.
- **Rendered book review:** The field manual shows 145 methods, with medicine
  entries and progression still present. The residue-filter test verifies the
  17 excluded cards across the full catalog. Screenshots: `host/recipe-book.png`
  and `client/recipe-book.png` beneath the evidence directory.
- **Visual/network issue still open:** The late client logs 54 Bevy `B0004`
  hierarchy warnings for other scene entities. The spill root/particle warning
  was fixed by making visibility a required puddle component and was absent in
  the final run. Successful chemistry-state checks do not establish that the
  rest of the scene hierarchy is visually correct. GPU/Steam startup warnings
  are also present in the captured logs.

The sandbox fixtures are placed off-station to avoid disrupting the workstation
scenario. They verify simulation and replication, not a camera view of every
hazard. **Pending human playtest:** readability of changing pools and analysis
property text, reaction sound clarity, internal-blast character presentation and
Medical recovery in play, whether small mistakes feel recoverable, and whether
an unfamiliar player can explain each lesson without new preparation chores.
No interactive tutorial or player-comprehension study is claimed.

Additional integration guard `sb18` verifies that idle machine reservoirs use
these rules without replaying effects. The existing carbon-dioxide test now
advances a heated fuel spill after extinguishing it, checking that it stays out.

For tutorial review, ask the player to predict what will happen, explain the
observed result, and choose a recovery action. Repeated ingredient babysitting
or an unexplained failure is a design issue, not a reason to add more tutorial
text. Keep exact experimental checks in this document and approachable
explanations in the handbook.
