//! Terrain and biomes from elsewhere: a world-generation mod's, for the
//! presets it provides (fork `NeuHorizon`, docs/research/worldgen-hooks.md,
//! M3). The generator asks a [`TerrainProvider`] for a chunk's biomes and
//! land instead of sampling its noise router. The land is final: no
//! aquifers, surface rules or carvers run over it (a provider makes its own
//! caves and tops); structures and features run as for any world. The game implements the trait (over
//! horizon-mods' sandbox); this crate only knows the trait.
use crate::registry::BlockStateId;
use horizon_core::position::ChunkPos;

/// A biome number that keeps what vanilla's biome source puts there.
pub const KEEP_BIOME: u32 = u32::MAX;

/// A provider for one world and dimension, shared by the generator's threads.
/// A world-generation mod's part in decoration (fork `NeuHorizon`, M3g):
/// after vanilla's features of each step, the blocks the mod sets around
/// the chunk being decorated. Any world, not only a mod's own preset.
pub trait Decorator: Send + Sync {
    /// One worker per generator worker thread.
    fn worker(&self) -> Result<Box<dyn DecoratorWorker>, String>;
}
/// What a decorator reads: the chunk being decorated and its eight
/// neighbours as generated so far.
pub trait DecorationView {
    fn block(&mut self, x: i32, y: i32, z: i32) -> Option<BlockStateId>;
    /// `surface`: vanilla's `WORLD_SURFACE`, else `OCEAN_FLOOR`; the y above
    /// the highest such block.
    fn height(&mut self, x: i32, z: i32, surface: bool) -> Option<i32>;
}
pub trait DecoratorWorker: Send {
    /// The blocks to set in feature step `step` (vanilla's
    /// `GenerationStep.Decoration` order, 0 to 10) of `chunk`: in the chunk
    /// and up to 16 blocks into its neighbours. An error leaves the chunk
    /// without the mod's part in this step.
    fn decorate(
        &mut self,
        chunk: ChunkPos,
        step: usize,
        view: &mut dyn DecorationView,
    ) -> Result<Vec<([i32; 3], BlockStateId)>, String>;
}

pub trait TerrainProvider: Send + Sync {
    /// A handle for one generation thread (the provider's own instance).
    fn worker(&self) -> Result<Box<dyn ProviderWorker>, String>;
}

/// One thread's handle on a [`TerrainProvider`].
pub trait ProviderWorker: Send {
    /// A biome number (the generator's `BiomeId`) for each quart of the
    /// chunk from `min_quart_y`, `quarts_y` high: x fastest, then z, then y;
    /// [`KEEP_BIOME`] for vanilla's.
    fn biomes(
        &mut self,
        chunk: ChunkPos,
        min_quart_y: i32,
        quarts_y: u32,
    ) -> Result<Vec<u32>, String>;
    /// The chunk's land.
    fn terrain(&mut self, chunk: ChunkPos) -> Result<ProviderLand, String>;
}

/// A chunk's land as a provider shapes it.
pub enum ProviderLand {
    /// 256 columns, x fastest, then z: runs of one state up to and
    /// including `top`, bottom up, ascending; air above the last.
    Columns(Vec<Vec<(BlockStateId, i32)>>),
    /// Density at the corners of 4 × 8 × 4 cells: 5 × 5 × (height / 8 + 1)
    /// values, x fastest, then z, then y, from the level's lowest y. Above 0
    /// is the default block; elsewhere, the default fluid up to sea level.
    Density(Vec<f32>),
}

/// Columns of runs, the form the generator fills and queries a provider's
/// land in.
pub(super) type Runs = Vec<Vec<(BlockStateId, i32)>>;

