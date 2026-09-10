# NPC thread integration phase

Status: implementation plan, not implemented.
Updated: 2026-09-08.
Parent: [Utility AI master plan](utility-ai-plan.md), P7 and controller ownership.
Evidence: [Antagonist source review](antagonist-system-review.md).

## Outcome and scope

Connect the systems already designed and implemented so existing characters are
useful station inhabitants whose existing troublesome behavior responds to the
world. This phase does not design new crimes, motives, campaign arcs or job loops.

The user confirmed that the assistant idea applies to existing characters.
Tech Boyle and Grower Aleksy keep their names, Engineering/Botany identities,
authored requests and consequences. Between those interactions they use a shared
assistant work profile to help several departments with existing jobs. Their
ordinary activity gives the player a reason to encounter them before trouble.

The noticeable change is a connected sequence: someone helps around the station,
visits Chemistry for an existing request, resumes ordinary activity, and can take
an existing harmful opportunity only when its prerequisites hold. Watching them,
protecting a target, withholding material or treating a casualty changes the
result. Food poisoning remains a possible acceptance scenario, never a required
event in ordinary play.

Included: these two characters' embodiment, shared job participation, controller
handoffs, existing covert action gates, real Botany delivery custody, Engineering's
existing glassware action, and the Medical/Security/save connections needed to
validate them. No other antagonist migration is part of this phase.

Preserve campaign drift, Cult inversion, Cult investigation/finale/aftermath,
department core cast, existing visit cadence, wards, deal benefits, chemistry
rules, and the authority-only nature of private NPC state. Do not add a pressure
director, relationship-driven motive generation, generic planning language,
new assets, new specialist jobs, or a special poisoning schedule.

## Existing seams and actual gaps

| Existing seam | Integration work |
| --- | --- |
| `StationResident`, recall-or-spawn, `ReturnsToDuty` | Both names currently occur in their scripts, not the ordinary crew roster. Establish one enduring body per name and reuse it for visits. |
| Authored Botany visit list and shared threat/intake helpers | Aleksy's list has no running spawner; presence currently opens the final approach immediately. Wire the authored sequence to the existing visit machinery. |
| `NpcJobProfile.cross_trained`, capability-filtered `JobBoard` | Configure an assistant profile. The selector already accepts cross-department work; no second scheduler is needed. |
| `UtilityControlBundle`, action phases, reservations, interruption | Route the older Engineering errand through the existing action executor; keep order visits and utility decisions mutually exclusive. |
| `NpcMemory`, `can_see`, stimuli and interviews | Apply these to covert opportunity selection and commit checks. Food selection currently queries nearby world meals without knowledge or observer gates. |
| Real solutions, `IllicitCustody`, delivery ownership | Botany reconstructs authored pure volume; its quantity gate actually reads remaining patience. Transfer the accepted material instead. |
| Custody snapshots and saved Botany progress | Restore private action eligibility and retain unresolved holder records until their bodies exist. Current restore drops absent holders. |
| Incident, Medical and investigation ledgers | Tie causal exposure to its incident. Investigation currently excludes poisoning, and diner attribution has no recovery lifetime. |

## Implementation decisions

### 1. Embody the two existing characters and give them assistant work

Implement a small scripted-resident adapter under utility AI. Read identity/color
from the existing scripts; register the exact two names with their home domain.
Use existing crew spawning and adopt an already-spawned same-name visitor instead
of creating a second body. Mark them as residents that return to duty and attach
the ordinary utility bundle, needs, memory and support-tier job profile.

Do not add these names to `station.crew.ron`: that is also the ordinary random
customer pool, and current tests deliberately exclude these scripted characters.
Do not expand the fixed two-core department rosters. Give scripted residents a
separate bounded registry, with home/fallback standing positions allocated after
the existing department roster slots so they cannot both inherit slot zero.

Use the same profile configuration for both assistants:

| Department | Allowed existing capabilities |
| --- | --- |
| Botany | `botany.inspect`, `botany.irrigate`, `botany.harvest` |
| Service | `service.ingredient_intake`, `service.serve`, `service.host`, `service.clean` |
| Engineering | `engineering.inspect`, `engineering.maintain` |

Boyle's primary domain remains Engineering; Aleksy's remains Botany. Cross-train
each in the other listed domains. Keep department affiliation and relationship
grouping unchanged. Do not grant Medical, Security or Bridge qualifications.
Cargo is deferred because its current `cargo.operations` capability groups more
work than this assistant profile should acquire in this pass. Omit `botany.tend`
because it also authorizes quarantine cleanup. Allowed routine work retains its
existing accident risks; the profile is not an exemption from station chemistry.

Use existing ticket scores, reservations, travel, needs and emergency priorities.
No forced department rotation, assistant-only jobs, productivity multiplier or
extra priority bucket. Assisting must advance the actual destination adapter's
work state. If no eligible work is available, existing need/social/home fallback
behavior applies. Do not inflate all covert scores to force an incident.

