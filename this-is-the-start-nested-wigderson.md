# ChemGame — Build Plan

## Status — M1–M8 complete (2026-08-11)

213 tests, clippy clean. Bevy 0.19, `bevy_replicon` 0.42, `bevy_replicon_renet` 0.18.
Singleplayer and `--host` both verified running the full game loop.

| Milestone | State |
|---|---|
| M1 `chem_sim` | done — fixed-point units, proportional transfer, priority/catalyst/temperature resolver |
| M2 lab + movement | done — 3D room from primitives, FPS controller, `MeshRayCast` interaction |
| M3 machines | done — dispenser, carryable beakers, ChemMaster buffer, pill/bottle packaging |
| M4 orders | done — crew NPCs walk in through a south doorway, handoff, 6-outcome grading |
| M5 radio + book | done — 12–38s delayed chatter, reference book on **B** |
| M6 discovery | done — test bench stock, analyzer, research points, hint purchase, `save.ron` |
| M7 co-op | **foundation done, input path remaining** — see below |
| M8 bodies + deep chemistry | done — see below |

### M8: bodies, temperature, hazards (2026-08-11)

Built against `~/.claude/plans/https-wiki-tgstation13-org-guide-to-chem-dazzling-newell.md`,
from /tg/station's Guide to chemistry. All eight planned phases landed.

**The headline: chemicals now act on people.** `crates/chem_sim/{effect,body,thermal}.rs`
hold the arithmetic (engine-free as always); `src/body/mod.rs` and `src/hazards/mod.rs`
wrap it.

- **Four damage types** (brute/burn/toxin/oxygen), a 2-second metabolism tick,
  overdose and critical-overdose tiers that *stack on* normal effects, and six
  felt statuses (sluggish/hastened/blurred/unsteady/drunk/irradiated).
- **A bloodstream is a `Solution`**, so reagents react inside you exactly as in
  a beaker. Injecting two halves of a recipe makes the product in your blood.
- **Routes matter**: inject 100% / ingest 60% through a stomach / touch 15%,
  with `Contact` damage scaled 2.0/1.0/0.5. This is what makes the new
  **syringe** (`ContainerKind::Syringe`, 15u, made at the ChemMaster) worth the trip.
- **Keys**: **R** drink or swallow, **F** apply held item (syringe draws from a
  container, injects into a body or yourself).
- **Three dead systems brought to life**: `ReactionEffect::Smoke`/`Explosion`
  now spawn clouds and blasts; `Solution.temperature` now drives a
  `MachineKind::ReactionChamber`; and six dispensables that appeared in no
  reaction (chlorine, copper, ethanol, iron, sulfur, water) got body effects.
- **Content**: 28→37 reagents, 9→15 reactions. Toxins (sulphuric acid, chloral
  hydrate, **phlogiston**), stimulants (hyperzine, synaptizine), alcohol (hooch,
  which smokes). Phlogiston is the showpiece: gated at 374K, exothermic, and it
  detonates past 420K.

  **Detonation is decided by batch size, not by elapsed time**, because the
  resolver is instant: the whole batch reacts in one pass and releases
  `Heat × scale` at once. Under ~19u of each reactant it settles below 420K; at
  20u of each it runs away. A 50u beaker cannot physically hold enough to
  detonate, which makes "make it small" a rule the player can rely on. Pinned by
  `a_big_batch_of_phlogiston_cooks_itself_into_a_blast` and
  `a_small_beaker_of_phlogiston_can_never_detonate` — the `Heat` value shipped
  at 1.2 first, where a *full* large beaker peaked at 414K and the reaction
  could never detonate at all.
- **Collapse** at 100 total damage with hysteresis to 80: drops your beaker,
  releases your machine claim, costs 3 reputation, and medical retrieve you
  after 25s. In co-op a syringe beats that clock.

**Decisions worth remembering:**

1. **No `FixedUpdate` for the metabolism tick** — a `MetabolismClock(Timer)` in
   `Update` instead. `Time<Fixed>` is a single global that would bind the whole
   app, and headless tests drive `app.update()` in a tight loop where a 2s fixed
   system would essentially never fire. Catch-up is clamped to 3 ticks.
