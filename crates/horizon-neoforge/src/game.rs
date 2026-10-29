//! What the compatibility layer knows of the game's blocks, from the
//! prepared resource root (the same tables Horizon loads): each block's
//! Java class chain, its properties with their values in order, its default
//! state and its sounds. A vanilla block class's own properties and default
//! come from a block of that class: the classes are not written out by hand.
use serde_json::Value;
use std::{collections::BTreeMap, io, path::Path};

#[derive(Clone, Debug, Default)]
pub struct VanillaBlock {
    pub id: String,
    /// The Java class chain, the block's own class first (`CropBlock`,
    /// `BushBlock`, `Block`).
    pub classes: Vec<String>,
    /// Its properties, by name, each with its values in state order.
    pub properties: Vec<(String, Vec<String>)>,
    /// The default state's values.
    pub default: BTreeMap<String, String>,
    /// Its `block.<x>.break` sound event.
    pub break_sound: Option<String>,
    /// Its default state's facts, as `Properties.ofFullCopy` takes them.
    pub hardness: Option<f32>,
    pub resistance: Option<f32>,
    pub requires_tool: bool,
    /// A `MapColor` id.
    pub map_color: Option<u8>,
    pub friction: Option<f32>,
    pub light: Option<u8>,
    pub collides: bool,
    pub occludes: bool,
}

/// Block classes the NeoForge 1.21.1 profile names otherwise than the game
/// Horizon runs (26.3). To come from the profile's mapping data
/// (docs/java-compat/README.md section 2.5.1); these are the ones the
/// pack's mods use.
const RENAMED: [(&str, &str); 1] = [("FarmBlock", "FarmlandBlock")];

/// A 1.21.1 block class's name in the game's data.
pub fn current(class: &str) -> &str {
    RENAMED
        .iter()
        .find(|(old, _)| *old == class)
        .map_or(class, |(_, new)| new)
}

#[derive(Default)]
pub struct GameData {
    pub blocks: BTreeMap<String, VanillaBlock>,
    /// Item ids.
    pub items: Vec<String>,
    /// Each game state's id, by `name[k=v,...]` (properties by name).
    states: std::collections::HashMap<String, usize>,
    /// Each state's outline and collision boxes, in blocks.
    outlines: Vec<Vec<[f64; 6]>>,
    collisions: Vec<Vec<[f64; 6]>>,
    /// The game's sound events, without `minecraft:` (`block.stone.break`).
    sounds: Vec<String>,
}

fn state_key(name: &str, props: &BTreeMap<String, String>) -> String {
    let p: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{name}[{}]", p.join(","))
}

fn boxes(v: &Value) -> Vec<[f64; 6]> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let a: Vec<f64> = b.as_array()?.iter().filter_map(Value::as_f64).collect();
            a.try_into().ok()
        })
        .collect()
}

fn read(root: &Path, name: &str) -> io::Result<Value> {
    let bytes = std::fs::read(root.join(name))?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::other(format!("{name}: {e}")))
}

impl GameData {
    /// From a prepared root (`.local/horizon-client/26.3`).
    pub fn load(root: &Path) -> io::Result<GameData> {
        let facts = read(root, "facts.json")?;
        let gameplay = read(root, "gameplay-facts.json")?;
        let behaviours = read(root, "block-behaviours.json")?;
        let mut blocks: BTreeMap<String, VanillaBlock> = BTreeMap::new();
        let states = facts["states"].as_array().cloned().unwrap_or_default();
        let defaults = facts["defaults"].as_object().cloned().unwrap_or_default();
        let rows = gameplay["states"].as_array().cloned().unwrap_or_default();
        for (i, s) in states.iter().enumerate() {
            let Some(name) = s[0].as_str() else { continue };
            let b = blocks
                .entry(name.to_owned())
                .or_insert_with(|| VanillaBlock {
                    id: name.to_owned(),
                    ..VanillaBlock::default()
                });
            let props = s[4]["properties"].as_object().cloned().unwrap_or_default();
            // Values in the order the states first show them.
            let mut names: Vec<&String> = props.keys().collect();
            names.sort();
            for k in names {
                let v = props[k].as_str().unwrap_or_default().to_owned();
                match b.properties.iter_mut().find(|(n, _)| n == k) {
                    Some((_, values)) => {
                        if !values.contains(&v) {
                            values.push(v);
                        }
                    }
                    None => b.properties.push((k.clone(), vec![v])),
                }
            }
            if defaults.get(name).and_then(Value::as_u64) == Some(i as u64) {
                b.default = props
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                    .collect();
                if let Some(r) = rows.get(i) {
                    b.break_sound = r["sound"]["break"]["id"].as_str().map(str::to_owned);
                    b.hardness = r["hardness"].as_f64().map(|v| v as f32);
                    b.resistance = r["explosion_resistance"].as_f64().map(|v| v as f32);
                    b.requires_tool = r["requires_tool"].as_bool().unwrap_or(false);
                    b.map_color = r["map_color"].as_u64().and_then(|rgb| {
                        horizon_items::blocks::MAP_COLORS
                            .iter()
                            .position(|c| u64::from(*c) == rgb)
                            .map(|i| i as u8)
                    });
                    b.friction = r["friction"].as_f64().map(|v| v as f32);
                    b.light = r["emission"].as_u64().map(|v| v as u8);
                    b.collides = r["outline"].as_array().is_some_and(|a| !a.is_empty());
                    b.occludes = r["can_occlude"].as_bool().unwrap_or(true);
                }
            }
        }
        for b in behaviours["blocks"].as_array().into_iter().flatten() {
            let (Some(name), Some(classes)) = (b["name"].as_str(), b["class"].as_array()) else {
                continue;
            };
            if let Some(v) = blocks.get_mut(name) {
                v.classes = classes
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect();
            }
        }
        let items = read(root, "items.json")
            .ok()
            .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
            .unwrap_or_default();
        // Every state's shapes: the outline from the gameplay facts, the
        // collision from the game's collision table.
        let mut index = std::collections::HashMap::new();
        for (i, s) in states.iter().enumerate() {
            let Some(name) = s[0].as_str() else { continue };
            let props: BTreeMap<String, String> = s[4]["properties"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                .collect();
            index.insert(state_key(name, &props), i);
        }
        let outlines = rows.iter().map(|r| boxes(&r["outline"])).collect();
        let collisions = Self::collisions(root, states.len()).unwrap_or_default();
        let sounds = read(root, "registries.json")
            .ok()
            .and_then(|v| {
                v["minecraft:sound_event"]["entries"].as_object().map(|o| {
                    o.keys()
                        .map(|k| k.trim_start_matches("minecraft:").to_owned())
                        .collect()
                })
            })
            .unwrap_or_default();
        Ok(GameData {
            blocks,
            items,
            states: index,
            outlines,
            collisions,
            sounds,
        })
    }

    fn collisions(root: &Path, count: usize) -> io::Result<Vec<Vec<[f64; 6]>>> {
        use horizon_world::{
            collision::CachedCollisionRegistry,
            nbt::{Limits, read_named},
            registry::{BlockRegistry, BlockStateId},
        };
        let registry =
            BlockRegistry::from_facts_json(std::fs::File::open(root.join("facts.json"))?)?;
        let bytes = std::fs::read(root.join("cached-collision.nbt"))?;
        let (named, _) =
            read_named(&bytes, Limits::tables()).map_err(|e| io::Error::other(e.to_string()))?;
        let cached = CachedCollisionRegistry::from_nbt(&named.tag, &registry)?;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let boxes = match cached.cached_shape(BlockStateId(i as u32)) {
                Ok(Some(shape)) => shape
                    .boxes()
                    .iter()
                    .map(|b| {
                        use horizon_core::geometry::Axis::{X, Y, Z};
                        [b.min(X), b.min(Y), b.min(Z), b.max(X), b.max(Y), b.max(Z)]
                    })
                    .collect(),
                _ => Vec::new(),
            };
            out.push(boxes);
        }
        Ok(out)
    }

