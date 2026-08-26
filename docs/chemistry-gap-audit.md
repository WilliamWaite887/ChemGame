# Chemistry Roadmap Gap Audit

This is a static coverage audit of the checked-in `Guide to chemistry` and
`Chemical recipes` TG Station snapshots against ChemGame's current RON data and
gameplay systems. The executable data and validation tests remain
authoritative. This document records absent guide entries and adaptation
decisions that are too detailed for the high-level coverage summary.

Audit baseline: 162 reaction definitions and 196 reagent definitions in
`assets/data/chem.reactions.ron` and `assets/data/chem.reagents.ron`.

## State definitions

| State | Meaning in this audit |
|---|---|
| `Implemented` | The guide mechanic is represented directly and has a reachable process, meaningful use, feedback, and validation. |
| `Adapted` | The guide entry is reachable and useful, but its TG-specific behavior or ingredients were translated into ChemGame's systems. |
| `Unreviewed` | The guide entry is confirmed absent. It is still in scope, but no final ChemGame adaptation has been implemented. |
| `Blocked by mechanic` | A recipe could be entered, but its defining use depends on a system ChemGame does not yet have. It must not be counted complete as inert content. |
| `Excluded` | The entry is outside the approved scope, a duplicate formula, or a non-recipe reference. |

## Roadmap status

| Wave | State | Current evidence and remaining gap |
|---|---|---|
| 0. Coverage and validation | Implemented | The data suite validates shipped identities, reachability, primary synthesis paths, process requirements, effects, hints, requests, and operating ranges. Loading now fails closed on duplicate identifiers, invalid numeric/effect profiles, non-positive stoichiometry, malformed process ranges, and reaction products trapped behind unreachable dependency cycles. |
| 1. Dangerous chemistry | Implemented | The approved Ash-to-RDX ladder, stabilizer failures, thermal initiation, controlled classification, charges, blast effects, attribution, and demolition demand are present. |
| 2. Quality-control chemistry | Implemented | Portion purity, weighted mixing, reagent pH, buffers, consumed-reactant-only reaction quality, pH/rate/purity controls, explicit dangerous failures, directional inverse recovery, purity-scaled body/world/research effects, analyzer purification, pH paper, and instrument feedback are present. |
| 3. Component backbone | Adapted | Acetaldehyde and Pentaerythritol form a reachable expert backbone. Hyper-Plasmium Oxide now comes from a salvage geode and feeds Exotic Stabilizer; Wittel remains excluded because no retained recipe requires it. |
| 4. Complete medicine ladder | Implemented | 41 of 54 guide medicines are playable: 13/13 core, 11/11 superior, and 17/30 unique. All 13 remaining unique entries are explicitly blocked by an unsupported body/species system or excluded by roadmap scope. Every supported treatment family has recurring service demand, and critical-care/hazard classification is regression-tested. |
| 5. Toxins and narcotics | Adapted | The narcotics table, controlled-drug chains, overdose, addiction, withdrawal, purge, contraband, evidence hooks, NPC mood behavior, specialist toxins, and counteragents are implemented. Unsupported organ-only behavior is adapted to readable damage/status channels; hydroponics-dependent herbicides remain mechanic-blocked rather than becoming inert filler. |
| 6. Pyrotechnics and utility | Implemented | Fire, cold, smoke, mixture-carrying foam, temporary Metal Foam barriers, corrosion, lubrication, sterilization, drying, Carbon Dioxide extinguishing, Pax pacification, catalytic heating, concussion, push/pull force, TaTP, EMP, and Tesla arcing all have reachable chemistry, world/body behavior, progression demand, and tests. Smart Metal Foam is explicitly excluded; Gravitum is mechanic-blocked. |
| 7. Delivery forms and tools | Implemented | Four-slot replicated hotbar inventory; bottles, pills, syringes, full-dose patches, 3u aimed sprays, 30u refillable smoke projectors, reaction smoke, splash/puddles, mixture-carrying foam, charges, pH paper, grinder, analyzer/HPLC, and fixed two-side staged mixing are complete. Injection, ingestion, patching, spray, splash/puddle contact, and inhalation now have distinct absorption, timing, contact, topical, ownership, and metabolism behavior. A portable mixer was deliberately declined; staged preparation remains laboratory work. |
| 8. Integration and completion | Implemented | The book reports Fundamentals/Intermediate/Advanced/Expert/Mastery stages, recommends an actionable next method, and exposes paginated prerequisite trees. Required orders remain stock- and knowledge-reachable; optional development work is limited to one unfamiliar reaction. Exact-quality demand, patience, team load, HPLC calibration, research rewards, and Security authorization are staged. External deliveries refill least-stocked sources. Fresh, intermediate, advanced, expert, and mastery saves round-trip while traversing the complete dependency frontier without a stall. |
| 9. Complete written chemistry guide | Implemented | `docs/chemistry-player-guide.md` is the standalone beginner-to-advanced handbook. A future interactive tutorial is deliberately separate and not part of this text-only wave. |

