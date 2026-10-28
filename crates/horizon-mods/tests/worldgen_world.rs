//! A world-generation mod through horizon-world's generator (M3a): the
//! stand-in mod in `tests/worldgen_guest` provides the land and biomes of
//! its preset, and vanilla's structures and features run on top. Needs the
//! prepared 26.3 resources (`.local/horizon-client/26.3`, or
//! HORIZON_TEST_RESOURCES) and `rustup target add wasm32-unknown-unknown`;
//! skips without the resources.
use horizon_core::position::ChunkPos;
use horizon_mods::{
    Manifest, Package,
    manifest::componentize,
    worldgen_runtime::{ColumnPos, WorldInfo, WorldgenMod},
    worldgen_world::{GeneratorPalette, ModDecorator, ModTerrain},
};
use horizon_world::{
    heightmap::HeightmapType,
    level::data::load_registry,
    worldgen::generator::{GeneratorData, OverworldGenerator},
};
use std::{path::Path, process::Command, sync::Arc};

fn resources() -> Option<std::path::PathBuf> {
    let root = std::env::var_os("HORIZON_TEST_RESOURCES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/horizon-client/26.3")
        });
    root.join("worldgen/world_presets.json")
        .exists()
        .then_some(root)
}

fn hills() -> WorldgenMod {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = root.join("target/mods-test");
    let status = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .args(["-p", "horizon-mods-test-worldgen-guest", "--target-dir"])
        .arg(&target)
        .status()
        .expect("cargo runs");
    assert!(status.success());
    let core = std::fs::read(
        target.join("wasm32-unknown-unknown/release/horizon_mods_test_worldgen_guest.wasm"),
    )
    .unwrap();
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/worldgen_guest/horizon-mod.toml"),
    )
    .unwrap();
    WorldgenMod::load(&Package {
        manifest: Manifest::parse(&manifest).unwrap(),
        wasm: Some(componentize(&core).unwrap()),
    })
    .unwrap()
}

#[test]
fn the_mods_land_and_biomes_in_the_generators_chunks() {
    let Some(root) = resources() else {
        eprintln!("skipping: no prepared 26.3 resources");
        return;
    };
    let registry = Box::leak(Box::new(load_registry(&root).unwrap()));
    let data = GeneratorData::load(&root.join("worldgen"), registry).unwrap();
    let m = hills();
    let info = WorldInfo {
        seed: 42,
        preset: "test_worldgen:hills".into(),
        dimension: "minecraft:overworld".into(),
        min_y: -64,
        height: 384,
        sea_level: 63,
        settings: "cells".into(),
    };
    let terrain = Arc::new(ModTerrain::new(&m, data.clone(), info.clone()));
    let generator = OverworldGenerator::with_provider(
        data.clone(),
        42,
        "minecraft:normal",
        "minecraft:overworld",
        true,
        terrain.clone(),
    )
    .unwrap();
    let mut worker = generator.worker();
    // The mod's own answer for the same columns, from a world of its own.
    let mut mine = m
        .world(info, Arc::new(GeneratorPalette(data.clone())))
        .worker()
        .unwrap();
    for pos in [
        ChunkPos::new(0, 0),
        ChunkPos::new(-3, 5),
        ChunkPos::new(17, -9),
    ] {
        let chunk = worker.generate_proto(pos).unwrap();
        let columns: Vec<ColumnPos> = (0..256)
            .map(|i| ColumnPos {
                x: pos.x * 16 + i % 16,
                z: pos.z * 16 + i / 16,
            })
            .collect();
        let heights = mine.heights(&columns).unwrap();
        for (i, h) in heights.iter().enumerate() {
            let (x, z) = (i as i32 % 16, i as i32 / 16);
            let floor = chunk
                .terrain
                .heightmap(HeightmapType::OceanFloorWg)
                .unwrap()
                .first_available(x, z)
                .unwrap();
            assert_eq!(floor, *h, "chunk {pos:?} column {x},{z}");
            assert_eq!(chunk.block_name(x, h - 1, z), "minecraft:grass_block");
        }
        assert_eq!(chunk.biome_at(0, 64, 0), "minecraft:plains");
    }
    // Structure placement asks the provider's columns: a start's base sits on the mod's land.
    let starts: usize = (-8..=8)
        .flat_map(|x| (-8..=8).map(move |z| (x, z)))
        .map(|(x, z)| worker.structure_starts(ChunkPos::new(x, z)).len())
        .sum();
    assert!(
        starts > 0,
        "vanilla's structures still start on the mod's land"
    );
    assert!(terrain.world().shared_stats().builds > 0);
}