2. **Damage never persists.** `OnEnter(ShiftPhase::Prep)` clears every body, so
   `progress.ron` needs no new field and no migration. A bad shift costs
   reputation, not the career.
3. **An instant resolver cannot be overheated by a slow ramp.** A reaction fires
   the moment the rising temperature crosses `min_temp`, hundreds of degrees
   before `overheat_temp`. Overheating requires reagents arriving into an
   already-hot beaker, or an exothermic runaway. Recorded in the test name
   `reagents_dropped_into_an_already_hot_chamber_waste_the_batch`.
4. **`overheat_temp` is distinct from `max_temp`**: `max_temp` stops a reaction,
   `overheat_temp` lets it run and go wrong (reactants consumed in full, yield
   scaled down, or `Detonate`).
5. **RON will not coerce an integer into `Kelvin`'s f32.** `Some((420.0))`, never
   `Some((420))`. Pinned by `every_new_field_round_trips_through_ron`.
6. **`Units` is reused as the damage scalar** rather than adding a new wire type,
   which inherits its dual text/binary serde encoding for free.
7. **Bevy caps a system at 16 params.** `handle_panel_clicks` hit it; its message
   writers are now a `#[derive(SystemParam)] struct PanelMessages`.
8. **Smoke spheres must be excluded from `update_focus`'s raycast filter** or the
   first cloud makes the whole lab unusable.
9. The reference book now **scrolls** — nine recipes fitted one screen, fifteen
   do not.

### M7: what exists and what is left

**Done.**

- Replicon + renet wired in. Launch modes: `--host`, `--join [addr]`, default
  singleplayer. Both singleplayer and `--host` verified running the full game.
- Shared lab state replicated (`Transform`, `Container`, `HeldBy`, `InSlot`,
  `Machine`, `Buffer`, `DispenseAmount`, `CrewMember`, `Player`); containers,
  machines, crew and chemists are marked `Replicated`. Entity-bearing fields use
  `#[entities]` so ids map across the wire.
- **One chemist entity per connection.** The server spawns them and tells each
  client which is theirs via a mapped `YouAreChemist` server message — `ClientId`
  is not serialisable, so identity has to be sent, not inferred. Replicon
  re-emits server messages addressed to `ClientId::Server` locally, so
  singleplayer takes the identical path.
- **Every player action is a client message** (`InteractRequested`,
  `DispenseRequested`, `EjectRequested`, `EmptyRequested`,
  `BufferTransferRequested`, `PackageRequested`, `AnalyzeRequested`,
  `DropRequested`, `MoveInput`). Handlers read `FromClient<E>` and resolve the
  actor from `client_id` via the `Chemist` component. `InteractRequested`
  deliberately has no `player` field: a message that named its own actor would
  let one client act as the other chemist.
- **Authority gated** on `ClientState::Disconnected` across machines, orders,
  crew movement, containers and knowledge.
- Movement is server-authoritative via `MoveInput`; **looking is not**. The
  camera is a separate entity driven by local yaw/pitch, so turning your head
  never waits on a round trip. Only walking does.
- Carrying is split: the server owns `HeldBy`, the client parents its own held
  beaker to its camera for a lag-free view. Someone else's beaker just sits
  where the server says.

- **Shared resources sync.** `Knowledge`, `Shift` and `RadioLog` are resources,
  which replicon does not replicate, so the server pushes each as a whole
  snapshot on change (`SendTargets::CLIENTS_ONLY`, so the host does not
  overwrite its own authoritative copy). `KnowledgeSync` reuses the save format
  — a save and a joining client then cannot disagree about what "known" means.
- **Two-instance connection verified.** Host and client processes connect,
  authorize and assign a chemist with no errors. Run them with
  `BEVY_ASSET_ROOT=. ./target/debug/chemgame.exe --host` and `… --join
  127.0.0.1:5327` — running the binary directly needs `BEVY_ASSET_ROOT`,
  because Bevy only finds `assets/` via `CARGO_MANIFEST_DIR` under `cargo run`.

**Bug the two-instance test caught** (and the headless tests could not): chemists
were spawned on `Added<ConnectedClient>`, but replicon's default
`AuthMethod::ProtocolCheck` drops targeted messages until the client's protocol
hash is verified. The chemist existed and the client was never told which one was
theirs. Now keyed on `Added<AuthorizedClient>`. The protocol check is worth
keeping — reagent ids are positions in the data files, so a client running
different chemistry would silently mis-read every solution.

