# World-generation hooks for mods (fork proposal, gap W)

Status: proposal on the `afterwinter` fork branch, 2026-10-27. Gap **W** of the code-mods plan
(#1189): "dimension types, chunk generators, portals, structure code; worldgen JSON carries, code
missing". Upstream ranks W late because few ATM10 mods need it. The Afterwinter pack (NeoForge
1.21.1) is built around a world-generation mod (Farview), with Biomes O' Plenty, TerraBlender,
Towns and Towers and Large Ore Deposits beside it, so this fork does W first.

## Requirements

1. **Cooperative by default.** One mod shapes the terrain while the others keep adding their
   biomes, structures and features on top. Installing a terrain mod must not switch the rest off.
2. **Precedence and exclusion when a mod needs them.** A mod may replace, remove or reserve:
   no villages inside its city sites, its own trees instead of the vanilla ones in its forests,
   no other structure set in a region it claims. Every such claim is declared, visible and
   overridable by the pack author.
3. **Deterministic.** Same seed and same set of mods: the same world, in any load order.
4. **Proven across variations.** A test matrix of mod combinations, seeds and load orders runs
   against NeoForge 1.21.1 and checks that combinations compose and that exclusions hold.
5. **Portable.** Everything lives in the repository: no machine paths, setup through
   environment variables, so development can move to another machine (the Nobara laptop).
6. **Horizon's rules stay.** WebAssembly sandbox, free when unused, vanilla stays vanilla (mods
   get named hooks, never patches), clean-room ports (mod code is not read; observed behaviour
   and the mod's own data files only; the mod's code, even our own Farview, is a hint at most).

## The stages a mod can join

Vanilla generates a chunk in stages. Each stage gets one hook interface:

