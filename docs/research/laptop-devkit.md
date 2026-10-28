# Horizon fork devkit for the test laptop

For the Nobara laptop (`nobara-pc`, Ryzen 5 5500U), which benchmarks the fork against FarView's
and the Afterwinter pack's own suites. Branch `afterwinter`; nothing here is pushed upstream.

## 1. Get the code and build

```sh
git clone -b afterwinter horizon-afterwinter.bundle horizon && cd horizon
rustup toolchain install stable          # tested with 1.99.1
rustup target add wasm32-unknown-unknown # world-generation mods are WebAssembly
sudo dnf install alsa-lib-devel systemd-devel   # audio and controllers; or build with --no-default-features
cargo test -p horizon-mods --features runtime,pack   # the mod runtime: 41 + 4 + 32 + 10 tests, about 2 minutes cold
```

The first `--release` build compiles wasmtime and its Cranelift compiler: several minutes, part
of it on one core, so low CPU use then is normal. Later builds take seconds.

Horizon needs the game's files from **your own** Minecraft 26.3, prepared once on each machine
(they are Mojang's: never copied between machines or committed):

```sh
cargo run --release -p horizon-prepare -- --prepare --server-jar ~/Downloads/server.jar   # JDK 25
```

What tripped the laptop's first prepare (2026-10-28), and the fix for each:

1. **It picked another launcher's folder.** The CurseForge app's `versions/26.3/26.3.json` was
   rewritten (another sha1), and there was no `26.3.jar` because 26.3 was never played there.
   Point `--minecraft-dir` at a folder in the official launcher's layout,
   `versions/26.3/26.3.jar` and `26.3.json`, with the jar checked against Mojang's version
   manifest.
2. **Prism isn't read directly.** Link Prism's `libraries/` and `assets/` into that folder.
3. **Nobara's `java-25-openjdk` is a JRE without `javac`.** Use a full JDK 25 (Temurin 25.0.5
   worked) with `JAVA_HOME` set.

The server jar is Mojang's own 26.3 `server.jar`; check its sha1 against the version manifest.

The server: `cargo build --release -p horizon-client --bin horizon-client -p horizon-server`
([docs/hosting.md](../hosting.md)).

## 2. How mods and world generation plug in

No NeoForge compatibility layer, by design: Horizon runs no Java mod code. Mods are WebAssembly
components in a sandbox ([docs/mods.md](../mods.md)). Data (biomes, structures, features as JSON,
blocks, items, assets) carries over with `horizon-port-pack`, which reads CurseForge zips and
instances, `.mrpack` and Prism instances ([docs/modpacks.md](../modpacks.md)).

