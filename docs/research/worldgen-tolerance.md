# World generation: what may differ between Horizon (26.3) and NeoForge 1.21.1

The fork's matrix (docs/research/worldgen-hooks.md) compares Horizon's generator with a NeoForge
1.21.1 server running the same mods, seed by seed (`tools/worldgen_probe.py horizon`). Horizon is
Minecraft 26.3, so some differences come from vanilla itself changing between the two versions.
This page lists each accepted difference with its evidence. Anything not listed here is a failure.

Horizon's own parity suite already checks its generator against vanilla **26.3** block for block
(tools/test_chunk_generation_parity.py and the others); this page only covers 1.21.1 against 26.3.

## Checked exactly (no tolerance)

| What | Evidence (vanilla, seeds 2, 42, 8675309, 81 chunks each, 2026-10-28) |
|---|---|
| Base height (`getBaseHeight(OCEAN_FLOOR_WG)`: the noise column before carvers), every 4th column | 3,888 of 3,888 equal |
| Structure starts each chunk holds (id and bounding box) | 243 of 243 chunks equal |
| Structure starts of the row's mods over a wide square (`--starts-radius`) | per row, below |

## Accepted, with a bound

| What | Bound | Why, and the evidence |
|---|---|---|
| Biome at the base height, through vanilla's biome zoom | 0.5 % of sampled columns | 1 of 3,888 differs (seed 42, x 36 z 12 at y 62: `dark_forest` on 1.21.1, `beach` on Horizon), on a boundary at sea level |
| Biomes where mods split the Overworld into climate regions (fork `afterwinter`, climate-regions.md) | over a wide square: 0.5 % of samples outside the regions' picks; biome shares no further from NeoForge than NeoForge's runs from each other | which region a place belongs to is Horizon's own function, matched by observation, so single columns differ; within a region the biome is vanilla's rule. Biomes O' Plenty, 64 km squares: 99.75 % and 99.73 % inside (the rest are 26.3's new vanilla biomes, which 1.21.1's lists lack), shares 1.97 % and 1.93 % apart against NeoForge's 3.25 % between two seeds |
| Decoration totals (logs, leaves, ore blocks) and ground columns | 20 % against the nearest NeoForge run | vanilla's own runs differ by up to 11 % (logs, seed 42, two load orders: features are placed in the order chunks finish); Horizon has 6 to 10 % fewer logs and leaves on every seed, with ore blocks within 0.5 % and ground (terrain without trees) equal on 3,881 of 3,888 columns: tree features changed between 1.21.1 and 26.3. Horizon reported no skipped or failed features. |
| Vanilla structure starts over a wide square (`--starts-radius 48`: 9,409 chunks per seed) | listed types only | every other vanilla type is identical on all three seeds; the differences are version changes: `trial_chambers` (22 starts, each in the same chunk with a different box: 26.3 changed their layout), `ruined_portal` and `ruined_portal_mountain` (4, same chunk, other box), and the `abandoned_camp_*` structures (7 on Horizon only: new in 26.3) |

## Third-party mods (clean room: data files and observed behaviour)

| Mod | Status on Horizon | Evidence |
|---|---|---|
| Towns and Towers 1.13.11 | **works** as horizon-port-pack carries it (data only) | seeds 2, 42, 8675309: all 13 of its structure starts in the 97 × 97-chunk square equal NeoForge's (id, chunk, box); base, biomes and decoration as vanilla's rows |
| Biomes O' Plenty (with TerraBlender) | **biomes work**; decoration not yet | carried by horizon-port-pack, its regions written from a NeoForge run (`worldgen_probe.py regions`); base terrain and structure starts equal, biomes within the region bound above; its features are mostly Java types (M3g) |
| Large Ore Deposits 9.1.7.0 | not yet | code features added by NeoForge biome modifiers; its config files (`config/adlods/Deposits/*.cfg`) describe every deposit and are the spec for a native port; needs mod features in decoration (M3) |

## To find out

- Which tree features changed between 1.21.1 and 26.3, so the decoration bound can be narrowed
  per feature rather than per total.
- The one biome column: whether 26.3's climate or its zoom differs there.
