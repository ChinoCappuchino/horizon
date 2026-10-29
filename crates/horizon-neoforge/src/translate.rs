//! A NeoForge mod's content, from its own jar: the layer runs the mod's
//! constructor and its registers as NeoForge would, then asks each block
//! for its states, shapes and light, and writes what it registered as a
//! Horizon content mod's declarations (docs/mods.md, "Blocks with their own
//! states").
use crate::{game::GameData, host::NeoForge, objects::*};
use horizon_java::{
    ClassFile,
    classfile::ElementValue,
    compile::compile,
    descriptor::Method,
    runtime::{Context, Runtime, Value},
};
use serde_json::{Map, Value as Json, json};
use std::{
    collections::BTreeMap,
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
};
use wasmtime::Result;

/// What a mod registered, as content declarations.
#[derive(Default)]
pub struct Translation {
    pub mod_id: String,
    /// Block id and its `data/<ns>/horizon/block/<path>.json`.
    pub blocks: Vec<(String, Json)>,
    pub items: Vec<(String, Json)>,
    /// Calls the layer does not model, and how often.
    pub unknown: BTreeMap<String, usize>,
    /// Entries that failed to register, with why.
    pub errors: Vec<String>,
    /// Methods the compiler did not compile.
    pub skipped: usize,
}

const SHAPE: &str = "(Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/BlockGetter;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/phys/shapes/CollisionContext;)Lnet/minecraft/world/phys/shapes/VoxelShape;";

/// Translates a mod's classes against the game data of a prepared root.
pub fn translate(classes: &[ClassFile], game: Arc<GameData>) -> Result<Translation> {
    let compiled = compile(classes).map_err(wasmtime::Error::msg)?;
    let skipped = compiled.skipped.len();
    let host = Rc::new(NeoForge::new(game.clone()));
    let mut rt = Runtime::new(compiled, host.clone())?;
    let mut cx = rt.context();
    let mut t = Translation {
        skipped,
        ..Translation::default()
    };

    // 1. The @Mod classes that run on a server, constructed.
    for class in classes {
        let name = class.name()?.to_owned();
        for a in class.annotations() {
            if a.type_descriptor != "Lnet/neoforged/fml/common/Mod;" {
                continue;
            }
            let client_only = a.values.iter().any(|(k, v)| {
                k == "dist"
                    && matches!(v, ElementValue::Array(vs) if vs.iter().all(|x| matches!(x, ElementValue::Enum { name, .. } if name == "CLIENT")))
            });
            if client_only {
                continue;
            }
            for (k, v) in &a.values {
                if k == "value"
                    && let ElementValue::Const(_, i) = v
                    && let Ok(id) = class.utf8(*i)
                {
                    t.mod_id = id.to_owned();
                }
            }
            let Some(init) = class.methods.iter().find(|m| m.name == "<init>") else {
                continue;
            };
            let object = cx.call(&format!("new {name}"), &[])?;
            let mut args = vec![object];
            for p in Method::parse(&init.descriptor)
                .map(|m| m.params)
                .unwrap_or_default()
            {
                args.push(opaque(p));
            }
            if let Err(e) = cx.call(&format!("{name}.<init>{}", init.descriptor), &args) {
                t.errors.push(format!("{name}: constructor: {e:#}"));
            }
        }
    }

    // 2. RegisterEvent: blocks, then items, then the rest.
    let registers = host.state.borrow().registers.clone();
    let order = |r: &str| match r {
        "block" => 0,
        "item" => 1,
        _ => 2,
    };
    let mut sorted: Vec<_> = registers
        .iter()
        .filter(|r| r.active.load(std::sync::atomic::Ordering::Relaxed))
        .collect();
    sorted.sort_by_key(|r| order(&r.registry));
    let mut blocks = Vec::new();
    let mut items = Vec::new();
    for r in sorted {
        let entries = r.entries.lock().unwrap().clone();
        for h in entries {
            match host.bind(&mut cx, &h) {
                Ok(v) => match r.registry.as_str() {
                    "block" => blocks.push((h.id.clone(), v)),
                    "item" => items.push((h.id.clone(), v)),
                    _ => {}
                },
                Err(e) => t.errors.push(format!("{}: {e:#}", h.id)),
            }
        }
    }

    // 3. Each block's declaration.
    let mut item_blocks: BTreeMap<String, String> = BTreeMap::new();
    for (id, v) in &items {
        let peer = peer_of(&mut cx, v);
        if let Some(Obj::Item(i)) = get(&peer)
            && let Some(b) = &i.block
        {
            let bp = peer_of(&mut cx, b);
            if let Some(Obj::Block(bd)) = get(&bp)
                && let Some(bid) = bd.id.lock().unwrap().clone()
            {
                item_blocks.insert(id.clone(), bid);
            }
        }
    }
    for (id, v) in &blocks {
        let peer = peer_of(&mut cx, v);
        let Some(Obj::Block(b)) = get(&peer) else {
            t.errors.push(format!("{id}: not a block"));
            continue;
        };
        let has_item = item_blocks.get(id) == Some(id);
        match block(&host, &mut cx, &game, b, v, has_item) {
            Ok(mut decl) => {
                // Its item's own properties (a pot that stacks to 1).
                if has_item
                    && let Some((_, iv)) = items.iter().find(|(i, _)| i == id)
                    && let Some(Obj::Item(i)) = get(&peer_of(&mut cx, iv))
                {
                    let comps = item(i)["components"].clone();
                    if comps.as_object().is_some_and(|c| !c.is_empty()) {
                        decl["item_components"] = comps;
                    }
                }
                t.blocks.push((id.clone(), decl))
            }
            Err(e) => t.errors.push(format!("{id}: {e:#}")),
        }
    }
    // 4. Items that are not their block's item.
    for (id, v) in &items {
        if item_blocks.get(id) == Some(id) {
            continue;
        }
        let peer = peer_of(&mut cx, v);
        let Some(Obj::Item(i)) = get(&peer) else {
            t.errors.push(format!("{id}: not an item"));
            continue;
        };
        let mut decl = item(i);
        if let Some(block) = item_blocks.get(id) {
            decl["places"] = json!(block);
        }
        t.items.push((id.clone(), decl));
    }
    t.unknown = host.state.borrow().unknown.clone();
    Ok(t)
}

