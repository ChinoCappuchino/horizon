//! A content mod's `assets/` and `data/` (API 0.2, docs/modpacks.md): the
//! files it adds to the game's resource tree, and the blocks and items it
//! declares in `data/<id>/horizon/block/` and `data/<id>/horizon/item/`.
//!
//! Content is data, not code: nothing here runs in the sandbox. What a mod
//! may add is fenced instead. Its own namespaces (its id and the ones its
//! manifest lists), plus additions to vanilla tags and to the convention
//! tags (`c`, which NeoForge and Fabric mods share). Paths are plain, files
//! and totals are capped, vanilla ids cannot be redefined, and no two mods
//! share a namespace ([`clash`]).
use crate::manifest::{Capability, MANIFEST, Manifest};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

/// Content files per mod, bytes per file and bytes per mod.
pub const MAX_FILES: usize = 65_536;
pub const MAX_FILE: u64 = 16 << 20;
pub const MAX_TOTAL: u64 = 512 << 20;

/// Checks a content path: `assets/` or `data/`, then `/`-separated parts of
/// `[a-z0-9_.-]`, none of them `.` or `..` (vanilla's resource-location
/// characters, so the path means the same on every OS).
pub fn check_path(name: &str) -> Result<(), String> {
    let bad = || format!("{name:?} is not a plain assets/ or data/ path");
    let mut parts = name.split('/');
    if !matches!(parts.next(), Some("assets" | "data")) {
        return Err(bad());
    }
    let mut count = 0;
    for part in parts {
        count += 1;
        let chars_ok = part
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
        if part.is_empty() || part == "." || part == ".." || !chars_ok {
            return Err(bad());
        }
    }
    // assets/<namespace>/<file> at least.
    if count < 2 {
        return Err(bad());
    }
    Ok(())
}

/// World-generation data a mod with capability `worldgen` may ship without
/// capability `registries`: `data/<ns>/{worldgen, dimension_type, structure,
/// tags/worldgen}/...`.
pub fn is_worldgen_data(name: &str) -> bool {
    let mut parts = name.splitn(3, '/');
    let (top, _namespace, rest) = (parts.next(), parts.next(), parts.next().unwrap_or(""));
    top == Some("data")
        && [
            "worldgen/",
            "dimension_type/",
            "structure/",
            "tags/worldgen/",
        ]
        .iter()
        .any(|p| rest.starts_with(p))
}

/// A vanilla identifier, `minecraft:path`.
fn vanilla_id(id: &str) -> bool {
    id.strip_prefix("minecraft:").is_some_and(|path| {
        !path.is_empty()
            && path
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_./-".contains(&b))
    })
}

/// What a declared block changes from its template; unset keeps the
/// template's value.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlockProperties {
    /// Destroy time in seconds of hand mining (-1: unbreakable).
    pub hardness: Option<f32>,
    pub explosion_resistance: Option<f32>,
    /// Light emitted, 0 to 15.
    pub light: Option<u8>,
    pub requires_tool: Option<bool>,
    pub friction: Option<f32>,
    /// A `MapColor` id, 0 to 61.
    pub map_color: Option<u8>,
    /// A vanilla block whose sounds this block uses.
    pub sounds: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockFile {
    template: String,
    #[serde(default)]
    properties: BlockProperties,
    #[serde(default = "yes")]
    item: bool,
    #[serde(default)]
    states: Option<BlockStates>,
}

/// A block's own states (fork `afterwinter`), in place of its template's:
/// the template then gives each state's other facts (behaviour, sounds,
/// hardness...), from its default state. States go as Minecraft's
/// `StateDefinition` makes them: properties by name, the last one changing
/// fastest, values in their listed order.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlockStates {
    pub properties: BTreeMap<String, PropertyValues>,
    /// The default state's values; a property not named has its first.
    #[serde(default)]
    pub default: BTreeMap<String, String>,
    /// The shapes, in sixteenths of a block: a state takes the first rule
    /// whose `when` it matches; a state no rule matches has the template's.
    #[serde(default)]
    pub shapes: Vec<ShapeRule>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum PropertyValues {
    Named(Vec<String>),
    /// `{"integer": [min, max]}`.
    Integer {
        integer: [u32; 2],
    },
}

