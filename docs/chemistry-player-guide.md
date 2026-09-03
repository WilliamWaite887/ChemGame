# ChemGame Chemistry Handbook

This is the complete written guide to ChemGame's current chemistry loop. It
starts with the first safe mixtures and ends with high-purity medicine,
multi-stage synthesis, HPLC purification, controlled substances, and energetic
payloads. It is a reference document, not the future interactive tutorial.

The shipped chemistry data remains authoritative. If a number here and the
in-game research book ever disagree, trust the book and report the stale line
in this file.

## The short version

1. All standard ChemMaster 5000 reagents are available from the start.
2. Progression comes from learning methods, making intermediates, controlling
   temperature and pH, preserving purity, and finding external ingredients.
3. A recipe ratio scales. `1:1 -> 2` means 5u + 5u makes 10u just as readily as
   10u + 10u makes 20u.
4. Catalysts must be present but are not consumed.
5. Agitated recipes require their authored sides to be prepared separately and
   combined through the Mixing Chamber. Pouring everything into one beaker is
   not equivalent.
6. Clean work matters. Extra reagents contaminate orders, poor pH lowers
   product purity, and dangerous recipes may burn, ruin themselves, or explode.
7. Read the patient and the package. The correct chemical in an unsafe dose or
   delivery form is still incorrect.
8. External-source orders are stock-aware. A botanical recipe may be recorded
   in the book, but a timed request will not rely on it until the relevant
   produce or extracted reagent is physically in the lab.

## Chemistry beyond the beaker

Compatible chemicals can react in glassware, inside a body, or when floor pools
touch. The same material rules apply, but swallowed chemistry waits for the next
digestion beat. Ordinary supplies remain stable; room air supports an ignited
fuel rather than quietly spoiling it.

- **Water reactivity:** Raw Potassium reacts with water and leaves Ash. Small
  amounts have smaller effects. A body supplies limited water, so swallowing
  potassium alone can hurt; adding water can accelerate it. Finished Dylovene
  does not inherit its potassium ingredient's reactivity.
- **Neutralization:** Sulphuric Acid or Nitric Acid consumes Lye and leaves
  Neutralized Salts. Extra acid or base remains. Inspect composition, not only pH.
- **Ignition:** Heated fuel burns; explicit Oxygen or Hydrogen Peroxide speeds
  an activated fire. Oil begins this behavior at 480K. Existing extinguisher
  chemistry cools affected floor mixtures; no atmospheric controls are needed.
- **Spills:** Touching pools mix as a whole. Walls and different floor levels
  keep them apart. New products can change a pool's color and effects.
- **Ash is residue:** Burning and destructive reactions can leave the existing,
  reusable Ash reagent. Its many production routes no longer occupy separate
  recipe cards or earn research. Relevant hazards appear with the source chemical.

The analyzer describes supported properties after identifying a sample. A label
is not a measurement. No extra pressure, sealing, humidity, or organ controls are
required. Dilution does not erase a dangerous dose, and hazardous mixtures do
not promise time to escape. Stop exposure and use established trauma/burn care
and Medical recovery after injuries; there is no new stomach-purge action.

Authors can use the [tutorial scenarios](chemistry-tutorial-scenarios.md) and
[simulation reference](chemistry-simulation.md) for exact setups and checks.

## Your first shift

### Use the four-slot hotbar

Picked-up items go into the four cells at the bottom center of the screen.
Press `1`–`4` to select a cell. Only the selected item is physically in your
hand, so `E`, `F`, `R`, machine loading, and dropping always act on that one
item. Picking up with the selected cell occupied fills the next free cell.
Dropping or loading an item frees its cell; a full hotbar refuses another
pickup until a cell is cleared.

### Make the three foundation medicines

These are deliberately simple and instant. Use equal amounts.

| Product | Ingredients | Primary use |
|---|---|---|
| Inaprovaline | Oxygen + Carbon + Sugar | Stabilizes injured/asphyxiating patients and softens new oxygen harm |
| Dylovene | Silicon + Nitrogen + Potassium | Heals toxin damage and slowly purges harmful chemicals |
| Kelotane | Silicon + Carbon | Heals burns |

Start with 5u of each ingredient. This is large enough to understand the
reaction and small enough that a mistake does not waste a whole beaker.

### Package an order

1. Check the requested chemical, quantity, purity, and delivery form.
2. Make only the amount you need, plus a small margin if the request is not
   exact.
3. Inspect the product before packaging. A correct ingredient mixed with
   leftovers is contamination.
4. Use the Mixing Chamber to package the requested bottle, pill, syringe,
   patch, spray, or charge.
5. Keep doses below the reagent's overdose threshold. Orders are clamped to
   safe deliverable amounts, but manually treating someone is your decision.

### Learn the first treatment choices

| Problem | Early answer | Later answer |
|---|---|---|
| Brute trauma | Bicaridine | Libital, Helbital, Probital, Tirimol |
| Burns | Kelotane | Dermaline, Aiuri, Lenturi, Oxandrolone, Hercuri |
| Toxin damage | Dylovene | Multiver, Pentetic Acid, Calomel, Syriniver |
| Oxygen deprivation | Inaprovaline or Dexalin | Salbutamol, Atropine, Convermol |
| Radiation | Potassium Iodide before exposure; Hyronalin after | Arithrazine, Pentetic Acid, Seiver |
| Mixed injury | Tricordrazine or Saline-Glucose | Cryoxadone, Regenerative Jelly, Pyroxadone, Penthrite |
| Hallucination/paranoia | Synaptizine or Haloperidol | Psicodine |
| Opioid overdose | Support breathing | Naloxone |

