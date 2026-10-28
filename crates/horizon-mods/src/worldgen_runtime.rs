//! Running world-generation mods (capability `worldgen`; fork `afterwinter`,
//! docs/research/worldgen-hooks.md). Such a mod runs apart from the game's
//! and the server's hooks: the generator makes a [`Worker`] per generation
//! thread from a [`WorldgenMod`], each its own instance opened for one world
//! and dimension, and checks every answer before it uses one.
use crate::{
    Capability, Package,
    worldgen::{KEEP_BIOME, Section, Stage, is_id},
};
use std::sync::Arc;
use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{Component, HasSelf, Linker},
};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../horizon-mod-sdk/wit",
        world: "horizon-worldgen-mod",
        additional_derives: [PartialEq],
    });
}
pub use bindings::exports::horizon::mods::worldgen_events::{
    BiomeRequest, BlockEdit, Climate, Column, ColumnPos, Land, Piece, Rotation, Run,
    Stage as WitStage, Step, StructureStart, TemplatePiece, WorldInfo,
};
use bindings::{
    HorizonWorldgenMod,
    horizon::mods::{log as mod_log, worldgen},
};

/// An instance's linear memory cap: a terrain provider keeps its regional
/// maps in it, once per generation worker.
const MEMORY: usize = 256 << 20;
/// Fuel (about one unit per instruction) for `open`, where a provider may
/// build its maps, and for each call after it.
const FUEL_OPEN: u64 = 4_000_000_000;
const FUEL_CALL: u64 = 1_000_000_000;
/// The most columns one `heights` call asks for.
pub const HEIGHT_COLUMNS: usize = 4096;
/// The most blocks one `decorate` answer, or one chunk's structure starts, set.
pub const EDITS: usize = 65536;
/// Structure starts in one chunk, and pieces in one start.
pub const STARTS: usize = 64;
pub const PIECES: usize = 1024;
/// Log lines one call may write: a mod runs on every worker.
const LOG_LINES: u32 = 4;

/// The game's numbers for block states and biomes, shared by the workers.
pub trait Palette: Send + Sync {
    fn state_id(&self, state: &str) -> Result<u32, String>;
    fn is_state(&self, id: u32) -> bool;
    fn biome_id(&self, biome: &str) -> Option<u32>;
    fn is_biome(&self, id: u32) -> bool;
}

/// What a `decorate` call reads: the chunk and its eight neighbours as
/// generated so far; none elsewhere.
pub trait Region: Send + Sync {
    fn block(&self, x: i32, y: i32, z: i32) -> Option<u32>;
    /// `surface`: vanilla's `WORLD_SURFACE`, else `OCEAN_FLOOR`.
    fn height(&self, x: i32, z: i32, surface: bool) -> Option<i32>;
}

/// Why a call gave nothing to use.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// The mod erred, or its answer broke the contract: the chunk goes
    /// without the mod's part.
    Refused(String),
    /// It trapped or ran over its budget: it is stopped for this world.
    Stopped(String),
}

fn stage(s: WitStage) -> Stage {
    match s {
        WitStage::Climate => Stage::Climate,
        WitStage::Terrain => Stage::Terrain,
        WitStage::Surface => Stage::Surface,
        WitStage::Structures => Stage::Structures,
        WitStage::Features => Stage::Features,
    }
}

/// A world-generation mod, compiled once and shared by the workers.
pub struct WorldgenMod {
    id: String,
    namespaces: Vec<String>,
    section: Section,
    engine: Engine,
    linker: Linker<State>,
    component: Component,
}