fn peer_of(cx: &mut Context<'_>, v: &Value) -> Value {
    match v {
        Value::Mod(_) => match cx.peer(v) {
            Ok(Value::Null) | Err(_) => v.clone(),
            Ok(p) => p,
        },
        _ => v.clone(),
    }
}

/// `[x1, y1, z1, x2, y2, z2]` in sixteenths, within the content format's
/// range.
fn sixteenths(boxes: &[[f64; 6]]) -> Json {
    Json::Array(
        boxes
            .iter()
            .map(|b| {
                Json::Array(
                    b.iter()
                        .map(|v| json!((v * 16.0 * 1e6).round() / 1e6))
                        .collect(),
                )
            })
            .collect(),
    )
}

fn block(
    host: &NeoForge,
    cx: &mut Context<'_>,
    game: &GameData,
    b: &BlockData,
    this: &Value,
    has_item: bool,
) -> Result<Json> {
    let def = b.def().unwrap_or_else(|| {
        Arc::new(StateDef {
            props: Vec::new(),
            block: Mutex::new(None),
        })
    });
    // The template: a game block of the class it extends, so the game's
    // behaviour for that class runs.
    let rep = game.representative(&b.class);
    // A plain block: the block it copies if that is plain too, else stone
    // (a plain block with nothing of its own).
    let template = match (&b.props.copy_of, rep) {
        (Some(copy), _)
            if b.class == "Block"
                && game
                    .blocks
                    .get(copy)
                    .is_some_and(|c| c.classes == ["Block"]) =>
        {
            copy.clone()
        }
        _ if b.class == "Block" => "minecraft:stone".into(),
        (_, Some(r)) => r.id.clone(),
        _ => "minecraft:stone".into(),
    };
    let mut properties = Map::new();
    let p = &b.props;
    if let Some(h) = p.hardness {
        properties.insert("hardness".into(), json!(h));
    }
    if let Some(r) = p.resistance {
        properties.insert("explosion_resistance".into(), json!(r));
    }
    properties.insert("requires_tool".into(), json!(p.requires_tool));
    if let Some(f) = p.friction {
        properties.insert("friction".into(), json!(f));
    }
    if let Some(c) = p.map_color {
        properties.insert("map_color".into(), json!(c));
    }
    if let Some(s) = &p.sounds {
        properties.insert("sounds".into(), json!(s));
    }
    if p.light.is_none()
        && let Some(l) = p.light_level
    {
        properties.insert("light".into(), json!(l));
    }

    let mut props = Map::new();
    for prop in &def.props {
        props.insert(prop.name.clone(), json!(prop.values));
    }
    let default_values = def.values(b.default_index());
    let default: Map<String, Json> = def
        .props
        .iter()
        .zip(&default_values)
        .map(|(p, v)| (p.name.clone(), json!(p.values[*v])))
        .collect();
    let mut shapes = Vec::new();
    let mut lights = Vec::new();
    for i in 0..def.count() {
        let state = obj(Obj::State(def.clone(), i));
        let values = def.values(i);
        let when: Map<String, Json> = def
            .props
            .iter()
            .zip(&values)
            .map(|(p, v)| (p.name.clone(), json!(p.values[*v])))
            .collect();
        let args = [state.clone(), Value::Null, Value::Null, Value::Null];
        let outline = match cx.call_virtual(this, "getShape", SHAPE, &args) {
            Ok(Some(s)) => {
                if i == 0 && std::env::var_os("HORIZON_NEOFORGE_DEBUG").is_some() {
                    let what = match get(&s) {
                        Some(Obj::Opaque(l)) => format!("placeholder {l}"),
                        Some(Obj::Shape(b)) => format!("{} boxes", b.len()),
                        Some(_) => "another object".into(),
                        None => format!("{s:?}"),
                    };
                    eprintln!("getShape of {:?}: {what}", b.id.lock().unwrap());
                }
                shape_boxes(&s)
            }
            Ok(None) => None,
            Err(e) => {
                if i == 0 {
                    host.state
                        .borrow_mut()
                        .unknown
                        .entry(format!(
                            "getShape failed: {}",
                            format!("{e:#}").lines().last().unwrap_or("")
                        ))
                        .and_modify(|n| *n += 1)
                        .or_insert(1);
                }
                None
            }
        };
        let collision = match cx.call_virtual(this, "getCollisionShape", SHAPE, &args) {
            Ok(Some(s)) => shape_boxes(&s),
            _ => None,
        };
        // No class of the jar has getShape: the game class's own. Collision
        // is the block's own shape (BlockBehaviour.getCollisionShape),
        // unless its game class makes another.
        let outline = outline.or_else(|| Some(host.vanilla_shape(b, &state, false)));
        let collision = collision.or_else(|| {
            if !p.collides {
                Some(Vec::new())
            } else if game.own_collision(&b.class) || b.cross.lock().unwrap().is_some() {
                Some(host.vanilla_shape(b, &state, true))
            } else {
                outline.clone()
            }
        });
        if let Some(outline) = outline {
            let collision = if p.collides {
                collision.unwrap_or_else(|| outline.clone())
            } else {
                Vec::new()
            };
            shapes.push(json!({
                "when": when,
                "outline": sixteenths(&outline),
                "collision": sixteenths(&collision),
                "occludes": p.occludes,
            }));
        }
        if let Some(f) = &p.light {
            let level = host.invoke(cx, f, "applyAsInt", "(Ljava/lang/Object;)I", &[state])?;
            lights.push(json!({"when": when, "light": int(&level).unwrap_or(0).clamp(0, 15)}));
        }
    }
    // Shapes past the content format's range are left to the template.
    shapes.retain(|s| {
        ["outline", "collision"].iter().all(|k| {
            s[k].as_array().into_iter().flatten().all(|b| {
                b.as_array()
                    .into_iter()
                    .flatten()
                    .all(|v| v.as_f64().is_some_and(|v| (-16.0..=32.0).contains(&v)))
            })
        }) && s["outline"].as_array().is_some_and(|a| !a.is_empty())
    });
    let mut states = Map::new();
    states.insert("properties".into(), Json::Object(props));
    states.insert("default".into(), Json::Object(default));
    states.insert("shapes".into(), Json::Array(shapes));
    states.insert("light".into(), Json::Array(lights));
    Ok(json!({
        "template": template,
        "item": has_item,
        "properties": properties,
        "states": states,
    }))
}