| Stage | What a mod can do | Composition |
|---|---|---|
| **Climate and biomes** | add biomes in climate regions (what TerraBlender's regions do), weighted | all contributors share the climate space by weight |
| **Terrain shape** | own the density field of a world preset (a terrain provider), or add terrain modifiers in regions | one provider per world preset; any number of modifiers |
| **Surface and carvers** | surface rules and carvers for the mod's biomes | per biome, as vanilla |
| **Structures** | add structure sets; reserve areas | union, minus reservations |
| **Features** | add features per biome and step; filter features in a region | union, minus filters |

A **world preset** with a terrain provider is what a player picks on the create-world screen
("Farview"). Without one, vanilla shapes the terrain and mods only add.

## Claims: how one mod kicks another out

A mod's world-generation manifest lists **claims**:

| Field | Values |
|---|---|
| `scope` | a region the mod computes (`provider-region:<name>`), biomes, a structure set, a feature id or tag |
| `action` | `add` · `replace` · `remove` · `reserve` |
| `priority` | integer, default 0 |
| `targets` | what is replaced or removed: ids, tags, or `*` (everything of that kind in scope) |

Resolution is fixed: higher priority wins; equal priority falls back to the mod id in sorting
order, never to load order. A `reserve` keeps a region free for the claiming mod's own content
(the sites Farview keeps for settlements, rails and lava). The pack author has the last word in
`config/horizon/worldgen-rules.toml`: change a priority, allow or deny a claim, or turn a mod's
contribution off in one stage.

At world creation the game writes a **world-generation report**: which mod contributes what in
each stage, which claims removed or replaced what, and every conflict that priority decided. A
claim that removes something no mod added is an error in the report, not silence.

## Calls and cost

Mods run in the WebAssembly sandbox like every other mod. Calls are batched per chunk and
sampled on vanilla's own grid (noise cells of 4×4×8 blocks, biomes per quart), never per block.
A provider that keeps a large map (Farview's regional atlas) caches it through the storage
interface per world, with a size cap. With no world-generation mod on, no hook is called and the
vanilla code path is unchanged, so the parity harnesses keep testing the same game.

## Proving it: the test matrix

The NeoForge oracle (`tools/neoforge_oracle.py`, set `HORIZON_NEOFORGE=21.1.251` for the
Afterwinter pack) gets a world-generation probe. For a fixed seed and an area of chunks it
exports surface heights, biomes per quart, structure starts and feature counts per chunk. The
same probe runs in Horizon, and the two are compared.

Rows of the matrix (each with three seeds and two shuffled load orders):

| Row | Mods |
|---|---|
| vanilla | none |
| one each | Farview · Biomes O' Plenty with TerraBlender · Towns and Towers · Large Ore Deposits |
| pairs | Farview + Biomes O' Plenty · Farview + Towns and Towers |
| pack | every world-generation mod of Afterwinter |

What each row must show:

1. Horizon matches NeoForge 1.21.1 for that combination, within the documented tolerance.
2. Load order changes nothing.
3. Every structure and feature a mod adds appears unless a declared claim removed it, and the
   report names that claim.

**Tolerance.** Horizon is Minecraft 26.3 and the oracle is 1.21.1. Differences that come from the
version (vanilla's own changes between the two) are listed per stage in
`docs/research/worldgen-tolerance.md`, each with the evidence, and are accepted; anything not on
that list is a failure.

## Milestones

| | Milestone | Proof |
|---|---|---|
| M1 | Behaviour spec for Farview (clean room) and the oracle probe (done) | probe exports for vanilla and Farview on NeoForge 1.21.1 |
| M2 | WIT interfaces and manifest schema for the five stages and claims (done, see below) | API reference generated, sandbox tests |
| M3 | Climate and terrain hooks in `horizon-world` | vanilla rows unchanged; a test provider |
| M4 | Structures, features, claims, the report | matrix rows with stand-in mods |
| M5 | Farview port (Rust to WebAssembly) from the spec | Farview rows and the pack row pass |
| M6 | Proposal upstream | issue with the design and the matrix results |

## M2: the contract

Landed on the fork, 2026-10-28. Nothing in the game calls it yet (that is M3); what exists is
the contract, the code that holds mods to it, and the tests.

**Where it lives.**

| Part | File |
|---|---|
| WIT: `worldgen` (what the mod reads), `worldgen-events` (what the game calls), world `horizon-worldgen-mod` | `crates/horizon-mod-sdk/wit/horizon.wit` |
| Generated reference | `docs/api/mods-reference.md` |
| Manifest: capability `worldgen`, the `[worldgen]` section | `crates/horizon-mods/src/manifest.rs` |
| Claims, the pack's rules, the plan that settles them, the report | `crates/horizon-mods/src/worldgen.rs` |
| Running a mod: instances per worker, every answer checked | `crates/horizon-mods/src/worldgen_runtime.rs` |
| SDK: `WorldgenMod`, `export_worldgen_mod!` | `crates/horizon-mod-sdk/src/lib.rs` (`mod worldgen`) |
| A stand-in mod that misbehaves on request, and its tests | `crates/horizon-mods/tests/worldgen_guest/`, `crates/horizon-mods/tests/worldgen.rs` |

**Version.** An experimental capability on the server-mod line (`api = "0.3"`, `side =
"server"`, `experimental = ["worldgen"]`): additive, so nothing upstream changes meaning. Its
first four imported functions are API level 8, `shared` level 9 and `prefetch` level 10, numbers upstream may later give to something else;
the fork renumbers if it ever merges upstream's level 8.

**The stages as calls.** `open(world-info)` once per instance, then per chunk:

| Stage | Call | Answer, checked by the game |
|---|---|---|
| climate | `biomes(request)` with vanilla's six climate values per quart (empty in a mod's preset) | a biome per quart; `4294967295` keeps what the next mod or vanilla puts there |
| terrain | `terrain(x, z)`; `heights(columns)` without generating | `density` on vanilla's 4 × 4 × 8 grid, or exact `columns` of runs that must ascend; heights inside the world |
| surface | `decorate(x, z, surface \| carvers)` | blocks in the chunk only |
| structures | `starts(x, z)` | starts in the mod's own namespaces, pieces (templates, or blocks inside the start's box) |
| features | `decorate(x, z, <vanilla's 11 steps>)`, reading `region-block` and `region-height` | blocks in the chunk and up to 16 blocks around it, vanilla's limit for features |
| (claims) | `regions(x, z)` | only names the manifest declares |

Block states and biomes go across as numbers (`state-id`, `biome-id`), resolved once, so a
chunk's answer carries no strings per block.

**The determinism contract.** Every answer follows from the call's arguments, the opened world
and what `region-block` reads. The game makes one instance per generation worker and may call
any of them for any chunk, in any order, more than once, so anything a mod keeps between calls
is a cache. The engine canonicalises NaNs and lowers relaxed SIMD deterministically, and a
world-generation mod imports nothing but `log` and `worldgen`: no clock, no storage, no
randomness that isn't seeded from the world. As a side effect, a call never runs inside another
call of the same instance: the re-entrancy behind Farview's "Runs must ascend" crash (see
[farview-spec.md](farview-spec.md)) can't happen in this model, and if a port still sends
runs that don't ascend, the game refuses that chunk's answer with the column named, instead of
crashing.

