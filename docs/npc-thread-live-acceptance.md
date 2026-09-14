# NPC thread — live acceptance checklist

For the player to run. Packets A–G (the NPC thread) and H–K (resident voice) are
all automated-green — 1695 tests as of this writing — but no test can tell you
whether the behaviour *reads* from the chair. That is what this is for.

Automated coverage proves the rules hold. It cannot prove you can notice an
antagonist approaching, understand why the batch changed colour, follow the
crew's reaction, or hear a station that sounds inhabited rather than noisy.
Those are the only questions here.

**This file is for someone who knows the codebase.** It names components,
packet numbers and `ailog.txt` greps. For handing to an outside playtester, use
`docs/playtest-what-to-watch-for.md` instead — the same questions in plain
language, with no internal vocabulary. The two cover the same ground; if you
change an acceptance criterion here, change it there too.

**Nothing in this file has ever been run.** Every packet shipped on automated
tests alone, and the three judgement calls most likely to change the design —
whether an antagonist's approach is noticeable (section 4), whether the work
interval is long enough to interrupt on reaction (section 4), and whether the
station now sounds inhabited or like a laugh track (section 7) — are all still
unanswered.

## Before you start

The evidence file is `ailog.txt`, written one directory above `saves/`. It is
**truncated on every launch**, so a line you find there is from this session —
but check the timestamps anyway, because a session you left running in another
window would also be writing to it.

Lines you will be looking for:

| Prefix | Means |
| --- | --- |
| `PICK <name> -> Action/target score=N  vs …` | who decided what, and what lost |
| `DONE <name> Action/target = Completed` | it finished |
| `DONE!<name> Action/target = Interrupted` | it did **not** — the `!` is greppable |
| `STATION …` | one whole-station line every snapshot interval |

A useful habit: note the wall-clock time you start each section, so you can tell
a fresh line from one written twenty minutes ago.

**No forced frequency is an acceptance criterion.** If a covert attempt never
happens in a given session that is a legitimate outcome, not a failed test. What
would be a failure is it never being *possible* — see section 4.

---

## 1. Ordinary life (≈10 minutes, no intervention)

Both assistants should be doing real work in more than one department, not
standing still and not orbiting one room.

- [ ] Find **Tech Boyle** and **Grower Aleksy**. Both have bodies, both move.
- [ ] Watch each for a few minutes. Do they do work that belongs to a department
      other than their home one? (Boyle is Engineering-primary, cross-trained
      into Botany and Service; Aleksy the reverse.)
- [ ] `grep PICK ailog.txt | grep -E "Boyle|Aleksy"` — are they choosing varied
      actions, or the same one forever?
- [ ] `grep "DONE!" ailog.txt | grep -E "Boyle|Aleksy"` — a steady run of
      `Interrupted` or `Unreachable` for one of them is the stall signature.
- [ ] `grep "(nothing)" ailog.txt` — repeated forever for one agent means they
      can see work and never take it.

**Reads-well question:** do they look like inhabitants, or like props that
occasionally twitch? Write down which, in one sentence.

## 2. Identity and ownership

- [ ] Neither name appears twice on the station at once.
- [ ] Neither shows up as a random customer at the counter.
- [ ] When one of them is doing something scripted, nobody else is also driving
      that body — no sliding, no snapping back, no walking in two directions.

## 3. The Botany ask (Aleksy)

Aleksy visits Chemistry with an authored request, more than once, escalating.

- [ ] He arrives and asks. You can hear/read what he wants.
- [ ] **Deliver the wrong thing, or nothing, on an early visit.** He should come
      back — this is not the final ask and must not be graded as a refusal.
- [ ] **On the final ask, deliver late but valid** (let the patience bar run
      most of the way down, then hand over a correct mixture). It must be
      **accepted**. This is the patience-as-quantity bug: a late-but-correct
      delivery used to be graded a refusal.
- [ ] What he walks away with is *what you actually handed him* — same mixture,
      same purity. If you gave him something dilute, he has something dilute.
- [ ] After the handover he goes back to ordinary work rather than vanishing.
- [ ] **Save and reload here.** He still has his batch, and the thread is still
      live. (Reload used to silently disarm the motive: stock kept, goal gone.)

## 4. Knowledge and prevention (the important section)

This is where the phase either earned its keep or didn't.

- [ ] **Stand and watch.** Position yourself in plain sight of an antagonist who
      has a reason and a target. Nothing should happen while you are looking.
      Confirm in the log: no `PICK … TamperContainer` / `PoisonFood` for them
      while you are in the room.
- [ ] **Leave the room and wait.** Now it becomes possible. It may or may not
      happen — both are fine — but the *offer* should appear in the log.
- [ ] **Walk in mid-act.** Start watching while they are on their way or
      working. Look for `DONE!… = Interrupted`. Nothing should have been
      transferred.
- [ ] **Take the target away.** Pick up the beaker they were heading for. They
      should fail cleanly and go back to work, not freeze or teleport.
- [ ] **Wait out the window.** Do nothing for a couple of minutes of station
      time with the target guarded. The attempt should expire with nothing
      added.