impl PropertyValues {
    pub fn values(&self) -> Vec<String> {
        match self {
            PropertyValues::Named(v) => v.clone(),
            PropertyValues::Integer { integer: [a, b] } => {
                (*a..=*b).map(|v| v.to_string()).collect()
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OneOrMore {
    One(String),
    More(Vec<String>),
}

impl OneOrMore {
    pub fn contains(&self, v: &str) -> bool {
        match self {
            OneOrMore::One(x) => x == v,
            OneOrMore::More(xs) => xs.iter().any(|x| x == v),
        }
    }
}

/// A box: `[x1, y1, z1, x2, y2, z2]` in sixteenths (`Block.box`).
pub type Box16 = [f64; 6];

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShapeRule {
    /// The states it is for: each property named has one of these values.
    #[serde(default)]
    pub when: BTreeMap<String, OneOrMore>,
    /// The outline (what the cursor shows and picks).
    pub outline: Vec<Box16>,
    /// What entities collide with; the outline if not given.
    #[serde(default)]
    pub collision: Option<Vec<Box16>>,
    /// Whether it hides the faces of blocks it touches and blocks light;
    /// by default only a full cube does.
    #[serde(default)]
    pub occludes: Option<bool>,
}

/// The most states one block declares.
pub const MAX_BLOCK_STATES: usize = 4096;
fn yes() -> bool {
    true
}

/// A block the mod adds: a vanilla template's behaviour, states and shapes
/// under a new id, with `properties` applied.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// `<mod id>:<path>`.
    pub id: String,
    /// The vanilla block it copies, `minecraft:<path>`.
    pub template: String,
    pub properties: BlockProperties,
    /// Also add a block item with the same id.
    pub item: bool,
    /// Its own states, in place of the template's.
    pub states: Option<BlockStates>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemFile {
    template: String,
    max_stack_size: Option<u8>,
    rarity: Option<Rarity>,
}

/// An item the mod adds: a vanilla template item's components under a new
/// id, with these changed.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    /// The vanilla item it copies, `minecraft:<path>`.
    pub template: String,
    pub max_stack_size: Option<u8>,
    pub rarity: Option<Rarity>,
}

#[derive(Debug)]
enum Source {
    Dir(PathBuf),
    Zip(PathBuf),
}

/// A mod's content: its files and declarations.
#[derive(Debug)]
pub struct Content {
    source: Source,
    /// Every content file (`assets/...`, `data/...`), sorted.
    pub files: Vec<String>,
    pub blocks: Vec<Block>,
    pub items: Vec<Item>,
    /// The namespaces it owns: the mod id, then the manifest's `namespaces`.
    pub namespaces: Vec<String>,
    /// The options of the world presets it provides (`[[worldgen.settings]]`),
    /// for the create-world screen.
    pub preset_settings: Vec<crate::worldgen::PresetSetting>,
}

/// Reads a mod's content files ([`Content::reader`]).
pub enum FileReader<'a> {
    Dir(&'a Path),
    Zip(Box<zip::ZipArchive<std::fs::File>>),
}
impl FileReader<'_> {
    /// One content file's bytes.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, String> {
        check_path(name)?;
        let reader: Box<dyn Read + '_> = match self {
            Self::Dir(root) => {
                Box::new(std::fs::File::open(root.join(name)).map_err(|e| format!("{name}: {e}"))?)
            }
            Self::Zip(zip) => Box::new(zip.by_name(name).map_err(|e| format!("{name}: {e}"))?),
        };
        let mut out = Vec::new();
        reader
            .take(MAX_FILE + 1)
            .read_to_end(&mut out)
            .map_err(|e| format!("{name}: {e}"))?;
        if out.len() as u64 > MAX_FILE {
            return Err(format!("{name} is over {} MiB", MAX_FILE >> 20));
        }
        Ok(out)
    }
}

/// Why two content mods cannot be on together: a namespace both own (one's
/// id or listed namespace is the other's), which would mix their files.
/// `a` and `b` are (mod id, owned namespaces).
pub fn clash(a: (&str, &[String]), b: (&str, &[String])) -> Option<String> {
    let shared = a.1.iter().find(|ns| b.1.contains(ns))?;
    Some(format!(
        "mods {} and {} both use the namespace {shared}; turn one of them off",
        a.0, b.0
    ))
}

