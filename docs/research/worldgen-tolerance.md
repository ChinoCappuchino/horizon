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
| Decoration totals (logs, leaves, ore blocks) and ground columns | 20 % against the nearest NeoForge run | vanilla's own runs differ by up to 11 % (logs, seed 42, two load orders: features are placed in the order chunks finish); Horizon has 6 to 10 % fewer logs and leaves on every seed, with ore blocks within 0.5 % and ground (terrain without trees) equal on 3,881 of 3,888 columns: tree features changed between 1.21.1 and 26.3. Horizon reported no skipped or failed features. |

## To find out

- Which tree features changed between 1.21.1 and 26.3, so the decoration bound can be narrowed
  per feature rather than per total.
- The one biome column: whether 26.3's climate or its zoom differs there.