## Equipment

### ChemMaster 5000

The ChemMaster 5000 supplies every standard base reagent from a fresh save. It
does not supply completed medicines, most intermediates, or physical
ingredients that belong to Botany, Cargo, minerals, or salvage.

Use small dispense steps for final corrections. Large buttons are for measured
bulk stock, not pH tuning.

### Reaction Chamber

The chamber heats or cools a loaded container toward a target. Its forecast
shows known reactions, current temperature and pH, the legal operating range,
minimum purity, missing catalysts, estimated product quality, reaction rate,
and energetic danger.

A temperature threshold is not always a safe target. Exothermic reactions heat
themselves after they begin. Leave room below an overheat temperature.

### Mixing Chamber

The Mixing Chamber has two jobs:

- combine separately prepared sides for an agitated reaction;
- package finished liquid as bottles, pills, syringes, patches, sprays, or
  sealed chemical charges;
- package a refillable Smoke Projector carrying up to 30u.

For agitation, divide the ingredients exactly as the method describes. The
machine records that they were separate before combining them.

### Grinder and external ingredients

Produce and sourced materials arrive as mixtures. Grinding them gives the
desired extract plus Plant Fibre or another contaminant. This is intentional:
the extract is obtainable immediately, but exact orders may require HPLC
cleanup.

Courier hauls favor the least-stocked physical specimens currently left in the
lab. Consuming a rare source therefore moves it toward the front of future
deliveries instead of leaving the career at the mercy of an unbounded random
roll. Exact timed requests still wait until their required source or extract is
physically present.

Important sources include:

- Corn for Corn Oil and Plant Fibre;
- Koibeans for Carpotoxin and Plant Fibre;
- Ambrosia Deus for Omnizine and Plant Fibre;
- Glowshrooms for Slime Jelly and Plant Fibre;
- Tobacco Leaves for Nicotine and Plant Fibre;
- Coffee Cherries for Coffee and Plant Fibre. Purified Coffee feeds the
  Pump-Up synthesis;
- Psychedelic Mushrooms for Mushroom Hallucinogen and Plant Fibre.
- Tea Fermentation Cultures for Tea, Universal Enzyme, and Plant Fibre. These
  feed the expert maintenance-drug ladder;
- Kronkus Fruit for Kronkus Extract and Plant Fibre. Purified extract feeds
  the Kronkaine synthesis.

### pH paper

pH paper gives an approximate band and consumes one strip. It is enough to
answer "acidic, near neutral, or alkaline?" The chamber and analyzer provide
precise readings.

### Sample Analyzer / HPLC

The analyzer can separate a selected reagent from contaminants and improve its
retained purity from the start of a career. Yield is lost during purification;
the rejected fraction goes to a reject beaker.

Use HPLC when:

- a ground botanical extract contains Plant Fibre;
- a near-boundary reaction produced usable but low-purity material;
- an exact order demands cleaner product than the synthesis supplied;
- a recipe produced an authored inverse that can be recovered and separated.

Purification is not duplication. Always expect less retained volume than the
starting sample.

## Temperature, pH, purity, and reaction speed

### Temperature

- `minimum` means the reaction cannot begin colder than that value.
- `maximum` means the reaction cannot begin hotter than that value.
- `overheat` is a failure threshold. Depending on the recipe, the batch loses
  yield, ruins itself, or detonates.
- Heating and cooling continue to affect a beaker outside the instant a
  reaction fires.
- Reaction speed improves with suitable temperature, but a fast reaction is
  not worth an explosion.

### pH

Every reagent contributes an authored pH to the mixture. Acidic and Basic
Buffer are consumed when used and shift the whole solution.

- Work near the optimum for the best product purity.
- Working near a legal boundary usually produces recoverable lower-purity
  output.
- Crossing the boundary stalls the intended reaction and may allow an inverse
  or failure reaction to win.
- Add buffer in small increments and re-read after each addition.

### Purity

Purity belongs to each reagent portion, not just the beaker. Mixing two
portions of the same reagent uses volume-weighted purity. Transfers, packaging,
smoke, foam, and other delivery forms preserve the carried quality.

Purity scales body effects, world effects, research value, and exact-order
acceptance. Impure ordinary medicine remains useful at reduced potency. Only
explicitly dangerous chemistry has catastrophic low-quality outcomes.

### Timed reactions

Some reactions finish immediately; others consume their ratio over time.
Removing a beaker early leaves a partial batch and remaining inputs. The
chamber's rate display tells you whether a batch is running, blocked, or done.

## Catalysts and agitation

### Catalysts

A catalyst is required but survives the reaction. One catalyst unit can often
support a much larger batch. Do not count it as product volume.

Common examples:

| Product | Catalyst |
|---|---|
| Dexalin | Plasma |
| Formaldehyde | Silver |
| Libital | Sodium |
| Helbital | Copper |
| Probital | Sodium |
| Aiuri | Sodium |
| Lenturi | Lithium |
| Granibitaluri | Iron |
| Modafinil | Bromine |
| Nitroglycerin, RDX, Penthrite | Stabilizing Agent |

### Agitated reactions

Prepare each side in its own beaker, then combine through the Mixing Chamber.

| Product | Side A | Side B |
|---|---|---|
| Bicaridine | Inaprovaline | Carbon |
| Hyronalin | Dylovene | Radium |
| Tricordrazine | Inaprovaline | Dylovene |
| Dermaline | Kelotane | Oxygen + Phosphorus |
| Arithrazine | Hyronalin | Hydrogen |
| Dexalin | Oxygen | Plasma catalyst |
| Epinephrine | Phenol + Acetone | Diethylamine + Oxygen |
| Pentetic Acid | Formaldehyde + Sodium | Cyanide + Diethylamine |
| Glycerol | Corn Oil | Sulphuric Acid |
| Acetone Oxide | Acetone | Hydrogen Peroxide + Oxygen |

Extra material on either side can prevent activation. Prepare the exact
partition first; add buffers only when you understand where they will land.

## Progression path

The research book names your current stage from successful orders and shows a
recommended next method. It prefers a recipe whose ingredients are obtainable
now; when none exists, it points one precursor beyond the current frontier.
This is guidance rather than a lock—you may inspect any card and pursue another
branch.

### Fundamentals: roughly 0–10 successful orders

Learn measurement, clean containers, simple packaging, and direct reactions.

Recommended methods:

- Inaprovaline, Dylovene, Kelotane;
- Bicaridine, Hyronalin, Tricordrazine, Dermaline;
- Sulphuric Acid, Space Cleaner, Saltwater, Ice;
- basic overdose awareness and syringe versus ingested dosing.

The new idea in this stage is agitation. Everything else should remain easy to
read and recover from.

### Intermediate: roughly 10–30 successful orders

Add catalysts, sourced ingredients, timed reactions, and controlled
temperature.

Recommended branches:

- Hyronalin -> Arithrazine;
- Oxygen over Plasma -> Dexalin;
- Sodium + Chlorine -> Sodium Chloride;
- Oil -> Acetone and Phenol;
- Mannitol, Saline-Glucose, Potassium Iodide;
- Corn -> Corn Oil -> Glycerol;
- Koibeans -> Carpotoxin -> Rezadone;
- Morphine -> Naloxone;
- cleaner, lubricant, sterilizer, drying and firefighting chemistry.

### Advanced: roughly 30–60 successful orders

Purity and pH become order requirements rather than optional efficiency.

Practice:

- Libital and acidic Libitoil inverse recovery;
- Hercuri and acidic Herignis inverse recovery below 250K;
- narrow-pH superior medicines such as Tirimol and Convermol;
- HPLC cleanup of sourced extracts;
- exact 80–90% purity orders;
- volatile Nitrous Oxide and dilution-sensitive Syriniver;
- Phlogiston batch-size control and safe fire handling.

### Expert: 60+ successful orders

Expert work joins multiple branches and punishes careless setup.

Practice:

- purified Omnizine + Slime Jelly -> Regenerative Jelly;
- Cryoxadone + Plasma + Phlogiston -> Pyroxadone in the 374–420K window;
- Acetaldehyde -> Pentaerythritol -> Penthrite;
- Fluorosulfuric Acid -> Nitric Acid -> stabilized energetic compounds;
- exact 95% purity requests;
- separating rare extracts without wasting the requested volume;
- packaging controlled charges only after the mixture is safe and complete.

## Medicine reference

The table emphasizes why each medicine exists. Consult the research book for
the live operating range and exact batch rate.

### Foundation and conventional medicine

| Medicine | Core recipe or source | Role and caution |
|---|---|---|
| Inaprovaline | Oxygen + Carbon + Sugar | Stabilization; does not erase existing oxygen debt |
| Dylovene | Silicon + Nitrogen + Potassium | Toxin healing and harmful-reagent purge; OD 20u |
| Kelotane | Silicon + Carbon | Basic burn healing; OD 15u |
| Bicaridine | Inaprovaline + Carbon, agitated | Brute healing; OD 15u |
| Hyronalin | Dylovene + Radium, agitated | Mild radiation cleanup; OD 30u |
| Tricordrazine | Inaprovaline + Dylovene, agitated | Slow all-damage treatment |
| Dermaline | Kelotane + Oxygen + Phosphorus, agitated | Strong burn treatment; OD 10u |
| Arithrazine | Hyronalin + Hydrogen, agitated | Strong radiation cleanup with a brute cost |
| Dexalin | Oxygen over Plasma, agitated | Airloss healing and stabilization; OD 20u |
| Potassium Iodide | Potassium + Iodine over Copper | Radiation prevention more than cure; OD 30u |
| Mannitol | Hydrogen + Water + Sugar over Copper | Clears blurred/unsteady concussion effects; OD 15u |
| Saline-Glucose | Sodium Chloride + Water + Sugar | Safe bulk trauma/burn support; OD 60u |

### Superior and specialist medicine