## Hardening pass

The audit-driven hardening pass closes the integrity gaps most likely to
silently corrupt a long save or multiplayer session:

- HPLC recovery is one-way and only failure products declare a recovery
  target; clean members of an inverse pair remain themselves.
- Every machine mutation verifies the sending chemist, claimed machine,
  expected machine kind, and conscious/active body state on the authority.
- LAN and Steam compatibility fingerprints include reagent, reaction,
  produce, status, and append-only container schemas. Inventory ownership,
  selection, payload kind, and entity references replicate with mapped IDs, so
  peers cannot interpret indexed chemistry or hotbar state against a different
  build.
- Reaction purity is calculated from consumed reactants, never unrelated
  filler or catalysts.
- Purity scales world effects and research value, and authored grants retain
  the reagent's real pH.
- Timed requests for botanical branches require the relevant physical stock;
  theoretical guide reachability alone is not enough.
- Catalog loading rejects duplicate IDs, bad numeric profiles, impossible
  ranges/amounts, and unreachable circular dependency products.

## Guide components and reaction agents

The component table contains 17 rows but only 16 distinct chemicals because it
lists two Lye formulas. The current component coverage is:

| State | Guide entries | Dependency or adaptation note |
|---|---|---|
| `Implemented` | Ash, Oil, Acetone, Diethylamine, Phenol, Ammonia, Saltpetre, Sodium Chloride, Lye, Hydrogen Peroxide, Acetone Oxide | Reachable from starting stock or authored intermediates and used downstream. |
| `Adapted` | Pentaerythritol, Acetaldehyde | Both expert intermediates are playable with temperature, pH, purity, yield loss, and downstream Penthrite use. |
| `Excluded` | second Lye formula | Duplicate formula, not a separate chemical. |
| `Excluded` | Wittel | Its Lavaland/geyser acquisition simulation is outside scope and no retained downstream recipe currently needs it. |
| `Adapted` | Hyper-Plasmium Oxide | A cargo salvage geode replaces the unsupported geyser source and grinds into Hyper-Plasmium Oxide plus Plasma. |
| `Implemented` | Exotic Stabilizer | Equal Hyper-Plasmium Oxide and Stabilizing Agent make the catalyst required to carry TaTP safely through its first temperature window. |

Acidic Buffer and Basic Buffer are implemented directly. Chiral Inverting
Buffer, Universal Indicator, Purity Tester Reagent, and Prefactor A/B are
`Adapted`: inverse recovery is an HPLC operation, pH and purity are instrument
readouts/pH-paper checks, and rate/yield control is carried by equipment,
catalysts, temperature, pH, and authored process requirements.

## Medicine ladder

`Adapted` here means the medicine has a recipe or source and a meaningful
ChemGame effect. A named exact order is useful progression content, but is not
required for every medicine when category-based medical incidents already
create demand for its treatment family.

### Core healing medicines — 13/13 adapted

