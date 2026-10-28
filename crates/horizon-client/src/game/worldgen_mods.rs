//! World-generation mods for the world being opened (fork `afterwinter`,
//! docs/research/worldgen-hooks.md, M3c): the enabled mod whose manifest
//! lists a world's preset becomes its generator's terrain provider. Its
//! shared cells of the layers it saves are kept with the world, in
//! `data/horizon/worldgen/`, and loaded back when the world opens.
use horizon_client::{mod_list, vanillagen::ProviderRequest};
use horizon_mods::{
    Capability, Manifest, Package,
    worldgen_runtime::{WorldInfo, WorldgenMod},
    worldgen_world::ModTerrain,
};
use horizon_world::worldgen::provider::{ProviderWorker, TerrainProvider};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};

/// The world-generation mods that are on, and where a world keeps their cells.
pub(super) struct Providers {
    mods: Vec<(PathBuf, Manifest)>,
    cells: PathBuf,
}

impl Providers {
    /// The mods that are on (`enabled`, and `--dev-mod`s), for the world in
    /// `world_root`.
    pub(super) fn new(enabled: &BTreeSet<String>, dev: &[PathBuf], world_root: &Path) -> Self {
        let here = std::env::current_dir().unwrap_or_default();
        let mut paths = horizon_mods::find(&here.join(mod_list::DIR));
        paths.retain(|p| !dev.contains(p));
        paths.extend(dev.iter().cloned());
        let mods = paths
            .into_iter()
            .filter_map(|path| {
                let package = Package::read(&path).ok()?;
                let m = package.manifest;
                let on = enabled.contains(&m.id) || dev.contains(&path);
                (on && m.capabilities.contains(&Capability::Worldgen)).then_some((path, m))
            })
            .collect();
        Providers {
            mods,
            cells: world_root.join("data/horizon/worldgen"),
        }
    }

    /// The provider of the request's preset, or none when no mod that is on
    /// provides it (or it fails to load: the game says why).
    pub(super) fn make(&self, request: &ProviderRequest<'_>) -> Option<Arc<dyn TerrainProvider>> {
        let (path, manifest) = self.mods.iter().find(|(_, m)| {
            m.worldgen
                .as_ref()
                .is_some_and(|w| w.presets.iter().any(|p| p == request.preset))
        })?;
        let m = match compiled(path) {
            Ok(m) => m,
            Err(e) => {
                log::error!(
                    "Horizon: world-generation mod {} did not load: {e}",
                    manifest.id
                );
                return None;
            }
        };
        let level = &request.level;
        let info = WorldInfo {
            seed: request.seed,
            preset: request.preset.to_owned(),
            dimension: request.dimension.to_owned(),
            min_y: level.min_y,
            height: level.height as u32,
            sea_level: level.sea_level,
            settings: request.settings.to_owned(),
        };
        let terrain = ModTerrain::new(&m, request.data.clone(), info);
        let file = self.cells.join(format!(
            "{}-{}.cells",
            manifest.id,
            request.dimension.replace([':', '/'], "_")
        ));
        if let Ok(bytes) = std::fs::read(&file) {
            match decode(&bytes) {
                Some(cells) => {
                    let kept = terrain.world().preload(cells);
                    log::info!(
                        "Horizon: {kept} saved world-generation cells of {} loaded",
                        manifest.id
                    );
                }
                None => log::warn!(
                    "Horizon: {} is damaged; its cells are built again",
                    file.display()
                ),
            }
        }
        log::info!(
            "Horizon: world-generation mod {} provides {} ({})",
            manifest.id,
            request.preset,
            request.dimension
        );
        Some(Arc::new(SavedTerrain { terrain, file }))
    }
}

/// Mods compiled once per process: a world reopened, or another dimension,
/// reuses them (until the file changes).
fn compiled(path: &Path) -> Result<Arc<WorldgenMod>, String> {
    type Cache = Mutex<HashMap<PathBuf, (Option<SystemTime>, Arc<WorldgenMod>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((when, m)) = cache.lock().unwrap().get(path)
        && *when == modified
    {
        return Ok(m.clone());
    }
    let m = Arc::new(WorldgenMod::load(&Package::read(path)?)?);
    cache
        .lock()
        .unwrap()
        .insert(path.to_owned(), (modified, m.clone()));
    Ok(m)
}

/// A mod's terrain that writes its saved layers' cells next to the world
/// when the world closes.
struct SavedTerrain {
    terrain: ModTerrain,
    file: PathBuf,
}
impl TerrainProvider for SavedTerrain {
    fn worker(&self) -> Result<Box<dyn ProviderWorker>, String> {
        self.terrain.worker()
    }
}
impl Drop for SavedTerrain {
    fn drop(&mut self) {
        let cells = self.terrain.world().saved_cells();
        if cells.is_empty() {
            return;
        }
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(self.file.parent().unwrap_or(Path::new(".")))?;
            let temp = self.file.with_extension("cells.tmp");
            std::fs::write(&temp, encode(&cells))?;
            std::fs::rename(&temp, &self.file)
        };
        if let Err(e) = write() {
            log::warn!(
                "Horizon: world-generation cells not saved to {}: {e}",
                self.file.display()
            );
        }
    }
}

const MAGIC: &[u8] = b"horizon-worldgen-cells 1\n";

/// `MAGIC`, then per cell: layer (u8), key length (u16 LE), key, data length
/// (u32 LE), data.
fn encode(cells: &[(u8, String, Arc<[u8]>)]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    for (layer, key, data) in cells {
        out.push(*layer);
        out.extend_from_slice(&(key.len() as u16).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out
}

fn decode(bytes: &[u8]) -> Option<Vec<(u8, String, Vec<u8>)>> {
    let mut rest = bytes.strip_prefix(MAGIC)?;
    let mut take = |n: usize| -> Option<&[u8]> {
        let (head, tail) = (rest.get(..n)?, rest.get(n..)?);
        rest = tail;
        Some(head)
    };
    let mut cells = Vec::new();
    while let Some(&[layer]) = take(1) {
        let key_len = u16::from_le_bytes(take(2)?.try_into().ok()?) as usize;
        let key = String::from_utf8(take(key_len)?.to_vec()).ok()?;
        let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        cells.push((layer, key, take(len)?.to_vec()));
    }
    Some(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_cells_round_trip_and_damage_is_refused() {
        let cells: Vec<(u8, String, Arc<[u8]>)> = vec![
            (1, "0,0".into(), Arc::from(&[1u8, 2, 3][..])),
            (3, "-12,7".into(), Arc::from(&[][..])),
        ];
        let bytes = encode(&cells);
        let back = decode(&bytes).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(
            (back[0].0, back[0].1.as_str(), back[0].2.as_slice()),
            (1, "0,0", &[1u8, 2, 3][..])
        );
        assert_eq!(
            (back[1].0, back[1].1.as_str(), back[1].2.len()),
            (3, "-12,7", 0)
        );
        assert!(decode(&bytes[..bytes.len() - 2]).is_none(), "a cut file");
        assert!(decode(b"something else").is_none());
    }
}
