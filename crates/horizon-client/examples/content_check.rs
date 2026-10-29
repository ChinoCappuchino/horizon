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
    // Villager professions, their job sites and trades; village pools.
    horizon_client::professions::load(root);
    let trades = horizon_client::merchant::Trades::load(root);
    for p in 15..horizon_client::professions::count() as u8 {
        let job = horizon_client::professions::job_site(p);
        let sets = (1..=5)
            .filter(|l| {
                trades
                    .sets
                    .contains_key(&horizon_client::merchant::Trades::set_for(
                        &horizon_client::professions::name(p),
                        *l,
                    ))
            })
            .count();
        println!(
            "profession {}: job site {:?} on {:?}, {sets} trade sets",
            horizon_client::professions::id(p),
            job.as_ref().map(|j| &j.id),
            job.as_ref().map(|j| &j.blocks),
        );
    }
    if let Ok(text) = std::fs::read_to_string(root.join("worldgen/template_pools.json"))
        && let Ok(pools) = serde_json::from_str::<serde_json::Value>(&text)
    {
        for (id, pool) in pools.as_object().into_iter().flatten() {
            let pool: serde_json::Value = match pool {
                serde_json::Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
                v => v.clone(),
            };
            let added: Vec<String> = pool["elements"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e["element"]["location"].as_str())
                .filter(|l| mods.iter().any(|m| l.starts_with(&format!("{}:", m.id))))
                .map(str::to_owned)
                .collect();
            if !added.is_empty() {
                println!("pool {id}: {}", added.join(", "));
            }
        }
    }
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
