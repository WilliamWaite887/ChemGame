# Playtesting ChemGame — what to watch for

Thanks for playing. This is a guide to **what we're unsure about**, not a list of
chores. You don't have to do all of it, and you don't have to do it in order.

The station runs a crew of AI characters who do their own work, hold their own
opinions, and occasionally do something they shouldn't. All of that is built and
all of it passes our automated tests. What the tests **cannot** tell us is
whether any of it is noticeable, understandable, or fun. That's what you're for.

**If you only answer two questions, make it these:**

> **1. Could you tell when someone was about to do something bad — before they
> did it?**
>
> **2. Does the station feel like people work there, or does it feel noisy?**

Everything below is detail. Those two are the point.

---

## How to report things

Plain sentences are perfect. "The guy in Engineering kept saying the same line
over and over, maybe 20 minutes in" is a great bug report. You do not need to
know why.

Three things make any report much more useful:

- **Roughly when it happened** (how long into the session). We can match it to
  our logs.
- **Where you were** (which room/department).
- **What you expected instead.** This is the valuable part. If something felt
  wrong but you can't say why, say exactly that — "it felt off when X happened"
  is a real finding.

**"I got bored" is a legitimate bug report.** So is "I didn't understand what
just happened." Please don't file those down into something politer.

### The log file (optional, only if you're comfortable)

The game writes a diagnostic file. If you can grab it after a session where
something odd happened, it helps a lot:

```
%LOCALAPPDATA%\ChemGame\ailog.txt
```

Paste that path into the File Explorer address bar. **It is wiped every time
the game launches**, so copy it out before relaunching.

You don't need to read it. Just send it along with a note about roughly when the
odd thing happened.

---

## 1. Do the crew look alive? (first 10 minutes, just watch)

Wander around and watch people. Don't intervene.

Look for:

- **Are they actually doing things**, or standing still and twitching?
- **Does anyone orbit one room forever**, or repeat the same action endlessly?
- **Does anyone get stuck** — walking into a wall, jittering, sliding without
  animating, snapping between positions?
- **Is anyone in two places at once?** You should never see the same named
  person twice.

Two characters are worth finding by name: **Tech Boyle** and **Grower Aleksy**.
They're meant to be the most "alive" people on the station and they do work
outside their own departments. If they seem duller than everyone else, that's
worth knowing.

> **The question:** do they read as *people who live here*, or as props that
> occasionally move? One sentence is enough.

---

## 2. Does the station sound right? (stand still for 2 minutes per room)

Crew talk out loud. Speech bubbles appear over their heads.

Go to each department in turn — Chemistry, Medical, Engineering, Botany,
Service, Cargo, Security — and just **stand there for two minutes** doing
nothing.

Look for:

- **Silence.** Did *anyone* speak? A totally silent department is a bug. Tell us
  which one.
- **Repetition.** Did the same person say the same line twice? In two minutes
  you should hear four or five lines from someone, and they should all be
  different. If you hear a repeat, note the line and the room.
- **Mismatch.** Does what they say match what they're doing? Someone sitting
  down on a break shouldn't say "nearly got this."
- **Someone knowing too much.** This one matters a lot. If a character says
  something they have **no way of knowing** — naming someone they didn't see,
  referring to something that happened in another room, or telling you a fact
  you only just learned — **write the line down word for word.** That's a
  serious bug, not a small one.

> **The question:** after ten minutes in a busy department, were you still
> reading the speech bubbles, or had you started ignoring them? If you tuned
> out, roughly when?
>
> Us adding more chatter than the game can carry is a real risk. If it feels
> like a laugh track, say so bluntly.

---

## 3. Catching someone in the act (the important bit)

Some crew members will, given the chance, tamper with food or lab equipment.
They will **not** do it while they think someone is watching.

This is the part of the game we're least sure about, so take your time.

**Try this:**

1. **Stand in plain sight and watch someone.** Nothing bad should happen while
   you're looking at them.
2. **Leave the room and come back.** Now it's possible. It might not happen —
   that's fine and normal, it's not guaranteed every session.