**Reads-well questions**, and these are the ones I most want answered:
1. Could you tell they were approaching, before anything happened?
2. Was the work interval long enough to interrupt on reaction, or did you only
   catch it because you knew to look?
3. When you interrupted, was it *legible* that you had interrupted something?

## 5. Engineering trouble (Boyle)

- [ ] Ignore an Engineering request until it expires. Nothing should happen to
      any glassware **at that instant** — you have a window, not a punishment.
- [ ] Within that window, if he gets an unobserved moment, a loose container's
      contents change. Look at it: colour/volume should differ, and it should
      look like something was poured in, because it was.
- [ ] The radio aftermath line should not claim anyone *saw* him unless someone
      did.
- [ ] Trigger it a second time. One trigger, one dose — not two.

## 6. Consequences (Medical and Security)

Only reachable if section 4 or 5 actually produced a completed act on food.

- [ ] Someone eats a tampered meal and shows real symptoms.
- [ ] Medical responds to the casualty as they would to any poisoning.
- [ ] An investigation opens. Security asks people what they saw.
- [ ] **A witness who saw nothing relevant says nothing useful.** Testimony
      should concern *that* meal, not the witness's most recent unrelated
      memory of someone handling a container. (This is the fix that stops an
      innocent person being named.)
- [ ] A tampered beaker that nobody drank from summons nobody.
- [ ] A *later, unrelated* poisoning is not blamed on the earlier meal.

## 7. The listening pass (added for the resident-voice phase)

Everything above asks what the crew *did*. This asks what they **said**, which
is a separate phase's work (`docs/resident-voice-plan.md`, packets H–K) and has
its own failure mode: before it, most of the station was structurally mute, and
the obvious overcorrection is a station that will not shut up.

**The question this section exists to answer, and it outranks every checkbox
below: does the station sound like people work there, or like a room with a
laugh track?** Volume is not the goal. A line the player has stopped reading is
worse than silence, because it trains them to ignore the channel the important
lines arrive on.

### 7a. Stand in each department for two minutes

One at a time, doing nothing. Chemistry, Medical, Engineering, Botany, Service,
Cargo, Security.

- [ ] Did anyone speak at all? A department that is still silent is the packet-H
      defect surviving somewhere — note **which** department, because that
      points at a specific `NpcActivity` never being set there.
- [ ] Did you hear the **same line twice** from any one person? The cooldown is
      16–30 s, so two minutes is four or five lines from one body. A repeat
      inside that window means that situation's pool is too thin — note the
      line and the room.
- [ ] Did the lines fit what the person was visibly doing? Someone sitting down
      should not be saying "nearly got this."
- [ ] **Did any line tell you something its speaker could not know?** This is
      the integrity rule and the one real failure. Write down the exact line.

### 7b. The witness barks

Only reachable once section 4 or 5 has produced a covert act somebody saw.

- [ ] Stand near someone who was in the room when it happened. They should
      sound uneasy without explaining why.
- [ ] **No bark ever names anybody.** A witness saw handling, not intent, and
      cannot tell an antagonist from a colleague tidying up. If a line names a
      person, that is a hard failure — write it down verbatim.
- [ ] Come back several minutes later. The unease should have faded on its own
      (the memory decays over ~420 s), not persisted all session.
- [ ] Ask them directly. What they say **under questioning** may be much more
      specific than what they mutter to the room — that difference is the
      design, not a bug.

### 7c. Interruption and the stall signature

- [ ] Walk in and stop someone mid-task. **Did you hear that you had stopped
      something?** Before this phase there was no acknowledgement at all, so an
      interruption was indistinguishable from nothing having happened.
- [ ] Was the acknowledgement legible as *interruption* rather than as a
      generic greeting? Cross-check `grep "DONE!" ailog.txt` for an
      `Interrupted` at the same moment.
- [ ] If you hear **"can't get to it"-type lines repeatedly in one room**, that
      is not flavour — it is the stall signature reaching you through the
      fiction. Note the room and cross-check `grep "Unreachable" ailog.txt`.
      This is the one line in the game that is a bug report.

**Reads-well questions for this section:**
1. After ten minutes in a busy department, were you still reading the bubbles,
   or had you started tuning them out? If you tuned out, roughly when?
2. Was there a moment where a line made you look at someone you would otherwise
   have walked past? That is the whole phase working.

## 8. Authority and reconnect

- [ ] Host a session, have a client join. The client sees activity and posture,
      never intent — no indication of who is armed or what they are planning.
- [ ] Save, quit, reload mid-thread. Nothing duplicates, nothing is lost, no
      second reward is granted for an already-paid delivery.

---

## What to send back

For each section, one line: **worked / didn't / never came up**. For the
reads-well questions in sections 1, 4 and 7, a sentence each — those are
judgement calls no test can make, and they are the ones most likely to change
the code.

Two answers are worth more than all the checkboxes combined, so give them even
if you skip everything else:

1. **Could you tell an antagonist was approaching, before anything happened?**
   (section 4)
2. **Does the station sound inhabited, or like a laugh track?** (section 7)

If something felt wrong but you can't say why, say that too, with roughly when
it happened; the log timestamps will find it.