The existing intake/visit path recalls these bodies. On resolution or expiry,
release the exact visit ownership and restore utility control through the shared
handoff API. Never overwrite an active medical transport, pursuit or incapacitated
owner. Routine work, needs and social actions may yield to an admitted authored
visit. Emergency actions, active visits, medical transport, pursuit, incapacity
and pending handoffs defer admission. Intake and recall must use the same shared
eligibility predicate and revalidate before taking control; rejection releases
admission and schedules the existing short busy-resident retry. A busy resident
never causes a fallback duplicate.

Wire Aleksy's already-authored visit sequence through the existing threat/intake
helpers, using `MINOR_FIRST_VISIT` and his script's gap multiplier. Add a saved
visit cursor, advancing one step per resolved visit. Earlier successful asks
advance normally and must not be charged as refusals by the final-ask handler.
Earlier failures retain the authored withholding consequence and advance to the
next step. Open the final `LiveApproach` only when that final visit is admitted;
presence alone cannot expose it. A successful final handover settles the thread;
refusal, expiry or failed final delivery closes that branch without stock or
another final ask. Persist that terminal distinction. For old unsupplied saves
with no cursor start at the first authored step; old supplied saves remain settled.

### 2. Connect physical delivery, private eligibility and persistence

Use an authority-only retained-delivery disposition on the final Botany request.
It must branch at the existing accepted-container ownership seam in order delivery,
before personal ingestion or container destruction. Implement one receipt record
containing a unique receipt ID, recipient identity, supplier account identity when
available, actual accepted `Solution`, claimed label and resolved request identity.
Keep `OrderResolved` as the grading event; it is not sufficient proof of custody.

Move the accepted contents into custody once, emptying/removing the source through
the existing delivery convention. Never retain both a full container and a copied
full custody solution. Preserve the batch's real mixture and purity. Grant the
authored deal benefits exactly once on the accepted receipt, independently of
whether a harmful opportunity is ever selected. Remove the patience-as-quantity
test; existing order grading determines whether this is the successful final ask.
Wrong, short, expired and earlier-step requests retain their existing outcomes.

Extend the existing custody snapshot with receipt identity and stable provenance.
Persist each character's current existing thread eligibility and whether that
authorization has already produced a completed act. Do not add a new motive
progression system. Keep the current single successful act per authorization;
document that its completion means the act occurred, not that a long-term motive
was proven satisfied. A prevented attempt does not count as a completed act.

Bind pending records to spawned identities after scripts and roster activation.
Missing holders remain pending and are included in later saves. Restore by
replacement with receipt-ID deduplication, never by appending. Do not persist
paths, reservations, `CurrentAction`, live entity IDs or absolute elapsed times.
Restart interrupted travel/work from current facts. Save remaining retry/expiry
durations for the Engineering authorization described below. Its concrete save
fields and tests land with packet D, using this packet's restoration contract.

Backward compatibility: new fields use serde defaults. An older supplied Botany
save with usable custody may reconstruct one unspent authorization; supplied
with no usable custody must not create material or another reward. Preserve the
contents actually saved, even if an older version reconstructed them incorrectly.
Older saves with no assistant state simply spawn/adopt the named residents. A
missing supplier remains unknown; never attribute it to the host by default.

### 3. Apply shared perception and execution checks to existing covert behavior

Add a small shared eligibility helper used by food contamination and glassware
tampering. Reuse `can_see`, the same room/solid geometry and the current perception
range, including the existing chemical concealment rule when perceiving people.
Factor the current sight-range calculation if needed so candidate checks and
witness sensing cannot disagree about concealment. Do not introduce another
visibility algorithm or read secret NPC motives to decide who is a witness.

Candidates require a valid existing authorization, appropriate material, and a
currently visible target or a still-valid memory of that target. First discovery
requires sight; seeing a nearby object can refresh its existing memory fact.
Remembered objects use their last-observed location for travel, not a remotely
updated position. Once the actor reaches that location it must reacquire the
same target visually before performing; a missing/moved target fails cleanly.
Selection must not rank unseen targets by their secret current volume or servings.
Fresh observations may provide the current opportunity details.

Evaluate observer risk from conscious, non-incapacitated crew and player bodies
the actor can perceive that have line of sight to the proposed handling location.
Use the same sight test in the reverse direction for their observation. A person
the actor cannot perceive does not magically enter its risk estimate, but can
still witness the act through ordinary sensing. For this phase one perceived
observer is sufficient to veto these cautious actors. Retain Aleksy's authored
nerve as a utility score factor; correct the current misleading witness-gate
comment rather than inventing a new nerve-to-risk formula.