3. **Walk in on someone mid-act.** You should be able to stop it.
4. **Take the thing away.** Pick up the beaker or food they were heading for.
   They should give up and go back to normal work — not freeze, not teleport,
   not stand there forever.

> **The three questions, and these are the ones we most need answered:**
>
> **a. Could you tell they were up to something before they did it?** Was there
> any warning at all — how they moved, where they went, something they said?
>
> **b. When you tried to interrupt, did you have enough time?** They spend a few
> seconds doing the deed. Was that long enough for you to notice and react — or
> did you only catch them because you already knew to look?
>
> *(We think this one may be too fast. We picked the number and never tested it
> on a real person. If it feels impossible, that's the answer we expect, and
> it's genuinely useful — please don't assume you were just being slow.)*
>
> **c. When you did interrupt, was it obvious you'd interrupted something?** Or
> did it just feel like nothing happened?

---

## 4. Rumours and witnesses

If someone **saw** something suspicious, they'll get uneasy and mutter about it.

- They should sound uneasy **without explaining why**. Vague is correct here.
- **They must never name anyone.** A witness saw someone handling something —
  they don't know if it was innocent. If any muttered line names a person,
  **that's a hard failure — copy it down exactly.**
- **Walk up and talk to them directly.** What they tell you when asked can be
  much more specific than what they mutter to themselves. That difference is
  intentional — it's the reward for going and asking.
- Come back several minutes later. They should have **gotten over it**. If
  someone is still muttering about the same thing half an hour later, tell us.

---

## 5. When the station breaks

One specific thing to listen for: crew complaining they **can't get somewhere**
— lines like "can't get to it from here" or "there's no way through."

**One of these is fine.** Hearing it repeatedly in the same room is not flavour
— it means a character is genuinely stuck and can't reach their work. Note the
room. This is the single most useful bug you can find, because it's the game
telling you about a real problem in its own voice.

---

## 6. Grower Aleksy's requests

Aleksy comes to Chemistry and asks you for things, more than once, getting more
insistent each time.

- **Deliberately mess up an early request** — give him the wrong thing, or
  nothing. He should come back and ask again. He should *not* treat it as final.
- **On his last request, deliver late but correct.** Let him wait almost to the
  end, then hand over the right thing. **It must be accepted.** If he refuses a
  correct delivery just because it was slow, that's a bug we've fixed before and
  want to be sure stays fixed.
- Check he walks away with **what you actually gave him** — same stuff, same
  quality. If you gave him something watered down, he should have something
  watered down.
- After the handover he should go back to normal work, not vanish.

---

## 7. Saving and loading

Save and reload at an interesting moment — mid-delivery, mid-conversation,
while something is unfolding.

- Does everything carry on, or does a storyline quietly die?
- Does anything **duplicate**? Two of the same person, two of the same item, or
  getting paid twice for one delivery.
- Does anyone lose something they were carrying?

---

## 8. Playing together (only if you're testing multiplayer)

One person hosts, another joins.

- The joining player should see people **moving and working** normally.
- The joining player should **never** get extra information — no indication of
  who's up to something, no labels the host doesn't have.
- Does anyone slide, rubber-band, or walk in two directions at once on the
  client's screen?

---

## The short version

If you're pressed for time, here's the whole thing in one list:

| # | Watch for | Bad sign |
| --- | --- | --- |
| 1 | Crew doing real, varied work | Standing still, stuck, repeating forever, duplicated people |
| 2 | Rooms that sound inhabited | Total silence, or the same line twice in two minutes |
| 3 | Speech that fits the speaker | Someone knowing something they couldn't know ← **most serious** |
| 4 | Warning before something bad happens | No warning at all; no time to react |
| 5 | Interruptions landing visibly | You stop something and nothing acknowledges it |
| 6 | Witnesses being vague | A witness naming a person ← **hard failure** |
| 7 | "Can't get through" lines | Hearing it repeatedly in one room |
| 8 | Late-but-correct deliveries accepted | Refused for being slow |
| 9 | Saves preserving everything | Duplicates, lost items, storylines dying |

And again, the two that matter most:

> **Could you tell something bad was coming?**
>
> **Does the station feel alive, or just noisy?**
