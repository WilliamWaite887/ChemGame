# Chemistry Guide Coverage

## Sandbox material layer

Water reactivity, acid/base neutralization, and activated fuel/oxidizer reactions
share the resolver used by containers, body compartments, and touching floor
pools. Ash is a reusable byproduct; 17 residue-producing internal reactions no
longer consume recipe-book cards or research progression. Stable reaction
identities remain for old saves.

See the [simulation reference](chemistry-simulation.md) and the
[tutorial scenarios](chemistry-tutorial-scenarios.md). Automated verification and
rendered/manual acceptance are recorded separately.

This ledger maps ChemGame's supported scope to the checked-in TG Station
`Guide to chemistry` and `Chemical recipes` snapshots. `Implemented` means the
chemical has an obtainable recipe/source, a player-facing identity, a useful
effect or documented inert role, recipe-book hints, and validation coverage.

The executable data files remain authoritative. Reachability, identity,
category, effect, hint, and request checks run in the Rust test suite so this
document cannot substitute for working content.

The player-facing written handbook is
[`chemistry-player-guide.md`](chemistry-player-guide.md). It covers the current
loop from a first shift through expert synthesis. A future interactive tutorial
is a separate project.

| Guide area | Status | ChemGame coverage |
|---|---|---|
| Reaction temperature and rates | Implemented | Temperature gates, thermal drift, exothermic/endothermic output, overheat yield/failure, and temperature-scaled timed reactions |
| Reaction pH and buffers | Implemented | Authored reagent pH, reaction operating windows/optima, live precise chamber/analyzer readout, consumable acidic/basic ChemMaster 5000 buffers, and portable approximate pH paper with five readable bands |
| Purity | Implemented | Per-reagent purity, volume-weighted mixing, transfer/package preservation, consumed-reactant-only reaction gates and output quality, pH-dependent synthesis, body and world-effect scaling, purity-scaled research, and exact minimum-purity orders |
| Catalysts and staged mixing | Implemented | Non-consumed catalysts plus provenance-enforced two-side agitation |
| Core components | Adapted | Ammonia, salt, saltwater, ice, stabilizer, oil, peroxide, ash, saltpetre, acetone, phenol, superacids, glycerol, acetone oxide, lye, formaldehyde, diethylamine, Lead, Carpotoxin, Cryptobiolin, botanical Omnizine, Glowshroom-sourced Slime Jelly, Kronkus Extract, Destroying Angel Amanitin, Curare Vine extract, Death Berry Tirizene/Coniine, Fly Amanita Amatoxin, Omega Weed Histamine, Bungo Fruit toxin, Giant Spider Venom, Toxic Pufferfish Tetrodotoxin, controlled Teslium, salvage-sourced Hyper-Plasmium Oxide, and Exotic Stabilizer |
| Core medicines | Adapted | Existing service ladder plus Multiver, Salicylic Acid, Oxandrolone, Salbutamol, Epinephrine, Atropine, Pentetic Acid, Cryoxadone, Synthflesh, Haloperidol, Libital/Libitoil, Helbital, Probital, Aiuri, Lenturi, Tirimol, Convermol, Calomel, Ammoniated Mercury, Granibitaluri, Seiver, Neurine, Diphenhydramine, Oculine, Rezadone, Antihol, Modafinil, Naloxone, cold-controlled Hercuri/Herignis, volatile-intermediate Syriniver, patch-focused Miner's Salve, Burning-dependent Pyroxadone, purified Regenerative Jelly, critical-only Penthrite behind Acetaldehyde/Pentaerythritol, and psychiatric Psicodine behind controlled Impedrezene |
| Toxins | Implemented | Acids, Chloral Hydrate, delayed-onset Sulfonal, medicine-purging Anacea, Cyanide, Unstable Mutagen, Mindbreaker Toxin, report-suppressing Mute Toxin, injury-conditional Heparin, exposure-scaled Amanitin, delayed Curare, Epinephrine-countered Lexorin, nonlethal Tirizene/Tiring Solution, multi-threshold Tetrodotoxin, ten-cycle Pancuronium and Sodium Thiopental, escalating Initropidril, direct Amatoxin, persistent respiratory Coniine, overdose-sensitive Histamine, radiological Polonium, delayed-collapse addictive Fentanyl, twelve-cycle Bungotoxin, neurological Lead Acetate, concentration-scaled Venom, touch-active Itching Powder, and twenty-cycle Rotatium. Unsupported organ-only details are adapted to readable damage/status channels; herbicides remain explicitly mechanic-blocked. |
| Narcotics | Implemented | Space Drugs, Bath Salts, Krokodil, Methamphetamine, Zombie Powder, Morphine, Ephedrine, Tobacco-sourced Nicotine, five-branch Aranesp/Epoetin Alfa, plasma-catalyzed Happiness/Sadness, Coffee-sourced Pump-Up, slow botanical Mushroom Hallucinogen, low-yield Maintenance Tar/Sludge/Powder, Kronkus-sourced Kronkaine, motor-disrupting bLaSToFF, and concealment-producing SaturnX. Coverage includes overdose, addiction, targeted purge, collapse resistance, NPC mood behavior, hostile detection, and stock-aware illicit demand. |
| Pyrotechnics | Adapted | Thermite, flash/smoke powders, Sonic Powder, Phlogiston, Chlorine Trifluoride, Cryostylane, Pyrosium, Sorium, Liquid Dark Matter, Napalm, Gunpowder, Nitroglycerin, RDX, catalyst-stabilized TaTP, immediate EMP chemistry, and water/heat-triggered Tesla arcs. Smart Metal Foam is deliberately excluded and Gravitum remains mechanic-blocked. |
| Energetic delivery | Implemented | Batch-scaled initiation, stabilizer failure, controlled-material scanning, breach effects, and sealed 5/10/20-second charges |
| Utility chemistry | Adapted | Cleaner, lubricant, sterilizer, drying agent, Fluorosurfactant/Chemical Foam mixture carriage, temporary Metal Foam barriers, expanding non-slip Firefighting Foam, high-temperature Carbon Dioxide area extinguishing, non-sedating Pax pacification, smoke transport, corrosion, fire, cooling, catalytic Oxygen heating, radial push/pull force, concussion, and slippery surfaces |
| Delivery forms and tools | Implemented | Four-slot replicated hotbar inventory; beakers, bottles, pills, syringes, full-dose topical patches, 3u aimed sprays, refillable 30u smoke projectors, reaction smoke, purity/ownership-preserving chemical foam, puddles, chemical charges, approximate pH paper, and HPLC purification. Injection, ingestion, patch, spray, splash/puddle, and inhalation are distinct routes. Staged agitation remains fixed laboratory equipment by design. |
| HPLC purification | Implemented | After 24 recorded methods, the Sample Analyzer separates a selected reagent, collects contaminants and yield loss in a reject beaker, raises retained purity deterministically, performs only explicitly authored one-way failure recovery, and retains a replicated before/after yield report |
| Process feedback | Implemented | The Reaction Chamber forecasts only recorded ambient methods and reports temperature, pH, purity, rate, missing stabilizers, predicted product quality, and explicit energetic danger |
| Recipe knowledge/progression | Implemented | All base ChemMaster 5000 stock and Sample Analyzer functions start available; hints, discovery, an actionable next-method recommendation, paginated synthesis trees, career-stage guidance, research, external produce, exact emergencies, and antagonist demand carry progression. Timed orders only count external branches when the required stock physically exists, and courier hauls refill least-stocked sources to prevent random starvation. Exact quality demand rises from 75% at 15 successes through 85%/90% to 95% mastery requests. Milestone-save simulations traverse the complete dependency frontier. |
| Noncraftable medicines | Excluded | Catalog/reference material rather than chemistry recipes |
| Virology and mutation toxins | Excluded | Requires a dedicated disease/genetics simulation |
| Species transformations | Excluded | Requires species anatomy and transformation systems |
| Plumbing | Excluded | Outside the hands-on laboratory loop |
| Food and drink chemistry | Excluded | Belongs to a future service/kitchen expansion |
| Lavaland/geyser simulation | Excluded | Essential inputs are adapted to ChemMaster 5000, botany, cargo, mineral, or salvage sources |
| Smart Metal Foam | Excluded | Deliberately omitted: automated space-tile topology and safe-path construction are unsupported, and ordinary temporary Metal Foam supplies the useful containment gameplay without duplicating it |
| Gravitum | Blocked by mechanic | Its defining weightless-body/object behavior requires a persistent physics and object-propulsion system rather than an inert reagent substitute |

## Completion rule

A newly imported guide entry must include all of the following before changing
to `Implemented`: stable ID appended to existing data, reachable inputs, recipe
or source, gameplay effect/inert rationale, book description and hints,
progression placement, and tests. Unsupported specialist entries remain
explicitly excluded instead of becoming inert filler.
