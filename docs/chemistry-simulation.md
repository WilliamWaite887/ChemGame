# Chemistry simulation reference

This is the implementation reference for the sandbox material layer. Read the
[player handbook](chemistry-player-guide.md) for operation and the
[tutorial scenarios](chemistry-tutorial-scenarios.md) for reproducible lessons.
The Rust simulation and shipped RON data are authoritative. A documented claim
is only verified when its matching test or playtest passes.

## Gameplay contract

The player chooses ingredients, quantities, temperature, location, and delivery.
There are no extra stirring, sealing, humidity, pressure, or organ-management
controls. Correct existing synthesis methods remain reliable. Small errors have
proportional consequences; there is no mandatory grace countdown for a dangerous
mixture. Background air never ignites a room-temperature fuel on its own.

| Rule | Useful decision and observable result | Complexity cost and recovery |
|---|---|---|
| Water reactivity | Keep raw reactive material away from water; fizz/bang, heating and Ash reveal the result | Learn one transferable property. Small doses permit treatment; dangerous immediate mixes have no guaranteed rescue delay. No dissolution chores. |
| Neutralization | Choose opposing amounts; analyze remaining ingredients and salts | Inspect leftovers using existing pH/composition tools. Add the missing counterpart cautiously or discard. No extra capacity meter. |
| Combustion | Choose heat, fuel quantity and oxidizer; watch temperature, fire and residue | Manage existing heater controls and distance. Extinguish floor fires or remove heat before another batch. No sealing or air-management chore. |
| Body chemistry | Choose dose, route and timing; bodily injury and positional effects reveal reactions | Learn the digestion beat and limited background water. Stop dosing, treat injuries or use Medical recovery. No organs or new death system. |
| Spill chemistry | Choose where pools meet; color, heat and effects change with composition | Keep incompatible spills apart and clean mistakes. No fluid controls or ongoing supply maintenance. |
| Shared Ash | Reuse a familiar byproduct in synthesis and inspect it like other material | No separate card for every production route; named recipe inputs still require their actual substance. |
| Reaction chains | Use products and accumulated heat to start another interaction | Read ingredients and temperature; stop the initiating exposure/heat, extinguish or clean where possible. No universal warning countdown. |

Reject additions that require repeated maintenance without creating a useful
choice. The table describes recovery actions that exist; none promises to undo
an immediate explosion after it has fired.

## Data and interfaces

`ReagentDef.material` defaults to inert behavior. It contains `water`, `oxidizer`,
`water_reactive`, `fuel`, signed `acid_base`, `neutral_product`, and `residue`.
Reaction profiles name a product, activation temperature, optional rate, and
energy per unit. Finished compounds never inherit precursor properties.
Positive acid/base capacity denotes acid, negative denotes base. Existing pH
remains an approachable, volume-weighted gameplay value; it is not molarity.

`resolve_in_environment(solution, reactions, dt, activation, environment)` is
the common entry point. Existing `resolve_step` and activation-aware wrappers
use the same implementation with a room environment. Reports distinguish
authored `ReactionEvent`s from `MaterialEvent`s. Material events record consumed
ingredients, products, environmental contact and energy; they have no recipe ID.
The engine is independent of Bevy and can be exercised in unit tests.

Material products and numeric profiles are validated at catalog load. Missing
products and self-producing profiles are rejected. Products use the reagent's
authored pH and the lower input purity; neutral salts and Ash do not inherit
reactivity. Ash is the existing reusable reagent, not a second waste identity.

## Evaluation and accounting

1. Evaluate activated immediate interactions and activated combustion.
2. Run eligible authored synthesis using existing priorities, conditions and
   agitation provenance. Water-reactive materials with an authored rate and
   neutralization give synthesis its turn first.
3. Advance remaining material reactions, including environmental contact.
4. Repeat for new products and heat, with a 128-transition safety limit.

Each call shares recipe-rate and material-rate spending across all passes.
Environment supplies are also shared, never recreated inside the loop. Zero time
runs immediate explicit chemistry only. Game containers, idle machine reservoirs and puddles advance
timed chemistry in 0.1-second quanta; bodies advance on two-second beats.
Callers should use those quanta rather than arbitrary sub-centisecond steps:
chemical quantities are fixed-point hundredths of a unit. Live non-body steps
cap a frame's catch-up at two seconds; metabolism permits at most three beats
per frame. An agitation run owns its destination's allowance even on the update
where it finishes, preventing an idle-buffer pass from advancing it twice.

