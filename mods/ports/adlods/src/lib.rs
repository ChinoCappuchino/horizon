//! Large Ore Deposits for Horizon (fork `afterwinter`, M3g): a clean-room
//! port. What each deposit is made of, where and how rare comes from the
//! pack's own config files (tools/adlods_port.py, built in); how deposits are
//! placed is Horizon's own, matched to what the mod does on NeoForge 1.21.1
//! (docs/research/large-ore-deposits.md):
//!
//! - every chunk is a deposit's origin with chance 1/rarity, per deposit;
//! - its centre is anywhere in the chunk, at a height drawn evenly from the
//!   deposit's altitude range, and its size evenly from its size range;
//! - its blocks are drawn from a flattened, horizontally stretched normal
//!   distribution around the centre (standard deviations 0.62, 0.42 and 0.23
//!   of the size's cube root, turned by a random angle) until 1.3 times that
//!   many distinct positions are drawn (some land where no ore may go);
//! - a drawn position becomes ore where the block there is replaceable (or a
//!   custom replacement's target), and not next to air for deposits that are
//!   not `exposed`.
//!
//! A deposit reaches up to about 30 blocks from its centre, past its chunk:
//! each chunk places the part of every deposit near it that falls inside
//! itself, so the parts match across chunks.
use horizon_mod_sdk::worldgen::*;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
struct Config {
    name: String,
    ores: Vec<Weighted>,
    replaceable: Vec<String>,
    custom: Vec<Custom>,
    rarity: u32,
    size: [u32; 2],
    altitude: [i32; 2],
    dimensions: Vec<String>,
    exposed: bool,
}
#[derive(Deserialize)]
struct Weighted {
    block: String,
    weight: u32,
}
#[derive(Deserialize)]
struct Custom {
    from: Vec<String>,
    to: String,
}

/// A deposit of this world's dimension, its blocks as the game numbers them.
struct Deposit {
    salt: u64,
    rarity: u64,
    size: [u32; 2],
    altitude: [i32; 2],
    exposed: bool,
    /// (ore, weight) for replaceable blocks.
    ores: Vec<(u32, u32)>,
    total: u32,
    replaceable: HashSet<u32>,
    /// A target block's own ore (deepslate's deepslate ore).
    custom: HashMap<u32, u32>,
}

/// How far from its origin chunk a deposit may reach, in chunks.
const REACH: i32 = 2;
/// Positions drawn per block of a deposit's size (docs/research/large-ore-deposits.md).
const DRAWN: f64 = 1.3;

struct Deposits {
    seed: u64,
    deposits: Vec<Deposit>,
    air: HashSet<u32>,
}

/// SplitMix64: deterministic numbers from the seed, a deposit and a chunk.
struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// In [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// In [lo, hi].
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1).max(1) as u64) as i64
    }
    /// A standard normal number (Box-Muller).
    fn normal(&mut self) -> f64 {
        let u = self.unit().max(1e-12);
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
}