| Guide entry | State | Current use or missing dependency |
|---|---|---|
| Libital | `Adapted` | Advanced trauma treatment; sodium-catalyzed and purity-sensitive, with Libitoil inverse recovery. |
| Helbital | `Adapted` | Strong trauma treatment with overdose risk. |
| Probital | `Adapted` | Physical trauma treatment built from Acetone and mineral inputs. |
| Aiuri | `Adapted` | Burn treatment using Ammonia and Sulphuric Acid. |
| Lenturi | `Adapted` | Superior burn treatment and the 95% HPLC mastery order. |
| Granibitaluri | `Adapted` | Mixed trauma/burn medicine with an iron catalyst and a specific medical order. |
| Synthflesh | `Adapted` | Combined topical trauma/burn repair. |
| Multiver | `Adapted` | Poison purge/antitoxin and a major downstream component. |
| Seiver | `Adapted` | Combined toxin/radiation treatment with a 90% exact request. |
| Tirimol | `Adapted` | Trauma-focused medicine with catalyst requirements. |
| Convermol | `Adapted` | Burn-focused medicine with a temperature-sensitive failure path. |
| Hercuri | `Adapted` | Cold-controlled Cryostylane/Lye/Bromine burn healer and extinguisher. Acidic batches form recoverable Herignis, and overdose causes dangerous chilling. |
| Syriniver | `Adapted` | Aggressive toxin healer/purge with a 6u dose ceiling. Its Nitrous Oxide intermediate has a narrow hot operating window and explosive overheat; unsupported IV/liver behavior becomes readable systemic cost. |

### Superior healing medicines — 11/11 adapted

| Guide entry | State | Current use or missing dependency |
|---|---|---|
| Salicylic Acid | `Adapted` | Moderate trauma medicine and Salbutamol precursor. |
| Oxandrolone | `Adapted` | High-potency burn medicine. |
| Salbutamol | `Adapted` | Airloss treatment and Choking counter. |
| Pentetic Acid | `Adapted` | Staged toxin/radiation chelation with minimum purity. |
| Atropine | `Adapted` | Critical-care toxin/airloss treatment and stabilizer. |
| Calomel | `Adapted` | Aggressive purge with a narrow safe dose. |
| Ammoniated Mercury | `Adapted` | Advanced purge and 85% toxicology request. |
| Cryoxadone | `Adapted` | Cold-synthesis broad recovery medicine. |
| Rezadone | `Adapted` | Superior mixed trauma/burn medicine using grinder-sourced Carpotoxin. |
| Regenerative Jelly | `Adapted` | All-four-damage restorative made from purified Omnizine and Slime Jelly. Ambrosia Deus and Glowshroom provide separate grinder sources contaminated with Plant Fibre, making HPLC cleanup the mastery step. |
| Pyroxadone | `Adapted` | Expert all-four-damage medicine that functions only while Burning is active. Cryoxadone, plasma, and Phlogiston replace the unsupported slime branch and require a narrow hot synthesis window. |

### Unique healing medicines — 17/30 adapted

| Guide entry | State | Current use or missing dependency |
|---|---|---|
| Mannitol | `Adapted` | Concussion/neural recovery with an early medical order. |
| Neurine | `Adapted` | Advanced neural/sensory recovery and a 90% exact order. |
| Potassium Iodide | `Adapted` | Radiation shielding/treatment and preventative request. |
| Saline-Glucose Solution | `Adapted` | Bulk mixed-injury support and high-volume request. |
| Ephedrine | `Adapted` | Controlled stimulant and Methamphetamine precursor. |
| Diphenhydramine | `Adapted` | Stimulant/hallucination counteragent with a specific request. |
| Oculine | `Adapted` | Specialist sensory/concussion treatment with a specific request. |
| Epinephrine | `Adapted` | Staged emergency stabilization across several damage types. |
| Antihol | `Adapted` | Alcohol counteragent and purge with a specific request. |
| Synaptizine | `Adapted` | Fast stimulant/neural aid with tradeoffs. |
| Modafinil | `Adapted` | Advanced wakefulness/sedation counter and exact request. |
| Naloxone | `Adapted` | Opioid overdose counter, harmful-reagent purge, exact request, and persistent Morphine/Krokodil habit recovery. |
| Morphine | `Adapted` | Controlled analgesic/sedative with addiction and overdose consequences. |
| Haloperidol | `Adapted` | Hallucination, paranoia, and stimulant counteragent. |
| Miner's Salve | `Adapted` | Low-complexity Oil/Iron/Water medicine with analgesia and a one-off topical repair bonus. Patches now preserve the topical route while delivering their full sealed dose. |
| Psicodine | `Adapted` | Counters Hallucinating, Paranoid, and Unsteady through a purity-controlled Mannitol/Impedrezene chain. Impedrezene is a controlled addictive narcotic whose cognitive harm remains meaningful on its own. |
| Penthrite | `Adapted` | Critical-only mixed healing and stabilization sit behind Acetaldehyde and Pentaerythritol. Unsupported Wittel is adapted to a surviving Stabilizing Agent catalyst; Epinephrine, Atropine, and the heated Phenol/Acetone Oxide pairing are explicit explosive incompatibilities. |
| Inacusiate | `Blocked by mechanic` | Its defining use is hearing/deafness treatment; ChemGame has no hearing impairment status. |
| Strange Reagent | `Blocked by mechanic` | Its defining use is corpse revival; ChemGame has incapacitation/apparent death but no supported death-and-revival chemistry loop. |
| Leporazine | `Blocked by mechanic` | Requires persistent patient body-temperature pathology, not merely solution temperature or Chilled status. |
| Higadrite | `Blocked by mechanic` | Liver failure and organ-specific toxin production are not represented. |
| Energized Jelly | `Blocked by mechanic` | Depends on jelly-species behavior, stun reduction, Teslium, and Slime Jelly. |
| Sanguirite | `Blocked by mechanic` | Requires bleeding/coagulation and wound severity. |
| Seraka Extract | `Blocked by mechanic` | Requires bleeding/coagulation plus a new mushroom source. |
| Pulped Banana Peel | `Blocked by mechanic` | Grinder acquisition is straightforward, but its only guide purpose is coagulation. |
| Ondansetron | `Blocked by mechanic` | Its defining anti-nausea/disgust use has no current patient status. |
| Spaceacillin | `Excluded` | Virology and disease progression are explicitly outside this roadmap. |
| Fishy Reagent | `Excluded` | Fish/aquatic revival depends on unsupported species and revival systems. |
| Insulin | `Excluded` | The snapshot lists a vendor-sourced, noncraftable medicine whose only role is an unsupported blood-sugar system. |
| Determination | `Excluded` | Endogenous wound response rather than an obtainable chemistry process, and wounds/bleeding are outside the present body model. |