impl Content {
    /// The content of the mod at `path` (a `.hmod` or a mod folder).
    pub fn read(path: &Path, manifest: &Manifest) -> Result<Content, String> {
        let (source, sized) = if path.is_dir() {
            let mut out = Vec::new();
            for top in ["assets", "data"] {
                walk(path, &path.join(top), &mut out)?;
            }
            (Source::Dir(path.to_owned()), out)
        } else {
            let mut zip = open_zip(path)?;
            let mut out = Vec::new();
            for i in 0..zip.len() {
                let entry = zip.by_index_raw(i).map_err(|e| e.to_string())?;
                let name = entry.name();
                if (name.starts_with("assets/") || name.starts_with("data/")) && !entry.is_dir() {
                    out.push((name.to_owned(), entry.size()));
                }
            }
            (Source::Zip(path.to_owned()), out)
        };
        let mut files = Vec::with_capacity(sized.len());
        let mut total = 0u64;
        for (name, size) in sized {
            check_path(&name)?;
            if size > MAX_FILE {
                return Err(format!("{name} is over {} MiB", MAX_FILE >> 20));
            }
            total += size;
            files.push(name);
        }
        if files.len() > MAX_FILES {
            return Err(format!("more than {MAX_FILES} content files"));
        }
        if total > MAX_TOTAL {
            return Err(format!("content is over {} MiB", MAX_TOTAL >> 20));
        }
        files.sort();
        files.dedup();
        let mut content = Content {
            source,
            files,
            blocks: Vec::new(),
            items: Vec::new(),
            namespaces: manifest.owned_namespaces().map(str::to_owned).collect(),
            preset_settings: manifest
                .worldgen
                .as_ref()
                .map(|w| w.settings.clone())
                .unwrap_or_default(),
        };
        content.check(manifest)?;
        Ok(content)
    }

    /// Where the files are read from: the mod's folder or its `.hmod`.
    pub fn source(&self) -> &Path {
        match &self.source {
            Source::Dir(path) | Source::Zip(path) => path,
        }
    }

    /// No files: a mod with code only.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// One content file's bytes.
    pub fn file(&self, name: &str) -> Result<Vec<u8>, String> {
        self.reader()?.read(name)
    }