Recheck target eligibility, reach, line of sight, custody and observer risk after
arrival, during performance, and immediately before mutation. Run invalidation
before generic performance completion. Completion handlers independently verify
their authorization and target so stale/duplicate result messages cannot act.
Use the shared cancellation path to release reservations and movement ownership.

Food keeps its existing work duration, serving threshold, real reagent transfer
and Routine bucket. A stale/missing/protected target or confiscated batch results
in no transfer. Require sufficient target capacity for the intended effective
dose before consuming an authorization; do not mark success for accidental tiny
overflow acceptance. Keep real chemical effects, not a poisoned-food flag.

After an interrupted or invalid covert attempt, impose a 30-second actor/action
retry cooldown and resume normal utility selection. Do not retain a reservation
while waiting. A naturally bad opportunity may remain unchosen indefinitely.

### 4. Migrate Engineering's existing glassware errand

Keep the authored expired-request trigger, chain advancement, SecondInspection
ward and existing ignored-request consequences. They create one bounded
authorization for the same named actor, not an immediate effect or a second
controller. A new trigger replaces any older unspent authorization instead of
stacking several attacks.

Expose the current behavior as `UtilityActionId::TamperContainer` through
`UtilityOpportunityBuffer`, using the existing action lifecycle and an exclusive
reservation on the exact container. Eligibility remains nonempty loose glassware:
held, stored and machine-slotted containers are protected. Current observation
and reachability are required. Among valid known targets use existing route
distance, with stable target identity as a deterministic tie-breaker.

Use a four-second work duration, the shared 30-second retry cooldown, and a
120-second opportunity window after visit resolution. These are named tuning
constants. The window elapses while doing ordinary work, so indefinite surveillance
or protecting the glassware eventually defeats this particular attempt. Keep
Routine priority. Use 0.65 base appeal before existing duty/ability considerations;
verify against ordinary work and fallback with positive controls before any tuning.

Retain the authored water/amount as one finite maintenance allotment for that
authorization. Materialize it once when arming, consume it only on successful
transfer, and discard unused allotment when the authorization expires. This
preserves the current ordinary-water source and bounds it; it is not an illicit
player delivery. Validate that this route cannot source a player-only reagent.

Mutate the exact container through its existing chemistry mutation API. On actual
transfer, finish the authorization once and emit the existing ambiguous handling
stimulus. Keep current qualitative aftermath lines, but remove any unconditional
claim that somebody witnessed the act. Named witness claims require actual sensed
evidence. Do not price ignored-request pressure again at arrival or mutation.

Remove the production `Meddling`/arrival mutation route after the utility path is
wired. Retain and adapt its existing protection tests. The actor returns to normal
assistant work after completion, interruption or expiry; no forced off-station
departure and no direct writes competing with the utility movement owner.

### 5. Connect observable consequences to Medical and Security

Keep one source-of-truth incident ledger. Physical act provenance is private;
knowledge of a crime still requires a witness, report or observable consequence.
Never open a public accusation from `TamperedMeals` alone.

For glassware, sensed handling records ambiguous witness memory. Preserve the
existing rule that `SuspiciousHandling` does not broadcast an automatic accusation
or start a case. That memory remains available to existing Security interviews
when a separately established incident warrants them. An unused tampered beaker
must not summon an officer from private knowledge. This phase does not add a
new beaker-analysis or player-accusation UI to manufacture such an incident.

For meals, replace permanent diner-to-culprit association with an exposure record
created only when a real serving containing the transferred contaminant enters
that diner's body. Link it to the meal act, reagent, dose and resulting poisoning
incident. Clear unbound exposure eligibility once the contaminant has cleared;
once bound, retain it only through that incident's resolution. A later unrelated
poisoning cannot reuse it. Multiple doses of the same act must not price the same
incident repeatedly. If overlapping sources cannot be distinguished, preserve
unknown attribution rather than inventing a culprit.

Ensure observable toxin injury reaches the existing casualty/report/Medical path;
do not create a patient case merely because food was adulterated. Extend existing
investigation eligibility to observable poisoning incidents, without consulting
private attribution to select witnesses or announce guilt. Ordinary poisoning can
therefore be investigated and legitimately produce no culprit.

Testimony for a poisoning case must concern the meal/exposure linked to that case,
not simply a witness's most recent unrelated handling memory. This relevance
check may use the recorded incident/exposure subject link, but the testimony's
named person and confidence still come only from the witness's own memory.

Price attributable harm once per `IncidentId`, preserving current severity-based
instability, department standing and primary campaign pressure. This phase does
not rebalance which campaign benefits from general disorder. Medical owns patient
recovery; Security owns investigation closure. Resolving treatment must not lose
already-recorded testimony, and opening an investigation must not duplicate the
medical harm charge. Use stable saved identity for any persistent provenance;
do not persist stale body/meal entity links.

