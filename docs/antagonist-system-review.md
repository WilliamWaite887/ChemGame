# Antagonist system review and next implementation slice

Reviewed: 2026-09-08. Status: code audit; implementation gates below are open.

This extends P7 in [utility-ai-plan.md](utility-ai-plan.md). The current request
is to examine the whole antagonist system and choose its next step. The useful
next step is to harden the shared motive, knowledge, custody and consequence
contracts, using food contamination and Engineering glassware tampering as two
tests of those contracts before expanding other threads.

User clarification: food poisoning is a possible scenario, not a main feature
or an NPC-system design center. The NPC system should create scenarios like it
from interacting motives, opportunities, ordinary actions and physical effects.
Do not build a food-poisoning director or require poisoning in normal play.

## Intended player experience

Antagonists create observable, preventable, recoverable problems that compete
for the chemist's attention. Their motives should explain their targets. The
player can accept a tempting bargain, notice preparation, prevent an act,
treat its victims, preserve evidence, or enlist Security. Each intervention
should change what can happen next. Ordinary work gives an antagonist cover.

Design from reusable interactions: acquiring and carrying materials, accessing
or handling objects, transferring contents, using equipment, taking possessions,
talking and reporting. Private motives can make an otherwise ordinary interaction
selfish or harmful. A cook handling food and someone contaminating it should use
compatible physical rules. Author character motives, preferences and constraints;
let current knowledge and world conditions determine which opportunity is useful.
Explicit action types are still useful for validation, but avoid a bespoke
character-to-crime pipeline for every new scenario. An NPC must also be able to
find no worthwhile harmful opportunity and continue an ordinary day.

Campaign progression owns stakes, escalation and endings. Utility AI owns the
actor's choice and execution. Physical chemistry owns exposure and harm.
Perception and investigation own what other people can legitimately conclude.
An incident must not independently become several rewards or penalties just
because multiple systems observe it.

## Current architecture

| Layer | Current implementation | Implication |
| --- | --- | --- |
| Campaign | `arc` defines Cult, Spy, Changeling, AI and Blob; authored counter tracks and showdown forms; timed plot drift and delivery modifiers | Campaign support is broader than covert utility behavior. Missing dedicated Rust modules do not mean those arcs are absent. |
| Cult | Investigation, ritual waves, wards, chemistry finale, and current station aftermath work | Preserve the existing physical intervention loop and Cult outcome inversion. See `cult-station-pass.md`. |
| Department and personal threads | Saboteur, smuggler, quack, obsessed, bent guard, botanist; additional illicit and rogue Security systems | These have distinct bargains and consequences. Do not flatten them into a generic random sabotage action. |
| Shared threat helpers | Authored script loading, visit cadence, chains, wards | These are scheduling helpers, not a station-wide antagonist decision system. |
| Covert utility | Botany supply can install a private goal; food poisoning competes in Routine and spends custody into meal chemistry | This is the only production `CovertGoal::new` insertion found outside the covert module's tests. |
| Consequences | Tampered meal attribution, poisoning incidents, Medical, ambiguous handling stimuli, investigation infrastructure | Useful seams exist, but whole-loop behavior is not established by isolated helper tests. |

## Findings from current source

1. **Delivery timing is mistaken for physical quantity.**
   `botanist::handle_botanist_resolution` tests
   `quality.remaining_fraction >= 0.5` as a minimum useful batch gate.
   `orders::complete_delivery` computes that field from remaining patience,
   not remaining solution. A successful late delivery can therefore be treated
   as a refusal by Botany. The handler then constructs an authored amount of
   pure reagent rather than transferring the delivered solution, and records
   `source_player: None`. Fix the delivery-to-custody contract before relying
   on claims of exact batch provenance, purity or recoverability.

2. **Witness risk and opportunity knowledge are absent from selection.**
   `covert::offer_food_poisoning` scans all served meals within 24 metres,
   sorts by servings, and scores servings times nerve. It does not read memory,
   occlusion, observers or exposure pressure. Nerve is a score multiplier,
   despite its comment describing a witness veto. The shared candidate path
   adds `CanAct`, but does not supply the missing covert witness gate.
   Consequence sensing after the act does not fix omniscient selection.

3. **Persistent stock does not preserve a persistent threat.**
   `CustodyRecord` saves stock; `shift` restores Botany's supplied flag and
   custody, but no private goal. The production goal insertion occurs only on
   the successful supply path. `IllicitCustody::restore` drops holders not yet
   embodied. Reload can therefore lose custody or leave a supplied character
   without a motive. A full save-entry/roster test is needed to establish the
   actual ordering behavior, beyond the proven missing goal restoration.

4. **Attempt completion and motive success are conflated.**
   `apply_food_poisoning` marks the goal achieved as soon as any contaminant
   lands. The player can recover the bowl before anyone eats, yet the actor is
   permanently satisfied. Distinguish an attempted act, a committed act,
   observed outcome, and a satisfied or abandoned motive. Retries need bounds
   and cooldowns so successful prevention buys useful time.