| Medicine | Recipe | Role and caution |
|---|---|---|
| Salicylic Acid | Phenol + Sodium + Carbon, above 350K | Moderate trauma treatment |
| Oxandrolone | Carbon + Phenol + Hydrogen, above 390K | Strong burn treatment |
| Salbutamol | Salicylic Acid + Lithium + Bromine | Airloss and Choking treatment |
| Epinephrine | Phenol/Acetone agitated with Diethylamine/Oxygen | Emergency stabilization; incompatible with Penthrite |
| Atropine | Ethanol + Acetone + Diethylamine | Critical toxin/airloss care; incompatible with Penthrite |
| Pentetic Acid | Formaldehyde/Sodium agitated with Cyanide/Diethylamine | Toxin and radiation chelation |
| Cryoxadone | Dexalin + Water + Ice, below 285K | Broad cold-associated recovery |
| Synthflesh | Bicaridine + Kelotane + Carbon | Trauma/burn repair |
| Libital | Phenol + Nitrogen + Oxygen over Sodium | Advanced trauma; acidic setup forms Libitoil |
| Helbital | Carbon + Fluorine + Sugar over Copper | Strong trauma treatment |
| Probital | Acetone + Copper + Phosphorus over Sodium | Trauma treatment from organic/mineral inputs |
| Aiuri | Ammonia + Hydrogen + Sulphuric Acid over Sodium | Burn treatment; keep below 315K |
| Lenturi | Ammonia + Silver + Sulfur + Chlorine over Lithium | High-purity mastery burn treatment |
| Tirimol | Acetone + Nitrogen over Sulphuric Acid/Oxygen | Narrow-pH emergency medicine |
| Convermol | Oil + Fluorine + Hydrogen, above 370K | Temperature-sensitive oxygen medicine |
| Calomel | Mercury + Chlorine, above 374K | Aggressive purge with a narrow safe dose |
| Ammoniated Mercury | Calomel + Ammonia | Advanced heavy-metal purge |
| Granibitaluri | Sodium Chloride + Carbon + Sulphuric Acid over Iron | Mixed trauma/burn treatment |
| Seiver | Aluminium + Nitrogen + Potassium | Combined radiation/toxin treatment |
| Neurine | Acetone + Mannitol + Oxygen | Advanced sensory/motor recovery |
| Diphenhydramine | Diethylamine + Oil + Bromine + Carbon + Ethanol | Counters stimulation and hallucination |
| Oculine | Multiver + Carbon + Hydrogen | Specialist visual/concussion counteragent |
| Rezadone | Carpotoxin + Cryptobiolin + Copper | Superior botanical trauma/burn recovery |
| Antihol | Multiver + Copper + Ethanol | Alcohol counteragent and purge |
| Modafinil | Acetone + Diethylamine + Phenol + Sulphuric Acid over Bromine | Advanced wakefulness medicine |
| Naloxone | Morphine + Hydrogen Peroxide + Bromine + Ethanol | Opioid antidote and purge |
| Hercuri | 3 Cryostylane + Lye + Bromine, below 250K | Burn treatment/extinguisher; acidic batch forms Herignis |
| Syriniver | 2 Nitrous Oxide + Mindbreaker + Fluorine + Sulfur | Powerful toxin purge; OD 6u, so dilute carefully |
| Miner's Salve | Oil + Iron + Water | Patch-focused trauma/burn salve |
| Psicodine | 2 Mannitol + Impedrezene + 2 Water | Hallucination, paranoia, and motor-confusion treatment; OD 30u |
| Regenerative Jelly | Purified Omnizine + purified Slime Jelly | Broad biological restorative |
| Pyroxadone | Cryoxadone + Plasma + Phlogiston, 374–420K | Heals all damage only while the patient is Burning |
| Penthrite | Pentaerythritol + Nitric Acid + Acetone over Stabilizer | Critical-only mixed healing; controlled explosive; OD 50u |

## Toxins, stimulants, and controlled drugs

These chemicals exist for research, counteragent training, antagonist play,
and station incidents. Controlled or illicit chemistry can draw Security
attention. A legitimate matching order authorizes the delivery, not unrelated
possession or exposure.