## Work order, ownership and handoff

Use one integration owner for shared scheduling, crew/order handoffs and save
types. If delegated later, packets may have separate owners but only one writes
the shared utility root and `shift` at a time. Read the master plan's Files in
flight and current diff before assigning work. Preserve other worktree changes,
especially the current Cult additions in `arc`, `shift`, `net`, UI and map files.

| Packet | Prerequisite | Owned surface | Completion gate | Status |
| --- | --- | --- | --- | --- |
| A: assistant embodiment | None | Scripted-resident adapter, crew recall/return, job-profile configuration, authored Botany visit wiring | Both names perform real existing jobs and survive visits without duplicates | Landed and activated |
| B: honest handover/save | A identity contract | Orders delivery seam, Botany handler, custody/progress save | Actual mixtures transfer once and reload preserves eligibility | Landed |
| C: perception/commit | A | Shared covert helper, perception observations, utility cancellation ordering | Seen/unseen/watched/interrupted positive and negative controls | Landed |
| D: Engineering migration | A, C | Saboteur adapter and shared action registration | Real guarded container mutation; old errand cannot also fire | Landed |
| E: consequence chain | B, C, D | Service exposure, incident/report, Medical and interviews | Prevention, harm, treatment and evidence form one causal chain | Landed |
| F: integration acceptance | A-E | Tests, traces, documentation | Automated gates pass; live findings and remaining manual checks recorded | Automated half landed; live half handed over |

A can build and test Boyle's adapter, but production activation of his residency
must land atomically with D's replacement handler: the old sabotage query uses
`NotResident` and would silently stop firing between those changes. Treat A-E as
one integrated release. Activate Aleksy's production request/supply path only
with B/C/E connected, so a newly live thread cannot expose the known custody and
consequence gaps. This needs coordinated registration, not a new user setting.

### Packet A — landed 2026-09-09

New `src/utility_ai/scripted_residents.rs`: a bounded two-name registry, the
shared assistant profile (Botany/Service/Engineering via the existing
`with_cross_training` builder), and an activation system matching the department
adapters' shape. `botany.tend`, `cargo.operations` and all Medical/Security/
Bridge work are withheld, each asserted rather than left implied.

`scripted_standing_slot` fixes a collision the plan predicted: a scripted
resident is absent from every roster, so `standing_slot` returns `None` and
`select_reference_actions`' `.unwrap_or((0, 1))` seated them on the department's
first core member. Ranks now continue past the roster instead.

`cargo test --bin chemgame utility_ai::scripted_residents`: 5 passed.

**Not yet activated in production.** Registration is in place but Boyle's
residency must land atomically with packet D — `saboteur`'s trigger query is
`NotResident`, so making him a resident silently stops it firing.

### Packet B — landed 2026-09-09

`orders`: new `RetainsDelivery` component and `DeliveryReceipt` message. The
receipt carries the *actual accepted solution*, supplier, claimed label and
resolved request; it is emitted from a third disposition in `complete_delivery`,
branching ahead of both personal ingestion and the linked-use path. The
container is emptied through the existing convention so the batch cannot exist
twice. `DeliveryQuality::remaining_fraction` now documents that it measures
promptness, not volume.

`botanist`: the patience-as-quantity gate is gone — order grading already
answers it, and `Outcome::Short` lands in the refusal branch. Custody receives
`receipt.accepted` rather than a reconstruction from the authored amount, with
`source_player` carried through. Added a visit cursor (`step`) so earlier
successes advance without being charged as refusals, and a terminal `closed`
state distinct from `supplied`. New `generate_botanist_visit` spawner using the
shared threat helpers — the thread previously had none, which is why its
`color`/`gap_multiplier` were dead. `RetainsDelivery` is attached only to the
final ask.

`restore_covert_goal` fixes the plan's sixth defect, found during
implementation: `CovertGoal` was never persisted and `supplied: true` blocked
the only path that inserted one, so a reloaded save left a holder carrying stock
he could never use. The motive is reconstructed from the saved physical facts,
gated on the batch still being spendable — confiscation ends it.

`shift`: `botanist_step` and `botanist_closed` persisted with serde defaults.

`cargo test --bin chemgame botanist`: 12 passed.
`cargo test --bin chemgame orders`: 86 passed.
`cargo test --bin chemgame`: 1649 passed.

Both fixes falsified: restoring the patience gate fails
`a_late_but_valid_final_delivery_is_a_handover_not_a_refusal`; restoring the
fabrication fails `custody_receives_the_delivered_mixture_not_an_authored_reconstruction`
with the authored 12u in place of the delivered 6u.

### Packet C — landed 2026-09-09

This is the packet that changes behaviour rather than correcting it.

