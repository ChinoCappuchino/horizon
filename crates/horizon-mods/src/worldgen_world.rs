//! A world-generation mod as horizon-world's terrain provider (feature
//! `world`; fork `afterwinter`, docs/research/worldgen-hooks.md, M3): the
//! generator asks [`ModTerrain`] for a chunk's biomes and land, and each of
//! its threads gets its own instance of the mod.
use crate::worldgen_runtime::{
    BiomeRequest, Failure, Land, Palette, Worker, World, WorldInfo, WorldgenMod,
};
use horizon_core::position::ChunkPos;
use horizon_world::{
    registry::BlockStateId,
    worldgen::{
        generator::GeneratorData,
        provider::{ProviderLand, ProviderWorker, TerrainProvider},
        structure::template::parse_state_string,
    },
};
use std::sync::Arc;

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