impl WorldgenMod {
    pub fn load(package: &Package) -> Result<WorldgenMod, String> {
        let m = &package.manifest;
        if !m.capabilities.contains(&Capability::Worldgen) {
            return Err("not a world-generation mod: it has no capability worldgen".into());
        }
        let wasm = package
            .wasm
            .as_ref()
            .ok_or("a world-generation mod needs code")?;
        let mut config = Config::new();
        // The same answer on every machine: NaNs come out canonical, and
        // relaxed SIMD takes its deterministic lowering.
        config
            .wasm_component_model(true)
            .consume_fuel(true)
            .cranelift_nan_canonicalization(true)
            .relaxed_simd_deterministic(true);
        let engine = Engine::new(&config).map_err(|e| e.to_string())?;
        let component =
            Component::new(&engine, wasm).map_err(|e| format!("not a component: {e}"))?;
        let imports: Vec<String> = component
            .component_type()
            .imports(&engine)
            .map(|(name, _)| name.to_string())
            .collect();
        m.check_imports(imports.iter().map(String::as_str))?;
        if let Some(other) = imports.iter().find(|i| {
            !i.starts_with("horizon:mods/log@") && !i.starts_with("horizon:mods/worldgen@")
        }) {
            return Err(format!(
                "a world-generation mod imports only log and worldgen, not {other}"
            ));
        }
        let functions = crate::api_level::imports(wasm)?;
        let (level, function) = crate::api_level::needed(functions.iter().map(String::as_str));
        if let Some(why) = crate::api_level::too_new(level, function) {
            return Err(why);
        }
        let mut linker = Linker::new(&engine);
        HorizonWorldgenMod::add_to_linker::<State, HasSelf<State>>(&mut linker, |s| s)
            .map_err(|e| e.to_string())?;
        Ok(WorldgenMod {
            id: m.id.clone(),
            namespaces: m.owned_namespaces().map(str::to_owned).collect(),
            section: m.worldgen.clone().unwrap_or_default(),
            engine,
            linker,
            component,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// A new instance, opened for the world `info` describes.
    pub fn worker(&self, info: WorldInfo, palette: Arc<dyn Palette>) -> Result<Worker, Failure> {
        let state = State {
            id: self.id.clone(),
            palette,
            region: None,
            logged: 0,
            limits: StoreLimitsBuilder::new()
                .memory_size(MEMORY)
                .instances(16)
                .build(),
        };
        let stopped = |e: wasmtime::Error| Failure::Stopped(crate::runtime::reason(&e));
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(FUEL_OPEN).map_err(stopped)?;
        let instance = self
            .linker
            .instantiate(&mut store, &self.component)
            .map_err(stopped)?;
        let exports = HorizonWorldgenMod::new(&mut store, &instance).map_err(stopped)?;
        let contribution = exports
            .horizon_mods_worldgen_events()
            .call_open(&mut store, &info)
            .map_err(stopped)?
            .map_err(Failure::Refused)?;
        let mut stages = Vec::new();
        for s in contribution.stages.into_iter().map(stage) {
            if !self.section.stages.contains(&s) {
                return Err(Failure::Refused(format!(
                    "open names stage {}, missing from the manifest's worldgen.stages",
                    s.name()
                )));
            }
            if !stages.contains(&s) {
                stages.push(s);
            }
        }
        if stages.contains(&Stage::Terrain) && !self.section.presets.contains(&info.preset) {
            return Err(Failure::Refused(format!(
                "open names stage terrain for preset {}, not one of the manifest's worldgen.presets",
                info.preset
            )));
        }
        Ok(Worker {
            store,
            exports,
            info,
            stages,
            regions: self.section.regions.clone(),
            namespaces: self.namespaces.clone(),
            stopped: None,
        })
    }
}

/// One instance of a world-generation mod, for one generation thread.
pub struct Worker {
    store: Store<State>,
    exports: HorizonWorldgenMod,
    info: WorldInfo,
    stages: Vec<Stage>,
    regions: Vec<String>,
    namespaces: Vec<String>,
    stopped: Option<String>,
}

impl Worker {
    /// The stages `open` named.
    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }
    /// Why the mod was stopped, once it is.
    pub fn stopped(&self) -> Option<&str> {
        self.stopped.as_deref()
    }

    /// One call: refused when the mod opened no such stage or erred,
    /// stopped for good when it traps or runs over its budget.
    fn call<R>(
        &mut self,
        stage: Option<Stage>,
        f: impl FnOnce(&HorizonWorldgenMod, &mut Store<State>) -> wasmtime::Result<Result<R, String>>,
    ) -> Result<R, Failure> {
        if let Some(why) = &self.stopped {
            return Err(Failure::Stopped(why.clone()));
        }
        if let Some(s) = stage
            && !self.stages.contains(&s)
        {
            return Err(Failure::Refused(format!(
                "the mod opened no stage {}",
                s.name()
            )));
        }
        self.store.data_mut().logged = 0;
        let result = self
            .store
            .set_fuel(FUEL_CALL)
            .and_then(|()| f(&self.exports, &mut self.store));
        match result {
            Ok(answer) => answer.map_err(Failure::Refused),
            Err(e) => {
                let why = crate::runtime::reason(&e);
                log::warn!(
                    "Horizon: world-generation mod {} stopped: {why}",
                    self.store.data().id
                );
                self.stopped = Some(why.clone());
                Err(Failure::Stopped(why))
            }
        }
    }

    /// Stage `climate`: a biome number per quart, [`KEEP_BIOME`] for none.
    pub fn biomes(&mut self, request: &BiomeRequest) -> Result<Vec<u32>, Failure> {
        let ids = self.call(Some(Stage::Climate), |m, s| {
            m.horizon_mods_worldgen_events().call_biomes(s, request)
        })?;
        let want = 16 * request.quarts_y as usize;
        if ids.len() != want {
            return refused(format!("biomes: {} numbers for {want} quarts", ids.len()));
        }
        let palette = &self.store.data().palette;
        if let Some((i, id)) = ids
            .iter()
            .enumerate()
            .find(|&(_, &id)| id != KEEP_BIOME && !palette.is_biome(id))
        {
            return refused(format!(
                "biomes: quart {i} has {id}, not a biome of this world"
            ));
        }
        Ok(ids)
    }

    /// Stage `terrain`: the chunk's land.
    pub fn terrain(&mut self, chunk_x: i32, chunk_z: i32) -> Result<Land, Failure> {
        let land = self.call(Some(Stage::Terrain), |m, s| {
            m.horizon_mods_worldgen_events()
                .call_terrain(s, chunk_x, chunk_z)
        })?;
        check_land(&self.info, &land, &*self.store.data().palette).map_err(Failure::Refused)?;
        Ok(land)
    }

    /// Stage `terrain`: column heights without generating.
    pub fn heights(&mut self, columns: &[ColumnPos]) -> Result<Vec<i32>, Failure> {
        if columns.len() > HEIGHT_COLUMNS {
            return refused(format!("heights: at most {HEIGHT_COLUMNS} columns a call"));
        }
        let heights = self.call(Some(Stage::Terrain), |m, s| {
            m.horizon_mods_worldgen_events().call_heights(s, columns)
        })?;
        if heights.len() != columns.len() {
            return refused(format!(
                "heights: {} for {} columns",
                heights.len(),
                columns.len()
            ));
        }
        let (bottom, top) = (self.info.min_y, self.info.min_y + self.info.height as i32);
        if let Some(h) = heights.iter().find(|h| !(bottom..=top).contains(*h)) {
            return refused(format!(
                "heights: {h} is outside the world ({bottom} to {top})"
            ));
        }
        Ok(heights)
    }

    /// Stages `surface` and `features`: the blocks the mod sets in one step,
    /// reading `region`.
    pub fn decorate(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        step: Step,
        region: Arc<dyn Region>,
    ) -> Result<Vec<BlockEdit>, Failure> {
        let stage = match step {
            Step::Surface | Step::Carvers => Stage::Surface,
            _ => Stage::Features,
        };
        self.store.data_mut().region = Some(region);
        let edits = self.call(Some(stage), |m, s| {
            m.horizon_mods_worldgen_events()
                .call_decorate(s, chunk_x, chunk_z, step)
        });
        self.store.data_mut().region = None;
        let edits = edits?;
        check_edits(
            &self.info,
            chunk_x,
            chunk_z,
            step,
            &edits,
            &*self.store.data().palette,
        )
        .map_err(Failure::Refused)?;
        Ok(edits)
    }

    /// Stage `structures`: the starts in the chunk.
    pub fn starts(&mut self, chunk_x: i32, chunk_z: i32) -> Result<Vec<StructureStart>, Failure> {
        let starts = self.call(Some(Stage::Structures), |m, s| {
            m.horizon_mods_worldgen_events()
                .call_starts(s, chunk_x, chunk_z)
        })?;
        check_starts(&self.namespaces, &starts, &*self.store.data().palette)
            .map_err(Failure::Refused)?;
        Ok(starts)
    }

    /// The mod's regions the chunk lies in, for its claims.
    pub fn regions(&mut self, chunk_x: i32, chunk_z: i32) -> Result<Vec<String>, Failure> {
        if self.regions.is_empty() {
            return Ok(Vec::new());
        }
        let names = self.call(None, |m, s| {
            m.horizon_mods_worldgen_events()
                .call_regions(s, chunk_x, chunk_z)
        })?;
        if let Some(n) = names.iter().find(|n| !self.regions.contains(n)) {
            return refused(format!(
                "regions: {n} is not in the manifest's worldgen.regions"
            ));
        }
        Ok(names)
    }
}

fn refused<T>(why: String) -> Result<T, Failure> {
    Err(Failure::Refused(why))
}

fn check_land(info: &WorldInfo, land: &Land, palette: &dyn Palette) -> Result<(), String> {
    let top = info.min_y + info.height as i32 - 1;
    match land {
        Land::Density(d) => {
            let want = 25 * (info.height as usize / 8 + 1);
            if d.len() != want {
                return Err(format!("terrain: {} densities, not {want}", d.len()));
            }
            if let Some(i) = d.iter().position(|v| !v.is_finite()) {
                return Err(format!("terrain: density {i} is not a number"));
            }
        }
        Land::Columns(columns) => {
            if columns.len() != 256 {
                return Err(format!("terrain: {} columns, not 256", columns.len()));
            }
            for (i, c) in columns.iter().enumerate() {
                let (x, z) = (i % 16, i / 16);
                let mut below = info.min_y - 1;
                for r in &c.runs {
                    if r.top <= below {
                        return Err(format!(
                            "terrain: column ({x}, {z}): runs must ascend (top {} after {below})",
                            r.top
                        ));
                    }
                    if r.top > top {
                        return Err(format!(
                            "terrain: column ({x}, {z}): top {} is above the world ({top})",
                            r.top
                        ));
                    }
                    if !palette.is_state(r.state) {
                        return Err(format!(
                            "terrain: column ({x}, {z}): {} is not a block state",
                            r.state
                        ));
                    }
                    below = r.top;
                }
            }
        }
    }
    Ok(())
}

fn check_edits(
    info: &WorldInfo,
    chunk_x: i32,
    chunk_z: i32,
    step: Step,
    edits: &[BlockEdit],
    palette: &dyn Palette,
) -> Result<(), String> {
    if edits.len() > EDITS {
        return Err(format!("decorate: {} blocks, at most {EDITS}", edits.len()));
    }
    // Surface and carvers stay in the chunk; features reach 16 blocks out.
    let (lo, hi) = match step {
        Step::Surface | Step::Carvers => (0, 15),
        _ => (-16, 31),
    };
    let (x0, z0) = (i64::from(chunk_x) * 16, i64::from(chunk_z) * 16);
    let ys = info.min_y..info.min_y + info.height as i32;
    for e in edits {
        let inside = (lo..=hi).contains(&(i64::from(e.x) - x0))
            && (lo..=hi).contains(&(i64::from(e.z) - z0))
            && ys.contains(&e.y);
        if !inside {
            return Err(format!(
                "decorate {step:?}: the block at ({}, {}, {}) is outside its area",
                e.x, e.y, e.z
            ));
        }
        if !palette.is_state(e.state) {
            return Err(format!(
                "decorate {step:?}: {} is not a block state",
                e.state
            ));
        }
    }
    Ok(())
}

fn check_starts(
    namespaces: &[String],
    starts: &[StructureStart],
    palette: &dyn Palette,
) -> Result<(), String> {
    if starts.len() > STARTS {
        return Err(format!(
            "starts: {} in one chunk, at most {STARTS}",
            starts.len()
        ));
    }
    let mut blocks = 0;
    for s in starts {
        let owned =
            s.id.split_once(':')
                .is_some_and(|(ns, _)| namespaces.iter().any(|n| n == ns));
        if !is_id(&s.id) || !owned {
            return Err(format!(
                "starts: {} is not in the mod's own namespaces",
                s.id
            ));
        }
        if s.min_x > s.max_x || s.min_y > s.max_y || s.min_z > s.max_z {
            return Err(format!("starts: {}'s box is inside out", s.id));
        }
        if s.pieces.len() > PIECES {
            return Err(format!(
                "starts: {} has {} pieces, at most {PIECES}",
                s.id,
                s.pieces.len()
            ));
        }
        for p in &s.pieces {
            match p {
                Piece::Template(t) if !is_id(&t.template) => {
                    return Err(format!(
                        "starts: {}: template {:?} is not an id",
                        s.id, t.template
                    ));
                }
                Piece::Template(_) => {}
                Piece::Blocks(edits) => {
                    blocks += edits.len();
                    for e in edits {
                        let inside = (s.min_x..=s.max_x).contains(&e.x)
                            && (s.min_y..=s.max_y).contains(&e.y)
                            && (s.min_z..=s.max_z).contains(&e.z);
                        if !inside {
                            return Err(format!(
                                "starts: {}: the block at ({}, {}, {}) is outside its box",
                                s.id, e.x, e.y, e.z
                            ));
                        }
                        if !palette.is_state(e.state) {
                            return Err(format!(
                                "starts: {}: {} is not a block state",
                                s.id, e.state
                            ));
                        }
                    }
                }
            }
        }
    }
    if blocks > EDITS {
        return Err(format!(
            "starts: {blocks} blocks in one chunk's pieces, at most {EDITS}"
        ));
    }
    Ok(())
}

struct State {
    id: String,
    palette: Arc<dyn Palette>,
    region: Option<Arc<dyn Region>>,
    logged: u32,
    limits: StoreLimits,
}

impl mod_log::Host for State {
    fn write(&mut self, level: mod_log::Level, message: String) {
        if self.logged >= LOG_LINES {
            return;
        }
        self.logged += 1;
        let level = match level {
            mod_log::Level::Debug => log::Level::Debug,
            mod_log::Level::Info => log::Level::Info,
            mod_log::Level::Warn => log::Level::Warn,
            mod_log::Level::Error => log::Level::Error,
        };
        let message: String = message.chars().take(256).collect();
        log::log!(level, "[mod {}] {message}", self.id);
    }
}

impl worldgen::Host for State {
    fn state_id(&mut self, state: String) -> Result<u32, String> {
        self.palette.state_id(&state)
    }
    fn biome_id(&mut self, biome: String) -> Option<u32> {
        self.palette.biome_id(&biome)
    }
    fn region_block(&mut self, x: i32, y: i32, z: i32) -> Option<u32> {
        self.region.as_ref()?.block(x, y, z)
    }
    fn region_height(&mut self, x: i32, z: i32, surface: bool) -> Option<i32> {
        self.region.as_ref()?.height(x, z, surface)
    }
}