**Shared cells** (API level 9, asked for by FarView's port: its regional maps are per region,
built on demand, unbounded, 10 to 15 layers from about 128 blocks to 16 km). `shared(layer, key)`
returns a cell's bytes, the same for every instance of the world (`WorldgenMod::world`, then
`World::worker` per thread). A cell the world lacks is built once by the mod's
`build-shared(layer, key)` on a builder instance, with its own time limit (60 s; a coarse
drainage cell takes about 2 s in Java). Another thread that asks for it meanwhile waits for that
build rather than starting its own. Inside a build of layer L only layers above L may be read, so
builds have no cycles and can't deadlock (FarView's halos are recomputed from coarser layers,
never read from finished neighbours). A world keeps up to 512 MiB of cells and drops the least
recently used past that; a dropped cell is rebuilt, identical by determinism. A cell is at most
64 MiB, a key 256 bytes. A failed or crashing build refuses the chunk that needed it; the
instance goes on (a crashed builder is replaced). Instances keep their own copies of the cells
they fetched, as FarView keeps whole cells and interpolates inside them.

**Building ahead, keeping, and time limits** (asked for by the FarView port's benchmarks,
2026-10-28). `prefetch(layer, keys)` (API level 10) queues cells to build before anyone needs
them, on up to two threads per world that run only when a core is idle (the OS's idle class); it
never waits or errs, and results can't change, since a cell is the same whoever builds it. The
manifest's `saved_layers` names layers slow enough to keep with the world's save:
`World::saved_cells` hands them to the game and `World::preload` takes them back (the game does
this from M3). Time limits are wasmtime epochs (a ticker every 10 ms) instead of fuel: 60 s for
`open`, 10 s per call, 60 s per cell build. Time a call spends blocked in `shared`, waiting for a
build or building on its builder, doesn't count toward its own limit (the build has its own): a
first chunk in a new world may wait on a chain of coarse cells far longer than 10 s. Fuel cost the FarView port 14 to 16 % of its
throughput; a time limit decides only whether a runaway call stops, never what a call answers.
NaN canonicalisation stays on although it costs 16 to 24 %: without it the same seed could give
different land on x86 and ARM machines, whose NaN bit patterns differ.

**Failures.** An `err`, or an answer that breaks the rules above, is *refused*: that chunk goes
without the mod's part, and the report will count it (M4). A trap or a call over its budget
*stops* that instance for the world. Limits: the time limits above, 256 MiB of memory per
instance, 4 log lines per call.

**Claims in the manifest.** A Farview-shaped example (a sketch: the port in M5 decides the
real one):

```toml
[worldgen]
presets = ["afterwinter_worldgen:earth", "afterwinter_worldgen:coastal_life"]
stages = ["climate", "terrain", "structures", "features"]
regions = ["settlements", "rails", "lava"]

[[worldgen.claims]]
name = "settlement_sites"       # the pack's rules call it afterwinter_worldgen/settlement_sites
stage = "structures"
action = "reserve"              # no other mod's structures in the sites
scope = "region:settlements"    # the chunks its regions() puts in `settlements`
priority = 10

[[worldgen.claims]]
name = "own_forests"
stage = "features"
action = "replace"
scope = "biome:#minecraft:is_forest"
targets = ["minecraft:trees_birch_and_oak"]
with = "afterwinter_worldgen:winter_trees"
```

A content mod without code (Biomes O' Plenty, Towns and Towers) may have `[worldgen]` with
claims only. Scopes: `*`, `region:<name>` (the mod's own), `biome:<id>`, `biome:#<tag>`,
`preset:<id>`. Targets: `*`, `<id>`, `#<tag>`, `mod:<id>`. Terrain has no claims: one provider
per preset. Checked at load: `replace` names a `with` in the mod's namespaces, `reserve` has a
scope narrower than everywhere, a region scope names a declared region, `add` only covers the
mod's own things, and no claim acts on its own mod.

**How a claim decides.** For a thing a mod adds (a biome, a structure, a feature) at a place
(preset, biome and its tags, the regions each mod's code puts the chunk in), the first claim in
this order that applies to it decides: highest priority, then the mod id in sorting order, then
the claim's name. Never the load order. `add` keeps the claimant's own things (a protection
against lower claims); `remove` and `reserve` take other mods' things out; `replace` swaps in
the claimant's `with`. A mod's claims never act on its own things.

**The pack author's last word.** `config/horizon/worldgen-rules.toml`:

```toml
[claims."afterwinter_worldgen/settlement_sites"]
priority = 50          # instead of the manifest's
[claims."t_and_t/villages"]
allow = false          # ignored
[mods.biomesoplenty]
off = ["features"]     # none of its features, by data or code
```

**The report.** `Plan::report()` writes the declared half: who takes part in which stage,
the claims in the order they decide (with the pack's priority changes marked), the pack's
rules, the conflicts with who wins and why (priority, or the mod-id tiebreak), and the
problems: a rule that names nothing, a claim that removes what no installed mod adds. The
counts of what each claim took out come with generation (M4).

**Tests** (`cargo test -p horizon-mods --features runtime,pack`): `worldgen::tests` (claims are
checked; priority, then mod id, never load order; the pack's rules; removing what nobody adds),
`manifest::tests::worldgen_is_a_server_capability_with_a_section`, and `tests/worldgen.rs`
against the stand-in mod: the same land from every worker in any order, climate-driven biomes,
features that read the region and stay in their area, structures in the mod's namespaces,
refused answers (runs that don't ascend, NaN densities, blocks outside the area, a foreign
structure id, a stage the manifest doesn't list) that leave the mod running, panics and
endless loops that stop only that instance, the game's hooks leaving the mod to the generator,
and a reserve claim deciding by the regions the mod's own code computes.

**Left for later.** Terrain *modifiers* (a mod adjusting another provider's density in its
regions): no Afterwinter mod needs one yet, and it is a new function, so adding it breaks
nothing. Applying claims to data-driven content (JSON biomes, structure sets, placed features)
is M4, where the generator knows each thing's owner and tags.

## M3 plan: steps

Mapped 2026-10-28. The generator in horizon-world already takes any height up to vanilla's 4064
as a parameter; the game around it (client chunk storage, light, meshing, saves, the LAN encoder,
server code) assumes the 384-high overworld through `horizon-client/src/chunk.rs`'s `MIN_Y`,
`HEIGHT` and `SECTIONS` (about 450 uses in 64 files, several u32 section masks). Presets aren't
data: the game remembers only "flat or not" and always builds `minecraft:normal`. So M3 goes in
steps that each prove something on their own, the height change late:

| Step | What | Proof |
|---|---|---|
| M3a | A provider seam in horizon-world's `Pipeline`: a trait for biomes per quart, the terrain fill (columns or density) and base heights/columns for structures, held per worker; horizon-world stays free of horizon-mods | the stand-in mod's land and biomes in Horizon's own chunks (`worldgen_probe` with a mod), vanilla rows unchanged |
| M3b | Data: dimension types as a bundle (replacing the three hardcoded heights), world presets from mod content, the preset id through create-world, level.dat (`horizon:preset`), reopening and `level-type` | a world made with a mod's preset reopens with it |
| M3c | The client bridge: `WorldgenMod`s loaded for the preset in `make_streaming`, one `World` per world and dimension, a worker per generation thread (streaming, queries, ring search, far decoration), saved cells kept with the world, settings from the manifest on the create-world screen | FarView's Earth in a real single-player world at vanilla height |
| M3d | Height: `chunk::{HEIGHT, SECTIONS}` per world, wider section masks, light, meshing, LOD, saves, server code | a 2048-high world: generate, light, render, save, reopen; vanilla's performance unchanged |
| M3e | Network: the mod dimension type synced as data, the hub's encoder using the real type, remote chunks above 384 | a LAN guest and a dedicated server's player see the same 2048-high world |
| M3f | Mod biomes (Biomes O' Plenty): biomes from mod content, climate regions, the client's biome table (7-bit palette), registry sync, feature order across the new biomes | the biomesoplenty matrix row against NeoForge |
| M3g | Mod features in decoration (Large Ore Deposits, FarView's decoration): `decorate` steps and biome-modifier-like additions | the large-ore-deposits row, statistically |

**M3a, done (2026-10-28).** `horizon_world::worldgen::provider::{TerrainProvider, ProviderWorker}`;
`OverworldGenerator::with_provider` keeps the preset's noise settings for sea level, default
block and fluid, and takes biomes (per quart, `KEEP_BIOME` falls back to vanilla's source) and
land (columns, or density interpolated into columns) from the provider. The land is final: no
aquifers, surface rules or carvers over it (as FarView's Java generator: its author, 2026-10-28).
Structure placement, `/locate biome` and the probe read the provider's biomes and columns
(`noise_column` takes the worker). `horizon_mods::worldgen_world::ModTerrain` (feature `world`)
is a world-generation mod as that provider; `worldgen_probe` (now in horizon-mods) takes
`--mod`. Proof: vanilla's probe output is byte-identical before and after on seeds 2, 42 and
8675309 (chunks and the wide starts); `tests/worldgen_world.rs` checks every column of
generated chunks against the stand-in mod's own heights. Left for M3c: far terrain (`lod_grid`,
`surface_sample`'s biome) still samples vanilla's noise in a provider world.

**M3b, done (2026-10-28).** Dimension types are data: `GeneratorData::dimension_height` reads a
`dimension_types` bundle (mods' `data/<ns>/dimension_type/`, merged by the content overlay; at
most 4064 high, multiples of 16) before vanilla's three. Mods' world presets merge into the
presets bundle. A world-generation mod may ship its world generation's data without
capability `registries` (`data/<ns>/{worldgen, dimension_type, structure, tags/worldgen}`). A
preset whose generator type is a provider's own runs over the noise settings it names (or
vanilla overworld's). The world's preset is saved as `horizon:preset` in `world_gen_settings`
with the preset's own overworld dimension (vanilla's saves keep only the dimensions; a vanilla
save's preset is read back from its overworld's noise settings), reopened with it, used by far
decoration, and taken by the dedicated server's `level-type` (any noise preset of the data).
This also gives Horizon vanilla's amplified, large biomes and single biome presets. The
create-world screen's world type button comes with M3c's settings page.

**M3c, done (2026-10-28).** The game loads world-generation mods for the world being opened
(`game/worldgen_mods.rs`): the enabled mod (or `--dev-mod`) whose manifest lists the world's
preset becomes its generator's provider (`VanillaTerrain::for_world_with`), compiled once per
process, opened with the world's settings (`horizon:preset_settings`). A preset of a mod that
isn't on refuses to open, saying so. Its saved layers' cells are written to the world's
`data/horizon/worldgen/<mod>-<dimension>.cells` when the world closes and loaded back when it
opens. Far terrain and far decoration in a provider world read the provider's land and biomes
(per chunk, so slower than vanilla's coarse noise grid: a follow-up). Create New World's World
Type cycles vanilla's four presets and then mods' presets; Customize opens a page drawn from the
mod's `[[worldgen.settings]]` (cycling buttons, sliders), carried to the screen through the
resources' `worldgen/preset_settings.json`; the dedicated server takes `level-type` and, for a
mod's preset, `generator-settings` as its settings JSON. Proof: a dedicated server with
`level-type=test_worldgen:hills` and the stand-in mod: its saved chunks' 739 grass columns are
all at the mod's own heights for seed 42; the world keeps `horizon:preset` and its settings, and
a second start loads the cells the first saved. FarView needs M3d: its land is above y 320.

**M3d, done (2026-10-28).** The client's chunk column is per world: `chunk::{height, sections,
set_height}` (from `MIN_Y`, up to `MAX_HEIGHT` 2048; static per-section tables sized
`MAX_SECTIONS`), set from the world's preset's dimension type before anything of the world is made
(its save keeps the shape). Section masks are u128 (dirty sections, light log, random ticks,
portals, the LAN encoder's light masks, gfx's detail columns); the occlusion graph is rebuilt
when the column changes; mobs' pathing, weather columns and the other 320/384 literals follow
the world's height. Single player's window is a client of its server thread, so the network
part came with it: the hub encodes with the world's real Overworld type
(`lan::data::dimension_type`), registry sync sends a taller Overworld's `dimension_type` as data
even to a client with the known pack, and a Horizon client sets its column from the dimension
type it gets. Proof: a dedicated server and a single-player client on `test_worldgen:tall`
(land at y 1563 to 1570 in a 2048-high world): saved chunks hold the land at the mod's heights
(all 764 surface columns), and the client renders it (a screenshot at y 1573). Not measured
yet: vanilla's frame and light times after the column became a runtime value (they read it
once per call), and the parity harnesses.

**M3e, done (2026-10-28).** The network side came with M3d; proved separately: a dedicated server
on `test_worldgen:tall` and a client in another process (no mod installed) joined it over
loopback: the client took the 2048-high Overworld from the synced `dimension_type`, received
117 columns with no decode errors and drew the land at y 1564. Not tested: a vanilla 26.3
client (the synced dimension type is what vanilla reads, so it should hold).

**M3f, generation and game done (2026-10-28); decoration with M3g.** Biomes O' Plenty works as
horizon-port-pack carries it, with its regions as data (docs/research/climate-regions.md): its
1.21.1 biomes upgraded to 26.3's shape, merged with their carvers and tags; climate regions as a
biome source; carvers of Java types left out; mods' biomes taking vanilla's feature order where
vanilla moved a feature (else the sorter finds a cycle). The game: every namespace's biomes in
the client's table, ambience, music, fog, particles and commands; the biome palette widened past
7 bits (136 biomes: 8); the LAN registry with every namespace's biomes, sent as data outside
`minecraft:`. Proof: the matrix's region comparison (below the per-chunk table: seeds 7 and 99,
64 km squares, Horizon's biome one of the regions' picks at 99.75 % of samples, shares within
NeoForge's own spread), a dedicated server on seed 99 saving 82,468 of 124,416 biome cells as
Biomes O' Plenty's and loading them back unchanged, and a client in another process joining it
and drawing old-growth woodland's grass colour. Not yet: decoration (Biomes O' Plenty's features
are mostly Java types: M3g), its surface rules (code), mods' biome tags over the network, and a
remote client building a biome it lacks from the server's data (it shows plains).

2048 sections per column is about five times the memory and light work of vanilla's 384; M3d is
measured against vanilla worlds so they don't get slower.

## M3 plan: preset settings

A preset's settings travel as `world-info.settings` (JSON text). The game writes them into the
world's save when the world is created and hands the same text to every `open` of that world, so
they never change afterwards. To let the create-world screen offer them for any mod, without
code of the mod's in the game, the manifest declares them:

```toml
[[worldgen.settings]]
preset = "farview:earth"
path = "climate.mode"            # where the value goes in the settings JSON
label = "Climate"
choices = ["latitude", "planet", "regional", "temperate", "winter"]
default = "latitude"

[[worldgen.settings]]
preset = "farview:earth"
path = "strengths.world_scale"
label = "World scale"
range = [0.5, 2.0]
default = 1.0

[[worldgen.settings]]
preset = "farview:earth"
path = "climate.band_width"
label = "Latitude band width"
range = [4096, 1048576]
step = 1024                      # optional: the slider's step (1 for whole blocks)
default = 65536
```

The game draws one widget per entry (a cycling button for `choices`, a slider for `range`),
builds the JSON from the chosen values (fields left at their default may be left out: the mod
fills defaults), and shows them read-only once the world exists. FarView's Earth (from its
author, 2026-10-28): `climate.mode` (latitude, planet, regional, temperate, winter),
`climate.warmth` -1 to 1 (0.35), `climate.band_width` 4096 to 1048576 blocks (65536),
`climate.equator_z` ±29,000,000 (0), and `strengths.world_scale` 0.5 to 2, `mountains`,
`detail`, `rivers`, `lakes` 0 to 2 (all 1.0). `world_scale` and `mountains` redraw the drainage
plans, so no setting may change after creation.

## Moving to another machine

All scripts take paths from environment variables (`HORIZON_PACK_DIR`, `HORIZON_MINECRAFT_DIR`,
`HORIZON_NEOFORGE`, `CARGO_TARGET_DIR`), and outputs go under `.local/` like the rest of
Horizon. A new machine needs the fork, Rust, Java 25 (and Java 21 for the 1.21.1 oracle), the
player's own Minecraft 26.3 and pack jars, then `horizon-prepare --prepare` and the matrix
script.
