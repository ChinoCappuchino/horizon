//! Villager professions and job-site POI types as registries
//! (`VillagerProfession`, `PoiTypes`), not fixed lists: the game's fifteen
//! come first, at their vanilla indices, and mods add more, from data or in
//! code (docs/mods.md, "Villager professions").
//!
//! A villager keeps its profession as an index (`extra.variant2`); saves
//! and the network use the id. A mod's data:
//!
//! - `data/<ns>/horizon/poi_type/<name>.json`: `{"blocks": [block ids],
//!   "max_tickets": 1, "valid_range": 1}`. Every state of each block is the
//!   POI.
//! - `data/<ns>/horizon/villager_profession/<name>.json`: `{"job_site":
//!   "<ns>:<poi type>", "work_sound": "<sound event>", "requested_items":
//!   [item ids], "secondary_job_sites": [block ids]}`.
//!
//! The Java compatibility layer registers NeoForge mods' professions and
//! POI types through [`register_poi`] and [`register_profession`].
use std::{
    path::Path,
    sync::{Arc, LazyLock, RwLock},
};

/// A POI type that is a job site: the blocks it is (any state).
#[derive(Clone, Debug, PartialEq)]
pub struct PoiType {
    pub id: String,
    pub blocks: Vec<String>,
    pub max_tickets: u32,
    pub valid_range: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profession {
    pub id: String,
    /// The POI type of its job site (`heldJobSite`), if it has one.
    pub job_site: Option<String>,
    /// The sound when it works at its job site, a sound event id.
    pub work_sound: Option<String>,
    /// Items it picks up (`requestedItems`).
    pub requested_items: Vec<String>,
    /// Blocks it works besides its job site (`secondaryPoi`: the farmer's
    /// farmland).
    pub secondary_job_sites: Vec<String>,
}

struct Registry {
    professions: Vec<(Arc<str>, Profession, Interned)>,
    pois: Vec<PoiType>,
}

/// What the mob code reads each tick, as `&'static` (made once for each
/// registration, which is rare).
#[derive(Clone, Copy)]
struct Interned {
    work_sound: Option<&'static str>,
    requested_items: &'static [&'static str],
}

