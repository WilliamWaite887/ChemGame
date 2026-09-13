# Resident voice — giving the utility crew something to say

Next phase after the NPC thread integration (packets A–G, committed in
`14ada22`). One sentence: **the crew the utility AI took over went mute, and the
covert thread we just built is completely silent.**

## The finding this phase exists for

`speech::notice_arrivals` decides what situation someone is in
(`speech/mod.rs:578-590`):

```rust
let situation = if hostile {
    Situation::Hostile
} else if errand {
    Situation::Errand
} else if waiting {
    Situation::Waiting
} else if ambient {
    Situation::Passing
} else {
    continue;          // <- everyone the utility AI controls lands here
};
```

`Ambient` is removed the moment a resident is migrated to utility control
(`crew/mod.rs:1599`, and `crew/mod.rs:1373` documents residents "executing
utility work without `Ambient`"). A utility-controlled resident has no
`Ambient`, no `Errand` and no `Order` — so they hit that `continue` and are
**structurally incapable of speaking**, no matter how many lines are authored.

This is not a content shortage. There are 363 authored lines across five pools.
It is that the situation vocabulary has four entries and none of them describe
what a utility agent is ever doing.

The irony worth naming: `speech`'s own module doc says it was built because the
saboteur's walk was "real and invisible". We then rebuilt that thread on the
utility scheduler, which removed `Ambient`, which put the walk back to silent.

## What this is not

- **Not generated or LLM dialogue.** `Knows::departments()` is the best thing in
  the speech module and its doc states the rule outright: "A line that hands the
  player a fact its speaker has no way of knowing is not a clue, it is a readout
  with a face on it." Every gossip line is gated on what that speaker could
  plausibly have heard. Generation cannot preserve that; it would produce fluent
  lines that leak private state, and dialogue would stop being evidence.
- **Not a rewrite of `speech`.** The bubble presentation, replication shape,
  earshot/occlusion test, cooldowns, `pick`'s role weighting and the
  per-speaker exhaustion tracking all stay exactly as they are.
- **Not new replication.** Everything below reads `NpcActivity`, which is
  already `replicate::<NpcActivity>()`'d (`utility_ai/mod.rs:138`) and is
  already documented as carrying "no scores, knowledge, allegiance, or private
  target identity". No private component is read, ever.

## Packets

### Packet H — the mute residents — **landed 2026-09-09**

`Situation` gained `Working`, `Helping`, `Eating`, `Resting`, `Treating`, fed by
`Situation::from_activity` off the public replicated `NpcActivity`.
`Traveling` maps to the existing `Passing`; `Idle`, `Down` and `Socializing`
deliberately return `None` — `Socializing` because `start_exchanges` already
owns that state and a solo greeting would talk over the two-hander it produces.

The activity arm is checked **last** in `notice_arrivals`, after every component
state, because a utility agent recalled onto an errand or handed an order still
carries an `NpcActivity` and what they are here to do outranks what they were
doing before.

41 authored lines across the five new situations, in the register the file asks
for: these people are at work and the player is a colleague walking past, not a
customer.

**The test that let this ship, fixed.** `every_situation_has_a_role_agnostic_line`
already existed — with the four situations *written out inline*. A hardcoded
list cannot notice a variant nobody added to it, so it passed while most of the
station was mute. It now iterates `Situation::ALL`, kept honest by `name()`'s
exhaustive match (a new variant is a compile error there) plus
`the_situation_list_the_tests_iterate_is_complete`.

Falsified: removing the activity arm fails exactly one test,
`a_resident_at_work_can_still_greet_someone_who_walks_in` — 1681 passed, 1
failed. Negative control `a_body_between_actions_stays_quiet` keeps it honest.

Commands: `cargo test --bin chemgame` → **1682 passed, 0 failed** (1678 before).
Clippy 124, unchanged; the one `speech` warning is the pre-existing
`type_complexity` on `notice_arrivals`' query, present before this change.

### Packet H — original scope

Extend `Situation` with the states a utility agent is actually in, sourced from
the public `NpcActivity` and nothing else.

| New situation | Source | Why it earns its place |
| --- | --- | --- |
| `Working` | `NpcActivity::Working` | The commonest state on the station and currently silent |
| `Helping` | `NpcActivity::Helping` | Distinguishes assisting from own duty |
| `Eating` | `NpcActivity::Eating` | The meal thread's only ambient presence |
| `Resting` | `NpcActivity::Resting` | Break rooms currently silent |
| `Treating` | `NpcActivity::Treating` | Medical response has no voice at the scene |

`Traveling` deliberately maps to the existing `Passing` rather than a new
variant: it is the same thing from the player's side, and splitting it would
divide the biggest authored pool for no gain.

`Idle` and `Down` get **no** line. Idle is the selector between actions, not a
state anyone is in for long; `Down` is a collapsed body, which `collapses`
already covers.

The ordering in `notice_arrivals` must keep `Errand`/`Hostile`/`Waiting` ahead
of activity, because those are components a body can carry *while* an activity
is set, and the existing comment already says the thing they are doing right now
is what they talk about.

Authored lines for each, following the file's stated rule ("Keep these SHORT").
Role-specific and role-agnostic pools per situation, as the existing sections do.

**Falsification:** with the new arms removed, a `Working` resident produces no
speech — which is today's behaviour and is exactly the defect.

### Packet I — the covert thread gets a voice — **landed 2026-09-09**

`Situation::Witnessed`, triggered by `unsettled()` — a free function over the
witness's own `NpcMemory`, asking for a `SuspiciousHandling` fact whose decayed
confidence still clears `WORTH_MENTIONING`.

The chain now closes end to end: a covert act emits `SuspiciousHandling`
(`covert.rs:848` for food, `covert.rs:1045` for glassware) → `witness_stimuli`
writes it into whoever could see → `unsettled` → a bark in the room. Before
this the covert thread made **no sound at all**: the approach was visible and
silent, so a player who happened not to be looking learned nothing.

**Threshold reuses `interviews`' `USABLE_TESTIMONY` (0.2) rather than inventing
a second number.** A witness too unsure to be worth recording under questioning
should not be muttering about it either, and two thresholds would drift into a
character who will not tell an investigator what they will say to the room.
Confidence decay does the rest: 420 s retention, fading continuously, so the
barks stop on their own with no second timer.

**Ordering:** `Witnessed` sits ahead of activity and behind every component
state. Someone who saw something odd is still doing their job — the unease is
what they mention — but a person on an errand or holding an order is here for a
reason that still outranks it.

**The integrity rule, enforced not just documented.** No witness line names
anybody. `a_witness_never_names_anyone` checks every `Witnessed` line against
the full `station.crew.ron` roster (full names *and* surnames), plus Boyle and
Aleksy, plus every department name. The witness saw handling, not intent, and
`SuspiciousHandling` is documented as deliberately ambiguous — innocent and
covert handling produce the same kind. `interviews` stays the only route to a
name. Falsified by authoring `"I saw Boyle back there."`, which fails the test.

Nothing reads `TamperedMeals`, `CovertGoal` or `TamperAuthorization`. The
culprit is safe by construction rather than by a check: `witness_stimuli`
already refuses to let anyone witness their own deed, so an antagonist never
holds a memory of their own act to mutter about.

Reading authority-only `NpcMemory` here is architecturally sound because the
decision half of `speech` runs `is_authority` and `Speech` itself carries only
words and a tone.

Falsified: disabling the `Witnessed` arm fails exactly one test — 1684 passed,
1 failed.

Commands: `cargo test --bin chemgame` → **1685 passed, 0 failed** (1682 before).
Clippy 124, unchanged.

### Packet I — original scope

The direct payoff for A–G. Two additions, both strictly evidence-shaped.

**`Situation::Witnessed`** — for a resident holding a `SuspiciousHandling`
memory. `perception::NpcMemory` and `MemoryFact` already exist and already
record perception-time rather than live truth, and `reports.rs` already refuses
to broadcast `SuspiciousHandling` to the radio. A spoken line is the correct
channel for it precisely because it is local, deniable and requires the player
to be standing there.

Hard rules, and the phase fails if any is broken:

- The line **never names the person handled**. It reports unease, not identity:
  the witness saw handling, not intent. `interviews` remains the only route by
  which a name is ever produced, and it already derives that from the witness's
  own memory.
- The line is gated on the memory's own confidence and age, using the existing
  `NpcMemory::best`/`best_about` accessors, not on ground truth.
- Nothing reads `TamperedMeals`, `CovertGoal` or `TamperAuthorization`. Those
  are authority-private and the acceptance harness asserts they are
  unreplicated.

**A `Knows` variant for it.** `SomethingWasHandled` or similar, so the existing
gossip pool can carry it under `departments()` — anyone can hold it, because
anyone can see a room.

**Falsification:** a witness with no memory says nothing; a witness whose memory
has decayed past threshold says nothing; the line is asserted not to contain the
handled party's name.

### Packet J — interrupted, and the rest of the utility lifecycle — **landed 2026-09-13**

`notice_action_results`, a new system reading `UtilityActionResolved`, matched
to the body **by entity**. Two outcomes speak; four stay silent.

**The authoring enum, and why it is not `ActionResult`.** `SpokenResult` is a
speech-local enum with exactly two variants, converted by `SpokenResult::of`.
The alternative — deriving `Deserialize` on `utility_ai::ActionResult` so the
RON could name it — was rejected for two reasons. Only two of that enum's six
variants describe anything a character experiences: `Completed` is most work
most of the time, and `ReservationUnavailable`/`InvalidTarget`/`TimedOut` are
scheduler bookkeeping nobody notices. Making them *authorable* invites exactly
the lines that should not exist. And it points the dependency the wrong way:
speech reads the utility AI's public output, and the utility AI should not grow
a serialization surface to serve a bark pool. `of`'s match is exhaustive, so a
new `ActionResult` variant is a compile error that forces the question.

**Two gates the plan did not specify, and both earn their place.**

*Cooldown.* The plan said "with the existing cooldown", but `notice_resolutions`
— the system whose shape this reuses — does **not** set one, because a line at
the counter is a one-off answer to something the player just did. These are
unprompted and fire on the scheduler's clock, and `Unreachable` repeats by
nature — that repetition is precisely what makes it a stall signature. Without
the gate a genuinely stuck agent becomes a stuck record, which is worse than the
silence it replaced. The countdown is still ticked by `notice_arrivals` alone;
this system reads it without decrementing, because two systems subtracting `dt`
from one timer would halve the authored cooldown.

*Earshot before cooldown.* Checked in that order deliberately. A resolution the
player could not hear must not spend the body's next chance to speak — otherwise
a resident whose work failed across the station arrives in the room a moment
later already mute.

**Nothing says what the work was.** A resident interrupted mid-sabotage and one
interrupted mid-repair draw from the same pool, for the same reason the `Errand`
pool does: a line that read differently for a covert act would label the act,
and the tell is supposed to be behaviour.
`an_interruption_never_says_what_was_interrupted` enforces it against the words
a line would reach for.

**Falsified, three times, each failing only its own tests:**

| Guard removed | Result |
| --- | --- |
| `Interrupted` arm of `SpokenResult::of` | 2 failed (`an_interrupted_worker_says_so`, `a_resolution_speaks_through_its_own_entity_and_no_one_elses`) |
| the cooldown gate | 1 failed (`a_repeatedly_failing_worker_does_not_become_a_stuck_record`) |
| earshot ahead of the cooldown set | 1 failed (`a_failure_across_the_station_is_neither_heard_nor_charged_for`) |

`ordinary_and_bookkeeping_outcomes_stay_quiet` is the negative control over all
four silent results.

Commands: `cargo test --bin chemgame` → **1694 passed, 0 failed** (1685 before).
Clippy 124, unchanged.

### Packet J — original scope

`UtilityActionResolved` already carries `ActionResult`, and `decision_log`
already writes `DONE!` for every non-`Completed` result. The room stays silent.

- A line on `ActionResult::Interrupted` — the player just stopped something and
  currently gets no acknowledgement that they did.
- A line on `Unreachable`, which is *also* the stall signature: if the player
  starts hearing "can't get to it" repeatedly, that is a real diagnostic
  reaching them through the fiction rather than through `ailog.txt`.

Reuses `notice_resolutions`' shape exactly — a `MessageReader`, matched to a
body, with the existing cooldown.

**Note:** `notice_resolutions` matches by *name* because `OrderResolved` carries
no entity. `UtilityActionResolved` carries `agent: Entity` directly, so this one
matches by entity and is strictly sounder. Worth stating so the next reader does
not "fix" it into consistency with the older one.

### Packet K — density and acceptance — **landed 2026-09-13**

**The count, measured rather than assumed.** The distribution after H–J was
healthier than this plan feared: every situation already had at least four
role-agnostic lines, twice the existing test's floor. The thin pools by *total*
were `Eating` (5), `Resting` (5) and `Treating` (5) — and those are exactly the
three a player stands **near** the longest, because a mess hall or a medbay is
somewhere you linger, unlike a corridor you cross. Repetition surfaces there
first, at a lower line count than anywhere else. Filled to 11, 10 and 10.

Arrivals now stand at **99 lines over ten situations**, plus 16 action-result
lines. The point the original scope made is the one that held up: the total was
never the number that mattered. The same body of writing spread over ten
situations instead of four is what made the station sound inhabited, and packet
H's redistribution did more for density than any new authoring.

**`Hostile` was deliberately left at 4**, and is now asserted to *stay* under
the floor rather than merely skipped. Its own note in the file — "nobody making
a speech is actually coming for you" — is a design choice, and a player hears at
most one of these before the encounter resolves, so breadth buys nothing and
dilutes lines chosen to land hard. Widening the exemption should require coming
to the test and arguing for it.

**The structural guard.** `no_situation_is_thin_enough_to_repeat_itself` floors
each situation at five *total* lines. Five because that is where repetition
becomes audible: the cooldown is 16–30 s, so two minutes in one room is four or
five lines from the same body, and a pool of four guarantees a repeat inside
that window. It iterates `Situation::ALL`, so it inherits H's fix — a variant
nobody added to the test cannot ship thin. Falsified by cutting `Resting` to
three: fails with `Resting has 3 lines; under 5 a body repeats itself…`.

`every_spoken_result_has_a_role_agnostic_line` and
`the_spoken_result_list_the_tests_iterate_is_complete` give `SpokenResult` the
same treatment.

**The listening pass** is `docs/npc-thread-live-acceptance.md` section 7, in
three parts: two minutes standing in each department (7a), the witness barks
and their decay (7b), and interruption plus the stall signature (7c). Its
framing question is this plan's own: does the station sound like people work
there, or like a room with a laugh track.

The doc's header now also states plainly that **nothing in it has ever been
run**, and names the three judgement calls still unanswered.

Commands: `cargo test --bin chemgame` → **1695 passed, 0 failed**.
Clippy 124, unchanged.

### Packet K — original scope

- Count lines per situation after H–J and fill the thin ones. The metric that
  matters is lines *per situation*, not the total: 363 over four situations is
  thin per situation; the same number over eleven would feel richer with no new
  writing, and that is the actual lever.
- A test asserting every `Situation` variant has at least one role-agnostic
  authored line, so a new variant can never ship mute. This is the structural
  guard that would have caught the original defect if `Situation` had been
  exhaustive over activity in the first place.
- Extend `docs/npc-thread-live-acceptance.md` with a listening pass: stand in
  each department and note whether the room sounds inhabited, and whether any
  line ever told you something its speaker could not know.

## Validation

Per-packet: `cargo test --bin chemgame speech` and `cargo test --bin chemgame
utility_ai`. Then the full `cargo test --bin chemgame` and `cargo clippy --bin
chemgame --all-targets`, recording the pre-existing 124 warnings separately.

Falsify each guard by removing it, confirming the specific test fails, and
restoring.

**Baseline to compare against:** 1678 passing, 124 clippy warnings, as of
`14ada22`.

The reads-well question no test answers, and the one to bring to the live pass:
does the station sound like people work there, or like a room with a laugh
track? Volume is not the goal — a line the player stops reading is worse than
silence.
