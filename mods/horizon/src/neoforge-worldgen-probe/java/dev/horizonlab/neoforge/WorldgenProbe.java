package dev.horizonlab.neoforge;

import com.google.gson.stream.JsonWriter;
import java.io.IOException;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Comparator;
import java.util.Map;
import java.util.TreeMap;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.ResourceLocation;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.tags.BlockTags;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.ChunkGenerator;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.structure.BoundingBox;
import net.minecraft.world.level.levelgen.structure.Structure;
import net.minecraft.world.level.levelgen.structure.StructureStart;
import net.neoforged.bus.api.IEventBus;
import net.neoforged.fml.ModList;
import net.neoforged.fml.common.Mod;
import net.neoforged.neoforge.common.NeoForge;
import net.neoforged.neoforge.event.server.ServerStartedEvent;
import net.neoforged.neoforgespi.language.IModInfo;

/**
 * Dev-only probe for the fork's world-generation matrix (docs/research/worldgen-hooks.md): once
 * the server has started, generates the overworld chunks in a square of {@code
 * horizon.probe.radius} chunks around ({@code horizon.probe.cx}, {@code horizon.probe.cz}) to full
 * status and writes, per chunk, surface and ocean-floor heights on a 4-block grid, the biome at
 * those columns, every valid structure start with its box, and counts of logs, leaves and ores, to
 * {@code horizon.probe.out}. Then it stops the server. It only observes what the installed mods
 * generate; it reads no mod code. Never shipped to players.
 */
@Mod("horizon_worldgen_probe")
public final class WorldgenProbe {
    private static final int STEP = 4;

    public WorldgenProbe(IEventBus modBus) {
        NeoForge.EVENT_BUS.addListener(this::started);
    }

    private void started(ServerStartedEvent event) {
        MinecraftServer server = event.getServer();
        Path out = Path.of(System.getProperty("horizon.probe.out", "worldgen-probe.json"));
        int radius = Integer.getInteger("horizon.probe.radius", 4);
        int cx0 = Integer.getInteger("horizon.probe.cx", 0);
        int cz0 = Integer.getInteger("horizon.probe.cz", 0);
        int startsRadius = Integer.getInteger("horizon.probe.starts_radius", 0);
        try {
            try (Writer w = Files.newBufferedWriter(out, StandardCharsets.UTF_8);
                    JsonWriter json = new JsonWriter(w)) {
                write(server.overworld(), json, radius, cx0, cz0);
            }
            int climateRadius = Integer.getInteger("horizon.probe.climate_radius", 0);
            if (climateRadius > 0) {
                Path climate = Path.of(out.toString().replaceFirst("\\.json$", "") + ".climate.json");
                try (Writer w = Files.newBufferedWriter(climate, StandardCharsets.UTF_8);
                        JsonWriter json = new JsonWriter(w)) {
                    writeClimate(server.overworld(), json, climateRadius, cx0, cz0);
                }
            }
            if (startsRadius > 0) {
                Path starts = Path.of(out.toString().replaceFirst("\\.json$", "") + ".starts.json");
                try (Writer w = Files.newBufferedWriter(starts, StandardCharsets.UTF_8);
                        JsonWriter json = new JsonWriter(w)) {
                    writeStarts(server.overworld(), json, startsRadius, cx0, cz0);
                }
            }
        } catch (Throwable e) {
            // A mod that fails while generating is a result, not a probe failure: record it next to the
            // output, then stop at once. System.exit would wait for the server's own shutdown hook,
            // which waits for this thread: the server would hang until the driver's timeout.
            e.printStackTrace();
            try {
                Files.deleteIfExists(out);
                Throwable root = e;
                while (root.getCause() != null && root.getCause() != root) {
                    root = root.getCause();
                }
                java.io.StringWriter trace = new java.io.StringWriter();
                root.printStackTrace(new java.io.PrintWriter(trace));
                Files.writeString(Path.of(out + ".error"), root + "\n" + trace, StandardCharsets.UTF_8);
            } catch (IOException ignored) {
                // nothing more to record
            }
            Runtime.getRuntime().halt(3);
        }
        System.out.println("horizon-worldgen-probe: wrote " + out);
        server.halt(false);
    }