    /// A reader of its content files. A packaged mod's archive is opened
    /// once for all of them (opening it reads its whole index).
    pub fn reader(&self) -> Result<FileReader<'_>, String> {
        Ok(match &self.source {
            Source::Dir(root) => FileReader::Dir(root),
            Source::Zip(path) => FileReader::Zip(Box::new(open_zip(path)?)),
        })
    }

    /// Where content may go, and the declarations.
    fn check(&mut self, manifest: &Manifest) -> Result<(), String> {
        let id = manifest.id.as_str();
        // A world-generation mod (fork `afterwinter`) may ship its world
        // generation's data without registries: presets, dimension types,
        // worldgen registries, structure templates and worldgen tags.
        let worldgen_only = manifest.capabilities.contains(&Capability::Worldgen)
            && self.files.iter().all(|f| is_worldgen_data(f));
        if !self.files.is_empty()
            && !manifest.capabilities.contains(&Capability::Registries)
            && !worldgen_only
        {
            return Err(format!(
                "has assets/ or data/ but not capability registries in {MANIFEST} \
                 (a worldgen mod may ship data/<ns>/worldgen, dimension_type, structure and tags/worldgen without it)"
            ));
        }
        let mut blocks = Vec::new();
        let mut items = Vec::new();
        for name in &self.files {
            let mut parts = name.splitn(3, '/');
            let (top, namespace, rest) = (
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
            );
            let own = namespace == id;
            let owned = own || manifest.namespaces.iter().any(|n| n == namespace);
            // Vanilla's own files stay vanilla's; a mod may only add to its
            // tags, and to the convention tags mods share.
            let shared = matches!(namespace, "minecraft" | "c");
            let allowed = owned || (top == "data" && shared && rest.starts_with("tags/"));
            if !allowed {
                return Err(format!(
                    "{name}: a mod adds files in its own namespaces ({top}/{id}/ and those listed in namespaces) or to vanilla and convention (c) tags"
                ));
            }
            if !own && top == "data" && rest.starts_with("horizon/") {
                return Err(format!(
                    "{name}: blocks and items are declared in data/{id}/horizon/"
                ));
            }
            let Some(decl) = (own && top == "data")
                .then(|| rest.strip_prefix("horizon/"))
                .flatten()
            else {
                continue;
            };
            let parse = |kind: &str| -> Option<String> {
                let path = decl
                    .strip_prefix(kind)?
                    .strip_prefix('/')?
                    .strip_suffix(".json")?;
                Some(format!("{id}:{path}"))
            };
            if let Some(block) = parse("block") {
                blocks.push((block, name.clone()));
            } else if let Some(item) = parse("item") {
                items.push((item, name.clone()));
            } else if parse("poi_type").is_some() || parse("villager_profession").is_some() {
                // Read by the game's profession registry (horizon_client::professions).
            } else {
                return Err(format!(
                    "{name}: data/{id}/horizon/ holds block/, item/, poi_type/ and \
                     villager_profession/ *.json only"
                ));
            }
        }
        let mut ids = BTreeSet::new();
        for (block, name) in blocks {
            let file: BlockFile =
                serde_json::from_slice(&self.file(&name)?).map_err(|e| format!("{name}: {e}"))?;
            check_block(&file).map_err(|e| format!("{name}: {e}"))?;
            if file.item {
                ids.insert(block.clone());
            }
            self.blocks.push(Block {
                id: block,
                template: file.template,
                properties: file.properties,
                item: file.item,
                states: file.states,
            });
        }
        for (item, name) in items {
            let file: ItemFile =
                serde_json::from_slice(&self.file(&name)?).map_err(|e| format!("{name}: {e}"))?;
            if !vanilla_id(&file.template) {
                return Err(format!(
                    "{name}: template {:?} is not a vanilla item (minecraft:...)",
                    file.template
                ));
            }
            if file.max_stack_size.is_some_and(|n| !(1..=99).contains(&n)) {
                return Err(format!("{name}: max_stack_size is 1 to 99"));
            }
            if !ids.insert(item.clone()) {
                return Err(format!("{name}: {item} is also a block's item"));
            }
            self.items.push(Item {
                id: item,
                template: file.template,
                max_stack_size: file.max_stack_size,
                rarity: file.rarity,
            });
        }
        Ok(())
    }
}

fn check_block(file: &BlockFile) -> Result<(), String> {
    if !vanilla_id(&file.template) {
        return Err(format!(
            "template {:?} is not a vanilla block (minecraft:...)",
            file.template
        ));
    }
    let p = &file.properties;
    let finite = |v: Option<f32>| v.is_none_or(f32::is_finite);
    if !finite(p.hardness) || !finite(p.explosion_resistance) || !finite(p.friction) {
        return Err("hardness, explosion_resistance and friction are numbers".into());
    }
    if p.hardness.is_some_and(|h| h < 0.0 && h != -1.0) {
        return Err("hardness is 0 or more, or -1 (unbreakable)".into());
    }
    if p.explosion_resistance.is_some_and(|r| r < 0.0) {
        return Err("explosion_resistance is 0 or more".into());
    }
    if p.friction.is_some_and(|f| !(0.0..=2.0).contains(&f)) {
        return Err("friction is 0 to 2".into());
    }
    if p.light.is_some_and(|l| l > 15) {
        return Err("light is 0 to 15".into());
    }
    if p.map_color.is_some_and(|c| c > 61) {
        return Err("map_color is a MapColor id, 0 to 61".into());
    }
    if p.sounds.as_deref().is_some_and(|s| !vanilla_id(s)) {
        return Err("sounds names a vanilla block (minecraft:...)".into());
    }
    if let Some(states) = &file.states {
        check_states(states)?;
    }
    Ok(())
}