`perception`: `concealed_sight_range(concealment, strength)` factored out of
`witness_stimuli`, which now calls it. One function, so an actor estimating
whether it is watched and a witness deciding whether it saw cannot disagree
about concealment.

`covert`: new `CovertSight` SystemParam holding rooms, solids, observers and
concealment, with `can_see` and `observed`. Selection no longer scans the world:
a meal qualifies only if the actor can see it now, or remembers it — and a
remembered meal is walked to at its *remembered* location and scored at the
threshold it had to clear to be remembered, never by its true current servings.
The witness veto `nerve`'s doc has always described now exists: one perceived
observer with a view of the counter skips the candidate outright.

The asymmetry is deliberate. An observer the actor cannot perceive does not
enter its risk estimate — it is not omniscient about who is nearby — but that
person can still witness the act. That is what getting caught looks like.

Commit-time rechecks in `apply_food_poisoning`: the actor must reacquire the
target visually on arrival and re-verify observers immediately before mutating,
so someone arriving during the walk stops the act. Attempt and motive are now
separate — a dose below `EFFECTIVE_DOSE_ML` (what a full bowl refuses back to
custody) lands nothing and satisfies nothing, where it used to retire the
antagonist on a few drops.

`cargo test --bin chemgame utility_ai::covert`: 19 passed (was 13).
`cargo test --bin chemgame`: 1655 passed.
`cargo clippy --workspace --all-targets`: no new warnings; the two in changed
files are the pre-existing `type_complexity` lint on query tuples.

Two existing tests needed real `WalkableAreas` added — without geometry nothing
is visible and the gates correctly refused, which is itself evidence they bind.
Both new gates falsified: removing the `find(!observed)` filter fails
`a_visible_observer_calls_the_act_off`; removing the `can_see` guard fails
`a_meal_in_view_is_a_candidate_and_the_same_meal_behind_a_wall_is_not`.

`CovertCooldowns` implements the specified 30-second retry cooldown, keyed by
actor *and* action so being scared off one thing does not postpone an unrelated
one. It starts on any non-completed resolution and on either arrival recheck
failing, so successful prevention buys real time rather than a second's delay.

`cargo test --bin chemgame utility_ai::covert`: 20 passed.
`cargo test --bin chemgame`: 1656 passed.

### Packet D — landed 2026-09-09

The second covert action, and P7's long-open "migrate one existing
department-minor errand as a second proof".

`UtilityActionId::TamperContainer = 15`, appended to the explicit-discriminant
enum. `activity_for` maps it to `Working` alongside the other covert acts, so it
stays indistinguishable from honest handling on the wire.

`covert` gained `TamperAuthorization` — a bounded 120s licence carrying a finite
allotment materialised once when armed, drawn down only on a real transfer, and
discarded whole when the window closes. Plus `offer_container_tampering` and
`apply_container_tampering`, both reusing C's `CovertSight` and
`CovertCooldowns` rather than re-deriving visibility or retry state. Targets are
ranked by `nav.nearest_reachable`, take an exclusive reservation, and are
re-verified visually on arrival.

`saboteur` no longer performs the act. Being ignored arms an authorization; the
`Meddling` component, the errand dispatch and the arrival mutation are deleted.
The module keeps the chain, cadence, ward, ignored-shenanigan pricing and the
aftermath chatter — now aired off the `SuspiciousHandling` stimulus, so a line
cannot be printed for an attempt that was vetoed or never found its moment. The
arrival path previously emitted **no** stimulus at all, so a witness standing
right there learned nothing.

**The atomic activation.** `handle_saboteur_resolution`'s `NotResident` filter
is gone, in the same change that made A's activation live. Guarded by
`the_thread_still_fires_for_a_tech_who_lives_here`.

`station.saboteur.ron`: removed "Boyle was seen leaning over the chem lab
counter with a bottle in his hand" — aired unconditionally, it asserted an
eyewitness who may not have existed and named the culprit for free.

`cargo test --bin chemgame saboteur`: 15 passed.
`cargo test --bin chemgame`: 1658 passed.

Falsified: restoring the `NotResident` filter fails **seven** tests including
the activation guard — the silent failure is now loud.

**Two fixture traps found, both worth knowing for E:**

- `can_see` treats different rooms as blocked, and the Reaction Bay is only six
  metres wide. The old fixtures offset the beaker by 4m, putting it through the
  west wall. Invisible when selection scanned by distance; fatal with a real
  sight test. Fixed with a named `on_the_bench()` helper rather than a magic
  offset repeated thirteen times.
- The covert provider requires `With<UtilityAgent>`, so a bare `CrewMember`
  fixture is skipped entirely. `tech()` now spawns Boyle as he actually exists:
  a `StationResident` with a `UtilityControlBundle`.

**Known limitation:** `TAMPER_APPEAL` (0.65), `TAMPER_SECONDS` (4.0) and
`TAMPER_WINDOW_SECONDS` (120.0) are the specification's starting values and have
not been measured against ordinary work with positive controls. Packet F's
harness is where that check belongs.

