//! Translates a NeoForge mod jar into a Horizon content mod's declarations
//! and reports what it registered.
//!
//!     translate <mod.jar> <prepared root> [content mod folder]
use std::{path::Path, sync::Arc};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [jar, root, rest @ ..] = args.as_slice() else {
        return Err("usage: translate <mod.jar> <prepared root> [content mod folder]".into());
    };
    let game = Arc::new(horizon_neoforge::GameData::load(Path::new(root))?);
    let classes = horizon_neoforge::read_jar(Path::new(jar))?;
    let started = std::time::Instant::now();
    let t = horizon_neoforge::translate(&classes, game).map_err(|e| format!("{e:#}"))?;
    println!(
        "{}: {} blocks, {} items in {:?}; {} errors; {} kinds of calls not modelled",
        t.mod_id,
        t.blocks.len(),
        t.items.len(),
        started.elapsed(),
        t.errors.len(),
        t.unknown.len()
    );
    for e in t.errors.iter().take(30) {
        println!("  error: {e}");
    }
    let mut unknown: Vec<_> = t.unknown.iter().collect();
    unknown.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (call, n) in unknown.iter().take(40) {
        println!("  {n:5} not modelled: {call}");
    }
    if let Some(dir) = rest.first() {
        horizon_neoforge::write(&t, Path::new(dir))?;
        println!("written to {dir}");
    }
    Ok(())
}
