//! Translates NeoForge mod jars that run together (a mod after the mods it
//! needs) into Horizon content mods, and reports what each registered.
//!
//!     translate <prepared root> <mod.jar>... [--out DIR]
//!
//! With --out, each mod's content goes in DIR/<mod id>/ (a content mod
//! folder, such as a port's carry/: declarations and data files replace
//! those there).
use std::{path::Path, sync::Arc};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.iter().position(|a| a == "--out").map(|i| {
        let dir = args.get(i + 1).cloned().unwrap_or_default();
        args.drain(i..(i + 2).min(args.len()));
        dir
    });
    let [root, jars @ ..] = args.as_slice() else {
        return Err("usage: translate <prepared root> <mod.jar>... [--out DIR]".into());
    };
    if jars.is_empty() {
        return Err("no jars".into());
    }
    let game = Arc::new(horizon_neoforge::GameData::load(Path::new(root))?);
    let mut classes = Vec::new();
    for jar in jars {
        classes.push(horizon_neoforge::read_jar(Path::new(jar))?);
    }
    let started = std::time::Instant::now();
    let all = horizon_neoforge::translate_all(&classes, game).map_err(|e| format!("{e:#}"))?;
    println!("translated in {:?}", started.elapsed());
    for t in &all {
        println!(
            "{}: {} blocks, {} items, {} data files",
            t.mod_id,
            t.blocks.len(),
            t.items.len(),
            t.files.len()
        );
    }
    let first = &all[0];
    for e in first.errors.iter().take(30) {
        println!("  error: {e}");
    }
    let mut unknown: Vec<_> = first.unknown.iter().collect();
    unknown.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    println!("{} kinds of calls not modelled", unknown.len());
    for (call, n) in unknown.iter().take(25) {
        println!("  {n:5} not modelled: {call}");
    }
    if let Some(dir) = out {
        for t in &all {
            let folder = Path::new(&dir).join(&t.mod_id);
            horizon_neoforge::write(t, &folder)?;
            println!("written to {}", folder.display());
        }
    }
    Ok(())
}