fn chunk_random(seed: u64, salt: u64, x: i32, z: i32) -> Random {
    let mut r = Random(
        seed ^ salt.wrapping_mul(0xd6e8_feb8_6659_fd93)
            ^ (x as u32 as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ ((z as u32 as u64) << 32),
    );
    r.next();
    r
}

impl Deposits {
    /// The positions of the deposit `d` starting in chunk (ox, oz), if one does.
    fn planned(&self, d: &Deposit, ox: i32, oz: i32) -> Option<Vec<(i32, i32, i32)>> {
        let mut r = chunk_random(self.seed, d.salt, ox, oz);
        if r.next() % d.rarity != 0 {
            return None;
        }
        let cx = f64::from(ox * 16) + r.unit() * 16.0;
        let cz = f64::from(oz * 16) + r.unit() * 16.0;
        let cy = r.range(i64::from(d.altitude[0]), i64::from(d.altitude[1])) as f64 + 0.5;
        let size = r.range(i64::from(d.size[0]), i64::from(d.size[1])) as f64;
        let scale = size.cbrt();
        let (major, minor, vertical) = (0.62 * scale, 0.42 * scale, 0.23 * scale);
        // More positions than the size: some fall where no ore may go (caves,
        // dirt, water), and the mod's deposits have their full size placed.
        let n = (size * DRAWN) as usize;
        let angle = r.unit() * std::f64::consts::PI;
        let (sin, cos) = angle.sin_cos();
        let mut seen = HashSet::with_capacity(n);
        let mut out = Vec::with_capacity(n);
        // Draws until n distinct positions, with a bound for the dense core.
        for _ in 0..n * 8 {
            if out.len() >= n {
                break;
            }
            let (a, b, v) = (
                r.normal() * major,
                r.normal() * minor,
                r.normal() * vertical,
            );
            let p = (
                (cx + a * cos - b * sin).floor() as i32,
                (cy + v).floor() as i32,
                (cz + a * sin + b * cos).floor() as i32,
            );
            if seen.insert(p) {
                out.push(p);
            }
        }
        Some(out)
    }

    fn ore_for(&self, d: &Deposit, here: u32, r: &mut Random) -> Option<u32> {
        if let Some(&ore) = d.custom.get(&here) {
            return Some(ore);
        }
        if !d.replaceable.contains(&here) || d.total == 0 {
            return None;
        }
        let mut pick = (r.next() % u64::from(d.total)) as u32;
        for &(ore, w) in &d.ores {
            if pick < w {
                return Some(ore);
            }
            pick -= w;
        }
        None
    }
}

impl WorldgenMod for Deposits {
    fn open(info: &WorldInfo) -> Result<(Self, Vec<Stage>), String> {
        let configs: Vec<Config> =
            serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/deposits.json")))
                .map_err(|e| format!("deposits: {e}"))?;
        let ids = |names: &[String]| -> HashSet<u32> {
            names.iter().filter_map(|n| state_id(n).ok()).collect()
        };
        let mut deposits = Vec::new();
        for (i, c) in configs.iter().enumerate() {
            if !c.dimensions.is_empty() && !c.dimensions.contains(&info.dimension) {
                continue;
            }
            // Blocks the game doesn't have (a mod the pack lacks) are left out.
            let ores: Vec<(u32, u32)> = c
                .ores
                .iter()
                .filter_map(|o| Some((state_id(&o.block).ok()?, o.weight)))
                .collect();
            if ores.is_empty() {
                continue;
            }
            let mut custom = HashMap::new();
            for rule in &c.custom {
                if let Ok(to) = state_id(&rule.to) {
                    for from in ids(&rule.from) {
                        custom.insert(from, to);
                    }
                }
            }
            let salt = c
                .name
                .bytes()
                .fold(0xcbf2_9ce4_8422_2325u64 ^ i as u64, |h, b| {
                    (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
                });
            deposits.push(Deposit {
                salt,
                rarity: u64::from(c.rarity.max(1)),
                size: c.size,
                altitude: c.altitude,
                exposed: c.exposed,
                total: ores.iter().map(|(_, w)| w).sum(),
                ores,
                replaceable: ids(&c.replaceable),
                custom,
            });
        }
        let air = ["minecraft:air", "minecraft:cave_air", "minecraft:void_air"]
            .iter()
            .filter_map(|n| state_id(n).ok())
            .collect();
        let stages = if deposits.is_empty() {
            Vec::new()
        } else {
            vec![Stage::Features]
        };
        Ok((
            Deposits {
                seed: info.seed as u64,
                deposits,
                air,
            },
            stages,
        ))
    }

    fn decorate(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        step: Step,
    ) -> Result<Vec<BlockEdit>, String> {
        if !matches!(step, Step::UndergroundOres) {
            return Ok(Vec::new());
        }
        let mut edits = Vec::new();
        let mut placed = HashSet::new();
        for d in &self.deposits {
            for oz in chunk_z - REACH..=chunk_z + REACH {
                for ox in chunk_x - REACH..=chunk_x + REACH {
                    let Some(points) = self.planned(d, ox, oz) else {
                        continue;
                    };
                    // The ore picks follow the deposit, not the chunk placing it.
                    let mut r = chunk_random(self.seed, !d.salt, ox, oz);
                    for (x, y, z) in points {
                        let pick = r.next();
                        if x >> 4 != chunk_x || z >> 4 != chunk_z || placed.contains(&(x, y, z)) {
                            continue;
                        }
                        let Some(here) = region_block(x, y, z) else {
                            continue;
                        };
                        let Some(ore) = self.ore_for(d, here, &mut Random(pick)) else {
                            continue;
                        };
                        if !d.exposed && self.next_to_air(x, y, z) {
                            continue;
                        }
                        placed.insert((x, y, z));
                        edits.push(BlockEdit {
                            x,
                            y,
                            z,
                            state: ore,
                        });
                    }
                }
            }
        }
        Ok(edits)
    }
}

impl Deposits {
    fn next_to_air(&self, x: i32, y: i32, z: i32) -> bool {
        [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ]
        .iter()
        .any(|(dx, dy, dz)| {
            region_block(x + dx, y + dy, z + dz).is_some_and(|b| self.air.contains(&b))
        })
    }
}

horizon_mod_sdk::export_worldgen_mod!(Deposits);
