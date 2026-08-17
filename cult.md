# Blood Cult — Current Mechanics

The Blood Cult is a campaign antagonist for the chemist. Its main loop is a
choice between accepting Corwin Ashe's suspicious requests for immediate stock
and dealing with the ritual manifestations that accepting them creates.

## Ritual progression

- Corwin Ashe, a recurring Cargo acolyte, makes three specific illicit
  requests: Space Drugs, Mindbreaker Toxin, then hazardous Flash Powder.
- A successful delivery advances the Cult's campaign plot and unlocks two
  ritual manifestations. A declined, expired, wrong, or otherwise failed
  delivery advances neither the ritual nor its plot.
- Each successful delivery also leaves a physical bottle of legitimate stock
  at the counter: Tricordrazine (10u), Saline-Glucose (15u), then
  Inaprovaline (10u).
- The request cadence is deliberately far slower than normal traffic, so
  Corwin reads as a recurring story thread rather than a routine customer.

## Ritual manifestations

There are six authored manifestations, two for each successful delivery:

| Stage | Manifestations | Treatment category |
| --- | --- | --- |
| First offering | Wet chalk sigil; Whispering residue | Utility cleaner; antitoxin |
| Clearer sight | Bleeding offering bowl; Scorched invocation | Trauma treatment; burn treatment |
| Final component | Airless candle; Rift-seal scar | Airloss treatment; antitoxin |

- Each manifestation is a visible red ritual anchor in the lab. Its radio
  clue describes observable symptoms but never names the remedy.
- Treat an anchor directly by carrying a container to it and interacting with
  it. The container must provide at least the authored amount of a reagent in
  the target treatment category; category peers are accepted.
- A correct treatment consumes the offered container, removes the anchor, and
  records a persistent ward. A wrong or insufficient mixture is not consumed.
- Anchors do not currently create their own timed hazard. Their pressure is
  the expanding case file and the strength of the later siege.
- The campaign persists discovered and neutralised manifestations. On loading
  a Cult save, every unresolved anchor is restored in the lab.

## Investigation and counterplay

- The standing board exposes a Cult case file as soon as a manifestation has
  been discovered, even before the station can name the antagonist. It shows
  discovered and neutralised counts, never the solution.
- The existing departmental counter-order track remains a separate route to
  stopping the Cult. Its Medical, Service, Security, and Engineering messages
  now reference the lab's hallucinations, residue, burns, and false wall.
- Completing department countermeasures can still stop the campaign before a
  showdown. Manifestations do not independently end the campaign.

## Final siege

When the Cult's plot reaches the showdown threshold, a breach opens in the
lab and vents Chloral Hydrate. The chemist wins the siege by treating the
breach with enough antitoxin-category reagent (the authored reference is
Dylovene).

Each neutralised manifestation makes that siege safer:

- +8 seconds to the deadline;
- +1.5 seconds between gas vents;
- -1 unit in each gas vent;
- -2 cure units required, with a floor of 6 units.

All six wards therefore produce the most forgiving version of the breach, but
never replace it with an automatic victory.

## Implementation notes

- Content lives in `assets/data/station.cult.ron`.
- Runtime progression, anchor interaction, reward spawning, and restore logic
  live in `src/cult/mod.rs`.
- Persisted incident state is part of `arc::Campaign`; siege scaling is in
  `src/showdown/mod.rs`.
- Focused Cult, Arc, UI, and Showdown test suites cover authored content,
  incident spawning, category treatment, persistence state, and siege scaling.