Code that shapes the world becomes a native world-generation mod: Rust built for
`wasm32-unknown-unknown` against the world `horizon-worldgen-mod`, with the SDK's
`horizon_mod_sdk::worldgen::WorldgenMod` and `export_worldgen_mod!`. The contract (stages, the
determinism rules, limits, claims to replace, remove or reserve other mods' content):
[worldgen-hooks.md](worldgen-hooks.md#m2-the-contract); every function and type:
[docs/api/mods-reference.md](../api/mods-reference.md) (`worldgen`, `worldgen-events`). A complete
small example is the stand-in mod `crates/horizon-mods/tests/worldgen_guest/` (`src/lib.rs`,
`horizon-mod.toml`); build one with `cargo horizon-mod build` in its folder
(`cargo build --release -p cargo-horizon-mod`, then `target/release/cargo-horizon-mod horizon-mod build`).

**Status:** the contract, the sandbox and its checks exist (milestone M2); the game's generator
doesn't call world-generation mods yet (M3, next). Until then a ported FarView can be built,
tested and benchmarked in the sandbox alone, below.

Things that matter for a FarView port:
- **Determinism.** The game runs one instance per generation thread and asks any instance for any
  chunk, in any order, more than once. Each instance is single-threaded, so nothing inside it
  runs re-entrantly; caches are fine, but an answer must depend only on the call, the world and
  what `region-block` reads.
- **Terrain as columns.** `terrain` may answer exact columns of runs (state, top), bottom up,
  each top above the one before. A column that doesn't ascend is refused, with the column named.
- **Shared regional maps.** `shared(layer, key)` / `build-shared(layer, key)`: each cell built
  once per world by whichever instance first needs it, kept (LRU, 512 MiB by default) and read
  by all; a build reads only coarser layers. See [worldgen-hooks.md](worldgen-hooks.md). The
  stand-in mod uses two layers with `--settings cells`.
- **Height.** Pass the dimension type's floor and height: the `highlands` presets are
  `--min-y -64 --height 2048`.
- **Limits:** 256 MiB of memory per instance; time limits of 10 s per call, 60 s for `open` and
  for each shared cell's build.
- **Ahead and kept:** `prefetch(layer, keys)` builds cells on idle cores before they're needed;
  `saved_layers` in the manifest marks layers the game keeps with the world (from M3).

## 3. Benchmarks

| What | Command | Compare with |
|---|---|---|
| A world-generation mod's terrain, in the sandbox | `cargo run --release -p horizon-mods --features runtime,pack --example worldgen_bench -- <mod.hmod> --threads 8 --size 64 --verify` | FarView core fill bench (in-process) |
| Horizon's own vanilla generation | `cargo run --release -p horizon-world --example chunkgen_bench -- .local/horizon-client/26.3 --threads 8 --size 32` | vanilla, and later FarView through the game (M3) |
| A server under bots | `python3 tools/server_bench.py --servers horizon,vanilla --bots 1,10 --behaviour explore` | the pack's dedicated server |

`worldgen_bench` options: `--preset ID --seed S --settings JSON --center CX CZ --min-y Y --height H
--sea-level Y --shared-mib M --passes N`; with `--passes`, pass 1 is cold and the rest hot
(same workers and world). It prints how the shared cells were used (built, found, waited for, dropped). `--verify` regenerates every chunk on one thread in reverse and fails if any differs.
On the Nobara VM (20 threads) the stand-in mod's trivial hills give 13,050 chunks/s on 8 threads:
the sandbox's ceiling, not a prediction for FarView, whose own work will dominate.

Baselines on the laptop (Ryzen 5 5500U, 2026-10-28): the stand-in mod 22,399 chunks/s on 8
threads (the sandbox's ceiling there); Horizon's vanilla generation (`chunkgen_bench --threads 8
--size 32`) 457.8 proto chunks/s, decoration 2,349.5 chunks/s; FarView's Java fill bench
~1,445 to 1,628 chunks/s; the whole pack's dedicated server ~103 to 169 chunks/s.

FarView's Earth terrain as a world-generation mod (the laptop's port, built with simd128),
`worldgen_bench`-style runs back to back with FarView's Java fill bench, chunks/s, first / hot
pass (2026-10-28; numbers from the laptop):

| Case | Java | Horizon, fuel limits | Horizon, epoch limits |
|---|---|---|---|
| seed 42, mountain, 1 thread | 117 / 161 | - / 205 | - / 225 |
| seed 42, 8 threads | 603 / 891 | 589 / 1,176 | 619 / 1,270 |
| seed 7036569954403863715, lowland, 1 thread | 263 / 362 | - / 399 | - / 432 |
| same seed, coast, 1 thread | 288 / 367 | 296 / 496 | 311 / 531 |
| same seed, 8 threads | 548 / 1,656 | 654 / 1,640 | 693 / 1,720 |

Prefetch adds 23 to 28 % to first passes (seed 42: 763; the other: 889). With saved layers
preloaded, a reopened world's first pass on 1 thread: 197 chunks/s (14.6 fully cold) on seed 42,
343 (160) on the other. Open question: with prefetch on, the 8-thread hot pass on the second
seed was about 10 % lower (1,551 against 1,720), possibly idle-class prefetch builds on the SMT
siblings of busy cores. The laptop's sustained power limit costs about 18 % after an hour of
load, so every comparison runs Java and Horizon back to back.

## 4. Splitting the work (clean room)

The Horizon side never reads FarView's code. FarView's own author (the laptop's session) ports
FarView to this API and runs the head-to-head benchmarks. What crosses between the two: jars,
benchmark numbers, crash reports, and API needs described as behaviour ("a call that returns
the climate at a position", "64 more MiB"), never code. Heavy jobs on the laptop run one at a
time.
