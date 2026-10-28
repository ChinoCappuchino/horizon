//! A stand-in world-generation mod for horizon-mods' worldgen tests: low
//! hills of stone, dirt and grass for its preset `test_worldgen:hills`,
//! deserts where vanilla's climate is hot, a log on every 4th column, a camp
//! in every 4th chunk and its region `camps` in every chunk with an even x.
//! The preset's settings ask it to misbehave: `runs` (runs that don't
//! ascend), `panic`, `spin` (an endless loop), `nan` (densities that aren't
//! numbers), `outside` (a log out of its area), `foreign` (a structure in
//! vanilla's namespace), `stage` (a stage its manifest doesn't list).
use horizon_mod_sdk::worldgen::*;

struct Hills {
    seed: i64,
    sea: i32,
    height: u32,
    settings: String,
    stone: u32,
    dirt: u32,
    grass: u32,
    log: u32,
    planks: u32,
    plains: u32,
    desert: u32,
}

/// SplitMix64's finaliser: a column's height from the seed alone.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Hills {
    /// The y of the column's grass.
    fn top(&self, x: i32, z: i32) -> i32 {
        let key = (self.seed as u64) ^ (u64::from(x as u32) << 32) ^ u64::from(z as u32);
        self.sea + (mix(key) % 8) as i32
    }
    fn asks(&self, what: &str) -> bool {
        self.settings.contains(what)
    }
}

impl WorldgenMod for Hills {
    fn open(info: &WorldInfo) -> Result<(Self, Vec<Stage>), String> {
        let biome = |b: &str| biome_id(b).ok_or(format!("no biome {b}"));
        let hills = Hills {
            seed: info.seed,
            sea: info.sea_level,
            height: info.height,
            settings: info.settings.clone(),
            stone: state_id("minecraft:stone")?,
            dirt: state_id("minecraft:dirt")?,
            grass: state_id("minecraft:grass_block")?,
            log: state_id("minecraft:oak_log[axis=y]")?,
            planks: state_id("minecraft:oak_planks")?,
            plains: biome("minecraft:plains")?,
            desert: biome("minecraft:desert")?,
        };
        let mut stages = vec![Stage::Climate, Stage::Structures, Stage::Features];
        if info.preset == "test_worldgen:hills" {
            stages.push(Stage::Terrain);
        }
        if hills.asks("stage") {
            stages.push(Stage::Surface);
        }
        Ok((hills, stages))
    }

    fn biomes(&mut self, request: &BiomeRequest) -> Result<Vec<u32>, String> {
        if request.climate.is_empty() {
            return Ok(vec![self.plains; 16 * request.quarts_y as usize]);
        }
        Ok(request
            .climate
            .iter()
            .map(|c| match c.temperature {
                t if t > 0.5 => self.desert,
                t if t < -0.5 => KEEP_BIOME,
                _ => self.plains,
            })
            .collect())
    }

    fn terrain(&mut self, chunk_x: i32, chunk_z: i32) -> Result<Land, String> {
        if self.asks("panic") {
            panic!("asked to");
        }
        if self.asks("spin") {
            let mut n = 0u64;
            while std::hint::black_box(n) != u64::MAX {
                n = n.wrapping_add(2);
            }
        }
        if self.asks("nan") {
            // Made at run time, as a provider's bad arithmetic would.
            let (top, bottom) = (std::hint::black_box(0.0f32), std::hint::black_box(0.0f32));
            return Ok(Land::Density(vec![
                top / bottom;
                25 * (self.height as usize / 8 + 1)
            ]));
        }
        let mut columns = Vec::with_capacity(256);
        for z in 0..16 {
            for x in 0..16 {
                let top = self.top(chunk_x * 16 + x, chunk_z * 16 + z);
                let mut runs = vec![
                    Run {
                        state: self.stone,
                        top: top - 4,
                    },
                    Run {
                        state: self.dirt,
                        top: top - 1,
                    },
                    Run {
                        state: self.grass,
                        top,
                    },
                ];
                if self.asks("runs") && (x, z) == (3, 7) {
                    runs.swap(0, 1);
                }
                columns.push(Column { runs });
            }
        }
        Ok(Land::Columns(columns))
    }

    fn heights(&mut self, columns: &[ColumnPos]) -> Result<Vec<i32>, String> {
        Ok(columns.iter().map(|c| self.top(c.x, c.z) + 1).collect())
    }

    fn decorate(
        &mut self,
        chunk_x: i32,
        chunk_z: i32,
        step: Step,
    ) -> Result<Vec<BlockEdit>, String> {
        if !matches!(step, Step::VegetalDecoration) {
            return Ok(Vec::new());
        }
        let mut edits = Vec::new();
        for z in (0..16).step_by(4) {
            for x in (0..16).step_by(4) {
                let (bx, bz) = (chunk_x * 16 + x, chunk_z * 16 + z);
                let y = region_height(bx, bz, false).ok_or("no height in the region")?;
                edits.push(BlockEdit {
                    x: bx,
                    y,
                    z: bz,
                    state: self.log,
                });
            }
        }
        if self.asks("outside") {
            edits.push(BlockEdit {
                x: chunk_x * 16 + 40,
                y: self.sea,
                z: chunk_z * 16,
                state: self.log,
            });
        }
        Ok(edits)
    }

    fn starts(&mut self, chunk_x: i32, chunk_z: i32) -> Result<Vec<StructureStart>, String> {
        if (chunk_x + chunk_z).rem_euclid(4) != 0 {
            return Ok(Vec::new());
        }
        let (x, z) = (chunk_x * 16 + 8, chunk_z * 16 + 8);
        let y = self.top(x, z) + 1;
        let planks = self.planks;
        let floor = (-1..=1)
            .flat_map(|dx| {
                (-1..=1).map(move |dz| BlockEdit {
                    x: x + dx,
                    y,
                    z: z + dz,
                    state: planks,
                })
            })
            .collect();
        let id = if self.asks("foreign") {
            "minecraft:village"
        } else {
            "test_worldgen:camp"
        };
        Ok(vec![StructureStart {
            id: id.into(),
            min_x: x - 1,
            min_y: y,
            min_z: z - 1,
            max_x: x + 1,
            max_y: y + 3,
            max_z: z + 1,
            pieces: vec![
                Piece::Blocks(floor),
                Piece::Template(TemplatePiece {
                    template: "test_worldgen:camp/tent".into(),
                    x: x - 1,
                    y: y + 1,
                    z: z - 1,
                    rotation: Rotation::None,
                    mirror: false,
                }),
            ],
        }])
    }

    fn regions(&mut self, chunk_x: i32, _chunk_z: i32) -> Result<Vec<String>, String> {
        Ok(if chunk_x.rem_euclid(2) == 0 {
            vec!["camps".into()]
        } else {
            Vec::new()
        })
    }
}

horizon_mod_sdk::export_worldgen_mod!(Hills);
