# Java mod compatibility: procedure for an AI agent

This document is for an AI agent (or a person) that adds support for a NeoForge
mod. Read all of it before you start.

Writing standard: ASD-STE100 rules (see [README.md](README.md#writing-rules-for-these-documents)).
Write all new documentation with the same rules.

## 1. Rules

WARNING: Do not read, copy or decompile the code of a mod that you do not own.
The compiler processes the bytecode. You only add compatibility classes and
hook-table lines. This is the clean-room rule of this fork.

1. Do not change a mod jar.
2. Do not put compiled mods of "All Rights Reserved" mods in a public place.
3. Compare each result with the same mod on NeoForge 1.21.1 (the oracle).
4. Add a test for each compatibility class that you write.
5. Record each change in the compatibility list.

## 2. What you can read

| Source | Can you read it? |
|---|---|
| Minecraft 26.3 decompiled sources (`.local/minecraft/26.3/sources`) | Yes |
| NeoForge public API (Javadoc, the API jar signatures) | Yes |
| The census of a mod (`census.py`: classes, members, Mixin targets) | Yes |
| The behaviour of a mod on NeoForge (the oracle) | Yes |
| The code of a mod (source or decompiled) | No |

## 3. Procedure: add support for a mod

1. Run the census on the mod jar.
2. Get the list of members that the mod uses.
3. Find the members that the compatibility layer does not have.
4. Write the compatibility class for each missing member.
5. Make each method call the related Horizon function.
6. Add a test for each new method.
7. Compile the mod.
8. Read the compiler log for Mixin targets that have no line in the hook table.
9. Add a line for each target to the hook table (see section 4).
10. Run the mod in Horizon.
11. Run the same mod on NeoForge with the oracle.
12. Compare the two results (see section 5).
13. Write the result in the compatibility list (see section 6).

## 4. Procedure: add a line to the hook table

1. Find the Minecraft method that the Mixin changes.
2. Find what the Mixin does from its behaviour on NeoForge.
3. Select the action:
   - If the Mixin makes Java faster, use `skip`.
   - If the Mixin repairs a problem that Horizon does not have, use `skip`.
   - If the Mixin changes gameplay, use `hook <name>`.
4. If you use `hook <name>`, find the related point in the Horizon code.
5. Add the hook point to Horizon, if it is not there.
6. Write the line in the table with a short reason.

Example lines:

```
net/minecraft/world/entity/LivingEntity.hurt(...)  hook living_hurt   # damage events
net/minecraft/client/renderer/LevelRenderer.renderLevel(...)  skip   # renderer speed fix
```

## 5. Comparison with NeoForge

Compare these items. All must be the same, or the difference must be in the
list of permitted differences:

| Item | How to compare |
|---|---|
| Registered blocks, items and entities | The ids and the default properties |
| Recipes and loot | The output of each recipe and loot table |
| Behaviour | A scripted test in the oracle and the same test in Horizon |
| Saved data | The NBT of a saved block entity or entity |

## 6. The compatibility list

Each mod has one entry. Use this format:

| Field | Content |
|---|---|
| Mod | Name, version, jar SHA-256 |
| Status | `works`, `partial` or `does not start` |
| Missing | The members or hooks that are not done |
| Skipped Mixins | The targets with `skip`, and why |
| Tested | The date and the tests that you did |

## 7. Terms

| Term | Meaning |
|---|---|
| Compatibility layer | The Java-shaped Minecraft and NeoForge classes that call Horizon |
| Member | A method or a field of a class |
| Mixin | Code in a mod that changes the code of Minecraft or of other mods |
| Hook table | The data that tells the compiler what to do with each Mixin into Minecraft |
| Oracle | NeoForge 1.21.1 with the same mods, used to compare results |
| Census | The list of classes, members and Mixins that a mod uses, read from its class files |