| Chemical | Recipe | Distinguishing behavior |
|---|---|---|
| Sulphuric Acid | Sulfur + Hydrogen + 2 Oxygen | Toxin/contact burn and corrosion |
| Chloral Hydrate | 3 Chlorine + Ethanol + Water | Sedation; overdose deepens sedation and harms |
| Sulfonal | Acetone + Diethylamine + Sulfur, pH 4–9 | Slow toxin damage; incapacitates after 22 active metabolism ticks |
| Anacea | Haloperidol + Impedrezene + Radium, pH 6–9, 70% inputs | Slowly harms while purging 5u of every therapeutic reagent per tick |
| Cyanide | Oil + Ammonia + Oxygen | Toxin damage and severe choking |
| Unstable Mutagen | Chlorine + Phosphorus + Radium | Toxin, radiation, and visible mutation |
| Mindbreaker Toxin | Silicon + Hydrogen + Dylovene | Hallucination/paranoia poison |
| Zombie Powder | Carpotoxin + Morphine + Copper | Apparent death and incapacitation |
| Mute Toxin | 2 Uranium + Water + Carbon, pH 6–14, at least 40% inputs | Suppresses communication: an isolated victim cannot radio-report chemical abuse, but a nearby witness still can |
| Heparin | Formaldehyde + Sodium Chloride + Lithium, pH 5–9.5, at least 60% inputs | Does not harm an uninjured patient; worsens existing brute trauma and blood-loss symptoms very slowly |
| Amanitin | Grind Destroying Angel; purify away Plant Fibre when needed | Quiet during exposure, then deals terminal toxin damage proportional to completed metabolism ticks; early purging reduces the final harm |
| Curare | Grind Curare Vine; purify away Plant Fibre when needed | Slow oxygen/toxin damage followed by deep incapacitating sedation after 11 active ticks |
| Lexorin | Salbutamol + Plasma + Hydrogen, pH 1.8–7, at least 40% inputs | Rapid oxygen damage and Choking; Epinephrine specifically purges an extra 2u per tick |
| Tirizene | Grind Death Berries; purify away Plant Fibre when needed | Nonlethal fatigue toxin that slows movement; Synaptizine clears both the active chemical and Sluggish effect |
| Tiring Solution | 2 Tirizene + Saline-Glucose, pH 5–9, at least 30% inputs | Stronger but capped nonlethal slowdown; repeated ticks refresh rather than stack its intensity |
| Sodium Thiopental | Sulfonal + Sodium + Ethanol, 350–600K, pH 6–10, at least 50% inputs | Slows immediately and causes a non-damaging knockout after 10 active ticks; Modafinil counters the sedation |
| Pancuronium | Curare + Salbutamol + Sodium Chloride, 300–500K, pH 5–8.5, at least 60% inputs | Silent for nine ticks, then causes Choking, oxygen damage, and deep paralysis on tick 10 |
| Tetrodotoxin | Grind a Toxic Pufferfish Specimen; purify away Saltwater when needed | Warning begins at tick 7, paralysis and damage at tick 13, then additional damage tiers at ticks 21 and 29; late Choking begins at tick 38 |
| Initropidril | Cyanide + Nitric Acid + Plasma, 420–650K, pH 3–8, at least 70% inputs | Expert poison: immediate toxin damage, respiratory collapse from tick 4, and deep incapacitation from tick 8 |
| Amatoxin | Grind Fly Amanita; purify away Plant Fibre when needed | Straightforward steady toxin damage with fast 0.4u-per-tick clearance; useful as an early botanical poison and purge exercise |
| Coniine | Grind Death Berries; separate Tirizene and Plant Fibre | Persistent toxin and Choking poison with exceptionally slow 0.024u-per-tick clearance; early treatment matters |
| Histamine | Grind Omega Weed; purify away Plant Fibre when needed | Mild trauma and Blurred vision normally; above 30u adds simultaneous brute, toxin, and oxygen damage |
| Polonium | Uranium + Radium + Chlorine, 500–700K, pH 5–9, at least 70% inputs | Expert isotope poison that continually refreshes intense Irradiated status; purge the source, then use Pentetic Acid, Arithrazine, Hyronalin, Seiver, or Potassium Iodide as appropriate |
| Fentanyl | Heat purified Space Drugs at 674–874K, pH 7–11, at least 50% purity | Addictive expert opiate with toxin/motor impairment and an exact tick-18 knockout; Naloxone purges the dose and treats persistent dependence |
| Bungotoxin | Grind Bungo Fruit; purify away Plant Fibre when needed | Mixed toxin/oxygen damage followed by Choking and reversible fainting exactly on tick 12 |
| Lead Acetate | Lead + Acetone + Oxygen, 350–600K, pH 4–9, at least 50% inputs | Heavy-metal poison adapting brain/ear injury into simultaneous brute/toxin harm and subtle Blurred vision |
| Venom | Grind a Giant Spider Venom Sac; separate Histamine | Damage scales with every unit still in active blood: each unit adds 0.1 toxin and 0.3 brute per tick, so dilution, metabolism, and purging continuously reduce danger |
| Itching Powder | Multiver + Ammonia + Welding Fuel, 280–700K, pH 5–9, at least 30% inputs | Touch-active scratching irritant: even a splash absorbs a reduced bloodstream dose that causes minor trauma and motor distraction |
| Teslium | Gunpowder + Silver + Plasma, 400–473K, pH 4–10, at least 60% inputs | Controlled Rotatium precursor; the upper limit stays one degree below Gunpowder's 474K ignition point |
| Rotatium | Teslium + Mindbreaker Toxin + Fentanyl, pH 3–9, at least 60% inputs | Deep expert chain with steady toxin harm and exact tick-20 onset of strong Blurred/Unsteady rocking distortion |
| Hyperzine | Sugar + Phosphorus + Sulfur | Speed followed by a sluggish crash; addictive |
| Synaptizine | Sugar + Lithium + Water | Fast status counter with toxin cost and OD 5u |
| Morphine | 2 Carbon + 2 Hydrogen + Ethanol + Oxygen | Controlled analgesic/sedative; addictive; oxygen-risk overdose |
| Ephedrine | Diethylamine + Sugar + Oil | Controlled stimulant and downstream precursor |
| Impedrezene | Mercury + Oxygen + Sugar | Addictive cognitive-impairing opiate and Psicodine precursor |
| Nicotine | Grind Tobacco Leaves; purify away Plant Fibre when needed | Legal addictive stimulant that adds Focus, resists mild Sedation, and harms oxygen/toxin above 15u |
| Aranesp | Epinephrine + Diethylamine + Phenol + Atropine + Morphine, pH 5–9, at least 50% inputs | Illicit addictive speed/focus with steady oxygen and toxin damage |
| Epoetin Alfa | The same Aranesp batch below 50% average input purity | Oxygen support with Blurred effects after 10 ticks and worse neurological effects after 31; HPLC-recoverable inverse |
| Happiness | 2 Nitrous Oxide + Epinephrine + Ethanol over 5u Plasma, pH 5–9, at least 40% inputs | Highly addictive positive mood that calms fear/confusion; visibly makes NPCs linger; overdose creates opposing mood swings |
| Sadness | The same Happiness batch below 40% average input purity | Withdrawn mood that makes NPCs leave and specifically purges Happiness and Psicodine; HPLC-recoverable inverse |
| Pump-Up | 2 Epinephrine + 5 purified Coffee, pH 5–9, at least 30% inputs | Controlled endurance stimulant: raises the collapse threshold and Focus at a mild oxygen cost; overdose begins above 30u |
| Mushroom Hallucinogen | Grind Psychedelic Mushrooms; purify away Plant Fibre when needed | Very slow, addictive perceptual and motor impairment; above 30u adds Paranoia and Blurred vision |
| Maintenance Tar | Agitate 3 Plant Fibre against 3 Welding Fuel for Organic Slurry; combine equal Slurry, Tea, and Welding Fuel at pH 5–9 | Produces 3 Tar plus 1 Sulphuric Acid contaminant; resists sedation/collapse but deals 1.5 toxin per tick |
| Maintenance Sludge | 3 Maintenance Tar + Fluorosulfuric Acid over 5u Hydrogen Peroxide, pH 5–9 | Four input units collapse to one Sludge; masks serious injury while toxin accumulates; OD above 25u |
| Maintenance Powder | 6 Maintenance Sludge + Nitric Acid + Universal Enzyme over 5u Acetone Oxide, pH 5–9 | Eight consumed units yield one expert focus drug; OD above only 15u |
| Kronkaine | 3 purified Kronkus Extract + 2 Welding Fuel + Ammonia, pH 6–10, at least 40% inputs | Very addictive speed/focus and collapse resistance at a toxin cost; above 20u adds serious toxin and oxygen strain |
| bLaSToFF | 2 Cyanide + 2 Silver + Lye, pH 7–12, at least 40% inputs | Hallucination plus strong deterministic motor surges; the warning cadence replaces random input loss; OD above 30u |
| SaturnX | Lead + Water + 2 Maintenance Tar, pH 5–9, at least 40% inputs | Fades the wearer and shortens hostile visual detection while steadily dealing toxin damage; OD above 25u |
| Space Drugs | Lithium + Mercury + Sugar | Hallucination and impaired movement; illicit/addictive |
| Bath Salts | Saltpetre + Ephedrine + Stabilizer | Severe illicit stimulant/hallucinogen |
| Krokodil | Diphenhydramine + Morphine + Space Cleaner | Addictive opioid with serious physical harm |
| Methamphetamine | Ephedrine + Iodine + Phosphorus + Hydrogen | Potent speed/focus with overdose and thermal risk |

