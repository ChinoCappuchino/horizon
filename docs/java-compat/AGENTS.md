# Java mod compatibility: guide for the next developer

This guide is for a person or an AI agent who continues the Java mod work.
Read all of it before you change the code. Then read [status.md](status.md).

Writing standard: ASD-STE100 rules (see
[README.md](README.md#writing-rules-for-these-documents)). Write all new
documentation with the same rules.

## 1. The aim

Horizon must play NeoForge 1.21.1 modpacks that players have now. The first
pack is Afterwinter (113 jars). The main mods are Create and Sable.

There are two ways to bring a mod into Horizon:

| Way | What it does | Status |
|---|---|---|
| Port | `horizon-port-pack` reads the data of a mod (blocks, items, recipes, tags, world generation) and writes a Horizon content mod. | Seven content mods work at the data tier. |
| Compile | `horizon-java` compiles the bytecode of the mod, its loader and vanilla to machine code. The mod's own code then runs. | Create, Sable, NeoForge and Minecraft compile. They do not run in the game yet. |

The port is fast and safe for data mods. The compiler is the way for mods
with logic (machines, entities, Mixins).

## 2. Rules

WARNING: Do not read, copy or decompile the code of a mod, of NeoForge or of
vanilla Minecraft. The tools process bytecode. You can read structure only:
class names, constant pools, annotations and `javap` signatures. This is the
clean-room rule of this fork.

WARNING: Do not commit Mojang files, mod jars, ported mods or the `.local`
folder.

1. Do not change a mod jar.
2. Do not publish a compiled or ported mod.
3. Compile JDK classes only from the player's own Java runtime. Do not
   distribute them.
4. Compare each runtime library method with the JVM (section 6.3).
5. Do not push to upstream without the owner's approval.

What you can read:

| Source | Can you read it? |
|---|---|
| Minecraft 26.3 decompiled sources (`.local/minecraft/26.3/sources`) | Yes, for Horizon's own parity work |
| Public API documentation (JDK Javadoc, NeoForge, Flywheel and Veil wikis) | Yes |
| Constant pools, annotations, `javap` signatures of a jar | Yes |
| The behavior of a mod on NeoForge (the oracle) | Yes |
| The source or decompiled code of a mod, NeoForge or vanilla 1.21.1 | No |
| Intermediate code made from vanilla methods, in detail | No (a profile is permitted) |

## 3. How the system works

### 3.1 The parts

| Part | Folder | Function |
|---|---|---|
| Class file reader | `crates/horizon-java/src/classfile.rs` | Reads `.class` files: constants, members, attributes. |
| WASM backend | `crates/horizon-java/src/compile.rs`, `runtime.rs` | The first backend. It makes one WASM module for each jar. |
| Native backend | `crates/horizon-java/src/native.rs` | Compiles a closed set of classes with Cranelift. This is the main backend now. |
| Runtime library | `crates/horizon-java/src/rt.rs`, `rt/` | Rust versions of JDK classes (`String`, boxes, collections, streams, threads, `Class`, logging, `PrintStream`). |
| Heap | `crates/horizon-java/src/heap.rs` | One heap at a fixed address, a buffer for each thread. There is no collector yet. |
| Registration bridge | `crates/horizon-neoforge` | Runs a mod's registration and writes what it registers as a Horizon content mod. |
| Port pipeline | `crates/horizon-port-pack` | Import, inventory, carry, port, coverage report. |
| Launcher | `launcher/` | `modpack import`, `modpack port`, `modpack play`, the Prism session handover. |
| Game | `crates/horizon-client` | Setup splash, mod items in creative search (`creative.rs`). Build with `--features mods`. |
| Oracle and harness | `tools/neoforge_oracle.py`, `tools/mod_port_harness.py` | Compare a port with the same mod on NeoForge 1.21.1. |

### 3.2 The native backend, step by step

`Native::compile` in `native.rs` does these steps in this sequence.
`HORIZON_NATIVE_TIMES=1` shows the time and memory of each step.

| Step | What it does |
|---|---|
| Class ids | Gives each class an id in preorder of the superclass tree. Id 0 is not used. Lambdas and runtime classes come after. |
| Selectors | Gives each virtual method name and descriptor a number. |
| Layouts | Calculates the fields and the size of each class. |
| Dispatch table | Puts all virtual methods in one global table: entry `class id + offset of the selector`, two words `[method, selector + 1]`. First fit by runs of class ids. |
| Type tables | Ancestors of each class, for `instanceof`, casts and `catch`. |
| Method index | Who declares each method (one time, for all call sites). |
| Small bodies | Builds the methods with at most 80 bytes of bytecode. Those within 40 hot instructions stay, as callees to inline. |
| Streamed bodies | Builds every other method. A worker thread inlines and compiles each body at once. Then the body is freed. |
| Lambda bridges | One function for each lambda site. |
| Runtime tables | Class names, sizes, superclasses, interfaces, the dispatch table: the runtime library reads them (`rt::Rt`). |

A call that the compiler cannot make becomes a "stop". A stop prints where
and why, then traps. A method that the compiler cannot compile becomes a
stand-in that traps with the reason.

### 3.3 Calls

| Call | How it works |
|---|---|
| Static or special | A direct call. Static-versus-instance mismatches are stops (`IncompatibleClassChangeError` on the JVM). |
| Virtual, one implementation | A direct call. |
| Virtual, more implementations | Guarded calls to the two most likely classes, then the dispatch table. |
| Through a JDK interface (`List`, `Supplier`) | The dispatch table, with a check of the selector tag in the entry. |
| To the runtime library | A direct call to a Rust function (`rt::Builtin`). A program class with the same name wins over the runtime class. |

### 3.4 Modpack mode

Decision of 2026-10-31 ([compiler.md](compiler.md) 4.5). A modpack runs on
the vanilla of its own version, compiled natively with its loader and mods.
Horizon gives the launcher, the window, input, audio and the drawing. The
Mixins of the mods then apply to the code that they were written for.

Rendering of mods uses one render-intent IR ([rendering.md](rendering.md)).

## 4. Tools

| Tool | Use |
|---|---|
| `cargo test --release -p horizon-java --features native --test native` | The native tests (18). Each test compares with the JVM. |
| `jar_bench --native-check <jars>` | Compiles the jars and lists what does not compile and the stops. |
| `jar_bench --run-main <jars> -- <Class> [args]` | Compiles the jars and runs `main`. |
| `jar_bench --run-native <jars> -- <Class> <n>` | Runs a benchmark class (`make`, then `run` five times). |
| `jar_bench --jdk <dir>` | Adds the JDK classes the program needs, from an extracted Java runtime (level 3). |
| `jar_bench --only <prefixes>` | Keeps only some classes and their supertypes. |
| `examples/alloc_census.rs`, `lib_census.rs`, `api_diff.rs` | Allocation, library use and 1.21.1-to-26.3 differences of a pack. |
| `python3 tools/neoforge_oracle.py` | Exports the facts of mods from a real NeoForge server. |

Environment variables of the native backend:

| Variable | Effect |
|---|---|
| `HORIZON_NATIVE_TIMES=1` | Time and resident memory of each compile step |
| `HORIZON_NATIVE_TRACE=1` | Each method as it is compiled, and the dispatch table size |
| `HORIZON_NATIVE_CLIF=<text>` | The Cranelift code of each method whose name has the text |
| `HORIZON_NATIVE_GUARDS=1` | The classes of each guarded call |
| `HORIZON_NATIVE_PERFMAP=1` | `/tmp/perf-<pid>.map` for `perf` |
| `HORIZON_NATIVE_MISSING=<text>` | Classes with the text that have no method for a selector |
| `HORIZON_BENCH_STOPS=<text>` | Each stop place or reason with the text |
| `HORIZON_BENCH_STOP_KEYS=<file>` | Each stop reason and its count |
| `HORIZON_BENCH_UNCOMPILED=<file>` | Each method that did not compile, and why |
| `HORIZON_RUN_SECONDS=<n>` | How long `--run-main` waits for the program's threads (120) |

## 5. Procedures

### 5.1 Make the coverage check of a modpack

CAUTION: The check uses up to 7 GB of memory. Close large programs first.

1. Extract the mod jars and their nested jars (`META-INF/jarjar`) into one folder.
2. Extract the nested jars of NeoForge's universal jar (MixinExtras, coremods).
3. Extract `java.base`: `jimage extract --dir <work>/jdk-classes --include 'regex:/java.base/.*' $JAVA_HOME/lib/modules`.
4. Run `ulimit -c 0` in the shell.
5. Run `jar_bench --jdk <work>/jdk-classes --native-check <mods> <NeoForge universal> <NeoForge client> <Minecraft srg jar> <libraries>`.
6. Give the jars in this sequence. The first copy of a class wins, so NeoForge's patched classes must come before Minecraft's.
7. Read the summary. Set `HORIZON_BENCH_STOP_KEYS` for the full list.

Use NeoForm's `client-<version>-srg.jar` from Prism's libraries. The plain
`minecraft-1.21.1-client.jar` is obfuscated: no mod finds its classes there.

### 5.2 Fill a gap in the runtime library

1. Find the member in the stop list (for example `call to java/util/Objects.hash`).
2. Read the JDK's public API documentation of the member.
3. Write the Rust function in the related file of `rt/` (`extern "C"`, the receiver first).
4. Add the function to the class's `Builtin` method list.
5. Add a check to a test method in `tests/java/t/*.java`.
6. Compile and run that Java method on the real JVM (`javac`, `java`). Make sure that all checks pass there.
7. Run the native tests.
8. Commit with the count of checks in the message.

Prefer level 3 (the player's JDK classes, `--jdk`) for large classes. Write a
Rust version only for classes that most mods use, or that level 3 cannot
compile.

### 5.3 Run vanilla's server natively

1. Make a new empty folder. Go into it.
2. Run `jar_bench --jdk <work>/jdk-classes --run-main <Minecraft srg jar> <libraries> -- net/minecraft/server/Main --nogui`.
3. Do not give the logging jars (slf4j, log4j, Mojang's `logging`). The runtime library has its own logging.
4. Read the last line. It names the method and the call that stopped.
5. Fill that gap (5.2). Then do the run again.

### 5.4 Port a content mod (the data tier)

1. Put the jar in the `mods` folder of the Prism instance.
2. Start the instance from Prism. The wrapper imports and ports the mods.
3. Read `coverage.md` in the port folder of the pack.
4. Run the oracle and the harness on the mod (see [docs/modpacks.md](../modpacks.md), step 6).
5. Write the result in the coverage list.

## 6. Pitfalls

| Problem | Cause | What to do |
|---|---|---|
| A crash report popup, a stuck run | A stop traps with SIGILL. A core dump of the 64 GiB heap starts. | Run `ulimit -c 0` before each run. |
| The session reaper stops a run | Low memory on the computer. | Run under `systemd-run --user --scope -p MemoryMax=14G`. Close large programs. Do not start the run again without the owner's approval. |
| A rebuild breaks a running check | Cargo writes the binary that the check uses. | Copy `jar_bench` to another file before a long run. |
| All of Minecraft is "outside the program" | The obfuscated Minecraft jar. | Use NeoForm's `-srg.jar` (5.1). |
| Cranelift panics in a release build | The verifier is off. Bad code (for example a wrong argument count) reaches the backend. | The method becomes a stand-in. `HORIZON_BENCH_UNCOMPILED` names it and gives the verifier's reason. |
| A runtime test fails on the JVM too | The expected value in the Java test is wrong. | Always run the Java test on the JVM first (5.2, step 6). |
| `pkill` kills the shell | The pattern is in the command line itself. | Use a pattern like `[j]ar_bench`. |

## 7. What is missing

| # | Work | Why it is needed | Where to start |
|---|---|---|---|
| 1 | JDK gaps of the server start (`BigInteger`, files, regular expressions) | Vanilla 1.21.1's server must start natively. | Procedure 5.3 |
| 2 | Collector | A run uses all of its heap. | [compiler.md](compiler.md) 4.3: escape analysis, value types, tick regions |
| 3 | Mixins at install | Sable changes 131 vanilla classes, Create 54. | [compiler.md](compiler.md) 4.1: compile the real Mixin library |
| 4 | NeoForge compiled from its jars | Events, registries, capabilities, networking. | [compiler.md](compiler.md) 4.1 |
| 5 | The game loop with compiled code | Ticks, the world, the client and server parts. | Modpack mode (3.4) |
| 6 | Render recorder and de-animation | A Create cogwheel must turn in the game. | [rendering.md](rendering.md), milestones 1 to 3 |
| 7 | Sub-levels and contraptions | Sable ships and Create contraptions. | [rendering.md](rendering.md) 3.2 |
| 8 | Shader pipeline and Veil | The mods' effects. | [rendering.md](rendering.md) 3.3 |
| 9 | Speed | A virtual call has one load more since the dispatch table. | Point the object header at the class's table row. |

## 8. Terms

| Term | Meaning |
|---|---|
| Stop | A call that the compiler could not make. It prints where and why, then traps. |
| Stand-in | A method that did not compile. It traps with the reason. |
| Selector | The number of a virtual method name and descriptor. |
| Runtime library | The Rust versions of JDK classes (`rt`). |
| Level 3 | JDK classes compiled from the player's own Java runtime. |
| Modpack mode | A pack runs on the vanilla of its own version, compiled natively. |
| Oracle | NeoForge 1.21.1 with the same mods, used to compare results. |
| Census | The classes, members and Mixins that a mod uses, read from its class files. |
| Coverage check | A native compile of a full pack that lists what does not compile and the stops. |
