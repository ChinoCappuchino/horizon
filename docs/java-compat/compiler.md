# Java mod compatibility: the compiler

Writing standard: ASD-STE100 rules (see [README.md](README.md#writing-rules-for-these-documents)).

## 1. Objective

The compiler changes Java bytecode into code that runs as fast as the JVM runs it.
It compiles vanilla Minecraft (from the player's own jar) and the mods. It runs
when the player installs the game or a mod. It does not run while the player plays.

Decision 2026-10-29 (owner): performance first. Mods run without a sandbox, as
they do on the JVM. The main backend makes native code. The WASM backend stays
for bring-up and to cross-check the native backend.

## 2. Measurements that set the work

The data is in `council/VANILLA-SPIKE.md` (not in the repository). Summary,
9 million iterations, JVM against WASM:

| Operation | JVM | WASM now | Cause |
|---|---:|---:|---|
| Static call | 12 ms | 28 ms | Class initialization test on each call |
| Field and array read | 12 ms | ~100 ms | Each branch is an indirect jump (`br_table`) |
| Virtual call | 20 ms | 254 ms | The class id read needs a runtime type test |
| New object | 12 ms | ~300 ms | No escape analysis; GC allocation |

Arithmetic without objects runs at the speed of the JVM (25.3 ms against 25.0 ms).
Results are equal to the JVM, bit for bit.

## 3. Structure

```text
class files ─► front end ─► typed IR ─► passes ─► WASM backend   (wasmtime)
                                              └─► native backend (Cranelift)
```

### 3.1 Front end

1. Read the class files (`classfile.rs`, `bytecode.rs`).
2. Make the control-flow graph (CFG): basic blocks, edges, exception edges.
3. Find the type of each local and stack slot before each instruction, as the
   classic JVM verifier does (type inference, join at merge points by the class
   hierarchy; two interfaces join to `java/lang/Object`). This works for all
   class file versions. Where a `StackMapTable` is present, use it to check the
   result.
4. Find the loops (dominator tree, natural loops).

### 3.2 Typed IR

Each value has a type: `int`, `long`, `float`, `double`, or a reference to a
class, an interface, an array, or `null`. Each block has its predecessors and
its successors. The IR keeps the bytecode position of each instruction, for
traps and profiles.

### 3.3 Passes (the same for the two backends)

All classes are known at install time (a closed world). The passes use this.

| Pass | Work | Removes |
|---|---|---|
| Class hierarchy analysis | Find the methods that have one implementation. Call them directly. | Dispatch |
| Initialization tests | Remove the test when the class has no static initializer, or when a dominating test is present. | Class initialization tests |
| Escape analysis | An object that does not leave the method becomes local values. | Allocation |
| Structured control flow | Make nested blocks and loops from the CFG (javac makes reducible CFGs). | Indirect jumps (WASM only; Cranelift takes a CFG) |

### 3.4 Backends

WASM backend: from the typed IR. References use the declared type, so a field
access needs no type test. All objects share one root struct type, so a class id
read needs no type test.

Native backend: from the typed IR to Cranelift IR. It has its own object model:
an object header with a class pointer, vtables, interface tables, allocation by
a bump pointer, and a garbage collector that uses Cranelift stack maps. It calls
Horizon's Rust code directly.

## 4. Mixins and native replacements (at install)

### 4.1 Mixins

Mixins change the bytecode of a class when it loads. In Horizon they change
it one time, at install:

1. Load the vanilla classes and the classes of the mods.
2. Apply the Mixins of the mods to the bytecode. Use the real Mixin library
   (MIT license) and its ASM library (BSD license), compiled by this compiler.
   Do not write a second Mixin implementation.
3. Compile the changed classes into the cache.

When the game runs, there is no Mixin cost: the hooks are part of the
compiled code.

NeoForge is open source (LGPL) and is mostly changes to vanilla classes. The
same steps can compile NeoForge from its jars. Then the mods use the real
NeoForge, and `horizon-neoforge` becomes smaller.

### 4.2 Native replacements

The compiler can replace a vanilla method with Horizon's Rust code, as it
replaces JDK methods with intrinsics (`Math.floor` and others). It does this
for a method only when the two conditions are true:

1. Horizon has an implementation that a differential test shows is equal to
   the compiled vanilla method.
2. No Mixin and no other mod change touches the method.

In all other conditions the compiled vanilla method runs, with the hooks of
the mods. All classes are known at install, so the decision is exact and has
no cost at run time.

Decision 2026-10-29 (owner): a Mixin on a method that Horizon replaces must
not always remove the Rust code. Each Rust replacement declares the vanilla
methods and call sites that it stands for (a correspondence map, made from
the bytecode by tools). When a Mixin injects at a point of the map (the start
or the end of a method, before or after a call), the Rust code calls the
compiled Mixin handler at that point, with the same cancel and return value
rules as Mixin. Only a Mixin change that the map cannot express (a change to
a local variable, an expression that is replaced, an `@Overwrite` of a method
with no hook) makes that one method use the compiled vanilla code. The install
report lists these methods. The list shows which hook points to add next.

### 4.3 Memory

Decision 2026-10-29 (owner): memory is managed by Horizon, not as in the JVM.
Remove the allocations that cost most, then collect the remainder only between
ticks.

| Part | Work |
|---|---|
| Escape analysis | An object that does not leave its method becomes local values. No allocation. This is correct for all classes. |
| Value types | An immutable class (for example `Vec3`, `BlockPos`, `AABB`) becomes a Rust value type when no code of the installed game uses its identity (`==` on two references, `synchronized`, identity hash). Vanilla uses identity on these classes only in their own `equals` (the `this == other` test), which is safe. The installed mods decide: in the Afterwinter pack, Create, Immersive Engineering, AE2 and other mods use `==` on `BlockPos` and `Vec3` (about 50 places), so these stay objects there. Tool: `examples/alloc_census.rs`. |
| Tick regions | Objects made in a tick go into a region. At the end of the tick, the objects that are still referenced move out; the region is then empty again. There is no collection during a tick. |
| Rust world state | Chunks, block states, entities and inventories stay in Horizon's Rust data. Java code gets handles to them. |
| Last tracer | A tracer for the long-lived objects of mods (their maps and caches), between ticks. |

`ItemStack` is not a value type: vanilla uses its identity in 31 places.

### 4.4 Library methods

Decision 2026-10-29 (owner): the library methods that almost all mods use
get Rust versions, to remove calls and objects. Data from the Afterwinter pack
(113 mods; tool `examples/lib_census.rs`): 1,091 library classes and 6,635
library methods, but a small set has most of the uses.

| Level | What | Uses in the pack |
|---|---|---|
| 1. Resolved at install | `invokedynamic`: lambdas (`LambdaMetafactory`), string concatenation (`StringConcatFactory`), record methods (`ObjectMethods`). Each call site becomes direct code. `java.lang.invoke` is not used at run time. | Lambdas: 109 mods, 11,855 sites. Concatenation: 103 mods, 4,790 sites. Records: 79 mods, 2,831 sites. |
| 2. Rust versions | `String`, boxing (`Integer.valueOf`, `intValue`, `Boolean`), `ArrayList`, `HashMap`, `HashSet`, iterators, `Optional`, `Objects`, `Math`. They make fewer objects (no node object per map entry, no iterator object after inlining). | 81 to 106 mods each |
| 3. Compiled library | All other library methods: the real classes, compiled from the player's own Java runtime. | the remainder |

Rules for level 2:

1. The behavior is the same as Java's, also where code can see it: the
   iteration order of `HashMap` (its hash and its resize policy), the
   exceptions, the identity of cached boxes (`Integer` from -128 to 127).
2. A differential test against the real compiled class must pass before a
   Rust version is used.
3. The Rust versions are classes of the object model, so that a mod class can
   extend them and override their methods.
4. Clean room: write the Rust versions from the public API documentation and
   from observed behavior. Compile the JDK classes only on the player's
   computer, from the player's Java runtime. Do not distribute them.

Level 3 in practice (2026-10-31). The Create and Sable coverage check
(Create, Sable, Aeronautics, Flywheel, Veil, NeoForge and Minecraft, 47,296
classes) leaves about 2,600 JDK members that the Rust versions do not have:
`TreeMap`, `LinkedHashMap`, `ConcurrentHashMap`, regular expressions, `File`
and NIO, and others. They come from the player's Java runtime (its
`lib/modules` image, read with the runtime's own `jimage`) at install. Which
JDK class is compiled is decided by the tool, not by hand:

1. Start from the JDK classes that the program uses and that have no Rust
   version, and add the JDK classes they need, transitively. A class with a
   Rust version ends the walk.
2. A JDK class is compiled only when it uses a Rust class through its public
   or protected API. A JDK class that extends a Rust class and uses its
   package-private members (`LinkedHashMap` uses `HashMap`'s nodes and hooks)
   is not compiled: the class and the classes that need it stay calls that
   stop, and the install report lists them. The family then gets a Rust
   version (level 2), or loses its Rust version and is compiled whole.
3. The JDK's native methods and `jdk.internal.misc.Unsafe` become runtime
   intrinsics on Horizon's object layout (a field offset is known at
   install). A native method without one is a stop, in the report.

The report of rule 2 and rule 3 lists, by count of uses, what to write in
Rust next.

### 4.5 Mods of other game versions (the version bridge)

Decision 2026-10-29 (owner): mods of older game versions run on the new
game through a bridge, as ViaVersion bridges protocol versions. The bridge
is a chain of steps, one per game version, and each step is shared by all
mods. There is no per-mod adaptation.

| Change between versions | What the bridge does |
|---|---|
| Unchanged member | Nothing. |
| Class renamed or moved (`ResourceLocation` to `Identifier`, `net/minecraft/Util` to `net/minecraft/util/Util`) | The install remaps the mod's references. Found by a tool, no manual work. |
| Member renamed | The same, after a check. |
| Signature changed (`CompoundTag.getInt` now gives `Optional`) | A generated or small written adapter method. |
| Class or member removed, or an API that changed its design (`ItemInteractionResult` merged into `InteractionResult`, GUI rendering) | A translation unit, written one time for each API change. |

Mixin targets go through the same chain. A Mixin whose injection point is in
a method that changed falls back as in 4.2.

Data from the Afterwinter pack (113 mods, 1.21.1 to 26.3; tool
`examples/api_diff.rs`), as a share of the mods' references to the game:

| Category | Share |
|---|---:|
| Unchanged | 75.9% |
| Inherited from a library (unchanged) | 2.3% |
| Class renamed or moved (automatic) | 4.1% |
| Member renamed (candidates) | 2.9% |
| Signature changed | 5.0% |
| Removed | 4.5% |
| Class removed | 4.2% |

The 13.7% that need adapters or translation units is concentrated: 23
classes have half of it, 123 classes have 80%, 244 have 90%. The largest are
`CompoundTag`, `GuiGraphics`, `RenderSystem`, `Minecraft`, `InteractionResult`,
`RenderType`, `Level`, `ItemInteractionResult`, `ItemStack`. Client rendering
changed most.

Each step of the chain is tested against the vanilla of both versions,
compiled locally (differential tests of each translation unit).

Loaders and their versions are settings of the install (a file like
`server.properties`); the default order is NeoForge 1.21.1, Fabric 26.x,
NeoForge 26.x, Fabric 1.21.1, Quilt, Forge 1.20.1, then older.

Target versions (decision 2026-10-29, owner):

1. Now: 1.21.1 (the proof of concept, with the Afterwinter pack).
2. After the proof of concept is merged with upstream and accepted:
   1.12.2 and 1.8.
3. Then the versions between them.

Modpack mode (decision 2026-10-31, owner). A modpack runs on the vanilla of
its own game version, compiled natively with its loader and mods, before the
bridge to the newest version exists: the mods' Mixins then apply to the code
they were written for (Sable changes 131 vanilla classes, Create 54). Horizon
gives the platform: the launcher, the window, input, audio and, through the
render-intent IR (rendering.md), the drawing. Memory follows 4.3: values and
Rust data first, a collector for what remains. Native replacements (4.2) and
a later conversion to the newest version are open; a 1.21.1 Horizon with the
pack running is a goal of its own. First target: vanilla 1.21.1's dedicated
server started natively, then with NeoForge, Create and Sable.

## 5. Steps

Do the steps in this sequence. Each step keeps all tests correct (Farmer's
Delight and Chef's Delight equal to NeoForge, the apple pie test, the vanilla
`SimplexNoise` sum).

| Step | Work | Result that shows it is complete |
|---|---|---|
| 1 | Front end and typed IR (CFG, types, loops) | The types of all methods of the pack and of vanilla agree with their `StackMapTable` |
| 2 | WASM backend from the typed IR, with structured control flow | Micro benchmarks: field reads near the JVM |
| 3 | Typed references and one root type | Virtual calls near the JVM |
| 4 | Class hierarchy analysis and initialization tests | Static calls equal to the JVM |
| 5 | Escape analysis | Allocation near the JVM |
| 6 | Native backend: code, object model | Micro benchmarks run natively |
| 7 | Native backend: garbage collector, exceptions | Vanilla `SimplexNoise` and the mod tests run natively |
| 8 | Install-time cache of compiled code, keyed by the jar SHA-256 | The second start does not compile |

## 6. Tools

- `examples/compile_jar.rs`: compile a jar and list what it imports.
- `examples/jar_bench.rs`: run a benchmark class against the JVM numbers.
- `HORIZON_JAVA_PERFMAP=1`: a perf map for profilers.
- `HORIZON_JAVA_COLLECTOR=drc`, `HORIZON_JAVA_INLINE=0`: compare the old settings.