/// A mod decorating a vanilla world (M3g): the stand-in mod sets a log on
/// every 4th column of each chunk in vanilla's vegetal step, reading the
/// ocean floor's height from the region, and vanilla's features still run.
#[test]
fn the_mods_decoration_in_a_vanilla_world() {
    let Some(root) = resources() else {
        eprintln!("skipping: no prepared 26.3 resources");
        return;
    };
    let registry = Box::leak(Box::new(load_registry(&root).unwrap()));
    let data = GeneratorData::load(&root.join("worldgen"), registry).unwrap();
    let m = hills();
    let info = WorldInfo {
        seed: 42,
        preset: "minecraft:normal".into(),
        dimension: "minecraft:overworld".into(),
        min_y: -64,
        height: 384,
        sea_level: 63,
        settings: String::new(),
    };
    let decorated = OverworldGenerator::new(data.clone(), 42)
        .unwrap()
        .with_decorators(vec![Arc::new(ModDecorator::new(&m, data.clone(), info))])
        .unwrap();
    let plain = OverworldGenerator::new(data.clone(), 42).unwrap();
    let region = |generator: &OverworldGenerator| {
        let mut worker = generator.worker();
        let mut chunks = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                chunks.push(worker.generate_proto(ChunkPos::new(dx, dz)).unwrap());
            }
        }
        let mut chunks: [_; 9] = chunks.try_into().ok().unwrap();
        let stats = worker.decorate(&mut chunks).unwrap();
        (chunks, stats)
    };
    let (before, _) = {
        let mut worker = plain.worker();
        let chunk = worker.generate_proto(ChunkPos::new(0, 0)).unwrap();
        (chunk, ())
    };
    let (chunks, stats) = region(&decorated);
    assert_eq!(stats.failed, 0, "{stats:?}");
    let centre = &chunks[4];
    let mut logs = 0;
    for z in (0..16).step_by(4) {
        for x in (0..16).step_by(4) {
            // The floor as the mod read it: before vegetal decoration, which
            // only adds above it here (or the mod's own logs).
            let floor = before
                .terrain
                .heightmap(HeightmapType::OceanFloorWg)
                .unwrap()
                .first_available(x, z)
                .unwrap();
            let name = centre.block_name(x, floor, z);
            logs += usize::from(name == "minecraft:oak_log");
        }
    }
    // Features before the vegetal step (lakes, rocks) may raise a column's
    // floor; most of the 16 still hold the mod's log where it read the floor.
    assert!(logs >= 12, "{logs} of 16 columns hold the mod's log");
    let (vanilla, _) = region(&plain);
    let vanilla_logs = (0..16)
        .flat_map(|z| (0..16).map(move |x| (x, z)))
        .filter(|&(x, z)| x % 4 == 0 && z % 4 == 0)
        .filter(|&(x, z)| {
            let floor = before
                .terrain
                .heightmap(HeightmapType::OceanFloorWg)
                .unwrap()
                .first_available(x, z)
                .unwrap();
            vanilla[4].block_name(x, floor, z) == "minecraft:oak_log"
        })
        .count();
    assert!(vanilla_logs < logs, "without the mod: {vanilla_logs}");
}