fn intern(p: &Profession) -> Interned {
    let leak = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
    Interned {
        work_sound: p.work_sound.as_deref().map(|s| leak(short(s))),
        requested_items: Box::leak(
            p.requested_items
                .iter()
                .map(|i| leak(i))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
    }
}

/// The index of the unemployed and of the nitwit.
pub const NONE: u8 = 0;
pub const NITWIT: u8 = 11;

/// The game's professions, in `VillagerProfession`'s order (the network's
/// registry ids), and their job sites.
const VANILLA: [(&str, &[&str], &str); 15] = [
    ("none", &[], ""),
    (
        "armorer",
        &["blast_furnace"],
        "entity.villager.work_armorer",
    ),
    ("butcher", &["smoker"], "entity.villager.work_butcher"),
    (
        "cartographer",
        &["cartography_table"],
        "entity.villager.work_cartographer",
    ),
    ("cleric", &["brewing_stand"], "entity.villager.work_cleric"),
    ("farmer", &["composter"], "entity.villager.work_farmer"),
    ("fisherman", &["barrel"], "entity.villager.work_fisherman"),
    (
        "fletcher",
        &["fletching_table"],
        "entity.villager.work_fletcher",
    ),
    (
        "leatherworker",
        &[
            "cauldron",
            "water_cauldron",
            "lava_cauldron",
            "powder_snow_cauldron",
        ],
        "entity.villager.work_leatherworker",
    ),
    ("librarian", &["lectern"], "entity.villager.work_librarian"),
    ("mason", &["stonecutter"], "entity.villager.work_mason"),
    ("nitwit", &[], ""),
    ("shepherd", &["loom"], "entity.villager.work_shepherd"),
    (
        "toolsmith",
        &["smithing_table"],
        "entity.villager.work_toolsmith",
    ),
    (
        "weaponsmith",
        &["grindstone"],
        "entity.villager.work_weaponsmith",
    ),
];

fn vanilla() -> Registry {
    let mut r = Registry {
        professions: Vec::new(),
        pois: Vec::new(),
    };
    for (name, blocks, sound) in VANILLA {
        let job_site = (!blocks.is_empty()).then(|| {
            r.pois.push(PoiType {
                id: format!("minecraft:{name}"),
                blocks: blocks.iter().map(|b| format!("minecraft:{b}")).collect(),
                max_tickets: 1,
                valid_range: 1,
            });
            format!("minecraft:{name}")
        });
        let farmer = name == "farmer";
        let p = Profession {
            id: format!("minecraft:{name}"),
            job_site,
            work_sound: (!sound.is_empty()).then(|| sound.to_owned()),
            requested_items: if farmer {
                ["wheat", "wheat_seeds", "beetroot_seeds", "bone_meal"]
                    .iter()
                    .map(|i| format!("minecraft:{i}"))
                    .collect()
            } else {
                Vec::new()
            },
            secondary_job_sites: if farmer {
                vec!["minecraft:farmland".to_owned()]
            } else {
                Vec::new()
            },
        };
        let interned = intern(&p);
        r.professions.push((name.into(), p, interned));
    }
    r
}

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(|| RwLock::new(vanilla()));

fn read() -> std::sync::RwLockReadGuard<'static, Registry> {
    REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The short name of an id: `farmer` for `minecraft:farmer`, the whole id
/// for another namespace.
fn short(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

/// Adds a POI type, or replaces the one with its id.
pub fn register_poi(poi: PoiType) {
    let mut r = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match r.pois.iter_mut().find(|p| p.id == poi.id) {
        Some(p) => *p = poi,
        None => r.pois.push(poi),
    }
}

/// Adds a profession, or replaces the one with its id; its index.
pub fn register_profession(p: Profession) -> u8 {
    let mut r = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let interned = intern(&p);
    if let Some(i) = r.professions.iter().position(|(_, q, _)| q.id == p.id) {
        r.professions[i].1 = p;
        r.professions[i].2 = interned;
        return i as u8;
    }
    let name: Arc<str> = short(&p.id).into();
    r.professions.push((name, p, interned));
    (r.professions.len() - 1).min(usize::from(u8::MAX)) as u8
}

/// How many professions there are.
pub fn count() -> usize {
    read().professions.len()
}

/// A profession's short name (`farmer`, `chefsdelight:chef`); an index out
/// of range is the unemployed.
pub fn name(index: u8) -> Arc<str> {
    let r = read();
    r.professions
        .get(usize::from(index))
        .unwrap_or(&r.professions[0])
        .0
        .clone()
}

/// A profession's id (`minecraft:farmer`).
pub fn id(index: u8) -> String {
    let r = read();
    r.professions
        .get(usize::from(index))
        .unwrap_or(&r.professions[0])
        .1
        .id
        .clone()
}

pub fn get(index: u8) -> Option<Profession> {
    read()
        .professions
        .get(usize::from(index))
        .map(|(_, p, _)| p.clone())
}

/// The index of a profession, by id or short name.
pub fn index(name: &str) -> Option<u8> {
    let name = short(name);
    read()
        .professions
        .iter()
        .position(|(n, _, _)| &**n == name)
        .map(|i| i as u8)
}

/// The profession whose job site a block is (`PoiTypes.forState`).
pub fn job_site_profession(block: &str) -> Option<u8> {
    let r = read();
    let poi = r
        .pois
        .iter()
        .find(|p| p.blocks.iter().any(|b| b == block))?;
    r.professions
        .iter()
        .position(|(_, p, _)| p.job_site.as_deref() == Some(poi.id.as_str()))
        .map(|i| i as u8)
}

/// The POI type of a profession's job site.
pub fn job_site(index: u8) -> Option<PoiType> {
    let r = read();
    let id = r
        .professions
        .get(usize::from(index))?
        .1
        .job_site
        .as_deref()?;
    r.pois.iter().find(|p| p.id == id).cloned()
}

/// The sound event of a profession's work, without `minecraft:`.
pub fn work_sound(index: u8) -> Option<&'static str> {
    read().professions.get(usize::from(index))?.2.work_sound
}

pub fn requested_items(index: u8) -> &'static [&'static str] {
    read()
        .professions
        .get(usize::from(index))
        .map_or(&[], |p| p.2.requested_items)
}

