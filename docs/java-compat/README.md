# Java mod compatibility: overview

Status: design and proof of concept, fork `afterwinter`, 2026-10-29.
Writing standard: ASD-STE100 (Simplified Technical English) rules. See
[Writing rules](#writing-rules-for-these-documents).

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
- [AGENT.md](AGENT.md): the procedure for an AI agent (or a person) that adds
  support for a mod.
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
