//! World-generation mods (capability `worldgen`, fork `afterwinter`,
//! docs/research/worldgen-hooks.md) in the sandbox: the stand-in mod in
//! `tests/worldgen_guest` answers every stage, misbehaves on request, and
//! its claims settle against another mod's. Needs `rustup target add
//! wasm32-unknown-unknown`.
use horizon_mods::{
    Mods, Package, Status,
    manifest::componentize,
    worldgen::{Candidate, Contributor, Decision, Place, Plan, Rules, Stage},
    worldgen_runtime::{
        BiomeRequest, BlockEdit, Climate, ColumnPos, Failure, Land, Palette, Piece, Region, Step,
        WorldInfo, WorldgenMod,
    },
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
};

const MANIFEST: &str = r##"
id = "test_worldgen"
name = "Test World Generation"
version = "0.1.0"
api = "0.3"
side = "server"
capabilities = ["worldgen"]
experimental = ["worldgen"]

[worldgen]
presets = ["test_worldgen:hills"]
stages = ["climate", "terrain", "structures", "features"]
regions = ["camps"]

[[worldgen.claims]]
name = "camp_sites"
stage = "structures"
action = "reserve"
scope = "region:camps"
priority = 10
"##;

/// The component, built once.
fn wasm() -> &'static [u8] {
    static WASM: OnceLock<Vec<u8>> = OnceLock::new();
    WASM.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/mods-test");
        let status = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
            .args(["-p", "horizon-mods-test-worldgen-guest", "--target-dir"])
            .arg(&target)
            .status()
            .expect("cargo runs");
        assert!(
            status.success(),
            "building the test mod needs `rustup target add wasm32-unknown-unknown`"
        );
        let core = std::fs::read(
            target.join("wasm32-unknown-unknown/release/horizon_mods_test_worldgen_guest.wasm"),
        )
        .unwrap();
        componentize(&core).unwrap()
    })
}

fn package(manifest: &str) -> Package {
    Package {
        manifest: horizon_mods::Manifest::parse(manifest).unwrap(),
        wasm: Some(wasm().to_vec()),
    }
}

fn hills() -> WorldgenMod {
    WorldgenMod::load(&package(MANIFEST)).unwrap()
}

const STATES: [&str; 6] = [
    "minecraft:air",
    "minecraft:stone",
    "minecraft:dirt",
    "minecraft:grass_block",
    "minecraft:oak_log[axis=y]",
    "minecraft:oak_planks",
];
const BIOMES: [&str; 2] = ["minecraft:plains", "minecraft:desert"];

struct TestPalette;
impl Palette for TestPalette {
    fn state_id(&self, state: &str) -> Result<u32, String> {
        let i = STATES.iter().position(|s| *s == state);
        i.map(|i| i as u32).ok_or(format!("no block state {state}"))
    }
    fn is_state(&self, id: u32) -> bool {
        (id as usize) < STATES.len()
    }
    fn biome_id(&self, biome: &str) -> Option<u32> {
        BIOMES.iter().position(|b| *b == biome).map(|i| i as u32)
    }
    fn is_biome(&self, id: u32) -> bool {
        (id as usize) < BIOMES.len()
    }
}

/// Stone up to y 69 everywhere.
struct Flat;
impl Region for Flat {
    fn block(&self, _x: i32, y: i32, _z: i32) -> Option<u32> {
        Some(if y < 70 { 1 } else { 0 })
    }
    fn height(&self, _x: i32, _z: i32, _surface: bool) -> Option<i32> {
        Some(70)
    }
}

fn info(preset: &str, settings: &str, seed: i64) -> WorldInfo {
    WorldInfo {
        seed,
        preset: preset.into(),
        dimension: "minecraft:overworld".into(),
        min_y: -64,
        height: 384,
        sea_level: 63,
        settings: settings.into(),
    }
}

fn worker(m: &WorldgenMod, settings: &str) -> horizon_mods::worldgen_runtime::Worker {
    m.worker(
        info("test_worldgen:hills", settings, 42),
        Arc::new(TestPalette),
    )
    .unwrap()
}

fn refused<T>(r: Result<T, Failure>) -> String {
    match r {
        Err(Failure::Refused(why)) => why,
        Err(Failure::Stopped(why)) => panic!("stopped, not refused: {why}"),
        Ok(_) => panic!("answered, not refused"),
    }
}