### Packet E — landed 2026-09-09

`TamperedMeals` gained a real `Exposure` record: diner, meal, culprit, reagent,
dose, time, and `bound_to`. The old table stored diners and meals together as
bare entity pairs, so the only question it could answer was "was this person
ever fed a tampered meal" — and it answered `true` forever.

`carry_to` now takes what actually moved, measured across the serving in
`service::apply_eaten_servings` rather than inferred. `price_covert_harm` binds
an exposure to an incident instead of looking one up: binding consumes it, so
one act pays for one incident, a repeat event finds it already bound, and a
later unrelated poisoning finds nothing to claim.

`forget_spent_exposures` retires records that can no longer explain anything —
unbound once the contaminant has cleared both blood and stomach, bound once the
incident resolves. Without it an exposure is a permanent accusation waiting for
a coincidence.

`interviews::worth_investigating` admits `IncidentKind::Poisoning`. Its absence
made the whole food thread unfalsifiable: the act emitted `SuspiciousHandling`
memories and no investigation was ever opened to collect them, so the one route
by which such a memory leaves a witness's head was never taken. Ordinary
poisonings are admitted too — gating on whether a culprit exists would consult
private truth to decide whether to ask the question.

`record_testimony` now narrows to the meal the incident's exposure names, via
`exposure_for`. The subject link comes from the record; the named person and
confidence still come only from the witness's own memory. Previously a witness
answered a poisoning case with their strongest *unrelated* handling memory —
a plot topped up yesterday, a donation an hour ago — which is how an innocent
person gets named.

`dose_for` feeds an authority-only `debug!` trace tying an incident to the
reagent and volume that explains it. Pricing deliberately does not read it:
severity is the victim's, not the dose's.

`cargo test --bin chemgame utility_ai::covert`: 22 passed.
`cargo test --bin chemgame utility_ai::interviews`: 8 passed.
`cargo test --bin chemgame`: 1661 passed.
`cargo clippy --workspace --all-targets`: no new warnings.

Falsified: reusing a bound exposure fails
`a_later_unrelated_poisoning_is_not_charged_to_the_earlier_meal`; removing
`Poisoning` from the eligibility list fails two interview tests.

**Known limitation:** treatment resolving an incident is what retires a bound
exposure, and that path is exercised only through `IncidentLedger::resolve` in
these tests, not through a real Medical treatment cycle. Packet F's harness is
where the full prevention → harm → treatment → evidence chain gets driven end to
end.

### Packet F — landed 2026-09-09 (automated half)

New `src/utility_ai/acceptance.rs`: 12 tests on an `App` built from the real
`UtilityAiPlugin`, driven into `AppState::Playing`. Every other test in this
phase wires its own systems onto a bare `App`, which is the right shape for
asking "does this rule hold" and is why they are small and fast — but it means
the systems under test are wired by the test rather than by `register`.

**What building the real plugin immediately exposed.** Running one frame in
`Playing` produced eleven system-parameter validation failures. These are the
hard `Res`/`MessageReader` dependencies `UtilityAiPlugin` has on its sibling
plugins, and a system that hits a missing one is *skipped with an error*, not
merely idle:

`CrewPosts`, `ErrandResolved`, `StationStability`, `StabilityEvent`, `Shift`,
`FulfillmentApplied`, `UnderworldStanding`, `RadioLog`.

They are listed explicitly in the fixture rather than papered over, because
leaving any out would make the harness prove less than it appears to.

**The fixture disables `TimePlugin`.** `MinimalPlugins` includes it, and it
overwrites `Time` from the wall clock at the start of every frame — silently
discarding the test's `advance_by`, so a fixture that "waits two minutes"
actually waits a microsecond. Every other test in this crate gets an inert clock
for free by not using `MinimalPlugins`; this one has to ask.

**Falsified, and this is the packet's whole justification.** Two mistakes were
introduced deliberately, each of which disables covert behaviour completely and
silently:

1. `clear_opportunity_buffer` reordered `.after(OpportunityProviders)` — deletes
   every opportunity in the game, every frame. **1665 pre-existing tests passed;
   only the 4 new ones failed.**
2. A dead run condition (`training_session`) added to the covert providers —
   covert selection never runs at all. **Same result: 1665 passed, 4 failed.**

Both restored. This is the class of defect the per-packet tests structurally
cannot see, and it is now covered.