    private static void write(ServerLevel level, JsonWriter json, int radius, int cx0, int cz0)
            throws IOException {
        Registry<Structure> structures = level.registryAccess().registryOrThrow(Registries.STRUCTURE);
        json.beginObject();
        json.name("seed").value(level.getSeed());
        json.name("generator").value(level.getChunkSource().getGenerator().getClass().getName());
        json.name("dimension_type").value(key(level.dimensionTypeRegistration()));
        json.name("mods").beginObject();
        for (IModInfo mod : ModList.get().getMods().stream()
                .sorted(Comparator.comparing(IModInfo::getModId)).toList()) {
            json.name(mod.getModId()).value(mod.getVersion().toString());
        }
        json.endObject();
        json.name("center").beginArray().value(cx0).value(cz0).endArray();
        json.name("radius").value(radius);
        json.name("chunks").beginArray();
        for (int cz = cz0 - radius; cz <= cz0 + radius; cz++) {
            for (int cx = cx0 - radius; cx <= cx0 + radius; cx++) {
                chunk(level, structures, level.getChunk(cx, cz, ChunkStatus.FULL, true), cx, cz, json);
            }
        }
        json.endArray();
        json.endObject();
    }

    /**
     * The overworld's climate and biomes over a wide square, and the climate regions other mods
     * registered with TerraBlender, read through its public API (by reflection: the probe neither
     * depends on it nor reads its code). Samples every {@code climate_step} blocks (16 unless set) at y 64 (quart y 16): the six
     * climate values of the noise sampler and the biome the biome source picks there.
     */
    private static void writeClimate(ServerLevel level, JsonWriter json, int radius, int cx0, int cz0)
            throws IOException {
        json.beginObject();
        json.name("regions").beginArray();
        try {
            Class<?> regions = Class.forName("terrablender.api.Regions");
            Class<?> type = Class.forName("terrablender.api.RegionType");
            Object overworld = type.getField("OVERWORLD").get(null);
            java.util.List<?> list = (java.util.List<?>) regions.getMethod("get", type).invoke(null, overworld);
            Registry<Biome> biomes = level.registryAccess().registryOrThrow(Registries.BIOME);
            for (Object region : list) {
                json.beginObject();
                json.name("name").value(String.valueOf(region.getClass().getMethod("getName").invoke(region)));
                json.name("weight").value(((Number) region.getClass().getMethod("getWeight").invoke(region)).intValue());
                json.name("points").beginArray();
                java.util.List<Object[]> points = new java.util.ArrayList<>();
                java.util.function.Consumer<Object> sink = pair -> {
                    try {
                        Object first = pair.getClass().getMethod("getFirst").invoke(pair);
                        Object second = pair.getClass().getMethod("getSecond").invoke(pair);
                        points.add(new Object[] {first, second});
                    } catch (ReflectiveOperationException e) {
                        throw new RuntimeException(e);
                    }
                };
                region.getClass().getMethod("addBiomes", Registry.class, java.util.function.Consumer.class)
                        .invoke(region, biomes, sink);
                for (Object[] point : points) {
                    net.minecraft.world.level.biome.Climate.ParameterPoint pp =
                            (net.minecraft.world.level.biome.Climate.ParameterPoint) point[0];
                    json.beginObject().name("biome").value(String.valueOf(
                            ((net.minecraft.resources.ResourceKey<?>) point[1]).location()));
                    json.name("parameters").beginArray();
                    for (net.minecraft.world.level.biome.Climate.Parameter q : java.util.List.of(
                            pp.temperature(), pp.humidity(), pp.continentalness(), pp.erosion(),
                            pp.depth(), pp.weirdness())) {
                        json.beginArray().value(q.min()).value(q.max()).endArray();
                    }
                    json.value(pp.offset());
                    json.endArray().endObject();
                }
                json.endArray().endObject();
            }
        } catch (ClassNotFoundException e) {
            // No TerraBlender in this row.
        } catch (ReflectiveOperationException e) {
            json.beginObject().name("error").value(String.valueOf(e)).endObject();
        }
        json.endArray();
        net.minecraft.world.level.biome.Climate.Sampler sampler = level.getChunkSource().randomState().sampler();
        net.minecraft.world.level.biome.BiomeSource source = level.getChunkSource().getGenerator().getBiomeSource();
        int step = Math.max(4, Integer.getInteger("horizon.probe.climate_step", 16));
        json.name("step").value(step);
        json.name("samples").beginArray();
        for (int z = (cz0 - radius) * 16; z <= (cz0 + radius) * 16; z += step) {
            for (int x = (cx0 - radius) * 16; x <= (cx0 + radius) * 16; x += step) {
                int qx = x >> 2, qy = 16, qz = z >> 2;
                net.minecraft.world.level.biome.Climate.TargetPoint t = sampler.sample(qx, qy, qz);
                json.beginArray().value(x).value(z)
                        .value(key(source.getNoiseBiome(qx, qy, qz, sampler)))
                        .value(t.temperature()).value(t.humidity()).value(t.continentalness())
                        .value(t.erosion()).value(t.depth()).value(t.weirdness()).endArray();
            }
        }
        json.endArray();
        json.endObject();
    }

