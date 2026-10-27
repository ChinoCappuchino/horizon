# Farview: behaviour spec (clean room, v0)

Farview (mod id `afterwinter_worldgen`, NeoForge 1.21.1, the Afterwinter pack's world generation) is
described here from two sources only, as Horizon's clean-room rule asks: the mod's own **data files**
and what the world-generation probe **observes** on NeoForge 1.21.1
(`tools/worldgen_probe.py`). Its code is not read; at most it is a hint, and nothing below comes from
it. This spec is what a native port (docs/research/worldgen-hooks.md, milestone M5) is written
from.

## From the data files

**Dimension types** (`data/afterwinter_worldgen/dimension_type/`):

| id | min_y | height | logical height |
|---|---:|---:|---:|
| `afterwinter_worldgen:highlands` | -64 | 2048 | 2048 |
| `afterwinter_worldgen:overworld` | -64 | 640 | 640 |

Vanilla's overworld is 384 high. Both are plain data: Horizon reads dimension types as data, so
the extra height needs no hook, only support for heights up to 2048 in the world and renderer.

**World presets** (`data/afterwinter_worldgen/worldgen/world_preset/`): 13 presets, all with an
overworld generator of type `afterwinter_worldgen:atlas` (a generator written in code: the gap
the hooks fill).

| preset | dimension | palette | terrain_version | other settings |
|---|---|---:|---:|---|
| earth | highlands | 106 biomes | 37 | railway_revision, climate (5 settings), biome_rules (13) |
| coastal_life | highlands | 106 | 37 | railway_revision, climate (5), biome_rules (13) |
| living_wilds | highlands | 106 | 13 | railway_revision, climate `winter` |
| landscapes | highlands | 106 | 12 | railway_revision, climate `winter` |
| temperate | highlands | 106 | 12 | railway_revision, climate `temperate` |
| watersheds | highlands | 6 | 8 | railway_revision |
| integrated | highlands | 6 | 7 | railway_revision |
| alpine | highlands | 6 | 6 | |
| civilisation | highlands | 6 | 5 | |
| rugged | highlands | 6 | 4 | |
| wildlands | highlands | 6 | 3 | |
| highlands | highlands | 6 | 2 | |
| atlas | overworld | 6 | (none) | |

`terrain_version` reads as a generation-algorithm revision per preset: a port must reproduce each
revision it supports, or declare which ones it covers. The palettes name the biomes the generator
may place; 63 biomes of Farview's own (`winter_*`) ship as data, the rest are vanilla's.

**Other data**: 5 configured and 5 placed features, plus tags for Towns and Towers, Immersive
Engineering and the `c:` conventions: the integrations Farview declares as data.

## Observed (probe, NeoForge 21.1.251)

Preset `earth`, seed 1, chunks -4..4 around the origin (radius 4), alone in the pack:

- Generator class reported: `AtlasChunkGenerator`; dimension `afterwinter_worldgen:highlands`.
- Surface heights 254 to 278 (average 264): the land sits about 200 blocks above vanilla's.
- Biomes at the surface: dark forest 675, forest 574, river 47 (of 1,296 columns).
- One mineshaft start; logs 10,714, leaves 37,972, ore blocks 92,439 in the 81 chunks.

The matrix (tools/worldgen_matrix.toml: seeds 2, 42, 8675309, two load orders, alone and with Biomes
O' Plenty, Towns and Towers and the rest of the pack's world generation) records the rest; its
results go in "Observed across the matrix" below once it has run.

## Still to observe

What the port must reproduce and the probe does not show yet:

1. The terrain shape at a larger scale: mountain ranges and connected rivers across regions
   (a coarse probe over a wide area, every 8th chunk).
2. The climate and biome rules of `earth`: which palette biome lands where.
3. The reserved sites: Farview keeps areas for its own settlements, rails and lava; with Towns and
   Towers present, which structures are kept out and where (the "reserve" claim of the hooks design).
4. Vanilla structures and features on top of Farview terrain: which still generate, and how often.
5. Determinism: the same seed gives the same world in every load order (the matrix checks it).

## Hooks this spec needs (from docs/research/worldgen-hooks.md)

| Behaviour | Stage | Claim |
|---|---|---|
| the atlas generator | terrain shape: a provider for the Farview presets | provider |
| palette, climate, biome rules | climate and biomes | `add` (own biomes) |
| settlement, rail and lava sites | structures, features | `reserve` |
| its trees, villages and other structures beside other mods' | structures, features | `add`, `replace` where it declares it |

## Presets: the baseline and the rest

`earth` is the baseline: the preset Afterwinter's players mostly use (the pack's owner, 2026-10-28).
Every other preset gets a matrix row of its own as well, so a port cannot quietly assume one terrain
version, one palette size (6 or 106 biomes) or one dimension type (`highlands` or `overworld`). A port
may support presets one at a time, but it says which ones, and the rest stay listed as not yet.
