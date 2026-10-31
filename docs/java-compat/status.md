# Java mod compatibility: status

Status of 2026-10-31. Fork branch `NeuHorizon`. Writing standard: ASD-STE100
(see [README.md](README.md#writing-rules-for-these-documents)).

## 1. Summary

Horizon plays a NeoForge 1.21.1 modpack from Prism Launcher. Seven content
mods are ported at the data tier: their blocks and items are in the game. The
behavior code of mods does not run in the game yet.

A native compiler turns Java bytecode into machine code with Cranelift. It
compiles Create, Sable, NeoForge and Minecraft 1.21.1 together in 3 minutes.
It runs vanilla code with the same results as the JVM.

## 2. What works

| Part | Status | Proof |
|---|---|---|
| Prism launch | Prism starts Horizon with the player's account. The first setup takes about 144 s. A cached start takes about 0 s. | `/mnt/shared/Horizon/fresh-test/horizon-fresh-test.sh` (local wrapper) |
| Mod jars | Horizon reads the jars from the `mods` folder of the Prism instance. | `launcher modpack import`, `modpack port` |
| Seven content mods | Their blocks and items are in the game and in creative search. | Section 3 |
| Setup progress | The splash shows each step and the mod count ("Mod code 3/7"). | `crates/horizon-client/src/game/setup_splash.rs` |
| Native compiler | It compiles vanilla Minecraft, NeoForge and mods to machine code. | `cargo test --release -p horizon-java --features native --test native` (18 tests) |
| Runtime library | Rust versions of the JDK classes that mods use most. | Each test compares its result with the JVM. |
| JDK classes | The compiler compiles the other JDK classes from the player's own Java runtime. | `jar_bench --jdk` (section 4) |

## 3. The seven content mods

These mods are "partial": the port carries their data. The comparison with
NeoForge (the harness) is not run for them yet. Their logic does not run.

| Mod | Licence | Carried | Left out (no vanilla template) |
|---|---|---|---:|
| Brewin' and Chewin' | MIT | 25 blocks, 18 items | 6 blocks |
| Chipped | Terrarium License | 6,936 blocks, 12 items | 45 blocks |
| Cook's Collection | All Rights Reserved | 8 blocks, 10 items | 2 blocks |
| Decorative Blocks | MIT | 33 blocks, 1 item | 23 blocks |
| End's Delight | MIT | 2 blocks, 56 items | 6 blocks |
| Expanded Delight | MIT | 41 blocks, 61 items | 4 blocks |
| Towns and Towers | CC-BY-NC-ND-4.0 | 1,112 world generation files | not compared |

Four of these mods need Farmer's Delight. Farmer's Delight is not ported yet.

The port writes `coverage.md` in the pack's port folder. Ports stay on the
player's computer. Do not publish ported mods.

## 4. The native compiler

| Data | Value |
|---|---|
| Program | Create, Sable, Aeronautics, Flywheel, Veil, NeoForge 21.1.251, Minecraft 1.21.1, 95 libraries |
| Classes | 47,296 |
| Methods | 448,653 |
| Compile time | 3 min 9 s (it was 43 min) |
| Peak memory | 6.8 GB (it was 12.2 GB) |
| Methods that do not compile | 165 |
| Instructions that stop (a missing JDK or library method) | 27,310 (it was 39,416) |
| Vanilla benchmark (`Shapes`, n = 200) | The same result as the JVM. 1.32 s for each pass. The JVM uses 0.53 s. |

The compiler has these parts:

| Part | What it does |
|---|---|
| Dispatch table | One global table for all virtual calls (49 MB for the program above). Class ids follow the class tree. |
| Streamed compile | Each worker compiles a method body when it is ready. Only small bodies stay for inlining. |
| Runtime library | Rust versions of `String`, the boxes, collections, streams, threads, `Class` reflection, `PrintStream` and more. |
| Level 3 | `jar_bench --jdk <dir>` adds the JDK classes that the program needs from the player's Java runtime. |

### 4.1 Procedure: compile the program and see what stops

1. Extract `java.base` from the local Java runtime:
   `jimage extract --dir <work>/jdk-classes --include 'regex:/java.base/.*' $JAVA_HOME/lib/modules`.
2. Build the tool:
   `cargo build --release -p horizon-java --features native --example jar_bench`.
3. Run the check on the jars:
   `jar_bench --jdk <work>/jdk-classes --native-check <jars>`.
4. Read the summary. It lists the stops by kind and by package.
5. Set `HORIZON_BENCH_STOP_KEYS=<file>` to write each stop reason with its count.

CAUTION: The check uses up to 7 GB of memory. Close large programs before you
start it.

### 4.2 Procedure: start vanilla 1.21.1's server natively

1. Make a new empty folder. Go into it.
2. Run `jar_bench --jdk <work>/jdk-classes --run-main <Minecraft jar> <libraries> -- net/minecraft/server/Main --nogui`.
3. Read the last line. It names the method and the call that stopped.

Use the Minecraft jar with Mojang's names (NeoForm's `client-…-srg.jar` in
Prism's libraries). Do not give the logging jars (slf4j, log4j, Mojang's
`logging`). The runtime library has its own logging.

Status: the server starts `main` and stops in `java.time.Duration`, at
`BigInteger.valueOf`.

## 5. Decisions

| Date | Decision | Document |
|---|---|---|
| 2026-10-29 | Mixins apply at install. Native replacements only where no Mixin changes the method. | [compiler.md](compiler.md) 4.1, 4.2 |
| 2026-10-29 | Memory: values and Rust data first, a collector for the rest. | [compiler.md](compiler.md) 4.3 |
| 2026-10-31 | JDK classes come from the player's Java runtime, chosen by a rule. | [compiler.md](compiler.md) 4.4 |
| 2026-10-31 | Modpack mode: a pack runs on the vanilla of its own version, compiled natively. | [compiler.md](compiler.md) 4.5 |
| 2026-10-31 | Rendering: one render-intent IR with thin adapters. | [rendering.md](rendering.md) |

## 6. Limits

1. The behavior code of mods does not run in the game. Machines, entities and
   custom block logic do not work.
2. Create, Sable and their libraries compile, but they do not run. They are
   not playable.
3. There is no garbage collector yet. A long run uses all of its heap.
4. Mixins do not apply yet.
5. Mod rendering (Flywheel, Veil, block entity renderers) is a plan only.

## 7. Next steps

| # | Step | Result |
|---|---|---|
| 1 | Fill the JDK stops of the server start (`BigInteger`, files, regular expressions) | Vanilla 1.21.1's server starts natively. |
| 2 | Collector (values first, then tick regions) | Long runs. |
| 3 | Mixins at install | Sable and Create change vanilla as they do on NeoForge. |
| 4 | NeoForge compiled from its jars | Events, registries and capabilities. |
| 5 | Render recorder and de-animation | A Create cogwheel turns in the game. |
