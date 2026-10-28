# Large Ore Deposits: what it does, as observed, and Horizon's port

Large Ore Deposits (`adlods`, 9.1.7.0 for NeoForge 1.21.1) adds large, rare ore deposits on top of
vanilla's veins. Horizon's port (`mods/ports/adlods`, fork `afterwinter`, M3g) is clean room: its
code is not read. What each deposit is made of comes from the pack's own config files; how deposits
are placed comes from observation on NeoForge.

## The configs (`config/adlods/`)

`Deposits/*.cfg` (Forge's config format) give each deposit its ores (ids or block tags, with
weights), the blocks it replaces, custom replacements (`#minecraft:deepslate_ore_replaceables ->
deepslate_iron_ore`), its rarity in chunks, its size range in blocks, its altitude range, its
dimensions and biomes, whether it may touch air (`exposed`), descendants and surface indicators.
In a config list every line is a value: one starting with `#` is a tag, not a comment.
`VanillaOres/*.cfg` all say `generation=vanilla` in the Afterwinter pack: vanilla's ore features
stay as they are and deposits come on top.

In the Afterwinter pack, 12 deposits have ores: coal, copper, diamond, emerald, gold, iron, lapis
and redstone in the Overworld, ancient debris, nether gold and nether quartz in the Nether, and a
gas pocket of a mod the pack lacks. The 18 deposits of other mods' metals and gems name `c:` tags
nothing fills, and generate nothing, on NeoForge as here.

`tools/adlods_port.py <config/adlods> <prepared resources> <out>` writes them as the port's data
(tags expanded); the mod is built with `ADLODS_DEPOSITS=<out>`. The output is the player's own and
is not committed.

## Observed (NeoForge 1.21.1, seed 7, 65 × 65 chunks)

`tools/worldgen_probe.py run large-ore-deposits --ores-radius 32` dumps every ore block of the
square; the same run without the mod (`vanilla`) gives vanilla's veins, identical in both, so the
difference is the mod's own blocks: 19,570, while 560 of vanilla's (0.06 %) are gone.

- **Count.** About one deposit per `rarity` chunks: coal 7 (rarity 816: 5.2 expected), iron 5
  (1020: 4.1), copper 2 (1452: 2.9), gold, diamond, emerald, lapis 1 each (3256 to 4480: about 1),
  redstone 0 (3080: 1.4).
- **Size.** Within the config's range: coal 977 to 1,819 blocks (900 to 1,800), iron 743 to 1,310
  (700 to 1,400), copper 772 and 1,316, gold 888 (500 to 1,000), diamond 471, emerald 434 (300 to
  600), lapis 326.
- **Variants.** Stone-like blocks become the ore, deepslate its custom replacement
  (`deepslate_*_ore`): a deposit across the boundary has both.
- **Shape.** A horizontal lens: the shortest of its three axes is vertical in 16 of 18 deposits.
  Standard deviations of the blocks' positions about 0.7, 0.45 and 0.22 times the cube root of the
  size along the long, short and vertical axes (for 1,400 blocks: 8, 5 and 2.5), at any horizontal
  angle.
- **Density.** Highest at the centre (0.5 to 0.9 of the blocks there are ore) and falling like a
  normal distribution's: about 0.3 at 1.5 standard deviations, 0.08 at 2.5, none past 3. Drawing
  positions from a normal distribution with those deviations until the size is reached gives the
  same profile.
- **Reach.** A deposit spans up to 41 blocks; 30 to 77 % of its blocks lie outside the chunk of
  its centre.

## The port

Each chunk is a deposit's origin with chance 1/rarity; the centre is anywhere in that chunk at a
height drawn evenly from the altitude range, the size evenly from the size range; positions are
drawn from the normal distribution above (deviations 0.62, 0.42 and 0.23 of the size's cube root)
until 1.3 times that many distinct ones are drawn, and each becomes
ore where the block is replaceable (or a custom replacement's target), and not next to air for
deposits that are not exposed. Every chunk places, in vanilla's `underground_ores` step, the part
inside itself of each deposit whose origin is within 2 chunks, so parts match across chunks.

Measured the same way on Horizon (`worldgen_probe` example with `--decorator adlods.hmod --ores`,
against the same run without it, seed 7, 65 × 65 chunks): the port's deposits have the mod's shape,
axes 0.70, 0.47 and 0.24 times the size's cube root against NeoForge's 0.70, 0.46 and 0.25; sizes
within the configs' ranges (copper 797 to 1,466, iron up to 1,413, diamond 500, redstone 500 and
529). Drawing positions until the size was reached left deposits a quarter short where caves, dirt
and water take positions; the port draws 1.3 times the size, as the mod's deposits have their full
size placed. Deposit counts (14 here, 18 on NeoForge, about 18 expected) are Poisson: one seed
cannot compare them; comparing them needs runs of several seeds (not run yet).

Not ported yet: surface indicators (flowers in circles above a deposit), descendants (none in the
pack), biome lists (all empty in the pack), relative altitudes (none in the pack).
