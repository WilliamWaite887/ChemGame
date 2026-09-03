# Tutorial and laboratory textbook

Implementation date: 2026-09-02. Human comprehension and timing acceptance remain pending.

## Approved experience

Training is a separate, optional, solo session in a compact laboratory. The core
course targets 10–15 minutes of active play without deadlines. Seven independent
practice lessons target 3–5 minutes each. A Lab Instructor supplies brief written
context; objectives, physical observations, and optional hints do the teaching.

The learning cycle is **predict, experiment, inspect, adjust**. Players should
leave able to prepare and deliver a small order, diagnose an unexpected mixture,
and find a system explanation independently. No quiz is required.

Chemistry and injuries remain real. Exercises provide free supplies and restart
at their authored beginning. Completion and skipping are distinct. Returning to
training resumes the unfinished exercise with fresh fixtures rather than restoring
its live world. A career never requires training and receives no training items,
research, recipe knowledge, relationships, or campaign unlocks.

Main menu Training provides the core course, individual exercises, and free
practice. Pause provides help, three levels of hint, exercise restart, skipping,
lesson selection, free practice, and leaving. After 45 active seconds without
meaningful progress, guidance offers a hint once; reading and reaction processing
do not consume that allowance.

Clarity revision requested on 2026-09-02: inspection is required once, when the
first bottle is introduced. Later samples go straight to analysis and later
bottles can be delivered without opening inspection. Analyzer measurements remain
part of the preparation lessons. Each first hint gives the concrete next action;
subsequent help addresses controls and common obstacles. The final exercise also
shows its current objective instead of withholding the next step.

A compact text box automatically marks the current machine, person, or pickup.
Pickup boxes follow actual item positions, including dropped glassware. Once an
item is carried, guidance points to the destination; if loaded elsewhere, it
points back to that machine for ejection. Boxes hide while reading or using a
panel and disappear when the exercise ends. They use current control bindings
and never name unmeasured ingredients.

## Course and exercise source map

| ID | Objective and completion evidence | Source and implementation |
|---|---|---|
| bearings | Meet instructor; pick up, select, load and retrieve glassware; read textbook | docs/chemistry-player-guide.md (first shift); containers, interaction, textbook |
| request | Hear, accept, and review a 10u Kelotane request | conversation-first-orders.md; order_intake and order directory |
| batch | Consult book and prepare clean 10u Kelotane | docs/chemistry-player-guide.md (foundation medicines); chemistry data and ChemMaster 5000 |
| delivery | Analyze, bottle, inspect and successfully deliver 10u | label-first-chemistry-plan.md; analyzer, mixer, inspection and actual delivery grading |
| mistake | Analyze the labelled sample; recover clean medicine by correction, separation or replacement | docs/chemistry-player-guide.md (troubleshooting); labels and actual chemistry |
| independent | Prepare and verify 20u; fulfil two successive 10u bottle requests with a visible current objective | Same real preparation and delivery path |
| measurement | Prepare 10u and 20u, divide material, recombine 30u | docs/chemistry-player-guide.md (ratios); proportional transfers |
| reports | Analyze, print, change and reanalyze a labelled sample | label-first-chemistry-plan.md; immutable analyzer reports |
| thermal | Warm Nitrogen, eject it, cool below 300K | docs/chemistry-player-guide.md (temperature); thermostat and ambient heat exchange |
| ph | Adjust Water, use pH paper, correct toward neutral | docs/chemistry-player-guide.md (pH); consumed buffer adjustments and strips |
| extraction | Grind Aloe, separate Kelotane, compare HPLC recovery | Current produce data; grinder, mixer and purification |
| application | Package two forms; administer Kelotane to a burned practice patient; observe recovery | docs/chemistry-player-guide.md (delivery routes); packaging and metabolism |
| spills | Make a Water pool, observe a small Potassium–Water reaction and clear the bay | chemistry-simulation.md and chemistry-tutorial-scenarios.md; real floor chemistry |

