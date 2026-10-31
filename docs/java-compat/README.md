# Java mod compatibility: overview

Status: design and proof of concept, fork `NeuHorizon`, 2026-10-29.
Writing standard: ASD-STE100 (Simplified Technical English) rules. See
[Writing rules](#writing-rules-for-these-documents).

The current status (what works, the numbers, the next steps) is in
[status.md](status.md). Rendering of mods is in [rendering.md](rendering.md).

## 1. Purpose

This system lets Horizon use mods that people wrote for NeoForge 1.21.1. You do
not port the mods by hand. The system compiles each original mod file (`.jar`)
into a Horizon WASM mod. The compiled mod uses a compatibility layer. The
compatibility layer gives the mod the Minecraft and NeoForge classes that it
expects.

## 2. The parts

The system has four parts:

| Part | Function |
|---|---|
| Compatibility layer | Gives Java-shaped Minecraft and NeoForge classes. Each class calls Horizon code in Rust. |
| Compiler | Changes the bytecode of a mod into WASM. It links the mod to the compatibility layer. |
| Cache | Keeps each compiled mod. The key is the hash of the original jar. |
| Hook table | Tells the compiler what to do with each Mixin. Each line is a Horizon hook or "skip". |

### 2.1 Compatibility layer

A mod calls Minecraft classes, for example `Level`, `BlockEntity` and `Item`.
Horizon does not have these Java classes. Horizon has the same functions in Rust.
The compatibility layer has a Java-shaped class for each class that mods use.
Each method of the class calls the related Horizon function.

You write the layer one time. All mods use it. When you add a method, all mods
that use the method get it.

### 2.2 Compiler

The compiler reads the original jar. It does not change the mod files on the
disk. It compiles the bytecode to WASM. It does not need the source code of the
mod. No person or AI reads the code of the mod.

The compiler also applies Mixins. A Mixin is code that changes other code:

- A Mixin into another mod: the compiler applies it, because both mods are
  bytecode that the compiler has.
- A Mixin into Minecraft: the compiler looks in the hook table.

### 2.3 Cache

The first compile of a mod can be slow. The cache keeps the result. The next
start uses the cache and is fast.

The cache key is the SHA-256 hash of the original jar. The game uses a cache
entry only when the original jar with the same hash is present. Thus a player
must have the original mods. Players get the original mods from the same place
as before (CurseForge, Modrinth).

CAUTION: A compiled mod is derived from the code of the mod. Do not put compiled
mods of "All Rights Reserved" mods in a public download. Each computer can
compile from its own jars. You can share the cache of mods that have a
permissive or copyleft licence.

### 2.4 Hook table

The hook table is data. Each line has a Mixin target (a Minecraft method) and an
action:

| Action | Meaning |
|---|---|
| `hook <name>` | Horizon calls the Mixin at the point `<name>` in its own code. |
| `skip` | The Mixin is not necessary in Horizon (for example, a performance fix for Java). |
| (no line) | The compiler writes a log entry and skips the Mixin. The mod runs without that change. |

The hook table is common to all mod packs. When one person adds a line, all mods
that use that target get it.

### 2.5 Versions of Minecraft and loaders

The system is not only for NeoForge 1.21.1. It must also run old mods, back to
Minecraft 1.12.2 and earlier. This gives the classic mods a second life in a new
engine.

The parts that change with the version:

| Part | Same for all versions? | Why |
|---|---|---|
| Compiler | Yes | JVM bytecode has the same format from Java 6 to Java 21. |
| Compatibility layer | No | Each loader version has a different API. The layer has one profile for each version. |
| Names | No | Before Minecraft 1.14, mods use obfuscated names (for example `func_71410_x`). A mapping table gives the related profile member. |
| Block data | No | Before Minecraft 1.13, blocks have numeric ids and metadata. The "Flattening" tables of Minecraft change them into block states. |

All profiles use the same Horizon world. The world is always Minecraft 26.3 in
Horizon.

#### 2.5.1 Profile data: get it when a mod needs it

A profile needs data: name mappings, the Flattening tables and hook-table lines.
Horizon does not keep all of it on the disk. These are the rules:

1. Horizon has only the data of the most common profiles, in a compact form.
2. When a mod needs other data, Horizon gets it from a Git repository or from
   its original source.
3. Horizon gets only the tables that the installed mods need.
4. Horizon verifies each table with its hash before it uses it.
5. Horizon keeps each table in its cache. The next start does not download it.

CAUTION: Some mapping tables (for example Forge's SRG and MCP names, and
Mojang's official mappings) have terms that do not let you redistribute them
complete. Get these tables from their original source on the computer that
uses them. Do not put them in a Horizon download.

The sequence of profiles:

| Profile | Reason |
|---|---|
| NeoForge 1.21.1 | The Afterwinter pack |
| Forge 1.20.1 | The largest modern set of mods |
| Forge 1.16.5 | Many large mod packs |
| Forge 1.12.2 | The classic mods |
| Forge 1.7.10 | The oldest large set of mods (for example GregTech: New Horizons) |

Fabric can be an additional profile at a later time.

### 2.6 Why the compiler is written in Rust

The compiler is a part of Horizon. It is written in Rust. These are the reasons:

1. Players do not need Java. Each computer can compile mods without a JVM.
2. The compiler compiles each jar separately. Thus the cache has one entry for
   each jar.
3. The compiler controls reflection, annotations and dynamic loading.
4. The compiler applies Mixins in the same step.
5. The compatibility layer is Rust. It calls Horizon directly.
6. Wasmtime changes the WASM into native code.

This is not a copy of Forge or NeoForge. Java and Rust do not work in the same
way. The compiler makes native WASM, with glue that is specific to Minecraft.

## 3. Comparison to Proton (Valve)

The design follows the method that Valve uses for Windows games on Linux:

| Proton | This system |
|---|---|
| Wine implements the Windows API one time. | The compatibility layer implements the NeoForge API one time. |
| Steam shares compiled shader caches. | The cache shares compiled mods, keyed to the original jar. |
| ProtonDB lists which games work. | The compatibility list shows which mods work, and the hook table grows with community input. |

## 4. What the census of the Afterwinter pack shows

Measured on 2026-10-29 from the constant pools of the class files (no code read):

| Item | Quantity |
|---|---:|
| Jars with code | 111 |
| Classes of mod code | 44,751 |
| Different Minecraft and NeoForge members (methods and fields) that mods use | 20,822 |
| Members that 10 or more mods use | 1,915 |
| Mixin classes | 2,166 (in 81 jars) |

The Mixin classes are in these jars:

| Jars | Mixin classes | Necessary in Horizon? |
|---|---:|---|
| Performance and rendering mods | about 560 | No |
| Libraries and tools | about 680 | Only the parts that other mods use |
| Content mods | about 675 | Yes: these are gameplay changes |
| Client mods and other | about 250 | Some |

About 410 Mixins change other mods, not Minecraft. The compiler applies them
without the hook table.

## 5. Related documents

- [plan.md](plan.md): the phases of the work.
- [AGENTS.md](AGENTS.md): the guide for the next developer (a person or an AI
  agent): how the system works, the tools, the procedures, the pitfalls and
  what is missing.
- [status.md](status.md): what works now, with the numbers.
- [upstream-proposal.md](upstream-proposal.md): a draft request to the Horizon
  project. Not sent.

## Writing rules for these documents

These documents use the writing rules of ASD-STE100 (Simplified Technical
English):

1. Write procedural sentences with not more than 20 words.
2. Write descriptive sentences with not more than 25 words.
3. Use the active voice.
4. Write one instruction in each sentence.
5. Use one word for one meaning. Use the same term each time.
6. Write a procedure as numbered steps. Start each step with a verb.
7. Put a warning or caution before the step that it is about.
8. Use a table for data that has more than two attributes.

## 6. How the compiler makes WASM

Decisions of 2026-10-29. The test `crates/horizon-java/tests/wasm_gc.rs` shows
that the wasmtime of Horizon (48) runs each part.

| Java | WASM | Note |
|---|---|---|
| An object | A GC struct. The first field is the class id. | A subclass is a subtype of its superclass struct. |
| An array | A GC array | One array type for each element type. |
| `throw` and `catch` | Exception handling (`throw`, `try_table`) | One tag carries a `java/lang/Throwable`. |
| A virtual call | A call through the class's table of methods | The compiler makes the table for each class. |
| A call to Minecraft or NeoForge | An import from the compatibility layer | The layer is Rust in Horizon. |
| The types of the stack at a branch | The `StackMapTable` of the method | Class files before Java 6 do not have it. The compiler calculates the types. |

Each jar is one WASM module. A module imports the classes of other jars that
it uses. Thus the cache keeps one module for each jar.
