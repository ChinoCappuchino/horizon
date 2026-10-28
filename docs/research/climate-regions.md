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

Over 64 km squares sampled every 256 blocks (seeds 7 and 99, `--climate-step 256`), the regions'
shares of the area, estimated from every sample's set of matching regions (maximum likelihood),
are their weights' shares: 34/33/27/6 % and 33/34/27/7 % against 33/33/27/7 %. Two samples are
in the same region 69 % of the time 256 blocks apart, 50 % at 512, and by chance (42 %) from
768 on.

## What Horizon does with it

- **Data.** A mod's region is a file, `data/<ns>/worldgen/horizon_climate_region/<name>.json`:
  `{"preset", "weight", "biomes": [{"coordinates", "offset", "biome"}]}` in quantized climate
  units. `tools/worldgen_probe.py regions <run>.climate.json <carried mod>` writes them from a
  NeoForge run. Entries naming a biome that is not installed are left out.
- **Placement** (`horizon_world::worldgen::biome_source::regions`). The preset's own list is
  region 0 (weight 10, TerraBlender's default), then the mods' regions by id. A jittered grid of
  points 576 blocks apart splits the world into cells (each position takes its nearest point's),
  and each cell picks a region by weight from the world seed. Its unit test checks the shares and
  the patch sizes above. Within a region the biome is vanilla's rule, exactly.
- **Results** (`worldgen_probe.py horizon biomesoplenty`). At NeoForge's climate, Horizon's biome
  is one of the regions' picks at 99.75 % (seed 7) and 99.73 % (seed 99) of 66,049 samples; the
  rest are 26.3's new vanilla biomes (dappled forest, pale garden, sulfur caves). Biome shares
  differ by 1.97 % and 1.93 %, less than NeoForge's two seeds differ from each other (3.25 %).
  Biomes O' Plenty is 40.2 % of the area on Horizon, 39.7 % on NeoForge (seed 7).