SaturnX uses Water as the neutral carrier because TG's `Nothing` drink belongs
to the deliberately excluded food/drink system. Its concealment never grants
full invisibility: the wearer remains visually readable, targetable at close
range, and vulnerable to every ordinary chemical and physical consequence.

## Pyrotechnics and energetic chemistry

### Basic fire and smoke

| Product | Recipe | Use or danger |
|---|---|---|
| Thermite | Aluminium + Iron + Oxygen | Corrodes designated doors/props |
| Flash Powder | Aluminium + Potassium + Sulfur | Blinds an area when released |
| Smoke | Phosphorus + Potassium + Sugar | Carries a mixture through an area |
| Smoke Powder | Smoke + Stabilizing Agent | Stabilized smoke payload material |
| Phlogiston | Plasma + Sulphuric Acid + Phosphorus, 374–420K | Self-heating incendiary; overheats explosively |
| Napalm | Oil + Welding Fuel + Ethanol | Persistent flammable fuel |
| Cryostylane | Water + Plasma + Nitrogen | Rapid cooling and cold chemistry |
| Chlorine Trifluoride | Chlorine + Fluorine + Plasma | Extremely energetic incendiary chemistry |

### Force and temperature agents

| Product | Recipe | Safe handling and use |
|---|---|---|
| Pyrosium | Plasma + Radium + Phosphorus | Synthesis cools the beaker. Afterwards Pyrosium survives while converting added liquid Oxygen into heat and inert Depleted Oxygen. Add Oxygen in measured increments and separate the residue with HPLC when needed. |
| Sonic Powder | Oxygen + Sugar + Phosphorus over Stabilizing Agent | Portable below 374K. At 374K it produces a concussive pulse that leaves nearby people unable to hear clearly and unsteady. Without stabilizer, it activates during mixing. |
| Sorium | Carbon + Mercury + Nitrogen + Oxygen over Stabilizing Agent | Portable below 474K. At 474K it throws nearby people away from the vessel. Without stabilizer, the repulsion happens during mixing. |
| Liquid Dark Matter | Carbon + Plasma + Radium over Stabilizing Agent | Portable below 474K. At 474K it pulls nearby people toward the vessel. Without stabilizer, attraction happens during mixing. |