5. **Causal attribution outlives the exposure.**
   `TamperedMeals::carry_to` records a diner in the same first-write-wins table
   as meals. Entries are cleared on session reset, with no exposure expiry or
   recovery cleanup in that type. Later poisoning of that diner can be charged
   to an earlier tampered meal. `price_covert_harm` also nudges the primary
   campaign for any attributed poisoning. Add exposure identity and explicit
   campaign relevance; do not use a permanent person-to-culprit association.
   Regression tests should cover later unrelated poisoning and repeat events.

6. **Older sabotage still uses a different consequence contract.**
   Engineering's expired request immediately prices ignored-shenanigan pressure,
   then dispatches a nonresident on a scripted errand. Arrival creates the
   contaminant directly in eligible glassware. Physical interruption exists,
   but utility choice, witnessed handling and custody are not this path's
   contract. Distinguish the existing social cost of ignoring a request from
   the physical success of sabotage; do not accidentally charge both as harm.

7. **P7 completion language exceeds current evidence.**
   Its remaining second migration is correctly open, but witness-risk and
   physical custody claims need the corrections above. Treat the reference as
   implemented with hardening outstanding, not as full antagonist acceptance.

## Next work packets, in dependency order

### A. Make shared antagonist contracts trustworthy

Ownership: one implementer across `orders`, `botanist`, `covert`, and `shift`.
Read the master plan's Files in flight and current git diff before editing;
`shift` currently includes unrelated Cult aftermath changes.

- Transfer an actual accepted solution and stable supplier identity into custody
  exactly once. Correct the patience-as-quantity gate and preserve deal rewards.
- Persist motive lifecycle with stable holder identity; retain pending custody
  until its holder exists. Never serialize transient entity IDs as identity.
- Restrict candidates to known/perceived opportunities, add witness-risk vetoes,
  and revalidate target, access, custody and risk before committing an effect.
- Separate spending an attempt from satisfying its goal. Allow bounded recovery
  from prevention without instant, unlimited retries.
- Tie poisoning attribution to an exposure and incident identity, with explicit
  expiry and campaign routing. Keep private truth separate from witness evidence.
- Express motive relevance through target/person/department context rather than
  merely recognizing an action name. Build the reusable opportunity and transfer
  seams needed by both examples; do not expand a poisoning-specific subsystem.

Acceptance gates:

- The same valid batch delivered early or late produces the same real custody;
  a bad or absent delivery cannot fabricate stock. Real mixtures retain contents.
- An unseen meal produces no candidate; a known accessible meal is a positive
  control. A cautious actor abstains when visibly watched and can act unobserved.
- A target removed or protected during travel/work consumes no stock and causes
  no contamination; an unchanged valid target receives the exact dose once.
- Save after handover, before NPC spawn, after spending, and after confiscation:
  reload preserves the appropriate future behavior and exact remaining volume.
- Removing a tainted meal before consumption prevents harm pricing. Treating a
  victim ends that exposure's attribution; later unrelated poisoning is innocent
  of the earlier act. Reprocessing an incident cannot price it twice.
- Validate schedule initialization and authority/client separation. Run targeted
  tests and workspace tests after integration; then inspect one accelerated
  fresh-save chain from bargain through prevention or Medical/Security response.
- The same motive can find different useful opportunities as the station changes,
  or none. A supplied NPC is not required to poison food. Scenario fixtures may
  arrange a poisoning opportunity deterministically; normal play must not script it.

### B. Prove a second action: Engineering glassware tampering

Give the existing tech a private motive after the authored trigger. Submit a
utility opportunity against a specific accessible container, with a work duration,
shared controller ownership, risk checks and ambiguous witness stimulus. Preserve
wards, chain progression and existing player protections. Define a finite ordinary
contaminant supply through the existing chemistry rules; special player-only
chemicals must still come from actual player delivery.

Acceptance: ordinary work can win; an unsafe opportunity is refused; holding,
storing or slotting the beaker prevents tampering; incapacity interrupts the act;
a valid act changes real chemistry once; witnesses can investigate without
learning secret intent. Prove the old errand and the new action cannot both fire.

### C. Tune pressure after both actions work

Measure time to first trouble, concurrent unresolved harms, recovery time, repeated
targeting and successful interventions. Use existing campaign and instability
contracts for pacing, with limits on stacked crises and room for recovery.
Review timed plot drift separately: removing it before embodied threats reliably
operate could leave an inert campaign. Decide how much background pressure should
remain only with fresh-save evidence. Then migrate other threads individually.

## Review ledger

- Source inspection completed for campaign seams, Cult work boundaries, Botany
  supply, covert selection/execution/attribution, persistence and Engineering's
  existing errand. Other department threads were surveyed, not exhaustively audited.
- No gameplay code changed in this review. This document is the implementation
  handoff; all acceptance gates above remain open.
- `cargo test --bin chemgame utility_ai::covert::tests -- --nocapture`:
  13 passed. These existing tests do not cover the gaps above. Build reported
  unused `defeated_gap_scale` / `scale_for_defeated` and a Steam DLL copy warning
  because another process held the DLL; the test executable completed successfully.
- No live gameplay, rendering, multiplayer or save-entry validation performed.