    /**
     * Every structure start in a wide square, from chunks generated only to STRUCTURE_STARTS (no
     * terrain fill, no features): {@code [{"chunk": [x, z], "id": ..., "box": [...]}]}, by chunk.
     */
    private static void writeStarts(ServerLevel level, JsonWriter json, int radius, int cx0, int cz0)
            throws IOException {
        Registry<Structure> structures = level.registryAccess().registryOrThrow(Registries.STRUCTURE);
        json.beginArray();
        for (int cz = cz0 - radius; cz <= cz0 + radius; cz++) {
            for (int cx = cx0 - radius; cx <= cx0 + radius; cx++) {
                ChunkAccess chunk = level.getChunk(cx, cz, ChunkStatus.STRUCTURE_STARTS, true);
                Map<String, BoundingBox> starts = new TreeMap<>();
                for (Map.Entry<Structure, StructureStart> e : chunk.getAllStarts().entrySet()) {
                    if (e.getValue().isValid()) {
                        ResourceLocation id = structures.getKey(e.getKey());
                        starts.put(id == null ? "?" : id.toString(), e.getValue().getBoundingBox());
                    }
                }
                for (Map.Entry<String, BoundingBox> s : starts.entrySet()) {
                    BoundingBox b = s.getValue();
                    json.beginObject().name("chunk").beginArray().value(cx).value(cz).endArray()
                            .name("id").value(s.getKey()).name("box").beginArray()
                            .value(b.minX()).value(b.minY()).value(b.minZ())
                            .value(b.maxX()).value(b.maxY()).value(b.maxZ()).endArray().endObject();
                }
            }
        }
        json.endArray();
    }