## Next chemistry targets

The implementable medicine ladder is complete under the current body model.
Changeling Adrenaline and Changeling Haste are species-ability secretions
rather than recipes and remain excluded with species transformations. The
narcotics half of Wave 5 is complete. The toxin table now includes its first
dedicated expansion: communication suppression, injury-conditional harm,
exposure-scaled terminal damage, delayed paralysis, a specific respiratory
counteragent, nonlethal fatigue, multi-threshold damage, botanical irritants,
slow radiological poisoning, a delayed-collapse expert opiate, adapted organ
damage, concentration-scaled venom, touch-active irritants, and delayed
perception distortion. The implementable toxin roster is complete. Herbicides
remain mechanic-blocked until there is live hydroponics.

## Other guide gaps by wave

| Guide area | Present/adapted examples | Highest-value absent entries | State |
|---|---|---|---|
| Narcotics | Krokodil, Methamphetamine, Bath Salts, Space Drugs, botanical Nicotine, purity-sensitive Aranesp/Epoetin Alfa, Happiness/Sadness, Coffee-sourced Pump-Up, botanical Mushroom Hallucinogen, the complete Maintenance Tar/Sludge/Powder ladder, Kronkus-sourced Kronkaine, motor-disrupting bLaSToFF, and concealment-producing SaturnX; Morphine, Ephedrine, and Modafinil are covered in the medicine table | None in the scoped guide narcotics table | `Implemented` |
| Toxins | Chloral Hydrate, Sulfonal, Anacea, Mindbreaker Toxin, Unstable Mutagen, Cyanide, Formaldehyde, Carpotoxin, Zombie Powder, Fluorosulfuric Acid, Nitric Acid, Mute Toxin, Heparin, Amanitin, Curare, Lexorin, Tirizene, Tiring Solution, Tetrodotoxin, Pancuronium, Sodium Thiopental, Initropidril, Amatoxin, Coniine, Histamine, Polonium, Fentanyl, Bungotoxin, Lead Acetate, Venom, Itching Powder, and Rotatium | Herbicides remain mechanic-blocked; species/organ-only non-recipes remain excluded | `Implemented` for the scoped craftable/source roster |
| Pyrotechnics | Stabilizer, Smoke, Smoke Powder, Flash Powder, Sonic Powder, Phlogiston, Napalm, Cryostylane, Pyrosium, Sorium, Liquid Dark Matter, Chlorine Trifluoride, Meth overheat, Gunpowder/Black Powder adaptation, Nitroglycerin, RDX, Thermite, TaTP, immediate EMP, and Teslium/Tesla Shock world arcing | Advanced foam and remaining expert reaction agents | `Adapted` |
| Utility | Ice, Saltwater, Cryptobiolin, Drying Agent, Foaming Agent, Fluorosurfactant, Chemical Foam, Firefighting Foam, temporary Metal Foam, Glycerol, Space Cleaner, Space Lube, Sterilizine, Carbon Dioxide area extinguishing, Pax pacification, Pyrosium heating, Sorium repulsion, and Liquid Dark Matter attraction | Gravitum remains mechanic-blocked; Smart Metal Foam is explicitly excluded | `Implemented` |
| Delivery/tools | Four-slot hotbar inventory, core containers, patches, aimed sprays, refillable smoke projector, reaction smoke, mixture-carrying foam, splash/puddles, charges, grinder, chamber, analyzer/HPLC, and pH paper | None in the approved Wave 7 scope; portable mixing was deliberately declined | `Implemented` |

