//! Builds the content root for content mod folders and loads it through
//! every loader the game uses for blocks and items, reporting each mod's
//! blocks and states (docs/mods.md). A check for a mod's content, as the
//! Java compatibility layer writes it.
//!
//! cargo run -p horizon-client --example content_check -- PREPARED OUT MODDIR...
use horizon_client::{
    assets::{Assets, RenderAssets},
    blocks::BlockFacts,
    items::Items,
    mod_content::{self, ContentMod},
};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [base, out, dirs @ ..] = args.as_slice() else {
        return Err("usage: content_check PREPARED OUT MODDIR...".into());
    };
    let mut mods = Vec::new();
    for dir in dirs {
        let dir = Path::new(dir);
        let package = horizon_mods::Package::read(dir)?;
        let content = horizon_mods::Content::read(dir, &package.manifest)?;
        println!(
            "{}: {} blocks, {} items declared",
            package.manifest.id,
            content.blocks.len(),
            content.items.len()
        );
        mods.push(ContentMod {
            id: package.manifest.id.clone(),
            content,
        });
    }
    let started = std::time::Instant::now();
    let warnings = mod_content::build(Path::new(base), &mods, Path::new(out))?;
    for w in warnings.iter().take(20) {
        println!("  warning: {w}");
    }
    let root = Path::new(out);
    let step = |what: &'static str| move |e: Box<dyn std::error::Error>| format!("{what}: {e}");
    let assets = Assets::load(root).map_err(step("assets"))?;
    let registry = assets.registry;
    let facts = BlockFacts::load(root, registry).map_err(step("block facts"))?;
    let items = Items::load(root, registry, &facts).map_err(step("items"))?;
    horizon_world::level::Behaviours::load(registry, root)
        .map_err(|e| format!("behaviours: {e}"))?;
    horizon_client::collisions::load(root, registry).map_err(step("collisions"))?;
    RenderAssets::load(root, &assets).map_err(step("render assets"))?;
    let _ = items;
    println!(
        "root built and loaded in {:?}: {} block states in all",
        started.elapsed(),
        registry.len()
    );
    for m in &mods {
        let mut states = 0;
        for b in &m.content.blocks {
            let n = (0..registry.len() as u32)
                .filter(|i| {
                    registry
                        .state(horizon_world::registry::BlockStateId(*i))
                        .is_some_and(|s| s.name == b.id)
                })
                .count();
            if n == 0 {
                println!("  {} has no states", b.id);
            }
            states += n;
        }
        println!(
            "{}: {states} states for {} blocks",
            m.id,
            m.content.blocks.len()
        );
    }
    Ok(())
}