    private static void chunk(ServerLevel level, Registry<Structure> structures, ChunkAccess chunk,
            int cx, int cz, JsonWriter json) throws IOException {
        json.beginObject();
        json.name("x").value(cx);
        json.name("z").value(cz);
        json.name("surface").beginArray();
        for (int z = 0; z < 16; z += STEP) {
            for (int x = 0; x < 16; x += STEP) {
                json.value(chunk.getHeight(Heightmap.Types.WORLD_SURFACE, x, z));
            }
        }
        json.endArray();
        json.name("floor").beginArray();
        for (int z = 0; z < 16; z += STEP) {
            for (int x = 0; x < 16; x += STEP) {
                json.value(chunk.getHeight(Heightmap.Types.OCEAN_FLOOR, x, z));
            }
        }
        json.endArray();
        // Ground: the terrain without what decoration puts on it. Vanilla places trees and other
        // features in the order neighbouring chunks finish, which differs between runs, so the heights
        // above (they include trees) are decoration; ground and the biomes sampled at it are terrain.
        int[] ground = new int[16];
        int i = 0;
        for (int z = 0; z < 16; z += STEP) {
            for (int x = 0; x < 16; x += STEP) {
                ground[i++] = ground(chunk, x, z);
            }
        }
        json.name("ground").beginArray();
        for (int g : ground) {
            json.value(g);
        }
        json.endArray();
        // Base: the generator's own terrain height from its noise, before any feature runs. Unlike
        // the ground (where a boulder or a fallen log still counts), it cannot depend on the order
        // in which chunks finish, so it and the biomes sampled at it are the terrain to compare exactly.
        ChunkGenerator generator = level.getChunkSource().getGenerator();
        RandomState random = level.getChunkSource().randomState();
        int[] base = new int[16];
        i = 0;
        for (int z = 0; z < 16; z += STEP) {
            for (int x = 0; x < 16; x += STEP) {
                base[i++] = generator.getBaseHeight((cx << 4) + x, (cz << 4) + z,
                        Heightmap.Types.OCEAN_FLOOR_WG, level, random);
            }
        }
        json.name("base").beginArray();
        for (int b : base) {
            json.value(b);
        }
        json.endArray();
        json.name("biomes").beginArray();
        i = 0;
        for (int z = 0; z < 16; z += STEP) {
            for (int x = 0; x < 16; x += STEP) {
                json.value(key(level.getBiome(new BlockPos((cx << 4) + x, base[i++], (cz << 4) + z))));
            }
        }
        json.endArray();
        // Structure starts sorted by id, so the output does not depend on map order.
        Map<String, BoundingBox> starts = new TreeMap<>();
        for (Map.Entry<Structure, StructureStart> e : chunk.getAllStarts().entrySet()) {
            if (e.getValue().isValid()) {
                ResourceLocation id = structures.getKey(e.getKey());
                starts.put(id == null ? "?" : id.toString(), e.getValue().getBoundingBox());
            }
        }
        json.name("starts").beginArray();
        for (Map.Entry<String, BoundingBox> s : starts.entrySet()) {
            BoundingBox b = s.getValue();
            json.beginObject().name("id").value(s.getKey()).name("box").beginArray()
                    .value(b.minX()).value(b.minY()).value(b.minZ())
                    .value(b.maxX()).value(b.maxY()).value(b.maxZ()).endArray().endObject();
        }
        json.endArray();
        int logs = 0;
        int leaves = 0;
        Map<String, Integer> ores = new TreeMap<>();
        for (LevelChunkSection section : chunk.getSections()) {
            if (section.hasOnlyAir()) {
                continue;
            }
            for (int y = 0; y < 16; y++) {
                for (int z = 0; z < 16; z++) {
                    for (int x = 0; x < 16; x++) {
                        BlockState state = section.getBlockState(x, y, z);
                        if (state.is(BlockTags.LOGS)) {
                            logs++;
                        } else if (state.is(BlockTags.LEAVES)) {
                            leaves++;
                        } else {
                            String id = BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();
                            if (id.endsWith("_ore") || id.contains("_ore_")) {
                                ores.merge(id, 1, Integer::sum);
                            }
                        }
                    }
                }
            }
        }
        json.name("logs").value(logs);
        json.name("leaves").value(leaves);
        json.name("ores").beginObject();
        for (Map.Entry<String, Integer> o : ores.entrySet()) {
            json.name(o.getKey()).value(o.getValue());
        }
        json.endObject();
        json.endObject();
    }

    /** Blocks that decoration features put on the terrain (trees, huge mushrooms, plants that block movement). */
    private static final java.util.Set<String> DECORATION = java.util.Set.of(
            "minecraft:cactus", "minecraft:bamboo", "minecraft:brown_mushroom_block",
            "minecraft:red_mushroom_block", "minecraft:mushroom_stem", "minecraft:pumpkin",
            "minecraft:melon", "minecraft:bee_nest", "minecraft:sugar_cane", "minecraft:big_dripleaf");

    /** The y above the highest solid block that is not decoration (the heightmap convention). */
    private static int ground(ChunkAccess chunk, int x, int z) {
        int bottom = chunk.getMinBuildHeight();
        for (int y = chunk.getHeight(Heightmap.Types.WORLD_SURFACE, x, z); y >= bottom; y--) {
            BlockState state = chunk.getBlockState(new BlockPos(x, y, z));
            if (!state.blocksMotion() || state.is(BlockTags.LOGS) || state.is(BlockTags.LEAVES)) {
                continue;
            }
            if (DECORATION.contains(BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString())) {
                continue;
            }
            return y + 1;
        }
        return bottom;
    }

    private static String key(Holder<?> holder) {
        return holder.unwrapKey().map(k -> k.location().toString()).orElse("?");
    }
}