Core recipes use only the starting methods. The supplied mistake sample contains
10u Kelotane and 5u Silicon, labelled “Kelotane.” Its contents are not revealed by
the objective before analysis. Adding 5u Carbon yields clean 20u Kelotane; remaking
is equally valid. Separating the original 10u of Kelotane also completes recovery.
The independent exercise still requires a verified 20u batch.

Implementation correction: ordinary bottles are single doses and Kelotane above
15u is graded as an overdose. The independent exercise therefore divides its 20u
batch into two 10u bottles for successive customers. It requires two successful
measured handovers, preserving normal chemistry and delivery grading. The original
20u single-bottle specification cannot pass those existing rules.

Concept-only topics cover catalysts, valid staged agitation, competing reactions
and advanced synthesis without revealing new formulas or inventing fake reactions.
Voluntary off-script discoveries use normal chemistry but stay in the training
session. No advanced sample is supplied as a shortcut to a recipe unlock.

The experiment bay has a training reset control, also available through Pause.
This clears training spills mechanically. It does not introduce a chemical cleaner
sample that would reveal a new recipe on analysis. Career cleanup chemistry is
explained in the textbook. Restart Exercise additionally restores health and all
fixtures; resetting only the spill bay does not heal the player.

## Textbook content map

The textbook is available immediately through Pause in all play modes, and never
changes Knowledge. Its 23 structured pages use short purpose, action, observation,
and experiment blocks, with small diagrams and related links. Each page has a
140-word maximum; titles and keywords are searchable. Current key bindings replace
control tokens. A glossary and symptom search links provide alternate entry points.

| Page IDs | Source material | Current gameplay authority |
|---|---|---|
| handling, ratios, experiments | chemistry-player-guide.md: first shift, short version | containers, knowledge, ChemMaster 5000 and chem_sim |
| labels | label-first-chemistry-plan.md: inspection | inspection and labels |
| chemmaster5000, chamber, mixer | docs/chemistry-player-guide.md (equipment) | machines and UI |
| analyzer | label-first-chemistry-plan.md: reports | analysis_reports |
| grinder, storage | docs/chemistry-player-guide.md (equipment); NPC RPG integration plan | Current produce catalogue, shift requisitions and storage |
| hplc, purity | docs/chemistry-player-guide.md (HPLC and quality) | Actual purification, retained volume and rejection rules |
| temperature, buffers, catalysts, agitation, chains | docs/chemistry-player-guide.md (conditions and process) | Reaction definitions and resolver; current machine activation |
| materials, hazards | chemistry-simulation.md; tutorial scenarios | chem_world, hazards, material resolver and bodies |
| orders | conversation-first-orders.md | Current intake and grading |
| packaging, dose | docs/chemistry-player-guide.md (delivery routes) | Packaging handlers and body absorption/metabolism |
| glossary | Terminology from the above | Cross-links to the corresponding pages |

Source corrections applied during adaptation:

- Base reagent stock starts available; recipe knowledge is not reaction eligibility.
- Botanical supplies are purchased; old automatic-delivery prose is not authoritative.
- Removing a sample from a heater does not inherently halt chemistry.
- Labels and printed measurements have different semantics.
- Matching pH does not prove absence of residual reactive material.
- HPLC improves retained purity at a yield cost; mixer extraction is distinct.

The last article and scroll position are remembered within a play session. Escape
steps from article to contents, then Pause. Solo reading pauses time. In co-op,
input is held but the shared world continues, with a visible notice.

## Runtime boundaries

`SessionKind` separates Career and Training independently from networking mode.
Training uses Singleplayer, has no SaveSlot, and does not assign a campaign. Career
generators, threat progression, background dialogue and incident scheduling are
gated separately from physical chemistry, machine processing and injuries.

`TutorialPlugin` owns authored lesson IDs, current exercise evidence, hints and
fixtures. Evidence comes from resulting state or a committed handoff, never from
an attempted request alone. Delivery evidence includes actual composition before
any label-belief override. Exercise generations reject stale handoff events.
Seeded bottles are not credited as newly packaged outputs. A consumed but unsuitable
handoff gets a replacement practice request rather than leaving the exercise stuck.