/// Every instance gives the same land in any order: the generator may ask
/// any worker for any chunk.
#[test]
fn every_worker_shapes_the_same_land_in_any_order() {
    let m = hills();
    let (mut a, mut b) = (worker(&m, ""), worker(&m, ""));
    assert_eq!(
        a.stages(),
        [
            Stage::Climate,
            Stage::Structures,
            Stage::Features,
            Stage::Terrain
        ]
    );
    let chunks = [(0, 0), (1, -3), (-7, 2), (100, 100)];
    let first: Vec<Land> = chunks
        .iter()
        .map(|&(x, z)| a.terrain(x, z).unwrap())
        .collect();
    let second: Vec<Land> = chunks
        .iter()
        .rev()
        .map(|&(x, z)| b.terrain(x, z).unwrap())
        .collect();
    assert!(first.iter().eq(second.iter().rev()));
    // The heights match the land without generating it.
    let Land::Columns(columns) = &first[0] else {
        panic!("columns")
    };
    let at: Vec<ColumnPos> = (0..16).map(|x| ColumnPos { x, z: 0 }).collect();
    let heights = b.heights(&at).unwrap();
    for (x, h) in heights.iter().enumerate() {
        assert_eq!(*h, columns[x].runs.last().unwrap().top + 1);
        assert!((63..=71).contains(h), "{h}");
    }
    // Another seed, other hills.
    let mut other = m
        .worker(info("test_worldgen:hills", "", 7), Arc::new(TestPalette))
        .unwrap();
    assert_ne!(other.terrain(0, 0).unwrap(), first[0]);
}

#[test]
fn biomes_follow_vanilla_climate_or_fill_the_preset() {
    let mut w = worker(&hills(), "");
    let climate = |temperature| Climate {
        temperature,
        humidity: 0.0,
        continentalness: 0.0,
        erosion: 0.0,
        depth: 0.0,
        weirdness: 0.0,
    };
    let request = |climate: Vec<Climate>| BiomeRequest {
        chunk_x: 0,
        chunk_z: 0,
        min_quart_y: 16,
        quarts_y: 1,
        climate,
    };
    let mixed: Vec<Climate> = (0..16)
        .map(|i| climate([0.9, 0.0, -0.9, 0.0][i % 4]))
        .collect();
    let keep = horizon_mods::worldgen::KEEP_BIOME;
    assert_eq!(w.biomes(&request(mixed)).unwrap()[..4], [1, 0, keep, 0]);
    assert_eq!(w.biomes(&request(Vec::new())).unwrap(), vec![0; 16]);
    // 16 numbers per layer of quarts asked.
    let tall = BiomeRequest {
        quarts_y: 3,
        ..request(Vec::new())
    };
    assert_eq!(w.biomes(&tall).unwrap().len(), 48);
}

#[test]
fn features_read_the_region_and_stay_in_their_area() {
    let m = hills();
    let mut w = worker(&m, "");
    let logs = w
        .decorate(2, -1, Step::VegetalDecoration, Arc::new(Flat))
        .unwrap();
    assert_eq!(logs.len(), 16);
    assert!(logs.iter().all(|e| e.y == 70 && e.state == 4));
    assert_eq!(
        logs[0],
        BlockEdit {
            x: 32,
            y: 70,
            z: -16,
            state: 4
        }
    );
    assert!(
        w.decorate(2, -1, Step::UndergroundOres, Arc::new(Flat))
            .unwrap()
            .is_empty()
    );
    // A stage it didn't open.
    let why = refused(w.decorate(0, 0, Step::Surface, Arc::new(Flat)));
    assert!(why.contains("opened no stage surface"), "{why}");
    let mut out = worker(&m, "outside");
    let why = refused(out.decorate(0, 0, Step::VegetalDecoration, Arc::new(Flat)));
    assert!(why.contains("(40, 63, 0) is outside its area"), "{why}");
}

#[test]
fn structures_start_in_the_mods_namespaces_and_regions_are_its_own() {
    let m = hills();
    let mut w = worker(&m, "");
    let starts = w.starts(0, 0).unwrap();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].id, "test_worldgen:camp");
    assert!(matches!(&starts[0].pieces[0], Piece::Blocks(b) if b.len() == 9));
    assert!(w.starts(1, 0).unwrap().is_empty());
    assert_eq!(w.regions(0, 5).unwrap(), ["camps"]);
    assert!(w.regions(1, 5).unwrap().is_empty());
    let why = refused(worker(&m, "foreign").starts(0, 0));
    assert!(
        why.contains("minecraft:village is not in the mod's own namespaces"),
        "{why}"
    );
}

/// A bad answer costs the mod that chunk; the next call goes on.
#[test]
fn a_bad_answer_is_refused_and_the_mod_goes_on() {
    let m = hills();
    let mut w = worker(&m, "runs");
    let why = refused(w.terrain(0, 0));
    assert!(
        why.starts_with("terrain: column (3, 7): runs must ascend"),
        "{why}"
    );
    assert!(w.heights(&[ColumnPos { x: 0, z: 0 }]).is_ok());
    assert_eq!(w.stopped(), None);
    let why = refused(worker(&m, "nan").terrain(0, 0));
    assert_eq!(why, "terrain: density 0 is not a number");
}

