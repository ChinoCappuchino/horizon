//! A world-generation mod as horizon-world's terrain provider (feature
//! `world`; fork `afterwinter`, docs/research/worldgen-hooks.md, M3): the
//! generator asks [`ModTerrain`] for a chunk's biomes and land, and each of
//! its threads gets its own instance of the mod.
use crate::worldgen_runtime::{
    BiomeRequest, Failure, Land, Palette, Region, Step, Worker, World, WorldInfo, WorldgenMod,
};
use horizon_core::position::ChunkPos;
use horizon_world::{
    registry::BlockStateId,
    worldgen::{
        generator::GeneratorData,
        provider::{
            DecorationView, Decorator, DecoratorWorker, ProviderLand, ProviderWorker,
            TerrainProvider,
        },
        structure::template::parse_state_string,
    },
};
use std::sync::{Arc, Mutex};

/// Block states and biomes by the generator's own numbers.
pub struct GeneratorPalette(pub Arc<GeneratorData>);
impl Palette for GeneratorPalette {
    fn state_id(&self, state: &str) -> Result<u32, String> {
        parse_state_string(self.0.registry(), state)
            .map(|id| id.0)
            .ok_or_else(|| format!("no block state {state}"))
    }
    fn is_state(&self, id: u32) -> bool {
        (id as usize) < self.0.registry().len()
    }
    fn biome_id(&self, biome: &str) -> Option<u32> {
        self.0.biome_id(biome).map(|id| id.0)
    }
    fn is_biome(&self, id: u32) -> bool {
        (id as usize) < self.0.biome_names().len()
    }
}

/// A world-generation mod in one world and dimension, as the generator's
/// terrain provider.
pub struct ModTerrain {
    world: World,
}
impl ModTerrain {
    pub fn new(m: &WorldgenMod, data: Arc<GeneratorData>, info: WorldInfo) -> ModTerrain {
        ModTerrain {
            world: m.world(info, Arc::new(GeneratorPalette(data))),
        }
    }
    /// The mod's world: shared cells, their stats, saving and preloading.
    pub fn world(&self) -> &World {
        &self.world
    }
    /// The same mod in the same world, decorating (M3g): its cells are
    /// shared with the terrain.
    pub fn decorator(&self) -> ModDecorator {
        ModDecorator {
            world: self.world.clone(),
        }
    }
}

/// A world-generation mod's part in decoration (M3g): `decorate` in each
/// of vanilla's feature steps, after vanilla's features.
pub struct ModDecorator {
    world: World,
}
impl ModDecorator {
    pub fn new(m: &WorldgenMod, data: Arc<GeneratorData>, info: WorldInfo) -> ModDecorator {
        ModDecorator {
            world: m.world(info, Arc::new(GeneratorPalette(data))),
        }
    }
    pub fn world(&self) -> &World {
        &self.world
    }
}
impl Decorator for ModDecorator {
    fn worker(&self) -> Result<Box<dyn DecoratorWorker>, String> {
        Ok(Box::new(ModWorker(self.world.worker().map_err(text)?)))
    }
}

/// Vanilla's feature steps (`GenerationStep.Decoration`), in order.
const STEPS: [Step; 11] = [
    Step::RawGeneration,
    Step::Lakes,
    Step::LocalModifications,
    Step::UndergroundStructures,
    Step::SurfaceStructures,
    Step::Strongholds,
    Step::UndergroundOres,
    Step::UndergroundDecoration,
    Step::FluidSprings,
    Step::VegetalDecoration,
    Step::TopLayerModification,
];

/// The generator's view of the region, lent to the mod for one call: the
/// runtime keeps its handle only during the call, and the view is taken
/// back (`None`) before the borrow ends, so nothing reads it afterwards.
struct Lent(Mutex<Option<*mut (dyn DecorationView + 'static)>>);
// SAFETY: the pointer is used only while `decorate` runs on the thread that
// lent it, behind the mutex, and is cleared before that call returns.
unsafe impl Send for Lent {}
unsafe impl Sync for Lent {}
impl Lent {
    fn with<T>(&self, f: impl FnOnce(&mut dyn DecorationView) -> Option<T>) -> Option<T> {
        let guard = self.0.lock().ok()?;
        // SAFETY: see the type; `Some` only while the borrow is live.
        let view = unsafe { &mut *(*guard)? };
        f(view)
    }
}
impl Region for Lent {
    fn block(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        self.with(|v| v.block(x, y, z).map(|s| s.0))
    }
    fn height(&self, x: i32, z: i32, surface: bool) -> Option<i32> {
        self.with(|v| v.height(x, z, surface))
    }
}

impl DecoratorWorker for ModWorker {
    fn decorate(
        &mut self,
        chunk: ChunkPos,
        step: usize,
        view: &mut dyn DecorationView,
    ) -> Result<Vec<([i32; 3], BlockStateId)>, String> {
        let step = *STEPS.get(step).ok_or("no such feature step")?;
        // SAFETY: the lifetime is erased only for the call below; `Lent`
        // gives the pointer back (None) before `view`'s borrow ends.
        let raw: *mut (dyn DecorationView + '_) = view;
        let raw: *mut (dyn DecorationView + 'static) = unsafe { std::mem::transmute(raw) };
        let lent = Arc::new(Lent(Mutex::new(Some(raw))));
        let edits = self.0.decorate(chunk.x, chunk.z, step, lent.clone());
        *lent.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(edits
            .map_err(text)?
            .into_iter()
            .map(|e| ([e.x, e.y, e.z], BlockStateId(e.state)))
            .collect())
    }
}
impl TerrainProvider for ModTerrain {
    fn worker(&self) -> Result<Box<dyn ProviderWorker>, String> {
        let worker = self.world.worker().map_err(text)?;
        Ok(Box::new(ModWorker(worker)))
    }
}

struct ModWorker(Worker);
impl ProviderWorker for ModWorker {
    fn biomes(
        &mut self,
        chunk: ChunkPos,
        min_quart_y: i32,
        quarts_y: u32,
    ) -> Result<Vec<u32>, String> {
        // A preset's provider shapes the climate too: vanilla's isn't sampled.
        self.0
            .biomes(&BiomeRequest {
                chunk_x: chunk.x,
                chunk_z: chunk.z,
                min_quart_y,
                quarts_y,
                climate: Vec::new(),
            })
            .map_err(text)
    }
    fn terrain(&mut self, chunk: ChunkPos) -> Result<ProviderLand, String> {
        Ok(match self.0.terrain(chunk.x, chunk.z).map_err(text)? {
            Land::Columns(columns) => ProviderLand::Columns(
                columns
                    .into_iter()
                    .map(|c| {
                        c.runs
                            .into_iter()
                            .map(|r| (BlockStateId(r.state), r.top))
                            .collect()
                    })
                    .collect(),
            ),
            Land::Density(d) => ProviderLand::Density(d),
        })
    }
}

fn text(f: Failure) -> String {
    match f {
        Failure::Refused(why) => why,
        Failure::Stopped(why) => format!("stopped: {why}"),
    }
}
