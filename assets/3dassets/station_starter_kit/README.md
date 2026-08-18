# ChemGame station starter kit

This folder contains an editable Blender starter kit for the six visual departments in ChemGame and all eight current Chemistry machines. The models use original geometry inspired by Space Station 13's chunky silhouettes, dark machine faces, bright status lights, hazard bands, and strong department color coding. No SS13 sprites were copied or converted.

## Included assets

Machines:

- ChemMaster 5000 — acquisition/dispenser cabinet
- Mixing Chamber — two-beaker refinement and packaging unit
- Reagent Grinder — hopper, motor, output chute, and beaker dock
- Sample Analyzer — open scanner hood and sample pedestal
- Reaction Chamber — insulated vessel, circular door, and thermostat
- Delivery Window — wide counter, transfer tray, glass, and intercom
- Standing Board — department cards and control strip
- Storage Locker — double-door chemistry storage

Department starter bays:

- Chemistry — fume hood, island, sink, glassware, and bottle rack
- Medical — treatment bed, scanner, IV stand, console, and supplies
- Engineering — power cabinet, generator, tool bench, pipes, and valve
- Cargo — conveyor, scanner frame, console, pallet, and crates
- Security — checkpoint, evidence locker, brig gate, camera, and beacon
- Service — kitchen run, drink dispenser, bar, stools, and hydro tray

Chemistry decoration modules:

- Supply shelf — labelled bottles, bins, glove box, and clipboard
- Analysis panel — diagnostic displays, sample rack, and status lights
- Emergency station — eyewash, shower pipe, extinguisher, and warning plate
- Service board — intercom, service lamps, order tickets, and shift notes

The full department bays are editable moodboards and prop groupings. Matching `*_dressing.glb` exports contain the same furniture without the starter floor, walls, pilasters, sign, stripes, or ceiling light, so they can be placed directly in the game's mapped rooms.

## Files

- `station_starter_kit.blend` — two scenes: a lit showroom and an origin-aligned asset library
- `glb/machine_*.glb` — one game-ready authored export per machine
- `glb/department_*_starter.glb` — six complete reference/showroom bays
- `glb/department_*_dressing.glb` — six shell-free runtime furniture groupings
- `glb/decor_chem_*.glb` — four wall-origin, shell-free chemistry decoration modules
- `station_starter_kit_manifest.json` — coordinate spaces, nominal envelopes, runtime collision sizes, measured visual bounds, and named sockets
- `preview_machines.png`, `preview_departments.png`, and `preview_chemistry_decor.png` — quick visual catalogs
- `generate_station_starter_kit.py` — fully procedural source
- `validate_station_starter_kit.py` — Blender-side source/GLB contract checks

The existing sibling files `../ChemDispenser.blend` and `../char_model.blend` are untouched.

## Editing in Blender

Open `station_starter_kit.blend` and use either scene:

- `CG_Showroom` lays everything out and lights it for browsing.
- `CG_AssetLibrary` keeps every source collection at the origin. The models overlap there by design; isolate an `ASSET_*` collection in the Outliner before editing it.

Each part is a separately named object. Materials are semantic and shared—for example, `MAT_Casing`, `MAT_Dark_Metal`, `MAT_Chemistry`, and `MAT_Screen_Analyzer`—so recoloring one material updates the matching parts throughout the kit. Machine interaction points use stable, asset-qualified node names such as `SOCK_MCH_ChemMaster5000__screen`; the `cg_socket` custom property contains the short semantic name.

Conventions:

- Metric scale; one Blender unit is one metre.
- In the Blender source, `+Z` is up, `+X` is right, and `-Y` is the working/front side.
- The Y-up GLB export maps Blender `(x, y, z)` to glTF/Bevy `(x, z, -y)`, so exported assets use `+Y` up and `+Z` as their working/front side.
- Origins sit at the center of each nominal body footprint on the floor; measured decorative overhangs can be asymmetric.
- Chemistry decoration origins sit at the floor projection of their wall-mount centre. Geometry stays on the front (`-Y`) side of the mounting plane so a TrenchBroom marker can be placed directly against an interior wall.
- Bevel modifiers remain editable in the `.blend` and are applied by the GLB exporter.
- Detailed mesh collision is intentionally omitted; keep ChemGame's existing simple collision boxes.

The manifest records both Blender-space and glTF/Bevy-space socket positions. `nominal_envelope` is the authoring target, `runtime_collision` mirrors the game's intentionally simple code-owned machine collider, and `visual_bounds` is measured from evaluated geometry and includes decorative overhangs. Consumers that need a node should look it up by its `cg_socket` property or by the stable asset-qualified name, not by an unqualified `SOCK_screen` guess.

Runtime integration note: Bevy keeps each replicated machine/gameplay root at identity scale and loads its authored GLB as a floor-offset visual child. The simple `Solid` remains code-owned, while container slots use the authored `SOCK_*` positions as their placement contract. Department dressing markers load the shell-free exports render-only; the TrenchBroom map remains authoritative for room shells, navigation, doors, and collision.

## Regenerating

Close the output `.blend`, then run from the repository root:

```powershell
& "C:\Program Files\Blender Foundation\Blender 5.2\blender.exe" --factory-startup --background --python "assets\3dassets\station_starter_kit\generate_station_starter_kit.py"
```

The generator replaces its generated `.blend`, previews, manifest, and GLBs. Make color or geometry changes in a copy if you want to keep them across regeneration.

Validate the editable source, all exported node contracts, imported bounds, normals, material culling, and previews with:

```powershell
& "C:\Program Files\Blender Foundation\Blender 5.2\blender.exe" --background "assets\3dassets\station_starter_kit\station_starter_kit.blend" --python "assets\3dassets\station_starter_kit\validate_station_starter_kit.py"
```

## Visual references

The design language was informed by the equipment lists and station screenshots on the [Goonstation Chemistry wiki](https://wiki.ss13.co/Chemistry), the [/tg/station Chemistry Lab wiki](https://wiki.tgstation13.org/Chemistry), the [/tg/station Medbay guide](https://wiki.tgstation13.org/Medbay), the [/tg/station Engineering guide](https://wiki.tgstation13.org/Engineering), the [official Space Station 13 site](https://spacestation13.com/), and the [/tg/station source repository](https://github.com/tgstation/tgstation). These models are fresh 3D interpretations rather than derived sprite assets.
