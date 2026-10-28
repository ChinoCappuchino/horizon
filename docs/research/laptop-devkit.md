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
- **Height.** Pass the dimension type's floor and height: the `highlands` presets are
  `--min-y -64 --height 2048`.
- **Limits:** 256 MiB of memory per instance, a fuel budget per call (about 10⁹ instructions;
  4 × 10⁹ for `open`, where regional maps can be built).

## 3. Benchmarks

| What | Command | Compare with |
|---|---|---|
| A world-generation mod's terrain, in the sandbox | `cargo run --release -p horizon-mods --features runtime,pack --example worldgen_bench -- <mod.hmod> --threads 8 --size 64 --verify` | FarView core fill bench (in-process) |
| Horizon's own vanilla generation | `cargo run --release -p horizon-world --example chunkgen_bench -- .local/horizon-client/26.3 --threads 8 --size 32` | vanilla, and later FarView through the game (M3) |
| A server under bots | `python3 tools/server_bench.py --servers horizon,vanilla --bots 1,10 --behaviour explore` | the pack's dedicated server |

`worldgen_bench` options: `--preset ID --seed S --settings JSON --center CX CZ --min-y Y --height H
--sea-level Y`. `--verify` regenerates every chunk on one thread in reverse and fails if any differs.
On the Nobara VM (20 threads) the stand-in mod's trivial hills give 13,050 chunks/s on 8 threads:
the sandbox's ceiling, not a prediction for FarView, whose own work will dominate.

## 4. Splitting the work (clean room)

The Horizon side never reads FarView's code. FarView's own author (the laptop's session) ports
FarView to this API and runs the head-to-head benchmarks. What crosses between the two: jars,
benchmark numbers, crash reports, and API needs described as behaviour ("a call that returns
the climate at a position", "64 more MiB"), never code. Heavy jobs on the laptop run one at a
time.
