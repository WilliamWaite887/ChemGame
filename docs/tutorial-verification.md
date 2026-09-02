# Tutorial verification

Implementation: 2026-09-02. This document separates executed software checks
from the required unfamiliar-player study.

## Automated checks

The focused tests exercise actual authorized machine actions, analyzer snapshots,
packaging and committed delivery outcomes. They include correction and fresh
preparation, separation, calibration restrictions, stale evidence, full inventory,
profile serialization, control substitution and textbook navigation.

Run from the repository root:

```powershell
cargo test --workspace --offline
cargo test --workspace --no-default-features --offline
cargo check --all-features --offline
cargo clippy --workspace --all-targets --offline
cargo build --offline
```

The initial implementation workspace run passed 1,133 game tests, 196 chemistry/support
tests and five doctests (1,334 total). That development executable built
successfully. The initial fallback workspace run passed 1,112 game tests plus the same
201 supporting tests/doctests (1,313 total). `cargo check --all-features` passed.

Clippy completed with 86 binary warnings and 80 test-target warnings (77 shared),
with no diagnostics in the new tutorial/textbook modules. Existing diagnostics
include unused imports, dead code and type complexity. The run also reported an
incremental-cache artifact that rustc automatically ignored and a Steam DLL copy
warning while a rendered process held the existing DLL. These were build-environment
warnings; the commands completed successfully. The final build reported ten
existing code warnings. Logs are under `target/tutorial-*.txt`.

## Rendered scenarios

```powershell
tools/run-tutorial-playtest.ps1
tools/run-tutorial-playtest.ps1 -Lesson measurement
tools/run-tutorial-playtest.ps1 -Lesson reports
tools/run-tutorial-playtest.ps1 -Lesson thermal
tools/run-tutorial-playtest.ps1 -Lesson ph
tools/run-tutorial-playtest.ps1 -Lesson extraction
tools/run-tutorial-playtest.ps1 -Lesson application
tools/run-tutorial-playtest.ps1 -Lesson spills
tools/run-tutorial-playtest.ps1 -Lifecycle
tools/run-textbook-playtest.ps1 -Mode solo
tools/run-textbook-playtest.ps1 -Mode coop
```

Build the executable first. These Windows scripts create isolated appdata folders
and restore their process environment after launch. They never use a player's
career saves. The default graphics backend is DX12; the tutorial script also
accepts `-Backend vulkan`. A working desktop graphics environment is required.

The debug-only tutorial driver executes real actions and waits for exercise
evidence. It relocates the player between stations and supplies held/loaded
relations in some equipment steps; it is not a human navigation or motor-input
test. Live keyboard and mouse input are suppressed only in the opted-in driver.
Screenshots and per-exercise result files are written under
`target/tutorial-playtest`. Textbook solo, host and client results are under
`target/textbook-playtest` and check the simulation clock while reading.

The initial desktop runs identified missing instruction about interacting a
second time to open loaded equipment, incorrect texture projection on training
brushes, a misleading career-shift banner, and test-driver sequencing issues.
The checks also found an impossible 20u single-dose Kelotane bottle (normal
overdose limit: 15u), a patient hint using a self-applied patch, and a driver
argument-concatenation bug. The independent course now uses two 10u bottle
handoffs from a verified 20u batch; patient practice uses a 5u syringe. Driver
sequencing, reach points, and argument construction were corrected. Desktop
launches were used for visual inspection.

The initial driver sent instructor interaction directly to the gameplay handler.
It therefore missed a missing Interactable component on training characters.
The character factory now supplies that component; the revised rendered driver
requires the real crosshair target and rebound interaction/application key paths
for the instructor and patient. Initial results below do not establish those
input paths.

## Initial executed rendered results

- Core course: PASS, all six exercises, including both final 10u handovers.
- All seven optional lessons: PASS.
- Solo textbook: PASS, with simulation-clock pause checked.
- LAN host and client textbook: PASS, with the shared clock continuing.
- Lifecycle scenario: PASS at all six resume boundaries and across two careers.
- Layouts inspected at 1280×720: training lab, textbook index/article, and
  exercise completion controls. Other resolutions still need a visual sweep.

The bay uses a facility reset control to clear spills; it does not supply an
advanced chemical cleaner or teach its recipe. The textbook explains career
cleanup. This adaptation and the split final delivery are recorded in the design.

## Unfamiliar-player acceptance — not yet performed

Recruit at least five people who have not played ChemGame. Use fresh training
profiles. Explain only how to start Training, then observe without coaching.
Record interruptions separately from active play time. Let players request the
in-game hints and reset controls normally.

| Player | Core active minutes | Finished without coaching | Independent delivery without hint 3 | Textbook answer seconds | Recovered without restarting course | Notes |
|---|---|---|---|---|---|---|
| P1 | | | | | | |
| P2 | | | | | | |
| P3 | | | | | | |
| P4 | | | | | | |
| P5 | | | | | | |

After the course, ask each player to explain:

- How to scale a two-ingredient recipe to a different order size.
- Why the label and a measurement can disagree.
- What they would check when a mixture did not react.

Ask them to find one textbook answer, such as the difference between purity and
contamination, and time the lookup. Ask them to recover from a supplied mixture
mistake using whatever method they prefer.

Acceptance targets: at least four finish within 15 active minutes without
coaching, at least four complete independent delivery without hint 3, answers
show understanding, textbook lookup takes at most 20 seconds, and every player
recovers without restarting the whole course. These are targets, not established
results. Revise the interaction, objective or feedback before adding more prose.

The rendered lifecycle scenario reconstructed every core boundary after injecting
a powered thermostat, a sample, a spill, incapacitation, queued analysis, and
alternating machine/conversation modes. It then opened two normal careers and
compared saved career and global account bytes before and after training. All
checks passed. These are controlled test setups, not a substitute for exploratory
play during every possible reaction or UI state.

Manual follow-up covers sustained free experimentation, reaction-specific resets,
human navigation, the fallback renderer, and readability across the supported
display settings. LAN
textbook testing also emitted the existing replicated-hierarchy B0004 warnings;
those warnings were not fixed by this feature.

## Clarity and object-marker follow-up

Requested 2026-09-02: teach inspection once, make hints direct, and automatically
mark the current machine or pickup with a text box. The focused suite checks one
authored inspection stage, both final handoffs without inspection, valid control
tokens, and marker selection as items are picked up, dropped, or loaded elsewhere.
A separate marker check ensures a labelled sample does not expose its contents.

The focused follow-up suite passed all 17 tutorial tests. Per the user's request,
the revised game was not launched for rendered testing; the user will check the
instructor interaction, callout placement, and course flow manually. The revised
crosshair-input test driver has not established a passing rendered result.

The follow-up development build passed with the same ten existing code warnings.
The default workspace suite passed 1,136 game tests, 196 supporting tests and five
doctests (1,337 total). This includes the instructor's interaction-component
regression, both deliveries without repeated inspection, and item-marker tests.

Follow-up clippy completed with the existing warning baseline and no diagnostics
in tutorial or textbook code. Its one new match-style suggestion was applied;
clippy was rerun successfully. This last rewrite preserves the tested condition.