**Tuning: measured, not assumed.** `TAMPER_APPEAL` (0.65) had never been checked
against ordinary work. The first version of the test passed at an appeal of
0.001 — because `Departments` was empty in the fixture, Boyle had no post,
`MaintainPost` was never offered, and tampering won by being the only candidate.
That measured plumbing and called it tuning. With a real post registered so
`MaintainPost` competes at `MAINTAIN_POST_WEIGHT` (0.25), a sweep puts the
crossover between **0.20 and 0.30** — exactly where the post weight sits. 0.65
therefore clears idling comfortably without entering the 0.30-0.50 band Security
and Bridge tickets occupy, and **needs no adjustment**. The measurement is
recorded in the constant's doc comment, and a negative control
(`without_an_authorization_the_same_actor_just_works`) keeps the positive one
honest by proving `MaintainPost` really is selectable in that fixture.

**Coverage gaps closed.** `IllicitCustody::spend`'s overflow-refund branch was
untested — every prior test hands it a `Solution::unbounded()`, so `add` never
refuses and the refund path was dead code as far as the suite was concerned. Two
tests now drive it against a real capacity limit. Falsified: dropping the refund
fails exactly one test, the new one. Replication is extended to the components
this phase added (`TamperAuthorization`, `CovertGoal`, `RetainsDelivery`), all
confirmed unreplicated.

`interviews::worth_investigating` was made `pub(super)` so the eligibility rule
can be asserted against the real `IncidentKind` rather than a local copy.

Commands: `cargo test --bin chemgame` → **1673 passed, 0 failed** (1661 before).
`cargo clippy --bin chemgame --all-targets` → 124 warnings, unchanged from
baseline; none in `acceptance.rs`. `cargo build --bin chemgame` clean.

**Known limitations, carried forward honestly:**

- `decision_log.rs` was **not** extended. On reading it, the existing `PICK` /
  `DONE` / `DONE!` / `STATION` lines already carry actor, action, target key,
  score, runners-up, result and a greppable failure marker — which is what the
  checklist needs. Adding receipt and incident IDs remains worthwhile and is not
  done.
- The seven named acceptance scenarios are covered as **automated** rules
  (selection, veto, expiry, mutation, custody, replication, eligibility) plus a
  written live checklist. They are not seven end-to-end automated scenarios; the
  prevention → harm → treatment → evidence chain is still not driven through a
  real Medical treatment cycle in one test.
- The covert providers still carry a pre-existing `too_many_arguments` and
  `type_complexity` warning. Both predate this phase.

**Handed over:** `docs/npc-thread-live-acceptance.md` — the live checklist, per
the user's decision that they playtest and I write it. It names what to watch,
where to stand, which `ailog.txt` lines confirm each step, and states plainly
that no forced poisoning frequency is an acceptance criterion. Its two
"reads-well" question sets are the ones no test can answer.

### Packet G — the walked sequence, landed 2026-09-09

Scenario 3's remaining gap: every test that needed Aleksy's final ask reached it
by writing `progress.step` directly (`advance_to_final_ask`). Nothing drove ask
1 → 2 → 3 → 4. That tested the destination and never the road, and the cursor
*is* the thread — it decides which reagent is asked for, whether
`RetainsDelivery` is attached, and whether a resolution is graded as final.

Six tests in `src/botanist/mod.rs`, built on `resolve_current_ask`, which reads
the reagent from whichever ask the cursor is on rather than taking one from the
caller. A test that passed in the reagent it expected would keep passing if the
cursor pointed elsewhere, which is the exact failure the section exists to catch.

- Helping with every early ask reaches the turn with zero refusals and costs
  Botany nothing.
- The asks are put in authored order, the last one is `player_only`, and none of
  the earlier ones are.
- Ignoring every ask still escalates to the turn and then closes the branch —
  the deliberate half of the cursor rule, without which ignoring the thread
  would be strictly safer than engaging and the central choice would never be
  offered.
- A save taken between asks resumes on the same ask and still runs to the end.
- A stray resolution after closure changes nothing.

**Defect found and fixed.** That last test failed on first run: `left: (4, 5,
true)` vs `right: (4, 4, true)`. The cursor was correctly clamped, but a
resolution arriving after the branch closed still **charged another refusal**,
cost Botany standing again, and aired another withheld line for an ask that had
already been answered. `generate_botanist_visit` stops at `supplied || closed`
(`botanist/mod.rs:188`); `handle_botanist_resolution` did not honour `closed` at
all — an asymmetry between the two halves of the same terminal state.
`OrderResolved` has several emitters (expiry, window delivery, counter handover)
in different systems, so "the last one already arrived" is not something this
reader can assume. Guard added at the top of the resolution loop.

Falsified: removing the guard fails exactly one test, the new one — 1677 passed,
1 failed.

Commands: `cargo test --bin chemgame` → **1678 passed, 0 failed** (1673 before).
`cargo clippy --bin chemgame --all-targets` → 124 warnings, unchanged.

**Next executable step:** the squashed commit of A–G, then the user's live pass.