/// The texture of a profession's layer on a villager model folder
/// (`entity/villager`): a mod's is in its own namespace.
pub fn texture(folder: &str, index: u8) -> String {
    let name = name(index);
    match name.split_once(':') {
        Some((ns, path)) => format!("{ns}:{folder}/profession/{path}"),
        None => format!("{folder}/profession/{name}"),
    }
}

/// Every profession's short name, in index order.
pub fn names() -> Vec<Arc<str>> {
    read()
        .professions
        .iter()
        .map(|(n, _, _)| n.clone())
        .collect()
}

/// Reads the POI types and professions of every namespace's
/// `data/<ns>/horizon/` under a data root.
pub fn load(root: &Path) {
    let Ok(namespaces) = std::fs::read_dir(root.join("data")) else {
        return;
    };
    let mut namespaces: Vec<_> = namespaces.flatten().map(|e| e.path()).collect();
    namespaces.sort();
    let json = |path: &Path| -> Option<serde_json::Value> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    };
    let strings = |v: &serde_json::Value, key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let entries = |dir: &Path| -> Vec<(String, std::path::PathBuf)> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
            .collect();
        files.sort();
        files
    };
    for ns in &namespaces {
        let Some(namespace) = ns.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        for (name, path) in entries(&ns.join("horizon/poi_type")) {
            let Some(v) = json(&path) else { continue };
            register_poi(PoiType {
                id: format!("{namespace}:{name}"),
                blocks: strings(&v, "blocks"),
                max_tickets: v["max_tickets"].as_u64().unwrap_or(1) as u32,
                valid_range: v["valid_range"].as_u64().unwrap_or(1) as u32,
            });
        }
        for (name, path) in entries(&ns.join("horizon/villager_profession")) {
            let Some(v) = json(&path) else { continue };
            register_profession(Profession {
                id: format!("{namespace}:{name}"),
                job_site: v["job_site"].as_str().map(str::to_owned),
                work_sound: v["work_sound"].as_str().map(str::to_owned),
                requested_items: strings(&v, "requested_items"),
                secondary_job_sites: strings(&v, "secondary_job_sites"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_games_professions_and_a_mods() {
        assert_eq!(&*name(5), "farmer");
        assert_eq!(index("minecraft:nitwit"), Some(NITWIT));
        assert_eq!(job_site_profession("minecraft:lectern"), index("librarian"));
        assert_eq!(
            job_site_profession("minecraft:lava_cauldron"),
            index("leatherworker")
        );
        assert_eq!(job_site_profession("minecraft:stone"), None);
        assert_eq!(work_sound(5), Some("entity.villager.work_farmer"));
        assert_eq!(
            texture("entity/villager", 9),
            "entity/villager/profession/librarian"
        );

        let dir = std::env::temp_dir().join(format!("horizon-professions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data/testmod/horizon/poi_type")).unwrap();
        std::fs::create_dir_all(dir.join("data/testmod/horizon/villager_profession")).unwrap();
        std::fs::write(
            dir.join("data/testmod/horizon/poi_type/oven_site.json"),
            r#"{"blocks": ["testmod:oven"], "max_tickets": 2, "valid_range": 3}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("data/testmod/horizon/villager_profession/baker.json"),
            r#"{"job_site": "testmod:oven_site", "work_sound": "testmod:oven.bake"}"#,
        )
        .unwrap();
        load(&dir);
        let baker = index("testmod:baker").unwrap();
        assert!(usize::from(baker) >= 15);
        assert_eq!(job_site_profession("testmod:oven"), Some(baker));
        assert_eq!(job_site(baker).unwrap().max_tickets, 2);
        assert_eq!(
            texture("entity/villager", baker),
            "testmod:entity/villager/profession/baker"
        );
        // Loading again replaces, not adds.
        load(&dir);
        assert_eq!(index("testmod:baker"), Some(baker));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
