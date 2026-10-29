//! A content mod's block with its own states (fork `afterwinter`,
//! docs/mods.md): a cooking pot with `facing` and `support`, its own shapes,
//! over the prepared 26.3 root. Every loader takes the overlay, and each
//! state has its properties, facts and shapes. Needs HORIZON_TEST_RESOURCES
//! (a prepared root, `.local/horizon-client/26.3`); skips without it.
use horizon_client::{
    assets::Assets,
    blocks::BlockFacts,
    items::Items,
    mod_content::{self, ContentMod},
};
use horizon_core::geometry::Axis;
use horizon_world::chunk::BlockState;
use std::{collections::BTreeMap, path::PathBuf};

const MANIFEST: &str = r#"
id = "testpot"
name = "Test pot"
version = "0.1.0"
api = "0.2"
side = "both"
capabilities = ["registries"]
"#;

const POT: &[u8] = br#"{
  "template": "minecraft:stone",
  "properties": {"hardness": 0.5, "sounds": "minecraft:lantern"},
  "states": {
    "properties": {
      "facing": ["north", "south", "west", "east"],
      "support": ["none", "tray", "handle"]
    },
    "default": {"facing": "north"},
    "shapes": [
      {"when": {"support": "tray"}, "outline": [[2, 0, 2, 14, 10, 14], [0, -1, 0, 16, 0, 16]]},
      {"outline": [[2, 0, 2, 14, 10, 14]]}
    ]
  }
}"#;

#[test]
fn a_mod_block_with_its_own_states() {
    let Some(base) = std::env::var_os("HORIZON_TEST_RESOURCES").map(PathBuf::from) else {
        eprintln!("skipping: HORIZON_TEST_RESOURCES is not set");
        return;
    };
    let dir = std::env::temp_dir().join(format!("horizon-own-states-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let hmod = dir.join("testpot.hmod");
    let bytes = horizon_mods::Package::write_files(
        MANIFEST,
        None,
        &[("data/testpot/horizon/block/cooking_pot.json", POT)],
    )
    .unwrap();
    std::fs::write(&hmod, bytes).unwrap();
    let package = horizon_mods::Package::read(&hmod).unwrap();
    let content = horizon_mods::Content::read(&hmod, &package.manifest).unwrap();
    let mods = [ContentMod {
        id: "testpot".into(),
        content,
    }];
    let root = dir.join("root");
    mod_content::build(&base, &mods, &root).unwrap();

    let assets = Assets::load(&root).unwrap();
    let registry = assets.registry;
    let state = |facing: &str, support: &str| {
        let properties: BTreeMap<String, String> = [("facing", facing), ("support", support)]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        registry
            .id(&BlockState {
                name: "testpot:cooking_pot".into(),
                properties,
            })
            .unwrap_or_else(|| panic!("no state facing={facing} support={support}"))
    };
    // 4 x 3 states, in StateDefinition order: support changes fastest.
    let first = state("north", "none");
    assert_eq!(state("north", "tray").0, first.0 + 1);
    assert_eq!(state("south", "none").0, first.0 + 3);
    assert_eq!(state("east", "handle").0, first.0 + 11);
    assert_eq!(registry.default_state("testpot:cooking_pot"), Some(first));

    let facts = BlockFacts::load(&root, registry).unwrap();
    let f = facts.get(state("west", "handle"));
    assert_eq!(f.hardness, 0.5);
    let items = Items::load(&root, registry, &facts).unwrap();
    assert!(items.get("testpot:cooking_pot").is_some());
    // The world's behaviours and shapes take the new states.
    horizon_world::level::Behaviours::load(registry, &root).unwrap();
    let collisions = horizon_client::collisions::load(&root, registry).unwrap();
    let boxes = |id| {
        let shape = collisions
            .cached_shapes()
            .cached_shape(id)
            .unwrap()
            .unwrap();
        let mut b: Vec<[f64; 6]> = shape
            .boxes()
            .iter()
            .map(|b| {
                [
                    b.min(Axis::X) * 16.0,
                    b.min(Axis::Y) * 16.0,
                    b.min(Axis::Z) * 16.0,
                    b.max(Axis::X) * 16.0,
                    b.max(Axis::Y) * 16.0,
                    b.max(Axis::Z) * 16.0,
                ]
            })
            .collect();
        b.sort_by(|a, b| a.partial_cmp(b).unwrap());
        b
    };
    assert_eq!(
        boxes(state("north", "none")),
        [[2.0, 0.0, 2.0, 14.0, 10.0, 14.0]]
    );
    assert_eq!(boxes(state("east", "tray")).len(), 2);
    // Models: every state has one (the template's, from the blockstate the
    // overlay writes when the mod has none).
    horizon_client::assets::RenderAssets::load(&root, &assets).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