fn check_states(states: &BlockStates) -> Result<(), String> {
    let name_ok = |n: &str| {
        !n.is_empty()
            && n.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    if states.properties.is_empty() {
        return Err("states: no properties".into());
    }
    let mut count = 1usize;
    for (name, values) in &states.properties {
        if !name_ok(name) {
            return Err(format!("states: property {name:?} is not a-z, 0-9 and _"));
        }
        let values = values.values();
        if values.is_empty() || values.iter().any(|v| !name_ok(v)) {
            return Err(format!("states: {name} needs values of a-z, 0-9 and _"));
        }
        if values.iter().collect::<BTreeSet<_>>().len() != values.len() {
            return Err(format!("states: {name} has a value twice"));
        }
        count = count.saturating_mul(values.len());
    }
    if count > MAX_BLOCK_STATES {
        return Err(format!(
            "states: {count} states, more than {MAX_BLOCK_STATES}"
        ));
    }
    let has = |p: &str, v: &str| {
        states
            .properties
            .get(p)
            .is_some_and(|vs| vs.values().iter().any(|x| x == v))
    };
    for (p, v) in &states.default {
        if !has(p, v) {
            return Err(format!("states: default {p}={v} is not a value"));
        }
    }
    for rule in &states.shapes {
        for (p, v) in &rule.when {
            let values = match v {
                OneOrMore::One(x) => vec![x.clone()],
                OneOrMore::More(xs) => xs.clone(),
            };
            if let Some(bad) = values.iter().find(|x| !has(p, x)) {
                return Err(format!("states: shape for {p}={bad}, not a value"));
            }
        }
        for b in rule.outline.iter().chain(rule.collision.iter().flatten()) {
            let ok = b
                .iter()
                .all(|v| v.is_finite() && (-16.0..=32.0).contains(v))
                && b[0] < b[3]
                && b[1] < b[4]
                && b[2] < b[5];
            if !ok {
                return Err(format!("states: box {b:?} is not min < max within -16..32"));
            }
        }
    }
    Ok(())
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    zip::ZipArchive::new(file).map_err(|e| e.to_string())
}

/// Files under `dir`, relative to `root` with `/` separators. Symbolic links
/// are not followed: a mod folder holds its own files.
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) -> Result<(), String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in read.flatten() {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            walk(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
            let parts: Option<Vec<&str>> = rel.iter().map(|p| p.to_str()).collect();
            let name = parts.ok_or_else(|| format!("{} is not UTF-8", rel.display()))?;
            out.push((name.join("/"), meta.len()));
        }
        if out.len() > MAX_FILES {
            return Err(format!("more than {MAX_FILES} content files"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Manifest, Package};

    const MANIFEST_TEXT: &str = r#"
id = "ruby"
name = "Ruby"
version = "0.1.0"
api = "0.2"
side = "both"
capabilities = ["registries"]
"#;
    const BLOCK: &[u8] = br#"{"template": "minecraft:iron_block",
        "properties": {"hardness": 4.0, "light": 3, "sounds": "minecraft:amethyst_block"}}"#;
    const ITEM: &[u8] = br#"{"template": "minecraft:diamond", "rarity": "uncommon"}"#;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horizon-content-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn hmod(name: &str, manifest: &str, files: &[(&str, &[u8])]) -> Result<Content, String> {
        let dir = scratch(name);
        let path = dir.join("m.hmod");
        std::fs::write(&path, Package::write_files(manifest, None, files)?).unwrap();
        let package = Package::read(&path);
        let content = package.and_then(|p| Content::read(&path, &p.manifest));
        std::fs::remove_dir_all(&dir).unwrap();
        content
    }

    #[test]
    fn content_only_mod_round_trips() {
        let files: &[(&str, &[u8])] = &[
            ("data/ruby/horizon/block/ruby_block.json", BLOCK),
            ("data/ruby/horizon/item/ruby.json", ITEM),
            ("data/ruby/recipe/ruby_block.json", b"{}"),
            ("data/minecraft/tags/block/mineable/pickaxe.json", b"{}"),
            ("data/c/tags/item/gems/ruby.json", b"{}"),
            ("assets/ruby/textures/block/ruby_block.png", b"png"),
        ];
        let content = hmod("ok", MANIFEST_TEXT, files).unwrap();
        assert_eq!(content.files.len(), 6);
        assert_eq!(
            content.blocks,
            [Block {
                id: "ruby:ruby_block".into(),
                template: "minecraft:iron_block".into(),
                properties: BlockProperties {
                    hardness: Some(4.0),
                    light: Some(3),
                    sounds: Some("minecraft:amethyst_block".into()),
                    ..BlockProperties::default()
                },
                item: true,
                states: None,
            }]
        );
        assert_eq!(content.items[0].id, "ruby:ruby");
        assert_eq!(content.items[0].rarity, Some(Rarity::Uncommon));
    }

    #[test]
    fn a_worldgen_mod_ships_its_worldgen_data_without_registries() {
        let manifest = Manifest::parse(
            "id = \"terra\"\nname = \"T\"\nversion = \"1\"\napi = \"0.3\"\nside = \"server\"\n\
             capabilities = [\"worldgen\"]\nexperimental = [\"worldgen\"]\n\
             [worldgen]\npresets = [\"terra:earth\"]\nstages = [\"terrain\"]\n",
        )
        .unwrap();
        let read = |name: &str, files: &[&str]| {
            let dir = scratch(name);
            for f in files {
                let path = dir.join(f);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b"{}").unwrap();
            }
            let content = Content::read(&dir, &manifest);
            std::fs::remove_dir_all(&dir).unwrap();
            content
        };
        let content = read(
            "worldgen-ok",
            &[
                "data/terra/worldgen/world_preset/earth.json",
                "data/terra/dimension_type/highlands.json",
                "data/terra/structure/camp.nbt",
                "data/minecraft/tags/worldgen/biome/is_forest.json",
            ],
        )
        .unwrap();
        assert_eq!(content.files.len(), 4);
        for refused in [
            "assets/terra/lang/en_us.json",
            "data/terra/horizon/block/rock.json",
        ] {
            let e = read("worldgen-refused", &[refused]).unwrap_err();
            assert!(e.contains("not capability registries"), "{refused}: {e}");
        }
        assert!(is_worldgen_data(
            "data/terra/worldgen/world_preset/earth.json"
        ));
        assert!(!is_worldgen_data("data/terra/recipe/x.json"));
        assert!(!is_worldgen_data("assets/terra/worldgen/x.json"));
    }

    #[test]
    fn folder_mod_reads_the_same() {
        let dir = scratch("folder");
        std::fs::write(dir.join(MANIFEST), MANIFEST_TEXT).unwrap();
        let block = dir.join("data/ruby/horizon/block");
        std::fs::create_dir_all(&block).unwrap();
        std::fs::write(block.join("ruby_block.json"), BLOCK).unwrap();
        let package = Package::read(&dir).unwrap();
        assert!(package.wasm.is_none());
        let content = Content::read(&dir, &package.manifest).unwrap();
        assert_eq!(content.files, ["data/ruby/horizon/block/ruby_block.json"]);
        assert_eq!(content.file(&content.files[0]).unwrap(), BLOCK);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_content_outside_its_fence() {
        let refused = |name: &str, files: &[(&str, &[u8])]| {
            let e = hmod(name, MANIFEST_TEXT, files).unwrap_err();
            assert!(!e.is_empty());
            e
        };
        let e = refused("other-ns", &[("data/other/horizon/block/a.json", BLOCK)]);
        assert!(e.contains("own namespaces"), "{e}");
        refused(
            "vanilla-asset",
            &[("assets/minecraft/textures/block/stone.png", b"x")],
        );
        refused(
            "vanilla-recipe",
            &[("data/minecraft/recipe/stone.json", b"{}")],
        );
        refused("convention-recipe", &[("data/c/recipe/stone.json", b"{}")]);
        refused("convention-asset", &[("assets/c/lang/en_us.json", b"{}")]);
        let e = refused(
            "modded-template",
            &[(
                "data/ruby/horizon/block/a.json",
                br#"{"template": "ruby:other"}"#,
            )],
        );
        assert!(e.contains("not a vanilla block"), "{e}");
        refused(
            "light",
            &[(
                "data/ruby/horizon/block/a.json",
                br#"{"template": "minecraft:stone", "properties": {"light": 16}}"#,
            )],
        );
        refused(
            "unknown-key",
            &[(
                "data/ruby/horizon/block/a.json",
                br#"{"template": "minecraft:stone", "glow": true}"#,
            )],
        );
        let e = refused(
            "clash",
            &[
                ("data/ruby/horizon/block/a.json", BLOCK),
                ("data/ruby/horizon/item/a.json", ITEM),
            ],
        );
        assert!(e.contains("also a block's item"), "{e}");
        refused("other-kind", &[("data/ruby/horizon/entity/a.json", b"{}")]);
        refused(
            "stack",
            &[(
                "data/ruby/horizon/item/a.json",
                br#"{"template": "minecraft:stick", "max_stack_size": 100}"#,
            )],
        );
    }

    #[test]
    fn listed_namespaces_hold_content() {
        let manifest = format!("{MANIFEST_TEXT}namespaces = [\"kaisyn\"]\n");
        let files: &[(&str, &[u8])] = &[
            ("data/kaisyn/worldgen/template_pool/a.json", b"{}"),
            ("data/kaisyn/structure/a.nbt", b"nbt"),
            ("assets/kaisyn/lang/en_us.json", b"{}"),
        ];
        let content = hmod("listed", &manifest, files).unwrap();
        assert_eq!(content.files.len(), 3);
        assert_eq!(content.namespaces, ["ruby", "kaisyn"]);
        // Declarations stay in the id's namespace.
        let decl: &[(&str, &[u8])] = &[("data/kaisyn/horizon/block/a.json", BLOCK)];
        let e = hmod("listed-decl", &manifest, decl).unwrap_err();
        assert!(e.contains("declared in data/ruby/horizon/"), "{e}");
        // A namespace it does not list is still outside.
        let other: &[(&str, &[u8])] = &[("data/other/structure/a.nbt", b"nbt")];
        assert!(hmod("listed-other", &manifest, other).is_err());
    }

    #[test]
    fn mods_sharing_a_namespace_clash() {
        let t_and_t = ["t_and_t".to_owned(), "kaisyn".to_owned()];
        let kaisyn = ["kaisyn".to_owned()];
        let ruby = ["ruby".to_owned(), "gems".to_owned()];
        assert_eq!(
            clash(("t_and_t", &t_and_t), ("kaisyn", &kaisyn)).as_deref(),
            Some("mods t_and_t and kaisyn both use the namespace kaisyn; turn one of them off")
        );
        assert_eq!(clash(("t_and_t", &t_and_t), ("ruby", &ruby)), None);
    }

    #[test]
    fn content_needs_the_registries_capability() {
        let code_only = MANIFEST_TEXT
            .replace("api = \"0.2\"", "api = \"0.1\"")
            .replace("capabilities = [\"registries\"]", "capabilities = []");
        // No code and no registries: nothing to load.
        let e = hmod("empty", &code_only, &[]).unwrap_err();
        assert!(e.contains("mod.wasm: missing"), "{e}");
        // Registries on API 0.1.
        let old = MANIFEST_TEXT.replace("api = \"0.2\"", "api = \"0.1\"");
        assert!(Manifest::parse(&old).unwrap_err().contains("needs api"));
        // Content without the capability, in a mod with code.
        let dir = scratch("no-cap");
        let path = dir.join("m.hmod");
        let files: &[(&str, &[u8])] = &[("assets/ruby/lang/en_us.json", b"{}")];
        std::fs::write(
            &path,
            Package::write_files(&code_only, Some(b"\0asm"), files).unwrap(),
        )
        .unwrap();
        let package = Package::read(&path).unwrap();
        let e = Content::read(&path, &package.manifest).err().unwrap();
        assert!(e.contains("not capability registries"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paths_are_plain() {
        assert!(check_path("assets/ruby/textures/block/ruby_block.png").is_ok());
        assert!(check_path("data/ruby/loot_table/blocks/a.json").is_ok());
        for bad in [
            "assets/ruby",
            "assets/../x/y",
            "assets/ruby//a",
            "assets/Ruby/a",
            "assets/ruby/a b",
            "/assets/ruby/a",
            "other/ruby/a",
            "assets\\ruby\\a",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
    }
}