**Remaining:**

1. **Play it with two windows.** The connection handshake works; nobody has yet
   confirmed a second chemist is visible, moves, and can work a machine.
2. Optional polish: interpolation for remote chemists, reconnect handling,
   held-beaker visuals for *other* players (currently they see it at the
   server's transform, which will look detached from the hand).

### Bug worth remembering

`Units` implemented `Deserialize` via `deserialize_any` so the RON data files
could accept either `15` or `15.0`. Replication serializes with **postcard**,
which is not self-describing and cannot answer `deserialize_any` — so every
solution silently failed to decode over the wire. `Units` now branches on
`is_human_readable()`: floats for text, raw `i32` hundredths for binary.
`quantities_survive_both_a_text_and_a_binary_round_trip` guards it.

**Deviations from the plan below, all deliberate:**

1. **Order gating was added in M5** (not planned). Generation is now 75% recipes you know, 25% one step beyond. Without it the first order could be the three-step Arithrazine chain, which reads as broken rather than hard.
2. **Discovery was pulled forward from M6** as a consequence: gating created 25% of orders that were permanently unfulfillable. Causing a reaction now records it (`ReactionsFired` → `Knowledge::learn`). M6 keeps the test bench, analyzer, research points and persistence.
3. **Six outcomes, not three** — Success / Short / Impure / Overdose / Wrong / Expired, so radio lines have something specific to react to.
4. **Only single-dose forms can overdose.** A beaker is bulk supply; a pill is swallowed whole. This is what makes the ChemMaster matter.
5. **No search in the book.** Nine recipes fit one screen; add it when the list grows.
6. **Data files use compound extensions** (`chem.reagents.ron`, `station.orders.ron`) because Bevy picks asset loaders by everything after the first dot.

**Bevy 0.19 postdates the assistant's knowledge cutoff.** Read `~/.cargo/registry/src/*/bevy-0.19.0/examples/` rather than guessing. Renames already hit: `AmbientLight`→`GlobalAmbientLight`, `shadows_enabled`→`shadow_maps_enabled`, `EventReader`→`MessageReader`, `BorderRadius` moved onto `Node`, `Val::Px(x)`→`px(x)`, child spawner is `ChildSpawnerCommands`.

**Not yet exercised by a human:** the delivery handoff and the outcome chatter. Everything else has been seen working in-game or is covered headlessly.

## Context

Brand new project. The repo is empty: a fresh git repo with no commits and one empty `main.rs` at the root.

The goal is a focused recreation of the **chemist** role from Space Station 13/14 — not a whole station sim. The player works a chem lab, receives requests from crew, fabricates the requested chemicals, hands them over, and then hears over the radio how it went. That last beat is the point of the game: the radio is the world reacting to your work, good or bad, on a delay. Everything else exists to serve it.

Decisions locked with the user:

| Question | Decision |
|---|---|
| Coop | **A second player is another chemist sharing your lab**, not a requester. Requests always come from NPC crew. Build single-player first, but hold the coop-ready invariants below from day one. |
| Presentation | **3D**, first-person lab. |
| Recipes | **In-game reference book that starts partly blank.** The player discovers locked recipes by experimenting during downtime on free equipment. Progression persists across shifts. |
| Chem data | **Faithful SS14 subset** — real names, ratios, overdose thresholds. |

Target: **Rust + Bevy 0.19** (current stable, released June 2026).

---

## Architecture

A Cargo workspace with the chemistry simulation as a **standalone crate with no Bevy dependency**.

```
ChemGame/
  Cargo.toml              # workspace root + bevy binary
  src/                    # bevy game (main.rs moves here from repo root)
  crates/chem_sim/        # pure Rust: reagents, solutions, reactions. No bevy.
  assets/data/*.ron       # reagents, reactions, crew, order templates, radio lines
```

This split is the single most important structural call. The reaction engine is the heart of the game and the part most likely to grow; keeping it Bevy-free means it is unit-testable in milliseconds without spinning up an `App`, and it stays reusable if a dedicated server is added later.

### `chem_sim` — the simulation core

**Use fixed-point units, not floats.** SS14 uses `FixedPoint2` for exactly this reason. Solutions are repeatedly split, transferred, and compared against reaction thresholds; float drift makes those thresholds flaky in ways that are miserable to debug.

```rust
pub struct Units(i32);        // hundredths of a unit
pub struct ReagentId(u32);    // interned; string names only at the data boundary
```

Three types carry the model:

- **`Reagent`** — id, display name, color, `Option<Units>` overdose threshold, metabolism effects, and whether it is dispensable from the base dispenser.
- **`Solution`** — `Vec<(ReagentId, Units)>` plus `max_volume` and `temperature`. Key methods:
  - `add` / `remove`, returning overflow so callers handle spillage explicitly
  - `transfer_to(&mut other, amount)` — must draw **proportionally** across contents. You cannot pour just the oxygen out of a mixed beaker, and getting this wrong quietly breaks every downstream recipe.
  - `color()` — volume-weighted blend, drives the beaker's rendered liquid color
- **`Reaction`** — reactants (ratios), **catalysts** (required present, not consumed), products, optional min/max temperature, a `priority`, and side effects (heat, smoke, explosion).

**The resolver** is a loop: each pass, find every applicable reaction, compute how many whole reaction-units can proceed (`min` over reactants of available÷required), consume and produce, repeat until nothing fires. Two non-obvious requirements:

- **Iteration cap.** Recipes can form cycles; an uncapped loop hangs the game.
- **Priority ordering.** When two reactions compete for the same reagent, the outcome must be deterministic. SS14 has a priority field for this; mirror it.

Catalysts are not optional polish — Dexalin needs them (see below).

### Seed chemistry data

These recipes were chosen deliberately: they demonstrate **chains**, **intermediates**, and **catalysts**, so the engine is exercised properly from day one.

| Product | Recipe | Notes |
|---|---|---|
| Inaprovaline | Oxygen 1 + Carbon 1 + Sugar 1 → 3 | base building block |
| Dylovene | Silicon 1 + Nitrogen 1 + Potassium 1 → 3 | base building block |
| Bicaridine | Inaprovaline 1 + Carbon 1 → 2 | chains off Inaprovaline |
| Tricordrazine | Inaprovaline 1 + Dylovene 1 → 2 | chains off two intermediates |
| Kelotane | Silicon 1 + Carbon 1 → 2 | intermediate |
| Dermaline | Kelotane 1 + Oxygen 1 + Phosphorus 1 → 3 | two-step |
| Hyronalin | Dylovene 1 + Radium 1 → 2 | intermediate |
| Arithrazine | Hyronalin 1 + Hydrogen 1 → 2 | three-step chain |
| Dexalin | Oxygen 2 → 1, **catalyst: Plasma** | exercises catalysts |

Overdose thresholds: Bicaridine 15u, Dermaline 10u, Dylovene 20u, Dexalin 20u; Inaprovaline and Arithrazine have none.

Base dispenser reagents: Oxygen, Hydrogen, Carbon, Nitrogen, Silicon, Phosphorus, Potassium, Sodium, Iron, Copper, Sulfur, Aluminium, Chlorine, Radium, Sugar, Water, Ethanol, Plasma.

All of this lives in `assets/data/*.ron`, loaded via `bevy_common_assets`'s `RonAssetPlugin` so it hot-reloads. No recipes hardcoded in Rust.

### Bevy side — plugins

| Plugin | Responsibility |
|---|---|
| `ChemDataPlugin` | Loads RON into a `ChemDb` resource; interns reagent names to `ReagentId` |
| `LabPlugin` | Spawns lab geometry, machines, lighting from primitive meshes |
| `PlayerPlugin` | First-person controller, cursor grab/release |
| `InteractionPlugin` | Camera-center raycast, `Interactable` component, "[E] Use" prompt |
| `MachinePlugin` | Dispenser / ChemMaster / grinder / analyzer / test bench; each owns a `Solution` |
| `ContainerPlugin` | Beakers as carryable entities; machine slots accept them |
| `OrderPlugin` | Order generation, deadline timers, delivery evaluation |
| `CrewPlugin` | NPCs walk to the delivery window, accept the handoff, leave |
| `RadioPlugin` | Delayed, event-driven chatter feed |
| `KnowledgePlugin` | Known/locked recipes, hint reveals, research points, save/load |
| `UiPlugin` | Machine panels, order queue, reference book, radio log |

States: `AppState { Loading, Playing }` and `InteractionState { Roaming, UsingMachine(Entity) }`. The interaction state controls cursor grab and which panel is open — routing all of it through one state avoids the classic bug where the camera keeps turning while a UI is focused.

**Machine UI**: 2D screen-space overlay on interact, with the machine's idle screen as an emissive material in-world. Diegetic worldspace UI is tempting but costs far more than it returns right now.

### The loop that matters

An order is `{ requester, request, deadline, tolerance }`, where `request` is either `NamedChem { reagent, units }` (the milestone-4 default) or `Symptom { .. }` — "something for radiation burns" — which forces a reference-book lookup. Build both variants into the enum now; ship named-chem first.

Delivery compares the handed-over `Solution` against the request and classifies:

- requested reagent present at ≥ requested units, nothing else → **success**
- contaminants present → **partial**, and the contaminant picks the radio line
- delivered dose exceeds the overdose threshold → **bad outcome**
- wrong chem entirely → **bad outcome**, often the funniest one

`RadioPlugin` then schedules the report **10–40 seconds later**, drawn from a table keyed by `(outcome, chem, requester_role)`. The delay is what makes it feel like a station rather than a score popup — do not collapse it into instant feedback.

### Coop — two chemists, one lab

The coop fantasy is **division of labor under time pressure**: one player works the dispenser and mixing while the other runs the ChemMaster, bottles output, and handles the delivery window. Orders arrive in a shared queue and either chemist can claim one. Beakers pass hand to hand. The radio addresses the lab, not a person, so a bad outcome lands on both of you.

Networking is deferred, but five invariants must hold from the first line of Bevy code or M7 becomes a rewrite:

1. **No "the player" singleton.** Player identity is an `Entity` from the start. Single-player is just a list of one. A `Res<Player>` or a `.single()` on the player query is the mistake that costs the most to undo.
2. **Machines carry `in_use_by: Option<Entity>`** from M3, and the interaction system respects it. Two chemists reaching for the dispenser is the normal case, not an edge case.
3. **Containers are entities with a holder component** — never a global inventory resource. A beaker must be able to sit on a bench, be carried by either chemist, or be locked in a machine slot.
4. **All state changes go through events/commands, never direct UI writes.** If a UI panel mutates a `Solution` in place, nothing is replicable and every panel has to be rewritten. Panels emit `DispenseRequested`, `TransferRequested`, etc.; systems apply them. This is the single highest-leverage constraint on the list.
5. **`chem_sim` stays headless and deterministic** — so a dedicated server can run it with no renderer.

When M7 arrives, use **`bevy_replicon`**: server-authoritative replication is the right shape for a lab game with no twitch action, and it is markedly simpler than `lightyear`'s prediction/rollback machinery, which buys nothing here.

### Progression — the book fills in as you learn

The book starts mostly blank. The player recovers the rest by experimenting on free equipment during downtime, and what they learn persists between shifts.

**The design problem to solve first:** with 18 base reagents, blind mixing is 816 unordered triples. Brute force is tedium, not a puzzle. Discovery has to be *deduction over a small candidate set*, and three things make it so.

**1. Locked entries are redacted, not hidden.** A chemist knows Bicaridine treats brute damage even if they've forgotten how to make it. So a locked entry always shows name, what it treats, and ingredient count — and reveals progressively finer hints ("one ingredient is a compound you already know", then "contains Carbon") as research accrues.

**2. Start the player on the three base recipes** — Inaprovaline, Dylovene, Kelotane. This is deliberate: every remaining recipe in the seed set is *known compound + one or two base reagents*. Combined with the hints, the first discovery collapses from 816 blind combinations to a handful of reasoned guesses, and it teaches the deduction pattern the rest of the game runs on.

**3. The analyzer makes reverse-engineering legitimate.** Feed it any solution, get its composition back. That opens three routes to a recipe: mix something unknown and analyze the result, analyze a sample vial an NPC hands over ("here's what the last chemist gave me"), or deduce it from hints. Sample handouts are also the anti-softlock valve when an order arrives for something unmakeable.

**The test bench** is separate equipment with unlimited base reagents whose output cannot be delivered for credit. Experimenting therefore costs no reagents — it costs **time**, against the deadlines of orders already queued. That is the right tension: downtime is a real resource, and choosing to research is choosing to fall behind.

State lives in a `Knowledge` resource, `HashMap<ReactionId, Entry>` where an entry is `Known` or `Locked { hints_revealed }`, serialized to `save.ron`. Research points come from successful deliveries and buy hint reveals.

Detection needs no new simulation work: `resolve()` already returns `Vec<ReactionEvent>` naming which reaction fired, so the game layer just checks each fired reaction against `Knowledge`, marks it `Known`, and emits `RecipeDiscovered` — which drives the toast, the book entry filling in, and a radio line. Keep `chem_sim` unaware of knowledge entirely; the resolver must always simulate real chemistry regardless of what the player knows.

---

## Milestones

Each is independently verifiable; the full loop closes at M5.

1. **M1 — `chem_sim` + tests.** Headless. Solution math, transfers, the resolver, all nine seed recipes covered by unit tests. No Bevy yet.
2. **M2 — Lab and movement.** 3D room from primitives, lighting, first-person controller, interaction raycast and prompt.
3. **M3 — Machines.** Dispenser, carryable beakers, ChemMaster (buffer → pills/bottles), overlay UIs, wired to `chem_sim`. *You can now make things.*
4. **M4 — Orders.** Generation, deadlines, NPC crew at the delivery window, handoff, outcome classification, score.
5. **M5 — Radio and book.** Delayed chatter feed, searchable reference book with locked and unlocked entries. *Loop closed.*
6. **M6 — Discovery.** Test bench, analyzer, redacted hints, research points, `save.ron` persistence, `RecipeDiscovered` flow. *Progression closed.*
7. **M7 — Coop.** `bevy_replicon`, server-authoritative lab, second chemist, shared order queue with claiming, beaker handoff, shared knowledge/research.

## Files to create

- `Cargo.toml` — workspace; `bevy = "0.19"`, `bevy_common_assets`, `ron`, `serde`, `rand`. Add `[profile.dev.package."*"] opt-level = 3` and a `dynamic_linking` dev feature — Bevy compile times without these are genuinely painful.
- `crates/chem_sim/src/{lib,units,reagent,solution,reaction,resolver}.rs`
- `crates/chem_sim/tests/reactions.rs`
- `src/main.rs` — **move the existing empty `main.rs` from the repo root**; a stray root `main.rs` will confuse cargo.
- `src/{lab,player,interaction,machines,containers,orders,crew,radio,knowledge,ui}/mod.rs`
- `assets/data/{reagents,reactions,crew,orders,radio}.ron`; hint text lives alongside each reaction entry
- `.gitignore` — `/target`, `save.ron`

## Verification

- `cargo test -p chem_sim` — resolver correctness, including the three-step Arithrazine chain, the Dexalin catalyst (plasma present at the end, unconsumed), proportional transfer, and cycle-cap termination.
- `cargo run` — walk to the dispenser, mix 15/15/15 O₂/Carbon/Sugar, confirm 45u Inaprovaline appears and the beaker liquid changes color.
- End-to-end by hand: take an order, fabricate, deliver, confirm a matching radio line arrives on delay. Then deliberately deliver an overdose and confirm the bad-outcome line fires.
- Headless integration test driving `App::update()` through order → delivery → radio, asserting the outcome classification without a window.
- Discovery: from a fresh save, confirm only Inaprovaline/Dylovene/Kelotane are unlocked; mix Inaprovaline + Carbon at the test bench and confirm Bicaridine's entry fills in, a `RecipeDiscovered` toast fires, and the unlock survives a restart. Separately confirm the analyzer reveals the composition of an NPC sample vial.
- Reachability check (worth a test, not just a playthrough): every seed recipe must be derivable from the three starting recipes plus base reagents. A recipe reachable only through an unreachable intermediate is a dead end that playtesting finds late and cheaply avoided here.
- Coop-readiness, checkable long before M7: spawn two player entities in a headless test and drive both through dispenser interaction. Machine locking should hold and no system should panic on `.single()`. This catches invariant violations while they are still one-line fixes.
