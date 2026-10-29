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

Step 3 (2026-10-29): the compiler (`crates/horizon-java/src/compile.rs`)
compiles all of the jar. It does not need a compatibility class to compile:
each Minecraft, NeoForge or JDK member becomes a WASM import.

| Item | Quantity |
|---|---:|
| Methods compiled | 79 of 79 |
| Lambdas | 64 |
| Imports (the members the layer must give) | 103 |
| WASM module | 41 KB |
| Compile time | 22 ms |

Steps 2 and 5, first part (2026-10-29): a recording layer
(`crates/horizon-java/examples/record.rs`) gives the 103 imports. It does the
steps of the NeoForge loader in sequence: the `@Mod` constructor, the
registrations, the config event, the setup event, the villager trades event
for each profession, and the server-start event. The NeoForge probe
(`tools/worldgen_probe.py run chefsdelight --content chefsdelight`) dumps the
same data from the original jar on NeoForge 21.1.251.
`tools/java_compat/neoforge_compare.py` compares the two results:

| Data | Quantity | Result |
|---|---:|---|
| POI types (blocks, tickets, range) | 2 | Equal |
| Villager professions (name, job site, sound) | 2 | Equal |
| Trade offers (items, counts, uses, XP, price multiplier) | 52 | Equal |
| Village house pools (elements and weights) | 5 | Equal |

The recording layer does not model one call: `LogUtils.getLogger`.

To do these steps again:

```sh
cargo run -p horizon-java --example record -- <pack>/mods/chefsdelight-1.0.6.jar record.json
HORIZON_NEOFORGE=21.1.251 HORIZON_PACK_MODS=<pack>/mods \
  tools/worldgen_probe.py run chefsdelight --radius 0 --content chefsdelight --accept-eula
tools/java_compat/neoforge_compare.py record.json \
  .local/worldgen-probe/chefsdelight/seed-1-order-0.content.json
```

Not done yet:

- Step 2, second part: give the imports from Horizon's game objects, not from
  a recording layer.
- Step 4: load the compiled mod into Horizon. The professions, the trades and
  the houses must then show in the game.
- Step 6: the cache and the start time.

Compiler, all of the pack (2026-10-29): the compiler compiles every method
of the Afterwinter pack. The object model is complete: classes that extend a
game class (through a peer object), virtual and interface calls (one
dispatcher for each name and descriptor), `instanceof`, arrays, exceptions,
records, enums, string concatenation and pattern `switch`.
`crates/horizon-java/examples/compile_pack.rs` measures it:

| Item | Before | Now |
|---|---:|---:|
| Methods compiled | 74.2 % | 100 % (337,977) |
| Jars compiled whole | 4 | 113 of 113 |
| Farmer's Delight 1.3.4 | 62.4 % | 100 % (2,319 methods, 0.5 s) |
| Time for the pack | | 53 s |

Registration, Farmer's Delight (2026-10-29): `crates/horizon-neoforge` is the
first part of the compatibility layer. It runs a NeoForge mod's constructor
and its registers from the original jar, as NeoForge does. It asks each
block for its states, shapes and light. A block that extends a game class
(a crop, a fence gate, a sign) takes the shapes and states of that class
from the game's data. It writes what the mod registered as a Horizon content
mod (docs/mods.md). The port pack's harness compares the result with the
original jar on NeoForge 21.1.251:

| Check | Result |
|---|---:|
| Blocks | 132 of 132 |
| Block states (properties, sounds, shapes) | 2,548 of 2,548 |
| Items | 185 of 185 |
| Drops | 11,692 of 11,795 |
| Recipes | 93 of 304 (the mod's own recipe types need its code) |

To do it again: `cargo run --release -p horizon-neoforge --example translate
-- <jar> .local/horizon-client/26.3 <work>/mods/farmersdelight/carry`, then
`HORIZON_CLIENT_FEATURES="--no-default-features --features mods"
tools/mod_port_harness.py --work <work> --jars <jar folder> farmersdelight`.

In the game, Farmer's Delight and Chef's Delight (2026-10-29): several jars
now run together (a mod and the mods it needs), and the layer fires the
events that make content as the game loads (config, setup, villager
trades, server start). `horizon-port-pack translate WORK --resources
PREPARED [MOD...]` writes what each mod registers into its carried content
mod. For Chef's Delight it writes its POI types and professions (Horizon's
profession registry), its 52 trades (as the game's trade data) and its
houses in the 5 village pools: all equal to NeoForge. On a Horizon dedicated
server with the two mods, a cooking pot is placed, a villager next to it
becomes `chefsdelight:chef`, and the chef house template loads.

Block code in the game, first part (2026-10-29): Horizon's block behaviour
has a hook for mods' code (`horizon_world::level::code::BlockCode`), which
`Behaviours` asks before the game class's behaviour. A running session of
the layer (`horizon_neoforge::world`) answers it on the server thread: the
game calls the block's `useWithoutItem` in the mod's own code, which acts on
the level (block changes, sounds, game events, player effects). In
`crates/horizon-client/tests/java_blocks.rs`, a player uses Farmer's
Delight's apple pie through the game's use path: the pie's code eats a
slice (its food values), sets `bites` to 1, and plays the eating sound; a
player who cannot eat gets `PASS`.

Not done yet: the session in the running game (the server starts it from
the mods' jars), `useItemOn`, placement, ticks, block entities and menus
(the cooking pot), client code.

A compiled method is not a working method. It runs only if the
compatibility layer gives the members that it uses. Farmer's Delight uses
about 2,200 members of Minecraft and NeoForge.

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