After each packet update this ledger with exact changes, commands/results, known
limitations, files still in flight and the next executable step. Do not mark P7
complete based only on adapter unit tests. If implementation cannot complete a
packet, leave it open with a concrete continuation note in this document.

## Validation and definition of done

Build a production-system integration harness with the real selector, controller
handoffs, opportunity providers, navigation arrival, chemistry and incident
consumers. Deterministic fixtures arrange opportunities and accelerate authored
timers; they must not inject a fake successful action in place of the executor.
Call schedule initialization so Bevy query conflicts fail immediately.

Required scenarios:

1. **Useful ordinary life.** Each existing assistant completes real jobs in at
   least two eligible departments in a controlled workload. A specialist-only
   job remains unavailable. Hunger/social/emergency actions retain their current
   priority. No meaningful covert opportunity produces ordinary work, not a stall.
2. **Identity and ownership.** Spawn-before-script and script-before-spawn converge
   on one body. Repeated visits, expiry, order interruption, recovery and load
   never duplicate names or create competing movement owners. A busy assistant
   delays admission. Department counters credit the destination task correctly.
3. **Honest supply.** The same valid batch delivered early or late yields the same
   real custody. Mixtures retain exact contents/purity. Wrong/short/refused batches
   cannot fund an act. Repeated receipt/result processing pays and transfers once.
   Save before holder spawn, after partial use and after confiscation preserves
   the exact appropriate result. Old saves do not fabricate supplies or rewards.
   Drive Aleksy's four authored asks through real intake and delivery: the final
   approach is unavailable early, earlier successes are not refusals, final
   refusal/expiry closes the branch, and reload resumes the saved step exactly.
4. **Knowledge and prevention.** A visible opportunity is a positive control;
   the identical target behind a wall produces none. A remembered target moved
   unseen is not tracked remotely. A visible observer vetoes the act; a concealed
   witness may still observe it. Arriving observers, held/slotted/stored/despawned
   targets, full meals, confiscation and incapacity cancel without illicit effects.
5. **Existing Engineering trouble.** One expired ask can yield exactly one real
   dilution after travel and work. A ward suppresses it. Guarding/removing targets
   prevents it; expiry ends the attempt. The ignored-request charge occurs once
   regardless of success. The removed legacy route cannot create a second dose.
6. **Consequences with controls.** Recovering contaminated food before ingestion
   produces no patient or harm charge. Real exposure followed by symptoms triggers
   Medical through perception/reporting. Witnesses can be interviewed; no witness
   can produce a legitimate unknown finding. Treatment resolves the injury. Later
   unrelated poisoning is not attributed to the old meal. Repeated incident events
   do not repeat standing, campaign or instability changes.
7. **Authority and reconnect.** Clients cannot choose goals, grant supplies, finish
   actions or inspect secret components. Public movement/activity/chemistry remain
   visible. Host/client and late-join checks cover both assistants and resulting
   incidents without replicating private goals, stock or attribution.

Run one targeted `cargo test --bin chemgame <filter>` command per changed subsystem,
then `cargo test --workspace` and `cargo clippy --workspace --all-targets`. Record
pre-existing warnings separately. Falsify critical regression tests by temporarily
removing their fix in a controlled local edit, then restore it and rerun; do not
write tests that merely mirror a helper's arithmetic.

Extend existing decision/action logs with actor, trigger/receipt/action identity,
candidate rejection reason, target, cancellation, actual transfer and linked
incident ID. Keep secrets in authority debug logs only. Rate-limit repeated
rejections. This is evidence for debugging, not a new player-facing dashboard.

For live acceptance, use `cargo run -- --solo` in a disposable fresh-save setup,
with existing development speed/timing controls and confirmed isolated save paths.
Never overwrite the user's Chemist career. Observe a normal station segment where
the assistants actually help; then targeted allowed/prevented behavior and an
observable Medical/Security response. Record simulation duration and trace
timestamps so a stale `ailog.txt` cannot count as evidence. Use at least ten
simulated minutes for ordinary-work observation; targeted scenarios need their
actual end state, not an arbitrary waiting interval.

Automated scenario success does not prove visible behavior reads well. Record
separately whether a player can notice the approach/work interval, interrupt it,
recognize the changed batch or casualty, and follow the crew response. No forced
poisoning frequency or guaranteed normal-session sabotage is an acceptance metric.

## Starting evidence and remaining limits

The source review previously ran the existing covert tests: 13 passed. That proves
only their present coverage; they omit the integration gaps above. No runtime
implementation, new integration test, live gameplay or multiplayer validation has
been performed for this plan.

This plan narrows the earlier audit's suggested work: broad motive lifecycle,
new harmful actions, campaign attribution redesign and station-wide pressure
tuning remain outside this phase. Reuse existing systems first. Add only the
small interfaces needed to connect them and to prevent specific duplication,
identity, knowledge or causality errors documented above.
