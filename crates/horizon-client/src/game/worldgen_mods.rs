//! World-generation mods for the world being opened (fork `afterwinter`,
//! docs/research/worldgen-hooks.md, M3c): the enabled mod whose manifest
//! lists a world's preset becomes its generator's terrain provider. Its
//! shared cells of the layers it saves are kept with the world, in
//! `data/horizon/worldgen/`, and loaded back when the world opens.
use horizon_client::{
    mod_list,
    vanillagen::{ModParts, ProviderRequest},
};
use horizon_mods::{
    Capability, Manifest, Package,
    worldgen::{Candidate, Contributor, Decision, Place, Plan, Rules, Stage},
    worldgen_runtime::{WorldInfo, WorldgenMod},
    worldgen_world::{ModDecorator, ModTerrain},
};
use horizon_world::worldgen::{
    FeatureClaims, FeatureDecision,
    provider::{ProviderWorker, TerrainProvider},
};
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
    /// The claims of every mod that is on, settled with the pack's rules.
    claims: Option<Arc<dyn FeatureClaims>>,
}

impl Providers {
    /// The mods that are on (`enabled`, and `--dev-mod`s), for the world in
    /// `world_root`.
    pub(super) fn new(enabled: &BTreeSet<String>, dev: &[PathBuf], world_root: &Path) -> Self {
        let here = std::env::current_dir().unwrap_or_default();
        let mut paths = horizon_mods::find(&here.join(mod_list::DIR));
        paths.retain(|p| !dev.contains(p));
        paths.extend(dev.iter().cloned());
        let on: Vec<(PathBuf, Manifest)> = paths
            .into_iter()
            .filter_map(|path| {
                let package = Package::read(&path).ok()?;
                let m = package.manifest;
                (enabled.contains(&m.id) || dev.contains(&path)).then_some((path, m))
            })
            .collect();
        let contributors: Vec<Contributor> = on
            .iter()
            .filter(|(_, m)| m.worldgen.is_some() || m.capabilities.contains(&Capability::Worldgen))
            .map(|(_, m)| Contributor::of(m))
            .collect();
        let claims = ModClaims::new(&contributors, &here);
        // By mod id: the order mods decorate in, never the folder's.
        let mut mods: Vec<(PathBuf, Manifest)> = on
            .into_iter()
            .filter(|(_, m)| m.capabilities.contains(&Capability::Worldgen))
            .collect();
        mods.sort_by(|a, b| a.1.id.cmp(&b.1.id));
        Providers {
            mods,
            cells: world_root.join("data/horizon/worldgen"),
            claims,
        }
    }

    /// What the mods that are on do in the request's world and dimension:
    /// the preset's terrain, if a mod provides it, and the decoration of
    /// every mod with the `features` stage (M3g). A mod that fails to load
    /// takes no part (the game says why).
    pub(super) fn make(&self, request: &ProviderRequest<'_>) -> ModParts {
        // Every generator of the world, provided or not, decorates by the claims.
        request.data.set_feature_claims(self.claims.clone());
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
        let load = |path: &Path, manifest: &Manifest| match compiled(path) {
            Ok(m) => Some(m),
            Err(e) => {
                log::error!(
                    "Horizon: world-generation mod {} did not load: {e}",
                    manifest.id
                );
                None
            }
        };
        let decorates = |m: &Manifest| {
            m.worldgen
                .as_ref()
                .is_some_and(|w| w.stages.contains(&Stage::Features))
        };
        let mut parts = ModParts::default();
        let provider = self.mods.iter().find(|(_, m)| {
            m.worldgen
                .as_ref()
                .is_some_and(|w| w.presets.iter().any(|p| p == request.preset))
        });
        if let Some((path, manifest)) = provider
            && let Some(m) = load(path, manifest)
        {
            let terrain = ModTerrain::new(&m, request.data.clone(), info.clone());
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
            // Its decoration shares the terrain's cells.
            if decorates(manifest) {
                parts.decorators.push(Arc::new(terrain.decorator()));
            }
            parts.terrain = Some(Arc::new(SavedTerrain { terrain, file }));
        }
        for (path, manifest) in &self.mods {
            let own = provider.is_some_and(|(p, _)| p == path);
            if own || !decorates(manifest) {
                continue;
            }
            if let Some(m) = load(path, manifest) {
                log::info!(
                    "Horizon: world-generation mod {} decorates {}",
                    manifest.id,
                    request.dimension
                );
                parts.decorators.push(Arc::new(ModDecorator::new(
                    &m,
                    request.data.clone(),
                    info.clone(),
                )));
            }
        }
        parts
    }
}

/// Placed features in the fork's own tags (docs/research/worldgen-hooks.md):
/// `horizon:ores` holds vanilla's real ores, every height variant and the
/// Nether's, without the stone and soil blobs (dirt, gravel, granite,
/// diorite, andesite, tuff, clay, infested stone, magma, soul sand,
/// blackstone), for mods that place ores their own way.
const HORIZON_FEATURE_TAGS: [(&str, &[&str]); 1] = [(
    "horizon:ores",
    &[
        "minecraft:ore_ancient_debris_large",
        "minecraft:ore_coal_lower",
        "minecraft:ore_coal_upper",
        "minecraft:ore_copper",
        "minecraft:ore_copper_large",
        "minecraft:ore_debris_small",
        "minecraft:ore_diamond",
        "minecraft:ore_diamond_buried",
        "minecraft:ore_diamond_large",
        "minecraft:ore_diamond_medium",
        "minecraft:ore_emerald",
        "minecraft:ore_gold",
        "minecraft:ore_gold_deltas",
        "minecraft:ore_gold_extra",
        "minecraft:ore_gold_lower",
        "minecraft:ore_gold_nether",
        "minecraft:ore_iron_middle",
        "minecraft:ore_iron_small",
        "minecraft:ore_iron_upper",
        "minecraft:ore_lapis",
        "minecraft:ore_lapis_buried",
        "minecraft:ore_quartz_deltas",
        "minecraft:ore_quartz_nether",
        "minecraft:ore_redstone",
        "minecraft:ore_redstone_lower",
    ],
)];

