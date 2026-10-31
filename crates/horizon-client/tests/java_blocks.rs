//! A NeoForge mod's block runs its own code in the game (fork `NeuHorizon`,
//! docs/java-compat/plan.md, phase 0, step 4): Farmer's Delight, from its
//! jar, translated into a content mod, its session installed as the block
//! code hook; a player uses an apple pie through the game's own use path,
//! and the pie's `useWithoutItem` eats a slice. Needs HORIZON_TEST_RESOURCES
//! (a prepared root) and HORIZON_JAVA_MODS (a folder with
//! FarmersDelight-*.jar); skips without them.
use horizon_client::{
    assets::Assets,
    mod_content::{self, ContentMod},
};
use horizon_core::{
    geometry::{Direction, Vec3},
    position::BlockPos,
    random::LegacyRandom,
};
use horizon_world::{
    level::{
        Behaviours, InteractionResult, LevelAccess, LevelEvent, PlayerContext, PlayerEffect,
        ticks::Ticks,
    },
    registry::BlockStateId,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

struct TestLevel {
    blocks: HashMap<BlockPos, BlockStateId>,
    air: BlockStateId,
    random: LegacyRandom,
    events: Vec<LevelEvent>,
}

impl LevelAccess for TestLevel {
    fn block_state(&self, pos: BlockPos) -> BlockStateId {
        self.blocks.get(&pos).copied().unwrap_or(self.air)
    }
    fn set_raw(&mut self, pos: BlockPos, state: BlockStateId) -> Option<BlockStateId> {
        Some(self.blocks.insert(pos, state).unwrap_or(self.air))
    }
    fn is_loaded(&self, _pos: BlockPos) -> bool {
        true
    }
    fn random(&mut self) -> &mut LegacyRandom {
        &mut self.random
    }
    fn game_time(&self) -> i64 {
        0
    }
    fn emit(&mut self, event: LevelEvent) {
        self.events.push(event);
    }
}

#[test]
fn a_pie_eats_a_slice_through_its_own_code() {
    let (Some(base), Some(mods)) = (
        std::env::var_os("HORIZON_TEST_RESOURCES").map(PathBuf::from),
        std::env::var_os("HORIZON_JAVA_MODS").map(PathBuf::from),
    ) else {
        eprintln!("skipping: HORIZON_TEST_RESOURCES or HORIZON_JAVA_MODS is not set");
        return;
    };
    let Some(jar) = std::fs::read_dir(&mods)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("FarmersDelight"))
        })
    else {
        eprintln!("skipping: no FarmersDelight jar");
        return;
    };
    let classes = horizon_neoforge::read_jar(&jar).unwrap();
    let game = Arc::new(horizon_neoforge::GameData::load(&base).unwrap());

    // The mod's content, as a content mod.
    let dir = std::env::temp_dir().join(format!("horizon-java-blocks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let package = dir.join("farmersdelight");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("horizon-mod.toml"),
        "id = \"farmersdelight\"\nname = \"Farmer's Delight\"\nversion = \"1.3.4\"\napi = \"0.2\"\nside = \"both\"\ncapabilities = [\"registries\"]\n",
    )
    .unwrap();
    let t = horizon_neoforge::translate(&classes, game.clone()).unwrap();
    horizon_neoforge::write(&t, &package).unwrap();
    let manifest = horizon_mods::Package::read(&package).unwrap().manifest;
    let content = horizon_mods::Content::read(&package, &manifest).unwrap();
    let root = dir.join("root");
    mod_content::build(
        &base,
        &[ContentMod {
            id: "farmersdelight".into(),
            content,
        }],
        &root,
    )
    .unwrap();
    let assets = Assets::load(&root).unwrap();
    let registry = assets.registry;
    let behaviours = Behaviours::load(registry, &root).unwrap();

    // The mod running, its blocks' code given to the game.
    let session = horizon_neoforge::Session::start(vec![classes], game).unwrap();
    horizon_neoforge::world::install(session, registry, &behaviours);

    let pie = registry
        .default_state("farmersdelight:apple_pie")
        .expect("an apple pie");
    let pos = BlockPos::new(0, 64, 0);
    let mut level = TestLevel {
        blocks: HashMap::from([(pos, pie)]),
        air: registry.default_state("minecraft:air").unwrap(),
        random: LegacyRandom::new(0),
        events: Vec::new(),
    };
    let mut ticks = Ticks::default();
    let player = PlayerContext {
        yaw: 0.0,
        pitch: 0.0,
        may_build: true,
        can_eat: true,
        game_master: false,
    };
    let hit = Vec3::new(0.5, 64.5, 0.5);
    let r = behaviours.use_without_item(&mut level, &mut ticks, pos, Direction::Up, hit, player);
    assert!(r.consumes_action(), "{r:?}");
    let bites = |level: &TestLevel| {
        registry
            .state(level.block_state(pos))
            .and_then(|s| s.properties.get("bites").cloned())
    };
    assert_eq!(bites(&level).as_deref(), Some("1"));
    let ate = level.events.iter().any(|e| {
        matches!(e, LevelEvent::Player { effect: PlayerEffect::Eat { nutrition, .. }, .. } if *nutrition > 0)
    });
    assert!(
        ate,
        "no Eat effect: {:?}",
        level
            .events
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<_>>()
    );
    // A player who cannot eat: the pie stays.
    let full = PlayerContext {
        can_eat: false,
        ..player
    };
    let r = behaviours.use_without_item(&mut level, &mut ticks, pos, Direction::Up, hit, full);
    assert_eq!(r, InteractionResult::Pass);
    assert_eq!(bites(&level).as_deref(), Some("1"));
    let _ = std::fs::remove_dir_all(&dir);
}
