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

## 4. Steps

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

## 5. Tools

- `examples/compile_jar.rs`: compile a jar and list what it imports.
- `examples/jar_bench.rs`: run a benchmark class against the JVM numbers.
- `HORIZON_JAVA_PERFMAP=1`: a perf map for profilers.
- `HORIZON_JAVA_COLLECTOR=drc`, `HORIZON_JAVA_INLINE=0`: compare the old settings.