/// The mods' claims on features (M3g): the first claim in the plan's order
/// that applies decides; a feature belongs to the mod owning its namespace.
/// Claims scoped to a mod's regions need the chunk and do not act here.
struct ModClaims {
    plan: Plan,
    /// Namespace → the mod that owns it.
    owners: HashMap<String, String>,
}
impl ModClaims {
    fn new(contributors: &[Contributor], game: &Path) -> Option<Arc<dyn FeatureClaims>> {
        if !contributors.iter().any(|c| {
            c.section
                .claims
                .iter()
                .any(|claim| claim.stage == Stage::Features)
        }) {
            return None;
        }
        let rules = match std::fs::read_to_string(game.join(horizon_mods::worldgen::RULES)) {
            Ok(text) => Rules::parse(&text).unwrap_or_else(|e| {
                log::warn!("Horizon: {e}; the pack's world-generation rules are not applied");
                Rules::default()
            }),
            Err(_) => Rules::default(),
        };
        let plan = Plan::new(contributors, &rules);
        for problem in plan.problems() {
            log::warn!("Horizon: world generation: {problem}");
        }
        let owners = contributors
            .iter()
            .flat_map(|c| c.namespaces.iter().map(|ns| (ns.clone(), c.id.clone())))
            .collect();
        Some(Arc::new(ModClaims { plan, owners }))
    }
}
impl FeatureClaims for ModClaims {
    fn decide(
        &self,
        preset: &str,
        biome: &str,
        biome_tags: &[&str],
        feature: &str,
        feature_tags: &[&str],
    ) -> FeatureDecision {
        let ns = feature.split_once(':').map_or("minecraft", |(ns, _)| ns);
        let owner = self.owners.get(ns).map_or(ns, String::as_str);
        let mut tags = feature_tags.to_vec();
        for (tag, members) in HORIZON_FEATURE_TAGS {
            if members.contains(&feature) {
                tags.push(tag);
            }
        }
        let place = Place {
            preset,
            biome,
            biome_tags,
            regions: &[],
        };
        let thing = Candidate {
            owner,
            id: feature,
            tags: &tags,
        };
        match self.plan.decide(Stage::Features, &place, &thing) {
            Decision::Keep { .. } => FeatureDecision::Keep,
            Decision::Drop { .. } | Decision::Off => FeatureDecision::Drop,
            Decision::Replace { with, .. } => FeatureDecision::Replace(with),
        }
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

    /// FarView's geology claim (M3g): `#horizon:ores` replaced with its own
    /// deposits in its preset only; the stone blobs, other presets and its
    /// own features untouched; the pack's rules can deny it.
    #[test]
    fn a_preset_claim_replaces_vanillas_real_ores_only() {
        let manifest = Manifest::parse(
            "id = \"farview\"\nname = \"FarView\"\nversion = \"1\"\napi = \"0.3\"\nside = \"server\"\n\
             capabilities = [\"worldgen\"]\nexperimental = [\"worldgen\"]\n\
             [worldgen]\npresets = [\"farview:earth\"]\nstages = [\"terrain\", \"features\"]\n\
             [[worldgen.claims]]\nname = \"geology_ores\"\nstage = \"features\"\naction = \"replace\"\n\
             scope = \"preset:farview:earth\"\ntargets = [\"#horizon:ores\"]\nwith = \"farview:geology_ores\"\n",
        )
        .unwrap();
        let game = std::env::temp_dir().join(format!("horizon-claims-{}", std::process::id()));
        std::fs::create_dir_all(&game).unwrap();
        let claims = ModClaims::new(&[Contributor::of(&manifest)], &game).expect("claims");
        let at = |preset: &str, feature: &str| {
            claims.decide(preset, "minecraft:plains", &[], feature, &[])
        };
        let geology = FeatureDecision::Replace("farview:geology_ores".into());
        assert_eq!(at("farview:earth", "minecraft:ore_iron_upper"), geology);
        assert_eq!(at("farview:earth", "minecraft:ore_quartz_nether"), geology);
        assert_eq!(
            at("farview:earth", "minecraft:ore_granite_upper"),
            FeatureDecision::Keep
        );
        assert_eq!(
            at("farview:earth", "minecraft:trees_plains"),
            FeatureDecision::Keep
        );
        assert_eq!(
            at("minecraft:normal", "minecraft:ore_iron_upper"),
            FeatureDecision::Keep
        );
        // The pack's last word.
        let rules = game.join(horizon_mods::worldgen::RULES);
        std::fs::create_dir_all(rules.parent().unwrap()).unwrap();
        std::fs::write(&rules, "[claims.\"farview/geology_ores\"]\nallow = false\n").unwrap();
        let denied = ModClaims::new(&[Contributor::of(&manifest)], &game).expect("claims");
        assert_eq!(
            denied.decide(
                "farview:earth",
                "minecraft:plains",
                &[],
                "minecraft:ore_iron_upper",
                &[]
            ),
            FeatureDecision::Keep
        );
        std::fs::remove_dir_all(&game).unwrap();
    }

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