/// A trap or an endless loop stops that instance for good, and no other.
#[test]
fn a_trap_or_an_endless_loop_stops_the_instance() {
    let m = hills();
    for (settings, why) in [("panic", "a panic"), ("spin", "too long")] {
        let mut w = worker(&m, settings);
        match w.terrain(0, 0) {
            Err(Failure::Stopped(reason)) => assert!(reason.contains(why), "{reason}"),
            other => panic!("{settings}: {other:?}"),
        }
        assert!(matches!(w.heights(&[]), Err(Failure::Stopped(_))));
        assert!(w.stopped().is_some());
    }
    assert!(worker(&m, "").terrain(0, 0).is_ok());
}

#[test]
fn open_is_held_to_the_manifest() {
    let m = hills();
    let why = refused(m.worker(
        info("test_worldgen:hills", "stage", 1),
        Arc::new(TestPalette),
    ));
    assert!(
        why.contains("stage surface, missing from the manifest"),
        "{why}"
    );
    // In vanilla's preset it adds, and doesn't shape the land.
    let mut normal = m
        .worker(info("minecraft:normal", "", 1), Arc::new(TestPalette))
        .unwrap();
    assert!(!normal.stages().contains(&Stage::Terrain));
    assert!(refused(normal.terrain(0, 0)).contains("opened no stage terrain"));
    // Terrain only for the presets the manifest lists.
    let elsewhere = MANIFEST.replace("[\"test_worldgen:hills\"]", "[\"test_worldgen:flat\"]");
    let why = refused(
        WorldgenMod::load(&package(&elsewhere))
            .unwrap()
            .worker(info("test_worldgen:hills", "", 1), Arc::new(TestPalette)),
    );
    assert!(
        why.contains("not one of the manifest's worldgen.presets"),
        "{why}"
    );
    // A palette without the mod's blocks: open errs, the mod has no part.
    struct Empty;
    impl Palette for Empty {
        fn state_id(&self, state: &str) -> Result<u32, String> {
            Err(format!("no block state {state}"))
        }
        fn is_state(&self, _: u32) -> bool {
            false
        }
        fn biome_id(&self, _: &str) -> Option<u32> {
            None
        }
        fn is_biome(&self, _: u32) -> bool {
            false
        }
    }
    let why = refused(m.worker(info("test_worldgen:hills", "", 1), Arc::new(Empty)));
    assert_eq!(why, "no block state minecraft:stone");
}

/// A world-generation mod needs the level its functions arrived at.
#[test]
fn it_needs_the_worldgen_level() {
    use horizon_mods::api_level::{LEVEL, imports, needed};
    let functions = imports(wasm()).unwrap();
    assert_eq!(needed(functions.iter().map(String::as_str)).0, LEVEL);
    assert!(
        functions
            .iter()
            .all(|f| f.starts_with("worldgen.") || f.starts_with("log."))
    );
}

/// The game's hooks leave a world-generation mod to the generator.
#[test]
fn the_game_leaves_it_to_the_generator() {
    let dir = std::env::temp_dir().join(format!("horizon-worldgen-mods-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("test_worldgen.hmod");
    std::fs::write(&path, Package::write(MANIFEST, wasm()).unwrap()).unwrap();
    let mods = Mods::load(&[path], |_| true, |_| false);
    assert_eq!(mods.entries()[0].status, Status::Running);
    assert!(!mods.is_running() && !mods.runs_server());
    let plain = MANIFEST.replace("capabilities = [\"worldgen\"]", "capabilities = []");
    assert!(
        Package::write(&plain, wasm()).is_err(),
        "[worldgen] needs the capability"
    );
}

/// The mod's reserve keeps its camps free of another mod's structures, by
/// the regions its own code computes.
#[test]
fn its_regions_decide_its_claims() {
    let m = hills();
    let mut w = worker(&m, "");
    let towns = horizon_mods::Manifest::parse(
        "id = \"t_and_t\"\nname = \"T\"\nversion = \"1\"\napi = \"0.2\"\ncapabilities = [\"registries\"]\nnamespaces = [\"towns_and_towers\"]\n",
    )
    .unwrap();
    let plan = Plan::new(
        &[
            Contributor::of(&towns),
            Contributor::of(&package(MANIFEST).manifest),
        ],
        &Rules::default(),
    );
    let village = Candidate {
        owner: "t_and_t",
        id: "towns_and_towers:village",
        tags: &[],
    };
    let decide = |w: &mut horizon_mods::worldgen_runtime::Worker, x: i32| {
        let names = w.regions(x, 0).unwrap();
        let regions: Vec<(&str, &str)> = names
            .iter()
            .map(|r| ("test_worldgen", r.as_str()))
            .collect();
        let place = Place {
            preset: "test_worldgen:hills",
            biome: "minecraft:plains",
            biome_tags: &[],
            regions: &regions,
        };
        plan.decide(Stage::Structures, &place, &village)
    };
    assert_eq!(
        decide(&mut w, 0),
        Decision::Drop {
            by: "test_worldgen/camp_sites".into()
        }
    );
    assert_eq!(decide(&mut w, 1), Decision::Keep { by: None });
    assert!(
        plan.report().contains(
            "| test_worldgen/camp_sites | structures | reserve | region:camps | * | 10 |"
        )
    );
}