    /// The sound event a `SoundEvents` field names: the event whose id, its
    /// dots as underscores, is the field's name or ends with it
    /// (`BUBBLE_COLUMN_UPWARDS_AMBIENT` is `block.bubble_column.upwards_ambient`).
    pub fn sound_for_field(&self, field: &str) -> String {
        let want = field.to_lowercase();
        let flat = |s: &str| s.replace('.', "_");
        let found = self.sounds.iter().find(|s| flat(s) == want).or_else(|| {
            self.sounds
                .iter()
                .find(|s| flat(s).ends_with(&format!("_{want}")))
        });
        format!(
            "minecraft:{}",
            found.cloned().unwrap_or_else(|| want.replace('_', "."))
        )
    }

    /// A game block's outline and collision in the state with these values
    /// (its default's for the rest).
    pub fn state_shapes(
        &self,
        block: &str,
        values: &BTreeMap<String, String>,
    ) -> Option<(Vec<[f64; 6]>, Vec<[f64; 6]>)> {
        let b = self.blocks.get(block)?;
        let mut props = b.default.clone();
        for (k, v) in values {
            if let Some(slot) = props.get_mut(k)
                && b.properties.iter().any(|(n, vs)| n == k && vs.contains(v))
            {
                *slot = v.clone();
            }
        }
        let i = *self.states.get(&state_key(block, &props))?;
        Some((
            self.outlines.get(i).cloned().unwrap_or_default(),
            self.collisions.get(i).cloned().unwrap_or_default(),
        ))
    }

    /// The vanilla block that stands for a block class: of that class
    /// itself, the one with the fewest states (then by id).
    pub fn representative(&self, class: &str) -> Option<&VanillaBlock> {
        let class = current(class);
        self.blocks
            .values()
            .filter(|b| b.classes.first().map(String::as_str) == Some(class))
            .min_by_key(|b| {
                (
                    b.properties.iter().map(|(_, v)| v.len()).product::<usize>(),
                    b.id.clone(),
                )
            })
    }

    /// Whether a game class's collision differs from its outline (it
    /// overrides `getCollisionShape`): in some state of its representative.
    pub fn own_collision(&self, class: &str) -> bool {
        let Some(rep) = self.representative(class) else {
            return false;
        };
        self.states.iter().any(|(k, &i)| {
            k.starts_with(&format!("{}[", rep.id)) && self.outlines.get(i) != self.collisions.get(i)
        })
    }

    /// Whether a class (simple name) is a block class of the game.
    pub fn is_block_class(&self, class: &str) -> bool {
        let class = current(class);
        self.blocks
            .values()
            .any(|b| b.classes.iter().any(|c| c == class))
    }

    /// A block with this break sound (`block.wood.break`), for a SoundType.
    pub fn block_with_sound(&self, sound: &str) -> Option<&str> {
        let want = format!("minecraft:block.{sound}.break");
        self.blocks
            .values()
            .find(|b| b.break_sound.as_deref() == Some(want.as_str()))
            .map(|b| b.id.as_str())
    }

    /// A property's values as the game lists them, for a property name any
    /// block of `class` has (`CropBlock.AGE` is wheat's `age`).
    pub fn class_property(&self, class: &str, name: &str) -> Option<Vec<String>> {
        let class = current(class);
        let rep = self.representative(class).or_else(|| {
            self.blocks
                .values()
                .find(|b| b.classes.iter().any(|c| c == class))
        })?;
        rep.properties
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }
}
