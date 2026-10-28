# Climate regions (TerraBlender's, as observed): how mods place biomes

Biomes O' Plenty (and other Afterwinter mods) place their biomes through TerraBlender, a library
that splits the Overworld into **regions**: each region is a full multi-noise parameter list (a
biome for every point of the climate space), and each position of the world belongs to one
region. Horizon's clean-room rule applies: TerraBlender's code is not read. What follows comes
from observation on NeoForge 1.21.1 only (`tools/worldgen_probe.py run ROW --climate-radius N`):
TerraBlender's registered regions are read through its public API (`terrablender.api.Regions`,
by reflection, so the probe neither depends on nor reads it), and the world's climate and biome
are sampled every 16 blocks.

## Observed (Biomes O' Plenty 1.21.1 with TerraBlender, seed 42, 2 km square, 2026-10-28)

| Region | Weight | Parameter points |
|---|---:|---:|
| `minecraft:overworld` (vanilla's) | 10 | 7,593 |
| `biomesoplenty:overworld_primary` | 10 | 8,373 |
| `biomesoplenty:overworld_secondary` | 8 | 8,373 |
| `biomesoplenty:overworld_rare` | 2 | 8,373 |

- **Within a region, vanilla's rule.** For each sample, the biome is the region's parameter
  point nearest to the sampled climate (`Climate.ParameterPoint.fitness`: squared distance
  outside each range, plus the offset squared): one of the four regions' picks equals the biome
  at 16,633 of 16,641 samples (99.95 %).
- **Regions are a function of position.** Where only one region's pick matches (thousands of
  samples), regions form irregular patches some hundreds of blocks across; the region does not
  follow the climate. 29 % of the samples are Biomes O' Plenty biomes.
- **Not observable without the code:** the exact function that assigns a position to a region.

## What Horizon does with it

Regions become data (a mod's region: weight and parameter points, from the dump), and Horizon
assigns positions to regions with its own seeded noise, weighted as observed. Within a region,
biomes are exact (vanilla's rule); where a region's patch begins and ends is not. The matrix
compares Horizon with NeoForge statistically: each biome's share of the area and the patch
sizes, within a tolerance recorded in worldgen-tolerance.md.
