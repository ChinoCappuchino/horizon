//! Terrain throughput of a world-generation mod (capability `worldgen`, fork
//! `afterwinter`): its `terrain` call over a square of chunks, one
//! [`Worker`] per thread as the generator will run it, in chunks a second.
//! It measures the mod in the sandbox alone, before the game's own stages
//! (the comparison with an in-process Java fill bench).
//!
//! worldgen_bench <mod.hmod or folder> [--preset ID] [--seed S] [--settings JSON]
//!     [--threads T] [--size N] [--center CX CZ] [--min-y Y] [--height H]
//!     [--sea-level Y] [--verify]
//!
//! `--verify` generates the square again on one thread and checks that every
//! chunk's land is the same: the determinism contract.
use horizon_mods::{
    Package,
    worldgen_runtime::{Failure, Land, Palette, WorldInfo, WorldgenMod},
};
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

/// Hands out a number for every state and biome the mod asks for: a bench
/// has no registry, and checks nothing.
#[derive(Default)]
struct Learning {
    states: Mutex<HashMap<String, u32>>,
    biomes: Mutex<HashMap<String, u32>>,
}
fn number(table: &Mutex<HashMap<String, u32>>, name: &str) -> u32 {
    let mut t = table.lock().unwrap();
    let next = t.len() as u32;
    *t.entry(name.to_string()).or_insert(next)
}
impl Palette for Learning {
    fn state_id(&self, state: &str) -> Result<u32, String> {
        Ok(number(&self.states, state))
    }
    fn is_state(&self, id: u32) -> bool {
        (id as usize) < self.states.lock().unwrap().len()
    }
    fn biome_id(&self, biome: &str) -> Option<u32> {
        Some(number(&self.biomes, biome))
    }
    fn is_biome(&self, id: u32) -> bool {
        (id as usize) < self.biomes.lock().unwrap().len()
    }
}

fn digest(land: &Land) -> u64 {
    let mut h = DefaultHasher::new();
    match land {
        Land::Density(d) => d.iter().for_each(|v| v.to_bits().hash(&mut h)),
        Land::Columns(c) => c.iter().for_each(|c| {
            c.runs.iter().for_each(|r| (r.state, r.top).hash(&mut h));
            0xffff_u32.hash(&mut h);
        }),
    }
    h.finish()
}

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().ok_or(
        "usage: worldgen_bench <mod.hmod or folder> [--preset ID] [--seed S] [--threads T] [--size N] [--verify]",
    )?;
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| args[i + 1].as_str())
    };
    let num = |name: &str, default: i64| -> Result<i64, Error> {
        Ok(opt(name).map(str::parse).transpose()?.unwrap_or(default))
    };
    let package = Package::read(Path::new(path))?;
    let section = package.manifest.worldgen.clone().unwrap_or_default();
    let preset = opt("--preset")
        .map(str::to_owned)
        .or_else(|| section.presets.first().cloned())
        .ok_or("the mod provides no preset: pass --preset")?;
    let threads = num("--threads", 8)? as usize;
    let size = num("--size", 32)? as i32;
    let (cx, cz) = match args.iter().position(|a| a == "--center") {
        Some(i) => (args[i + 1].parse()?, args[i + 2].parse()?),
        None => (0, 0),
    };
    let info = WorldInfo {
        seed: num("--seed", 1)?,
        preset: preset.clone(),
        dimension: "minecraft:overworld".into(),
        min_y: num("--min-y", -64)? as i32,
        height: num("--height", 384)? as u32,
        sea_level: num("--sea-level", 63)? as i32,
        settings: opt("--settings").unwrap_or("").into(),
    };
    let started = Instant::now();
    let m = WorldgenMod::load(&package)?;
    println!("{} loaded in {:.2?}", m.id(), started.elapsed());
    let palette: Arc<dyn Palette> = Arc::new(Learning::default());
    let chunks: Vec<(i32, i32)> = (0..size * size)
        .map(|i| (cx - size / 2 + i % size, cz - size / 2 + i / size))
        .collect();

    // Open every worker first: `open` is per world, not per chunk.
    let opened = Instant::now();
    let workers = (0..threads)
        .map(|_| m.worker(info.clone(), palette.clone()))
        .collect::<Result<Vec<_>, Failure>>()
        .map_err(|e| format!("open: {e:?}"))?;
    println!("{threads} workers opened in {:.2?}", opened.elapsed());

    let next = AtomicUsize::new(0);
    let digests = Mutex::new(vec![0u64; chunks.len()]);
    let failures = Mutex::new(Vec::new());
    let t = Instant::now();
    std::thread::scope(|s| {
        for mut w in workers {
            let (next, chunks, digests, failures) = (&next, &chunks, &digests, &failures);
            s.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(x, z)) = chunks.get(i) else { break };
                    match w.terrain(x, z) {
                        Ok(land) => digests.lock().unwrap()[i] = digest(&land),
                        Err(e) => failures.lock().unwrap().push(((x, z), e)),
                    }
                }
            });
        }
    });
    let secs = t.elapsed().as_secs_f64();
    let failures = failures.into_inner().unwrap();
    let done = chunks.len() - failures.len();
    println!(
        "terrain: {done} chunks in {secs:.2} s on {threads} threads = {:.0} chunks/s ({preset}, seed {}, {size} x {size} around {cx},{cz})",
        done as f64 / secs,
        info.seed
    );
    for ((x, z), e) in failures.iter().take(5) {
        println!("  chunk {x},{z}: {e:?}");
    }
    if failures.len() > 5 {
        println!("  ... {} failures in all", failures.len());
    }

    if args.iter().any(|a| a == "--verify") {
        let mut w = m
            .worker(info.clone(), palette.clone())
            .map_err(|e| format!("open: {e:?}"))?;
        let digests = digests.into_inner().unwrap();
        let mut differ = 0;
        // Backwards, so no chunk comes in the order the threads took it.
        for (i, &(x, z)) in chunks.iter().enumerate().rev() {
            if let Ok(land) = w.terrain(x, z)
                && digest(&land) != digests[i]
            {
                differ += 1;
            }
        }
        println!(
            "verify: {differ} of {done} chunks differ when generated again on one thread, in reverse"
        );
        if differ > 0 {
            return Err("the mod's land depends on more than its arguments".into());
        }
    }
    Ok(())
}