/// A provider's land as runs per column: density interpolated between its
/// corners as vanilla interpolates its noise cells.
pub(super) fn runs(
    land: ProviderLand,
    min_y: i32,
    height: i32,
    sea_level: i32,
    [block, fluid, air]: [BlockStateId; 3],
) -> Result<Runs, String> {
    match land {
        ProviderLand::Columns(columns) => {
            if columns.len() != 256 {
                return Err(format!("{} columns, not 256", columns.len()));
            }
            Ok(columns)
        }
        ProviderLand::Density(d) => {
            let ny = (height / 8 + 1) as usize;
            if d.len() != 25 * ny {
                return Err(format!("{} densities, not {}", d.len(), 25 * ny));
            }
            let at = |x: usize, y: usize, z: usize| d[(y * 5 + z) * 5 + x];
            let mut out = Vec::with_capacity(256);
            for z in 0..16usize {
                for x in 0..16usize {
                    let (cx, fx) = (x / 4, (x % 4) as f32 / 4.0);
                    let (cz, fz) = (z / 4, (z % 4) as f32 / 4.0);
                    let mut runs: Vec<(BlockStateId, i32)> = Vec::new();
                    for dy in 0..height {
                        let (cy, fy) = ((dy / 8) as usize, (dy % 8) as f32 / 8.0);
                        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
                        let plane = |y: usize| {
                            lerp(
                                lerp(at(cx, y, cz), at(cx + 1, y, cz), fx),
                                lerp(at(cx, y, cz + 1), at(cx + 1, y, cz + 1), fx),
                                fz,
                            )
                        };
                        let value = lerp(plane(cy), plane((cy + 1).min(ny - 1)), fy);
                        let y = min_y + dy;
                        let state = if value > 0.0 {
                            block
                        } else if y <= sea_level {
                            fluid
                        } else {
                            air
                        };
                        // Runs are contiguous: air inside the column is a run too.
                        match runs.last_mut() {
                            Some((last, top)) if *last == state => *top = y,
                            _ => runs.push((state, y)),
                        }
                    }
                    if runs.last().is_some_and(|&(state, _)| state == air) {
                        runs.pop();
                    }
                    out.push(runs);
                }
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: BlockStateId = BlockStateId(1);
    const FLUID: BlockStateId = BlockStateId(2);
    const AIR: BlockStateId = BlockStateId(0);

    /// Density from a function of the corner's y index, the same in x and z.
    fn density(height: i32, f: impl Fn(usize) -> f32) -> Vec<f32> {
        let ny = (height / 8 + 1) as usize;
        (0..25 * ny).map(|i| f(i / 25)).collect()
    }

    #[test]
    fn density_is_interpolated_into_contiguous_runs() {
        // Solid below y 0 (corner 8), open above, sea at 63: between corners 8
        // and 9 the density falls from 1 to -1, so y 0 to 3 stay solid.
        let d = density(384, |y| if y <= 8 { 1.0 } else { -1.0 });
        let columns = runs(ProviderLand::Density(d), -64, 384, 63, [BLOCK, FLUID, AIR]).unwrap();
        assert_eq!(columns.len(), 256);
        assert!(columns.iter().all(|c| *c == [(BLOCK, 3), (FLUID, 63)]));
        // A cave: open between corners 4 and 6 (y -32 to -16), solid elsewhere up to y 0.
        let d = density(384, |y| match y {
            5 => -1.0,
            0..=8 => 1.0,
            _ => -1.0,
        });
        // Below sea level the cave would be the fluid; with the sea under it, air.
        let column = &runs(
            ProviderLand::Density(d),
            -64,
            384,
            -100,
            [BLOCK, FLUID, AIR],
        )
        .unwrap()[0];
        assert_eq!(column[0].0, BLOCK);
        assert_eq!(
            column[1].0, AIR,
            "the cave is an air run, not a gap: {column:?}"
        );
        assert_eq!(column[2].0, BLOCK);
        assert!(column.windows(2).all(|w| w[0].1 < w[1].1), "runs ascend");
    }

    #[test]
    fn wrong_sizes_are_refused() {
        assert!(
            runs(
                ProviderLand::Columns(vec![vec![]; 255]),
                -64,
                384,
                63,
                [BLOCK, FLUID, AIR]
            )
            .is_err()
        );
        assert!(
            runs(
                ProviderLand::Density(vec![0.0; 24]),
                -64,
                384,
                63,
                [BLOCK, FLUID, AIR]
            )
            .is_err()
        );
    }
}
