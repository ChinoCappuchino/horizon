//! Mods' biomes, carried over (fork `afterwinter`, docs/research/climate-regions.md):
//! a 1.21.1 biome file in 26.3's shape. 1.21.1 keeps a biome's look and
//! sounds in `effects` as integers and its mobs in `spawners`; 26.3 moves
//! most of them into `attributes` (colours as `#rrggbb`), keeps only the
//! water, grass and foliage colours in `effects`, and has one carver list
//! where 1.21.1 had one per carving step.
//!
//! An overworld value equal to the dimension's own (the fog, the water's fog,
//! the cave mood) is left out, as vanilla's 26.3 biomes leave it out: written
//! as a biome's, it would hold the fog at one colour where 26.3's changes
//! with the time of day.
use serde_json::{Map, Value, json};

/// 1.21.1's fog and water fog colours, and the cave mood, of every vanilla
/// overworld biome: the dimension's own in 26.3.
const DEFAULT_FOG: i64 = 0xc0d8ff;
const DEFAULT_WATER_FOG: i64 = 0x050533;
const DEFAULT_MOOD: &str = "minecraft:ambient.cave";

/// `effects` keys that stay in `effects` (colours as `#rrggbb`).
const KEPT_COLORS: [&str; 4] = [
    "water_color",
    "grass_color",
    "foliage_color",
    "dry_foliage_color",
];

/// Whether `v` is a 1.21.1 biome (its carvers by step, or spawners).
pub fn is_old(v: &Value) -> bool {
    v["carvers"].is_object()
        || v.get("spawners").is_some()
        || v["effects"].get("sky_color").is_some()
}

/// Rewrites a 1.21.1 biome in 26.3's shape, in place; whether it changed.
pub fn upgrade(v: &mut Value) -> bool {
    if !is_old(v) {
        return false;
    }
    let Some(o) = v.as_object_mut() else {
        return false;
    };
    let mut attributes = match o.remove("attributes") {
        Some(Value::Object(a)) => a,
        _ => Map::new(),
    };
    // One carver list: the air step's, then the liquid step's.
    if let Some(Value::Object(steps)) = o.get("carvers").cloned() {
        let mut list = Vec::new();
        for step in ["air", "liquid"] {
            match steps.get(step) {
                Some(Value::Array(a)) => list.extend(a.iter().cloned()),
                Some(one @ Value::String(_)) => list.push(one.clone()),
                _ => {}
            }
        }
        o.insert("carvers".into(), Value::Array(list));
    }
    let effects = match o.remove("effects") {
        Some(Value::Object(e)) => e,
        _ => Map::new(),
    };
    let mut kept = Map::new();
    for (key, value) in &effects {
        match key.as_str() {
            k if KEPT_COLORS.contains(&k) => {
                kept.insert(key.clone(), color(value));
            }
            "grass_color_modifier" => {
                kept.insert(key.clone(), value.clone());
            }
            "sky_color" => {
                attributes.insert("minecraft:visual/sky_color".into(), color(value));
            }
            "fog_color" if value.as_i64() != Some(DEFAULT_FOG) => {
                attributes.insert("minecraft:visual/fog_color".into(), color(value));
            }
            "water_fog_color" if value.as_i64() != Some(DEFAULT_WATER_FOG) => {
                attributes.insert("minecraft:visual/water_fog_color".into(), color(value));
            }
            "music" => {
                let m = json!({
                    "sound": value["sound"],
                    "min_delay": value["min_delay"],
                    "max_delay": value["max_delay"],
                });
                attributes.insert(
                    "minecraft:audio/background_music".into(),
                    json!({"default": m}),
                );
            }
            "particle" => {
                attributes.insert(
                    "minecraft:visual/ambient_particles".into(),
                    json!({
                        "argument": [{"particle": value["options"], "probability": value["probability"]}],
                        "modifier": "append",
                    }),
                );
            }
            _ => {}
        }
    }
    // The sounds of the place: its loop, mood and additions, in one attribute.
    let mut sounds = Map::new();
    if let Some(s) = effects.get("ambient_sound") {
        sounds.insert("loop".into(), s.clone());
    }
    if let Some(m) = effects
        .get("mood_sound")
        .filter(|m| m["sound"].as_str() != Some(DEFAULT_MOOD))
    {
        sounds.insert("mood".into(), m.clone());
    }
    if let Some(a) = effects.get("additions_sound") {
        sounds.insert("additions".into(), a.clone());
    }
    if !sounds.is_empty() {
        attributes.insert(
            "minecraft:audio/ambient_sounds".into(),
            Value::Object(sounds),
        );
    }
    o.insert("effects".into(), Value::Object(kept));
    // Mobs: by category, a count range or one count, as 26.3 writes them.
    let spawners = o.remove("spawners");
    let costs = o.remove("spawn_costs").unwrap_or(json!({}));
    if let Some(Value::Object(categories)) = spawners {
        let mut by_category = Map::new();
        for (category, list) in categories {
            let entries: Vec<Value> = list
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| {
                    let (lo, hi) = (&e["minCount"], &e["maxCount"]);
                    let count = if lo == hi {
                        lo.clone()
                    } else {
                        json!({"type": "minecraft:uniform", "min_inclusive": lo, "max_inclusive": hi})
                    };
                    json!({"type": e["type"], "count": count, "weight": e["weight"]})
                })
                .collect();
            if !entries.is_empty() {
                by_category.insert(category, Value::Array(entries));
            }
        }
        attributes.insert(
            "minecraft:gameplay/natural_mob_spawns".into(),
            json!({
                "argument": {"spawn_costs": costs, "spawns_by_category": by_category},
                "modifier": "overlay",
            }),
        );
    }
    if let Some(p) = o.remove("creature_spawn_probability") {
        attributes.insert(
            "minecraft:gameplay/creature_world_gen_spawn_probability".into(),
            p,
        );
    }
    o.insert("attributes".into(), Value::Object(attributes));
    true
}