## `Chemical recipes` snapshot reconciliation

The second snapshot is chiefly a manual batch/macro guide. Its entries do not
create a second completion requirement when the underlying chemical is already
covered. It contributes several useful gap checks:

- Its medicine recipes confirm the implemented Anacea, Sulfonal, Antihol,
  Oculine, Haloperidol, Diphenhydramine, Modafinil, and Naloxone methods.
- Mutadone and Clonexadone appear in the manual page but not in the current
  guide's craftable medicine tables. Keep them `Unreviewed` legacy candidates;
  do not count them toward the 34/54 guide total.
- Aranesp and Fentanyl are absent from the manual drug set. Both are now
  implemented from the primary guide rather than counted toward medicine-ladder
  completion.
- The manual pyrotechnic entries are chemically covered, including Teslium as
  both a controlled precursor and a water/474K-triggered electrical arc.
  Gunpowder is the approved adaptation of the guide's Black Powder family.
- Manual drink recipes, virology recipes, carpet/fun macros, and food/drink
  mixtures are `Excluded` by the roadmap. Plastic sheets remain a utility-wave
  candidate; mixture-carrying Chemical Foam, Firefighting Foam, and temporary
  Metal Foam barriers are implemented.

## Final wave: complete beginner-to-advanced chemistry guide

The standalone Markdown handbook is the complete written player curriculum for
the chemistry loop. A proper interactive tutorial can be built later. The
written guide must remain text-only and:

1. Teach direct dispensing, measuring, containers, transfer, packaging, and
   the first safe medicines without assuming SS13 knowledge.
2. Introduce dependency trees, sourced ingredients, catalysts, agitation,
   heating/cooling, pH, buffers, purity, rates, and recoverable failures in the
   same order the progression curve demands them.
3. Explain overdoses, incompatibilities, contraband, delayed effects,
   stabilization, world hazards, safe handling, and every delivery form.
4. Provide instrument-specific walkthroughs for the Reaction Chamber,
   grinder, Mixing Chamber, pH paper, Sample Analyzer/HPLC, sprays, patches,
   smoke/foam payloads, and chemical charges.
5. Include searchable synthesis trees and beginner/intermediate/advanced/
   expert paths, with live recipe data supplying quantities, temperature, pH,
   purity, catalysts, timing, hazards, and prerequisites wherever possible.
6. End with optimization and troubleshooting material: scaling batches,
   preserving purity, recovering inverses, handling contamination, meeting
   exact orders, diagnosing stalled reactions, and preparing safe payloads.
7. Keep every implemented chemical discoverable from the handbook or its
   grouped references, and editorially cross-check recipes against the
   authoritative data whenever chemistry content changes.

## Completion cautions

- The 41/54 medicine figure is an entry count. Wave 4 is complete within the
  current body model; nine remaining entries are mechanic-blocked and four are
  explicitly excluded by scope.
- Adding an absent reagent ID and recipe is insufficient. A blocked medicine
  stays blocked until its defining patient condition, delivery behavior, or
  world effect exists or an explicit adaptation gives it a different meaningful
  role.
- Exact requests currently provide authored demand for a useful subset of the
  medicine ladder; category-based orders cover additional medicines. New
  treatment families such as opioid reversal, coagulation, or temperature
  pathology need incidents/orders at the same time as their chemistry.