Pulse strength and reach increase with batch size. They affect chemists and
crew but do not move laboratory machinery, and forced movement stops at solid
walls and the station's walkable boundary. Sugar replaces TG's Cola in Sonic
Powder because food/drink chemistry is outside the current scope.

### Electrical and expert energetic agents

| Product or event | Recipe | Safe handling and use |
|---|---|---|
| EMP | Iron + Uranium + Aluminium | Discharges immediately. Larger batches disable machinery and fail nearby airlocks open for longer; equipment outside the radius is unaffected and recovers automatically. |
| Teslium | Gunpowder + Silver + Plasma at 400–473K, pH 4–10, at least 60% inputs | Controlled conductive intermediate. Keep it dry and below 474K unless an arc is intended. It also shocks and disorients a body that receives a dose. |
| Tesla Shock | Teslium + Water, or Teslium heated to 474K | Burns and disorients nearby bodies and briefly disrupts nearby equipment. Batch size increases reach and strength. |
| Exotic Stabilizer | Hyper-Plasmium Oxide + Stabilizing Agent | Expert energetic catalyst. Obtain the oxide by grinding a Salvaged Hyper-Plasmium Geode from cargo. |
| TaTP | Acetone Oxide + Nitric Acid + Pentaerythritol over Exotic Stabilizer at 401–499K, pH 0–6, at least 80% inputs | The catalyst survives. Omitting it detonates the synthesis batch. Finished TaTP remains portable below its deterministic 550K activation threshold. |

EMP and Tesla pulses are world events, not bottled products: their recipes
leave inert calibration residue after releasing their energy. An EMP also
releases a machine's current operator, and affected instrument panels display
their remaining electromagnetic lockout time.

### Energetic synthesis ladder

1. Make Oil from Welding Fuel + Carbon + Hydrogen.
2. Heat Oil above 480K to make Ash. Room air supports a slow burn; explicit
   Oxygen accelerates it. Collect the cooled residue for the next step.
3. Make Saltpetre from 3 Oxygen + Potassium + Nitrogen.
4. Make Multiver from 2 Sodium Chloride + 2 Ash at 380–410K.
5. Combine Multiver + Saltpetre + Sulfur for Gunpowder.
6. Make Acetone from Oil + Welding Fuel + Oxygen.
7. Make Phenol from Oil + Chlorine + Water.
8. Make Fluorosulfuric Acid above 380K.
9. Make Nitric Acid above 480K.
10. Agitate 3 Corn Oil against Sulphuric Acid for Glycerol.
11. Agitate 2 Acetone against Hydrogen Peroxide + Oxygen for Acetone Oxide.
12. Combine Glycerol + Sulphuric Acid + Nitric Acid over Stabilizing Agent for
    Nitroglycerin.
13. Combine 2 Phenol + Acetone Oxide + Nitric Acid over Stabilizing Agent above
    404K for RDX.
14. For the expert branch, grind a Salvaged Hyper-Plasmium Geode, combine its
    Hyper-Plasmium Oxide with Stabilizing Agent, then use that Exotic
    Stabilizer to synthesize TaTP at 401–499K. Keep the product below 550K.

Gunpowder, Nitroglycerin, and RDX initiate at 474K. Stabilizing Agent is not
consumed; omitting it from Nitroglycerin or RDX synthesis consumes the batch
and detonates it.

### Penthrite hazards

Penthrite is both medicine and explosive. Keep it below 450K and away from:

- equal Epinephrine;
- equal Atropine;
- Phenol + Acetone Oxide above 315K.

Do not stage Penthrite treatment beside an adrenaline tray.

### Chemical charges

The Mixing Chamber can seal up to 50u into a portable charge. Choose a 5, 10,
or 20 second thermal fuse. Once armed, a charge can be moved and retains its
owner attribution. Use legitimate demolition orders for authorized work.
Unrequested energetic material remains controlled contraband.

## Utility chemistry

| Product | Recipe | Function |
|---|---|---|
| Space Cleaner | Water + Ammonia | Removes chemical residue and puddles |
| Space Lube | Water + Silicon + Oxygen | Creates slippery surfaces |
| Sterilizine | Ethanol + Space Cleaner + Silver | Strong surface cleaning/sterilization |
| Foaming Agent | Lithium + Hydrogen | Foam carrier precursor |
| Firefighting Foam | Foaming Agent + Cryostylane + Water, below 310K | Cooling and fire suppression |
| Fluorosurfactant | 2 Fluorine + 2 Carbon + Sulphuric Acid | Concentrated precursor for general chemical foam |
| Chemical Foam | Fluorosurfactant + Water | Expands to carry its entire mixture, purity, and owner across an area; slippery but not solid |
| Metal Foam | Foaming Agent + 3 Iron | Expands into a temporary 36-second movement barrier while retaining its mixed payload |
| Carbon Dioxide | Carbon + 2 Oxygen at 777–900K, pH 5–9, at least 30% inputs | Released liquid CO2 extinguishes burning bodies and nearby chemical fires in a 3m area; bloodstream exposure causes Choking |
| Pax | Mindbreaker Toxin + Multiver + 2 Sodium Chloride + Sodium, pH 5–9, at least 30% inputs | Controlled non-sedating pacifier: clears Paranoia and stops hostile NPC pursuit/attacks while leaving ordinary movement and interaction intact |
| Drying Agent | Sodium + Silicon + Oxygen | Neutralizes slippery residue |
| Saltwater | Water + Sodium Chloride | Utility solution and precursor |
| Ice | Water below its cold threshold | Cooling stock |
| Hydrogen Peroxide | Water + Oxygen | Oxidizer and medicine/explosive precursor |
| Lye | Sodium + Water | Strong alkaline component |
| Formaldehyde | Ethanol + Oxygen over Silver | Preservative and advanced component |
| Diethylamine | Ammonia + Ethanol | Alkaline advanced component |