Water-reactive and fuel interactions consume equal volumes of material and
explicit partner. Environmental contact consumes tracked reactive material but
does not add extractable water/oxygen volume. All tracked consumed volume becomes
the product. Neutralization consumes acid and base in proportion to their signed
capacities, at up to one acid unit per second.

Energy is consumed quantity × profile energy × input purity. Heating is
`20 × energy / max(remaining mixture volume, 1)` kelvin, capped at 2000K.
Transfers mix temperatures by volume; this release uses equal game heat capacity
per unit. Dilution reduces the temperature rise without deleting reactive units.
These are gameplay coefficients, not physical measurements.

Water-reactive energy becomes proportional blast power without a flat bonus.
Combustion produces proportional nearby burn damage, not a glass-destroying
explosion. A blast below power 1 does not destroy its container or use the large
explosion sound. Existing authored explosive profiles retain their authored
behavior. Material effects are delivered during progress, independently of
the recipe-discovery batching used by timed synthesis. Sub-power-1 blasts use
the quieter reaction cue without a station-wide emergency announcement.

## Environment and bodies

Room air supplies up to 0.5 contact units of oxygen per second to activated fuel.
Every container type uses this same rule. Body contact supplies 0.5 water units
per second, shared between stomach and blood. It does not supply room oxygen
inside the body. These budgets cannot supply ordinary synthesis or be extracted.

Swallowed chemistry waits until the next metabolism beat, so the delay is from
almost zero to two seconds. The stomach reacts before transferring its normal
two-unit digestion share. Blood then reacts with the remaining environmental
budget before ordinary metabolism. Direct bloodstream exposure evaluates
immediate chemistry without earning another environmental budget.

Reaction reports enter a transient body outbox for central delivery. The outbox
is excluded from both RON and postcard serialization, preventing effect replay
on load or join. Its source compartment selects the correct smoke payload.
Attribution is conservative: conflicting exposure owners become unknown; being
the patient does not make someone the attacker.

An internal blast applies the ordinary distance-zero damage once, then the same
falloff to bystanders. The source body is never destroyed as glassware. Existing
collapse and recovery systems remain responsible for the aftermath.

## Floor and network integration

Puddles merge as whole mixtures when their footprints overlap, their elevations
are within 0.15m, and no solid intersects the connecting segment. Initial spills
seat on the highest authored floor below their release point. Deterministic
entity ordering prevents a puddle from being consumed twice in one merge pass.

Newly produced chemicals activate world profiles on the following update.
Previously released effects are not replayed just because a puddle merged.
Consumed fuel loses its fire profile. Existing extinguisher chemistry also
cools affected floor mixtures to at most room temperature so residual heat
does not immediately restart the fire; this is an intentional game shortcut.
Smoke takes material from the actual
container, buffer, body compartment or puddle; it does not copy an extra dose.
Chemical fire does not spread through a wall or between separate floor levels.

Only the authority advances chemistry and applies damage. Clients receive
solutions, bodies, puddles, smoke, and positional sound/effect results. Reaction
origins preserve source kind, position and attribution through effect handling.
Protocol revision 15 and the catalog fingerprint reject incompatible builds.
New reagent data is appended so existing numeric reagent identities stay stable;
legacy recipe identities remain in the catalog. Body/puddle wire shapes remain
compatible within the new protocol, and the transient effect outbox is never
serialized. Real postcard/RON round trips guard this contract.

Ash-producing reactions retain stable internal identities for old saves, but
are treated as byproduct behavior rather than research or recipe-book cards.
The old oil-to-Ash recipe is retained as a material-only legacy identity;
combustion now performs the transformation. Related hazard descriptions remain
on the source chemical's detail page. Residue does not inflate research counts.

## Verification and boundaries

Stable `sb01`–`sb17` identifiers link engine and ECS checks to tutorial material.
Run `cargo test -p chem_sim --test material`, the game tests containing `sb`,
then the workspace suite and clippy. Record actual results below or in the
tutorial verification ledger when completing a change.

This release does not simulate mixing smoke clouds, coatings on objects,
atmospheric depletion, spatial heating of nearby containers, pressure,
permanent death, or gibbing. Tests verify mechanics; player comprehension,
sound quality and tutorial pacing require rendered/manual inspection.