The map is `assets/maps/tutorial.map`. Its `training_spot` markers own instructor,
customer and departure, patient, supplies, sample, spawn, annex and cleanup locations. Existing
machine markers and queue machinery are reused. The non-TrenchBroom build reads
the same layout for its simple geometry and fixtures.

Analysis and HPLC purification are available on every analyzer from the start.
Training uses the same capability and authority validation as a career rather
than carrying a special calibration bypass.

Training progress is a separate versioned `training.ron` file alongside the slot
directories. It records completed IDs, skipped IDs and the resume exercise, not
the world or notebook. Unknown versions fall back to fresh progress. Restarts pass
through normal session teardown to clear machines, actors, hazards and UI state.
Keep Experimenting instead retains the current lab and its discoveries; its core
resume bookmark remains available.

## Validation

Commands and final results are recorded in tutorial-verification.md. Focused guards
cover authored references, recipe disclosure, map connectivity, analyzer availability,
progress serialization, actual machine/analysis/packaging/delivery behavior, and
stale evidence. The opt-in debug `--tutorial-playtest` harness uses isolated appdata
and exercises real handlers while relocating the player between workstations.
Relocation is a harness shortcut, not evidence of human navigation usability.

Human acceptance remains a required playtest, with at least five unfamiliar players:

- At least four complete the core course within 15 active minutes without coaching.
- At least four complete independent delivery without the most explicit hint.
- Players explain ratios, why analysis matters, and a useful next diagnostic step.
- Players find a requested textbook answer within 20 seconds.
- Everyone recovers from a mistake without restarting the whole course.

Revise unclear interactions and observations before increasing text length.

## Verification cases in the source map

| Content | Verification case |
|---|---|
| Core IDs bearings through independent | `tools/run-tutorial-playtest.ps1`; stage-by-stage results in `target/tutorial-playtest/verified-core.txt` |
| Measurement, reports, thermal, pH, extraction, application, spills | The same driver with `-Lesson measurement`, `reports`, `thermal`, `ph`, `extraction`, `application`, or `spills`; separate verified result files |
| Correction and replacement solutions | `tutorial::tests::measured_mistake_accepts_both_correction_and_fresh_preparation` |
| Scan, package and handoff authorization | `tutorial::tests::actual_preparation_analysis_packaging_and_delivery_complete_only_after_verified_handoff` |
| Purification capability | `tutorial::tests::hplc_is_available_in_training_and_career_without_unlocking_recipes` |
| Full inventory | `tutorial::tests::full_inventory_keeps_packaged_material_available_for_recovery` |
| Restart/resume identity | `tutorial::tests::restarting_discards_pending_actions_and_evidence_but_keeps_profile`; existing session teardown tests |
| Every article and linked lesson | `textbook::tests::textbook_content_is_short_linked_and_complete` and `tutorial::tests::training_content_links_and_goals_are_valid` |
| Bindings, search and reading position | `textbook::tests::search_and_bindings_are_live` and `reading_preserves_each_articles_scroll_and_resolves_all_control_tokens` |
| Escape hierarchy | `textbook::tests::escape_visits_contents_then_pause_before_resuming` |
| Solo/host/client textbook and time behavior | `tools/run-textbook-playtest.ps1 -Mode solo` and `-Mode coop` |
| Authored map markers and paths | `tutorial::map::tests::training_map_has_all_markers_and_connected_workstations`; rendered customer travel |

A listed case is a traceable validation route, not a claim that it passed. See
`docs/tutorial-verification.md` for the actual executed results and remaining
player acceptance work. The rendered driver relocates the player and uses real
machine, chemistry, intake and delivery handlers. It suppresses live keyboard
and mouse input while running; ordinary play never installs that driver.

The opt-in `-Lifecycle` scenario also verifies every core resume boundary, dirty
exercise teardown, two separate career creations and unchanged career/global
files around training. `keep_experimenting_retains_the_lab_and_resume_bookmark`
checks that continuing in the current lab does not erase its contents or bookmark.