## Delivery routes

| Form/route | Behavior |
|---|---|
| Syringe / injected | Full dose lands immediately; contact chemicals are especially dangerous |
| Ingested | 60% of the dose is absorbed gradually through the stomach |
| Splash / puddle contact | A 10u hand splash or floor pour; 15% is absorbed and contact/topical effects apply |
| Patch | Full dose without becoming an injection; activates topical healing |
| Spray | Aimed 3u application; 35% is absorbed topically while mixture quality and owner are preserved |
| Smoke / inhaled | 40% reaches blood directly without skin-contact or topical effects; clouds preserve source mixture, purity, and attribution |
| Chemical foam | Expands more slowly than smoke and preserves the complete mixed payload, per-reagent purity, and owner; general foam is slippery, Firefighting Foam is not, and Metal Foam is temporarily solid |
| Bottle or pill | Portable measured delivery; pills are swallowed |
| Charge | Sealed energetic package with a thermal fuse |

The Smoke Projector is reusable. Package it from a prepared Mixing Chamber
buffer, carry it to the deployment point, and press `R`; it empties its payload
into a volume-scaled cloud and remains as an empty tool that can be refilled.
Because inhalation bypasses skin effects, a topical salve in smoke is not a
substitute for a patch. Conversely, an inhaled poison is more efficient than a
thrown splash and affects everyone who remains in the cloud.

Smart Metal Foam is intentionally not implemented. Ordinary Metal Foam
already provides temporary containment, while the smart version depends on
automatic space-tile and safe-path construction that the station simulation
does not support. Gravitum likewise remains mechanic-blocked until bodies and
objects can become persistently weightless; neither entry is represented by an
inert substitute.

Miner's Salve demonstrates why form matters: it heals weakly systemically but
gets its full one-off repair from a patch. Acids demonstrate the opposite: a
needle drives contact damage much harder than a splash.

## Troubleshooting

### Nothing is reacting

Check, in order:

1. Is the recipe known and represented in the chamber forecast?
2. Are all reactants present in the exact ratio?
3. Is a catalyst missing?
4. Does the recipe require separately prepared agitation sides?
5. Is temperature inside the operating range?
6. Is pH inside the operating range?
7. Are the inputs pure enough?
8. Did another higher-priority reaction consume a shared ingredient?
9. Is this a timed reaction that has started but not finished?

### The product is impure

- Move pH toward the displayed optimum before making more.
- Purify sourced intermediates before combining them.
- Use clean glassware and avoid leftovers.
- Separate the product with HPLC, accepting some yield loss.
- If an inverse formed, collect and analyze it instead of blindly adding more
  ingredients.

### The order says contaminated

Exact orders reject unrelated chemicals even when the requested amount is
present. Separate the desired reagent, remake it cleanly, or package only a
clean portion. Catalysts that remain in the beaker may also need separation
before delivery.

### The patient got worse

- Check total delivered dose against the overdose threshold.
- Check whether the medicine has a condition: Pyroxadone requires Burning;
  Penthrite's major healing requires critical injury; Miner's Salve wants a
  patch.
- Look for an incompatibility or a harmful precursor left in the dose.
- Remember that purity scales potency.
- Use the correct counteragent: Naloxone for opioids, Antihol for alcohol,
  Psicodine/Haloperidol for relevant mental effects, and an appropriate purge
  for harmful bloodstream chemistry.

### The batch exploded

Common causes are missing Stabilizing Agent, crossing an overheat threshold,
making too large an exothermic batch, heating an energetic product to its
activation temperature, mixing water-reactive material with water, or mixing an
incompatibility. Rebuild at a
smaller scale and leave thermal headroom.

## Mastery checklist

You understand the current chemistry loop when you can:

- fill early orders without consulting ratios;
- prepare and activate an agitated recipe correctly;
- distinguish a consumed reactant from a surviving catalyst;
- tune pH toward an optimum instead of merely entering the legal range;
- preserve and verify purity through transfer and packaging;
- purify a dirty botanical extract with enough retained volume for an order;
- diagnose a stalled timed reaction from the chamber readout;
- select a medicine by patient state, route, dose, and counteragent needs;
- safely synthesize and package a controlled energetic compound;
- complete a 95% exact order without brute-force overproduction;
- trace any expert product back through its intermediate and external-source
  dependencies.
