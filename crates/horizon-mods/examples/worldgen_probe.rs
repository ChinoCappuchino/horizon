//! Horizon's side of the fork's world-generation matrix
//! (docs/research/worldgen-hooks.md, tools/worldgen_probe.py): the same JSON
//! the NeoForge probe (`WorldgenProbe.java`) writes, from Horizon's own
//! generator, so the two can be compared chunk by chunk.
//!
//! worldgen_probe <client resource dir> <out.json> [--seed S] [--radius R]
//!     [--center CX CZ] [--preset ID] [--starts-radius N]
//!     [--mod <file.hmod> [--settings JSON]]
//!
//! With `--mod`, the world-generation mod provides the terrain and biomes of
//! its preset (its manifest's first, or `--preset`) through the generator's
//! provider seam, over vanilla's overworld settings (M3a: at vanilla height
//! until dimension types are data).
//!
//! Per chunk, on a 4-block grid: `base` (the top of the noise column's
//! solid blocks, before carvers: vanilla's `getBaseHeight(OCEAN_FLOOR_WG)`), `biomes` at it (through vanilla's
//! biome zoom, as `level.getBiome` reads them), the structure starts the
//! chunk holds; after features: `surface`, `floor`, `ground` and counts of
//! logs, leaves and ore blocks. The resource dir may be a mod-content
//! overlay root (`mod_content::build`) to probe mods' data.
use horizon_core::{
    biome::{BiomeZoom, obfuscate_seed},
    position::{BlockPos, ChunkPos},
};
use horizon_mods::{
    Package,
    worldgen_runtime::{WorldInfo, WorldgenMod},
    worldgen_world::ModTerrain,
};
use horizon_world::{
    heightmap::HeightmapType,
    level::data::load_registry,
    worldgen::generator::{GeneratorData, OverworldGenerator, ProtoChunk},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

type Error = Box<dyn std::error::Error + Send + Sync>;
const STEP: i32 = 4;

/// Blocks decoration puts on the terrain: the Java probe's list, which the
/// ground skips (with logs and leaves).
const DECORATION: [&str; 10] = [
    "minecraft:cactus",
    "minecraft:bamboo",
    "minecraft:brown_mushroom_block",
    "minecraft:red_mushroom_block",
    "minecraft:mushroom_stem",
    "minecraft:pumpkin",
    "minecraft:melon",
    "minecraft:bee_nest",
    "minecraft:sugar_cane",
    "minecraft:big_dripleaf",
];

/// `#minecraft:logs` by name: logs, wood, stems and hyphae, stripped or not.
fn is_log(name: &str) -> bool {
    name != "minecraft:mushroom_stem"
        && ["_log", "_wood", "_stem", "_hyphae"]
            .iter()
            .any(|s| name.ends_with(s))
}
fn is_leaves(name: &str) -> bool {
    name.ends_with("_leaves")
}
/// Block id without its properties.
fn block_id(name: &str) -> &str {
    name.split('[').next().unwrap_or(name)
}

fn grid(f: impl Fn(i32, i32) -> Value) -> Value {
    let mut out = Vec::new();
    for z in (0..16).step_by(STEP as usize) {
        for x in (0..16).step_by(STEP as usize) {
            out.push(f(x, z));
        }
    }
    Value::Array(out)
}

fn height(chunk: &ProtoChunk, kind: HeightmapType, x: i32, z: i32) -> i32 {
    chunk
        .terrain
        .heightmap(kind)
        .and_then(|m| m.first_available(x, z).ok())
        .unwrap_or(chunk.terrain.min_y())
}

/// The y above the highest block that blocks motion and isn't decoration.
fn ground(chunk: &ProtoChunk, x: i32, z: i32) -> i32 {
    let states = chunk.data().block_states();
    let bottom = chunk.terrain.min_y();
    let mut y = height(chunk, HeightmapType::WorldSurface, x, z);
    while y >= bottom {
        let name = block_id(chunk.block_name(x, y, z));
        let solid = states.get(chunk.block(x, y, z)).motion;
        if solid && !is_log(name) && !is_leaves(name) && !DECORATION.contains(&name) {
            return y + 1;
        }
        y -= 1;
    }
    bottom
}

fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let resources = PathBuf::from(args.next().ok_or("client resource directory")?);
    let out = PathBuf::from(args.next().ok_or("output path")?);
    let (mut seed, mut radius, mut center) = (1i64, 4i32, (0, 0));
    let mut preset = String::from("minecraft:normal");
    let mut starts_radius = 0i32;
    let mut mod_path: Option<PathBuf> = None;
    let mut settings = String::new();
    while let Some(arg) = args.next() {
        let mut next = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--seed" => seed = next()?.parse()?,
            "--radius" => radius = next()?.parse()?,
            "--center" => center = (next()?.parse()?, next()?.parse()?),
            "--preset" => preset = next()?,
            "--starts-radius" => starts_radius = next()?.parse()?,
            "--mod" => mod_path = Some(PathBuf::from(next()?)),
            "--settings" => settings = next()?,
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    let registry = Box::leak(Box::new(load_registry(&resources)?));
    let data = GeneratorData::load(&resources.join("worldgen"), registry)?;
    let mut terrain = None;
    let generator = match &mod_path {
        None => {
            OverworldGenerator::for_dimension(data.clone(), seed, &preset, "minecraft:overworld")?
        }
        Some(path) => {
            let package = Package::read(path)?;
            if preset == "minecraft:normal" {
                preset = package
                    .manifest
                    .worldgen
                    .as_ref()
                    .and_then(|w| w.presets.first().cloned())
                    .ok_or("the mod provides no preset: pass --preset")?;
            }
            let m = WorldgenMod::load(&package)?;
            let base = OverworldGenerator::for_dimension(
                data.clone(),
                seed,
                "minecraft:normal",
                "minecraft:overworld",
            )?;
            let info = WorldInfo {
                seed,
                preset: preset.clone(),
                dimension: "minecraft:overworld".into(),
                min_y: -64,
                height: 384,
                sea_level: base.sea_level(),
                settings: settings.clone(),
            };
            let provider = Arc::new(ModTerrain::new(&m, data.clone(), info));
            terrain = Some(provider.clone());
            OverworldGenerator::with_provider(
                data.clone(),
                seed,
                "minecraft:normal",
                "minecraft:overworld",
                true,
                provider,
            )?
        }
    };
    let mut worker = generator.worker();

    // Terrain two chunks beyond the probed square; features one beyond, so
    // every probed chunk also gets its neighbours' features, as at FULL.
    let reach = radius + 2;
    let side = 2 * reach + 1;
    let at = |x: i32, z: i32| ((z + reach) * side + x + reach) as usize;
    let mut chunks: Vec<Option<ProtoChunk>> = Vec::with_capacity((side * side) as usize);
    for z in -reach..=reach {
        for x in -reach..=reach {
            let pos = ChunkPos::new(center.0 + x, center.1 + z);
            chunks.push(Some(worker.generate_proto(pos)?));
        }
    }
    let zoom = BiomeZoom::new(obfuscate_seed(seed));
    let mut probed = BTreeMap::new();
    for z in -radius..=radius {
        for x in -radius..=radius {
            let chunk = chunks[at(x, z)].as_ref().expect("generated");
            let (bx, bz) = ((center.0 + x) * 16, (center.1 + z) * 16);
            // Vanilla's getBaseHeight: the noise column (terrain and aquifers)
            // alone, before carvers cut into it.
            let states = data.block_states();
            let base: Vec<i32> = (0..16)
                .map(|i| {
                    let (_, spans) = worker.surface_column(bx + (i % 4) * 4, bz + (i / 4) * 4);
                    spans
                        .iter()
                        .filter(|(_, _, state)| states.get(*state).motion)
                        .map(|&(_, end, _)| end)
                        .max()
                        .unwrap_or(chunk.terrain.min_y())
                })
                .collect();
            let quarts: Vec<[i32; 3]> = (0..16)
                .map(|i| {
                    let pos = BlockPos::new(bx + (i % 4) * 4, base[i as usize], bz + (i / 4) * 4);
                    zoom.quart_at_block(pos)
                })
                .collect();
            let biomes: Vec<Value> = worker
                .noise_biomes(&quarts)
                .into_iter()
                .map(|b| json!(b.unwrap_or_else(|| "?".into())))
                .collect();
            let mut starts: Vec<(String, [i32; 6])> = chunk
                .own_starts()
                .iter()
                .map(|s| {
                    let (min, max) = (s.bounding_box.min, s.bounding_box.max);
                    (
                        s.name.clone(),
                        [min[0], min[1], min[2], max[0], max[1], max[2]],
                    )
                })
                .collect();
            starts.sort();
            probed.insert((x, z), (base, biomes, starts));
        }
    }
    let (mut unsupported, mut failed) = (0, 0);
    let mut skipped = std::collections::BTreeSet::new();
    // Features in nine phases of disjoint 3×3 regions, as chunkgen_bench.
    for phase in 0..9 {
        for z in -radius - 1..=radius + 1 {
            for x in -radius - 1..=radius + 1 {
                if (x.rem_euclid(3)) * 3 + z.rem_euclid(3) != phase {
                    continue;
                }
                let mut region = Vec::with_capacity(9);
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        region.push(chunks[at(x + dx, z + dz)].take().expect("in place"));
                    }
                }
                let mut region: [ProtoChunk; 9] = region.try_into().map_err(|_| "region")?;
                let stats = worker.decorate(&mut region)?;
                unsupported += stats.unsupported;
                failed += stats.failed;
                skipped.extend(stats.skipped);
                for (i, chunk) in region.into_iter().enumerate() {
                    let (dx, dz) = (i as i32 % 3 - 1, i as i32 / 3 - 1);
                    chunks[at(x + dx, z + dz)] = Some(chunk);
                }
            }
        }
    }
    let mut out_chunks = Vec::new();
    for ((x, z), (base, biomes, starts)) in probed {
        let chunk = chunks[at(x, z)].as_ref().expect("decorated");
        let (mut logs, mut leaves) = (0u64, 0u64);
        let mut ores: BTreeMap<String, u64> = BTreeMap::new();
        let (bottom, top) = (
            chunk.terrain.min_y(),
            chunk.terrain.min_y() + chunk.terrain.height(),
        );
        for y in bottom..top {
            for lz in 0..16 {
                for lx in 0..16 {
                    let name = block_id(chunk.block_name(lx, y, lz));
                    if is_log(name) {
                        logs += 1;
                    } else if is_leaves(name) {
                        leaves += 1;
                    } else if name.ends_with("_ore") || name.contains("_ore_") {
                        *ores.entry(name.to_string()).or_default() += 1;
                    }
                }
            }
        }
        out_chunks.push(json!({
            "x": center.0 + x,
            "z": center.1 + z,
            "surface": grid(|lx, lz| json!(height(chunk, HeightmapType::WorldSurface, lx, lz))),
            "floor": grid(|lx, lz| json!(height(chunk, HeightmapType::OceanFloor, lx, lz))),
            "ground": grid(|lx, lz| json!(ground(chunk, lx, lz))),
            "base": base,
            "biomes": biomes,
            "starts": starts.iter().map(|(id, b)| json!({"id": id, "box": b})).collect::<Vec<_>>(),
            "logs": logs,
            "leaves": leaves,
            "ores": ores,
        }));
    }
    let doc = json!({
        "seed": seed,
        "generator": "horizon:OverworldGenerator",
        "dimension_type": "minecraft:overworld",
        "preset": preset,
        "mods": mod_path.as_ref().map_or(json!({}), |_| json!({ "worldgen": preset })),
        "shared_cells": terrain.as_ref().map(|t| {
            let s = t.world().shared_stats();
            json!({ "built": s.builds, "kept": s.cells, "bytes": s.bytes })
        }),
        "center": [center.0, center.1],
        "radius": radius,
        "chunks": out_chunks,
        "features": {"unsupported": unsupported, "failed": failed, "skipped": skipped},
    });
    std::fs::write(&out, serde_json::to_string(&doc)?)?;
    if starts_radius > 0 {
        // Every start in a wide square, as the Java probe's STRUCTURE_STARTS pass lists them.
        let mut all = Vec::new();
        for z in center.1 - starts_radius..=center.1 + starts_radius {
            for x in center.0 - starts_radius..=center.0 + starts_radius {
                let mut starts: Vec<(String, [i32; 6])> = worker
                    .structure_starts(ChunkPos::new(x, z))
                    .iter()
                    .map(|s| {
                        let (min, max) = (s.bounding_box.min, s.bounding_box.max);
                        (
                            s.name.clone(),
                            [min[0], min[1], min[2], max[0], max[1], max[2]],
                        )
                    })
                    .collect();
                starts.sort();
                for (id, b) in starts {
                    all.push(json!({"chunk": [x, z], "id": id, "box": b}));
                }
            }
        }
        let path = out.with_extension("starts.json");
        std::fs::write(&path, serde_json::to_string(&all)?)?;
        println!("wrote {} structure starts to {}", all.len(), path.display());
    }
    println!(
        "wrote {} chunks to {}",
        doc["chunks"].as_array().map_or(0, Vec::len),
        out.display()
    );
    Ok(())
}