fn shape_boxes(v: &Value) -> Option<Vec<[f64; 6]>> {
    match get(v) {
        Some(Obj::Shape(b)) => Some(b.clone()),
        _ => None,
    }
}

fn item(i: &ItemData) -> Json {
    let p = &i.props;
    let mut components = Map::new();
    if let Some(n) = p.stack {
        components.insert("minecraft:max_stack_size".into(), json!(n));
    }
    if let Some(d) = p.durability {
        components.insert("minecraft:max_damage".into(), json!(d));
        components.insert("minecraft:max_stack_size".into(), json!(1));
        components.insert("minecraft:damage".into(), json!(0));
    }
    if let Some(f) = &p.food {
        components.insert(
            "minecraft:food".into(),
            json!({
                "nutrition": f.nutrition,
                "saturation": f.nutrition as f32 * f.saturation_modifier * 2.0,
                "can_always_eat": f.always_edible,
            }),
        );
        components.insert(
            "minecraft:consumable".into(),
            json!({"consume_seconds": if f.fast { 0.8 } else { 1.6 }}),
        );
    }
    if p.fire_resistant {
        components.insert(
            "minecraft:damage_resistant".into(),
            json!({"types": "#minecraft:is_fire"}),
        );
    }
    let mut out = json!({
        "template": "minecraft:paper",
        "components": components,
    });
    if let Some(r) = &p.rarity {
        out["rarity"] = json!(r);
    }
    out
}

/// Writes the declarations into a content mod folder (a port's `carry/`):
/// `data/<ns>/horizon/{block,item}/<path>.json`, replacing those there.
pub fn write(t: &Translation, dir: &Path) -> std::io::Result<()> {
    let mut cleared = std::collections::BTreeSet::new();
    for (kind, list) in [("block", &t.blocks), ("item", &t.items)] {
        for (id, decl) in list {
            let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
            let folder = dir.join(format!("data/{ns}/horizon/{kind}"));
            if cleared.insert(folder.clone()) {
                let _ = std::fs::remove_dir_all(&folder);
                std::fs::create_dir_all(&folder)?;
            }
            let file = folder.join(format!("{path}.json"));
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(file, serde_json::to_vec_pretty(decl)?)?;
        }
    }
    Ok(())
}
