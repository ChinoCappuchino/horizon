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
| M1 | Behaviour spec for Farview (clean room) and the oracle probe | probe exports for vanilla and Farview on NeoForge 1.21.1 |
| M2 | WIT interfaces and manifest schema for the five stages and claims | API reference generated, sandbox tests |
| M3 | Climate and terrain hooks in `horizon-world` | vanilla rows unchanged; a test provider |
| M4 | Structures, features, claims, the report | matrix rows with stand-in mods |
| M5 | Farview port (Rust to WebAssembly) from the spec | Farview rows and the pack row pass |
| M6 | Proposal upstream | issue with the design and the matrix results |

## Moving to another machine

All scripts take paths from environment variables (`HORIZON_PACK_DIR`, `HORIZON_MINECRAFT_DIR`,
`HORIZON_NEOFORGE`, `CARGO_TARGET_DIR`), and outputs go under `.local/` like the rest of
Horizon. A new machine needs the fork, Rust, Java 25 (and Java 21 for the 1.21.1 oracle), the
player's own Minecraft 26.3 and pack jars, then `horizon-prepare --prepare` and the matrix
script.