/// A colour as 26.3 writes it: `#rrggbb` (one already a string stays).
fn color(v: &Value) -> Value {
    match v.as_i64() {
        Some(c) => format!("#{:06x}", c & 0xff_ffff).into(),
        None => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Biomes O' Plenty's aspen glade, as its 1.21.1 jar has it (trimmed).
    fn glade() -> Value {
        json!({
            "carvers": {"air": ["minecraft:cave", "minecraft:canyon"]},
            "downfall": 0.8,
            "effects": {
                "fog_color": 12638463, "foliage_color": 13746762, "grass_color": 14527306,
                "mood_sound": {"block_search_extent": 8, "offset": 2.0,
                    "sound": "minecraft:ambient.cave", "tick_delay": 6000},
                "music": {"max_delay": 24000, "min_delay": 12000, "replace_current_music": false,
                    "sound": "minecraft:music.overworld.forest"},
                "sky_color": 8168703, "water_color": 4159204, "water_fog_color": 329011
            },
            "features": [[], ["minecraft:lake_lava_underground"]],
            "has_precipitation": true,
            "spawn_costs": {},
            "spawners": {
                "creature": [
                    {"type": "minecraft:sheep", "maxCount": 4, "minCount": 4, "weight": 12},
                    {"type": "minecraft:rabbit", "maxCount": 3, "minCount": 2, "weight": 4}
                ],
                "misc": []
            },
            "temperature": 0.6
        })
    }

    #[test]
    fn a_1_21_1_biome_reads_as_26_3_writes_it() {
        let mut v = glade();
        assert!(upgrade(&mut v));
        assert_eq!(v["carvers"], json!(["minecraft:cave", "minecraft:canyon"]));
        assert_eq!(
            v["effects"],
            json!({"foliage_color": "#d1c24a", "grass_color": "#ddab4a", "water_color": "#3f76e4"})
        );
        let a = &v["attributes"];
        assert_eq!(a["minecraft:visual/sky_color"], "#7ca4ff");
        // The overworld's own fog, water fog and mood are the dimension's.
        for gone in [
            "minecraft:visual/fog_color",
            "minecraft:visual/water_fog_color",
            "minecraft:audio/ambient_sounds",
        ] {
            assert!(a.get(gone).is_none(), "{gone}");
        }
        assert_eq!(
            a["minecraft:audio/background_music"]["default"],
            json!({"sound": "minecraft:music.overworld.forest", "min_delay": 12000, "max_delay": 24000})
        );
        let spawns = &a["minecraft:gameplay/natural_mob_spawns"];
        assert_eq!(spawns["modifier"], "overlay");
        assert_eq!(
            spawns["argument"]["spawns_by_category"],
            json!({"creature": [
                {"type": "minecraft:sheep", "count": 4, "weight": 12},
                {"type": "minecraft:rabbit", "count": {"type": "minecraft:uniform",
                    "min_inclusive": 2, "max_inclusive": 3}, "weight": 4}
            ]})
        );
        assert!(v.get("spawners").is_none() && v.get("spawn_costs").is_none());
        assert_eq!(v["features"], glade()["features"]);
        // Once is enough.
        assert!(!upgrade(&mut v));
    }

    #[test]
    fn a_cave_biome_keeps_its_own_fog_sounds_and_particles() {
        let mut v = glade();
        v["effects"] = json!({
            "fog_color": 1843465, "water_fog_color": 267827, "sky_color": 8168703,
            "water_color": 4159204,
            "ambient_sound": "minecraft:ambient.crimson_forest.loop",
            "mood_sound": {"block_search_extent": 8, "offset": 2.0,
                "sound": "minecraft:ambient.crimson_forest.mood", "tick_delay": 6000},
            "additions_sound": {"sound": "minecraft:ambient.basalt_deltas.additions", "tick_chance": 0.0111},
            "particle": {"options": {"type": "minecraft:electric_spark"}, "probability": 0.0008925}
        });
        v["carvers"] = json!({"air": "#biomesoplenty:caves", "liquid": ["minecraft:ocean_cave"]});
        assert!(upgrade(&mut v));
        let a = &v["attributes"];
        assert_eq!(a["minecraft:visual/fog_color"], "#1c2109");
        assert_eq!(a["minecraft:visual/water_fog_color"], "#041633");
        let sounds = &a["minecraft:audio/ambient_sounds"];
        assert_eq!(sounds["loop"], "minecraft:ambient.crimson_forest.loop");
        assert_eq!(
            sounds["mood"]["sound"],
            "minecraft:ambient.crimson_forest.mood"
        );
        assert_eq!(sounds["additions"]["tick_chance"], 0.0111);
        assert_eq!(
            a["minecraft:visual/ambient_particles"],
            json!({"argument": [{"particle": {"type": "minecraft:electric_spark"},
                "probability": 0.0008925}], "modifier": "append"})
        );
        assert_eq!(
            v["carvers"],
            json!(["#biomesoplenty:caves", "minecraft:ocean_cave"])
        );
    }
}
