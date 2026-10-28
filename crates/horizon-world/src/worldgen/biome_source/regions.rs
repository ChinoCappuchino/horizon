//! Climate regions (fork `afterwinter`, docs/research/climate-regions.md): a
//! world's multi-noise biomes split between weighted parameter lists, as
//! TerraBlender does for the mods that use it. Each region is a full list (a
//! biome for every climate); every position belongs to one region, and that
//! region's list picks the biome by vanilla's rule.
//!
//! Which region a position belongs to is Horizon's own function, matched to
//! NeoForge 1.21.1 by observation: regions cover areas in proportion to their
//! weights, in patches that stop being alike about 768 blocks apart. Here a
//! jittered grid of points, [`CELL`] quarts apart, splits the world into
//! cells (each position takes its nearest point's cell), and each cell picks
//! a region by weight from the world seed.
use crate::worldgen::biome::MultiNoiseBiomeSource;

/// The grid's spacing, in quarts: 576 blocks.
pub const CELL: i32 = 144;
/// The weight of vanilla's own list, as TerraBlender's default.
pub const VANILLA_WEIGHT: u32 = 10;

#[derive(Debug)]
pub struct ClimateRegions {
    seed: u64,
    /// (name, weight, list); vanilla's first, then the mods' by name.
    regions: Vec<(String, u32, MultiNoiseBiomeSource)>,
    total: u64,
}

impl ClimateRegions {
    /// `regions` in the order they keep; weights of 0 never place a region.
    pub fn new(seed: i64, regions: Vec<(String, u32, MultiNoiseBiomeSource)>) -> Self {
        let total = regions.iter().map(|(_, w, _)| u64::from(*w)).sum();
        ClimateRegions {
            seed: seed as u64,
            regions,
            total,
        }
    }
    pub fn regions(&self) -> &[(String, u32, MultiNoiseBiomeSource)] {
        &self.regions
    }
    /// The region of the column at quart `(x, z)`.
    pub fn region_at(&self, x: i32, z: i32) -> usize {
        let (cx, cz) = (x.div_euclid(CELL), z.div_euclid(CELL));
        let mut best = (i64::MAX, 0, 0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (nx, nz) = (cx + dx, cz + dz);
                let h = self.hash(nx, nz, 0);
                let px = i64::from(nx) * i64::from(CELL) + (h % CELL as u64) as i64;
                let pz = i64::from(nz) * i64::from(CELL) + ((h >> 32) % CELL as u64) as i64;
                let d = (i64::from(x) - px).pow(2) + (i64::from(z) - pz).pow(2);
                if d < best.0 {
                    best = (d, nx, nz);
                }
            }
        }
        self.pick(best.1, best.2)
    }
    /// The region cell `(x, z)` takes, by weight.
    fn pick(&self, x: i32, z: i32) -> usize {
        if self.total == 0 {
            return 0;
        }
        let mut at = self.hash(x, z, 1) % self.total;
        for (i, (_, w, _)) in self.regions.iter().enumerate() {
            let w = u64::from(*w);
            if at < w {
                return i;
            }
            at -= w;
        }
        0
    }
    /// A well-mixed hash of the seed, a cell and a salt (SplitMix64's finaliser).
    fn hash(&self, x: i32, z: i32, salt: u64) -> u64 {
        let mut h = self.seed
            ^ (x as u32 as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (z as u32 as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
            ^ salt.wrapping_mul(0x1656_67b1_9e37_79f9);
        h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        h ^ (h >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worldgen::climate::{Parameter, ParameterPoint};

    fn list(biome: &str) -> MultiNoiseBiomeSource {
        let point = ParameterPoint {
            coordinates: [Parameter::new(-10000, 10000); 6],
            offset: 0,
        };
        MultiNoiseBiomeSource::new(vec![(point, biome.into())]).unwrap()
    }

    fn four(seed: i64) -> ClimateRegions {
        ClimateRegions::new(
            seed,
            vec![
                ("minecraft:overworld".into(), 10, list("minecraft:plains")),
                ("bop:primary".into(), 10, list("bop:a")),
                ("bop:secondary".into(), 8, list("bop:b")),
                ("bop:rare".into(), 2, list("bop:c")),
            ],
        )
    }

    /// The shares and patch sizes measured on NeoForge 1.21.1 with Biomes O'
    /// Plenty (docs/research/climate-regions.md): area in proportion to weight;
    /// alike 69 % of the time 256 blocks apart, 50 % at 512, by chance from 768.
    #[test]
    fn regions_cover_their_weights_in_patches_of_the_observed_size() {
        let r = four(42);
        let (step, n) = (64, 256); // quarts: 256 blocks apart, 64 km square
        let mut grid = vec![0usize; n * n];
        for j in 0..n {
            for i in 0..n {
                grid[j * n + i] = r.region_at(i as i32 * step, j as i32 * step);
            }
        }
        let mut count = [0usize; 4];
        grid.iter().for_each(|&g| count[g] += 1);
        let total = grid.len() as f64;
        for (i, w) in [10.0, 10.0, 8.0, 2.0].iter().enumerate() {
            let share = count[i] as f64 / total;
            assert!((share - w / 30.0).abs() < 0.03, "region {i}: {share}");
        }
        let alike = |k: usize| {
            let (mut same, mut all) = (0, 0);
            for j in 0..n {
                for i in 0..n - k {
                    all += 1;
                    same += usize::from(grid[j * n + i] == grid[j * n + i + k]);
                }
            }
            same as f64 / all as f64
        };
        // Same region by chance: the sum of the squared shares.
        let chance: f64 = [10.0f64, 10.0, 8.0, 2.0]
            .iter()
            .map(|w| (w / 30.0).powi(2))
            .sum();
        let excess = |k| (alike(k) - chance) / (1.0 - chance);
        assert!(
            (0.35..0.6).contains(&excess(1)),
            "256 blocks: {}",
            excess(1)
        );
        assert!(
            (0.05..0.25).contains(&excess(2)),
            "512 blocks: {}",
            excess(2)
        );
        assert!(excess(3).abs() < 0.05, "768 blocks: {}", excess(3));
    }

    #[test]
    fn the_seed_moves_the_regions_and_one_seed_always_places_them_alike() {
        let (a, b, c) = (four(1), four(1), four(2));
        let at = |r: &ClimateRegions| {
            (0..400)
                .map(|i| r.region_at(i * 37, i * -53))
                .collect::<Vec<_>>()
        };
        assert_eq!(at(&a), at(&b));
        assert_ne!(at(&a), at(&c));
    }
}
