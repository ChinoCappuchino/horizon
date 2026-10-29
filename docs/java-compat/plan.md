# Java mod compatibility: plan

Writing standard: ASD-STE100 rules (see [README.md](README.md#writing-rules-for-these-documents)).

## 1. Phases

| Phase | Work | Result that shows it is complete |
|---|---|---|
| 0 | Proof of concept with one small mod | Chef's Delight runs in Horizon from its original jar |
| 1 | Registration: blocks, items, block entities, menus, recipes from code | The "Delight" mods of the pack run |
| 2 | Events and network messages | Mods that react to game events run (for example Cold Sweat) |
| 3 | Mixins and the hook table | The Mixins of the content mods are applied or skipped by the table |
| 4 | Client code: renderers and screens | Custom block renderers and machine screens show |
| 5 | Large mods | Create, Mekanism, Applied Energistics 2 run |
| 6 | Older profiles: Forge 1.20.1, 1.16.5, 1.12.2, 1.7.10 | A classic mod of each profile runs |

Phase 6 can start early for the compiler and the name mappings. The compiler is
the same for all profiles.

Do the phases in this sequence. A phase can start before the last phase is
complete, if the work does not depend on it.

## 2. Phase 0: proof of concept

### 2.1 Objective

Show that each part of the system works on one real mod:

1. The compiler changes the jar into WASM.
2. The compatibility layer gives the classes that the mod uses.
3. The mod registers its content in Horizon.
4. The cache keeps the result, and the second start is fast.

### 2.2 The mod

Chef's Delight 1.0.6 (from the Afterwinter pack). Data from the census:

| Item | Quantity |
|---|---:|
| Classes | 7 |
| Minecraft and NeoForge members that it uses | 51 |
| Mixin classes | 0 |

Second mod, if Phase 0 is fast: Ocean's Delight (15 classes, 130 members, no
Mixins).

### 2.3 Steps

1. Write the class-file reader of the Rust compiler. Test it on all 44,751
   classes of the Afterwinter pack. (Decision 2026-10-29: the compiler is Rust,
   not TeaVM. See [README.md section 2.6](README.md#26-why-the-compiler-is-written-in-rust).)
2. Write the compatibility classes for the 51 members.
3. Compile the jar.
4. Load the result as a Horizon WASM mod.
5. Compare with the mod on NeoForge (the oracle). Compare the registered
   content and the trades.
6. Measure the compile time and the start time with the cache.
7. Record the results in this document.

### 2.4 Results

Step 1 (2026-10-29): the class-file reader and the bytecode decoder
(`crates/horizon-java`) read all classes of the Afterwinter pack:

| Item | Quantity |
|---|---:|
| Jars | 113 |
| Classes | 44,751 (0 errors) |
| Methods with code | 337,977 (0 errors) |
| Instructions | 7,307,157 |
| Class file versions | 49 to 65 (Java 5 to Java 21) |

Steps 2 to 7: not done yet.

## 3. Progress on the startup splash

The first start after a mod is installed or updated can be slow, because the
compiler compiles it. The startup splash ([../startup-splash.md](../startup-splash.md))
shows this work:

1. Each mod that the compiler completes is one drop.
2. The stage line shows the step and the mod, for example
   "Compiling Create · 12 / 75".
3. A mod that comes from the cache is also one drop. It flies in fast.

## 4. The order of mods for the Afterwinter pack

Use the census to select the next mod. Select the mod that needs the smallest
quantity of new members. This gives the most working mods for the least work.
Run the census again after each phase.

## 5. Risks

| Risk | Effect | What to do |
|---|---|---|
| A mod uses Java reflection | The compiler cannot find the target in advance | Add the reflection target to the mod's entry in the compatibility list |
| A mod uses threads | WASM in Horizon has one thread for each instance | Run the work on the calling thread. Record it in the list |
| A Mixin changes rendering | Horizon renders in a different way | Mark the target `skip` in the hook table, or write a client hook (Phase 4) |
| A licence does not let you share a compiled mod | Players cannot download the cache of that mod | Compile on each computer from the original jar |
