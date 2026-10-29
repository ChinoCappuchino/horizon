//! The host a compiled NeoForge mod runs against while it registers its
//! content: Minecraft's block and item classes as far as registration uses
//! them, NeoForge's registers, and the JDK's basics. A call it does not
//! model returns a placeholder (or 0) and is counted in the report.
//!
//! A mod's block or item extends a game class: the game class's
//! constructor (`extend`) makes the peer that holds its properties, and
//! the game calls the mod's overrides (`createBlockStateDefinition`,
//! `getShape`) through the jar's dispatchers, as Java does.
use crate::{game::GameData, objects::*};
use horizon_java::{
    compile::{HostConstant, Object},
    runtime::{Context, Host, Import, Value},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use wasmtime::{Result, bail};

const DIRECTIONS: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
const HORIZONTAL: [&str; 4] = ["north", "south", "west", "east"];
/// `MapColor` ids, in order.
const MAP_COLORS: [&str; 62] = [
    "NONE",
    "GRASS",
    "SAND",
    "WOOL",
    "FIRE",
    "ICE",
    "METAL",
    "PLANT",
    "SNOW",
    "CLAY",
    "DIRT",
    "STONE",
    "WATER",
    "WOOD",
    "QUARTZ",
    "COLOR_ORANGE",
    "COLOR_MAGENTA",
    "COLOR_LIGHT_BLUE",
    "COLOR_YELLOW",
    "COLOR_LIGHT_GREEN",
    "COLOR_PINK",
    "COLOR_GRAY",
    "COLOR_LIGHT_GRAY",
    "COLOR_CYAN",
    "COLOR_PURPLE",
    "COLOR_BLUE",
    "COLOR_BROWN",
    "COLOR_GREEN",
    "COLOR_RED",
    "COLOR_BLACK",
    "GOLD",
    "DIAMOND",
    "LAPIS",
    "EMERALD",
    "PODZOL",
    "NETHER",
    "TERRACOTTA_WHITE",
    "TERRACOTTA_ORANGE",
    "TERRACOTTA_MAGENTA",
    "TERRACOTTA_LIGHT_BLUE",
    "TERRACOTTA_YELLOW",
    "TERRACOTTA_LIGHT_GREEN",
    "TERRACOTTA_PINK",
    "TERRACOTTA_GRAY",
    "TERRACOTTA_LIGHT_GRAY",
    "TERRACOTTA_CYAN",
    "TERRACOTTA_PURPLE",
    "TERRACOTTA_BLUE",
    "TERRACOTTA_BROWN",
    "TERRACOTTA_GREEN",
    "TERRACOTTA_RED",
    "TERRACOTTA_BLACK",
    "CRIMSON_NYLIUM",
    "CRIMSON_STEM",
    "CRIMSON_HYPHAE",
    "WARPED_NYLIUM",
    "WARPED_STEM",
    "WARPED_HYPHAE",
    "WARPED_WART_BLOCK",
    "DEEPSLATE",
    "RAW_IRON",
    "GLOW_LICHEN",
];

fn simple(class: &str) -> &str {
    class.rsplit('/').next().unwrap_or(class)
}

/// What the host keeps between calls.
#[derive(Default)]
pub struct State {
    strings: HashMap<String, Value>,
    statics: HashMap<String, Value>,
    pub registers: Vec<Arc<Register>>,
    /// Mod objects whose game constructor is running, and their peers: the
    /// peer is in the object only once the constructor returns.
    constructing: Vec<(Value, Value)>,
    pub blocks: Vec<Arc<BlockData>>,
    pub items: Vec<Arc<ItemData>>,
    /// Calls not modelled, and how often.
    pub unknown: BTreeMap<String, usize>,
    pub mod_bus_listeners: Vec<Value>,
    /// Listeners given to a bus (`addListener`), by bus.
    pub listeners: Vec<(String, Value)>,
    /// Objects and classes given to a bus (`register`), by bus.
    pub registered: Vec<(String, Value)>,
    /// What mods add to template pools' raw lists (`rawTemplates`).
    pub raw_pools: BTreeMap<String, Value>,
}

pub struct NeoForge {
    pub game: Arc<GameData>,
    pub state: RefCell<State>,
    /// While a block's code runs in the game: the level it acts on.
    pub world: RefCell<Option<World>>,
}

/// The game a block's code runs in, for the length of one call.
pub struct World {
    /// Valid for the call it was set for (see `crate::world`).
    pub level: *mut (dyn horizon_world::level::code::CodeLevel + 'static),
    pub registry: &'static horizon_world::registry::BlockRegistry,
    /// The mods' blocks by id.
    pub blocks: Arc<BTreeMap<String, Value>>,
    /// The block a player uses.
    pub using: horizon_core::position::BlockPos,
}

impl NeoForge {
    pub fn new(game: Arc<GameData>) -> Self {
        NeoForge {
            game,
            state: RefCell::new(State::default()),
            world: RefCell::new(None),
        }
    }

    fn unknown(&self, import: &Import) -> Value {
        *self
            .state
            .borrow_mut()
            .unknown
            .entry(import.to_string())
            .or_default() += 1;
        let ret = if import.op == "new" || import.op == "extend" {
            "L".to_owned()
        } else if matches!(import.op.as_str(), "getstatic" | "getfield") {
            import.descriptor.clone()
        } else {
            match import.descriptor.rsplit_once(')') {
                Some((_, r)) => r.to_owned(),
                None => "V".to_owned(),
            }
        };
        match ret.as_bytes().first() {
            Some(b'V') | None => Value::Void,
            Some(b'L' | b'[') => opaque(format!("{}.{}", simple(&import.owner), import.name)),
            Some(b'J') => Value::Long(0),
            Some(b'F') => Value::Float(0.0),
            Some(b'D') => Value::Double(0.0),
            _ => Value::Int(0),
        }
    }

    /// The same object each time for a key (statics, enum constants).
    fn interned(&self, key: &str, make: impl FnOnce() -> Value) -> Value {
        if let Some(v) = self.state.borrow().statics.get(key) {
            return v.clone();
        }
        let v = make();
        self.state
            .borrow_mut()
            .statics
            .insert(key.to_owned(), v.clone());
        v
    }

    fn string(&self, s: &str) -> Value {
        if let Some(v) = self.state.borrow().strings.get(s) {
            return v.clone();
        }
        let v = obj(Obj::Str(s.to_owned()));
        self.state
            .borrow_mut()
            .strings
            .insert(s.to_owned(), v.clone());
        v
    }

    /// A game enum's constant, one object per name (`NORTH`).
    pub fn constant(&self, class: &str, name: &str) -> Value {
        self.interned(&format!("enum:{name}"), || {
            obj(Obj::Const(class.to_owned(), name.to_owned()))
        })
    }

    /// A game block by id, one object each.
    pub fn game_block(&self, id: &str) -> Value {
        let key = format!("block:{id}");
        self.interned(&key, || {
            let vanilla = self.game.blocks.get(id).cloned().unwrap_or_default();
            let props: Vec<Arc<Prop>> = vanilla
                .properties
                .iter()
                .map(|(n, v)| self.property(n, v.clone()))
                .collect();
            let def = Arc::new(StateDef {
                props,
                block: Mutex::new(None),
            });
            let default = default_index(&def, &vanilla.default);
            // The properties `ofFullCopy` takes from it.
            let props = BlockProps {
                copy_of: Some(id.to_owned()),
                hardness: vanilla.hardness,
                resistance: vanilla.resistance,
                sounds: Some(id.to_owned()),
                light: None,
                light_level: vanilla.light,
                occludes: vanilla.occludes,
                collides: vanilla.collides,
                requires_tool: vanilla.requires_tool,
                map_color: vanilla.map_color,
                friction: vanilla.friction,
            };
            let data = Arc::new(BlockData {
                this: None,
                id: Mutex::new(Some(id.to_owned())),
                class: vanilla
                    .classes
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "Block".into()),
                props,
                def: Mutex::new(Some(def.clone())),
                default: AtomicUsize::new(default),
                cross: Mutex::new(None),
            });
            let v = obj(Obj::Block(data));
            *def.block.lock().unwrap() = Some(v.clone());
            v
        })
    }

    /// The level a block's code acts on, while it runs.
    #[allow(clippy::mut_from_ref)]
    fn level(&self) -> Option<&mut dyn horizon_world::level::code::CodeLevel> {
        let w = self.world.borrow();
        // SAFETY: crate::world sets the pointer for one call of the game's
        // into a block's code and clears it after; the level outlives it,
        // and the code runs on that one thread.
        w.as_ref().map(|w| unsafe { &mut *w.level })
    }

    /// A game state as the mods see it.
    pub fn java_state(&self, id: horizon_world::registry::BlockStateId) -> Option<Value> {
        let (registry, blocks) = {
            let w = self.world.borrow();
            let w = w.as_ref()?;
            (w.registry, w.blocks.clone())
        };
        let state = registry.state(id)?;
        let def = match blocks.get(&state.name) {
            // A mod's block: its peer holds the definition.
            Some(_) => {
                let peer = self
                    .state
                    .borrow()
                    .blocks
                    .iter()
                    .find(|b| b.id.lock().unwrap().as_deref() == Some(state.name.as_str()))
                    .cloned()?;
                peer.def()?
            }
            None => match get(&self.game_block(&state.name)) {
                Some(Obj::Block(b)) => b.def()?,
                _ => return None,
            },
        };
        let values: Vec<usize> = def
            .props
            .iter()
            .map(|p| {
                state
                    .properties
                    .get(&p.name)
                    .and_then(|v| p.values.iter().position(|x| x == v))
                    .unwrap_or(0)
            })
            .collect();
        let index = def.index(&values);
        Some(obj(Obj::State(def, index)))
    }

    /// A mod's state as the game has it.
    pub fn game_state(
        &self,
        cx: &mut Context<'_>,
        v: &Value,
    ) -> Option<horizon_world::registry::BlockStateId> {
        let Some(Obj::State(d, i)) = get(v) else {
            return None;
        };
        let block = d.block.lock().unwrap().clone()?;
        let name = self.item_id(cx, &block).ok().filter(|n| !n.is_empty())?;
        let properties = d
            .props
            .iter()
            .zip(d.values(*i))
            .map(|(p, v)| (p.name.clone(), p.values[v].clone()))
            .collect();
        let registry = self.world.borrow().as_ref()?.registry;
        registry.id(&horizon_world::chunk::BlockState { name, properties })
    }

    fn pos(v: &Value) -> Option<horizon_core::position::BlockPos> {
        match get(v) {
            Some(Obj::Pos(x, y, z)) => Some(horizon_core::position::BlockPos::new(*x, *y, *z)),
            _ => None,
        }
    }

    /// A game profession (`VillagerProfession.FARMER`), one object each.
    pub fn game_profession(&self, id: &str) -> Value {
        self.interned(&format!("profession:{id}"), || {
            obj(Obj::Profession(Arc::new(Profession {
                id: Mutex::new(Some(id.to_owned())),
                name: id.trim_start_matches("minecraft:").to_owned(),
                held: Value::Null,
                requested: Value::Null,
                sound: Value::Null,
            })))
        })
    }

    /// The template pools mods looked up, with what they added.
    pub fn pools(&self) -> Vec<(String, Value)> {
        let st = self.state.borrow();
        let mut out: Vec<(String, Value)> = st
            .statics
            .iter()
            .filter_map(|(k, v)| {
                let id = k.strip_prefix("pool:")?;
                match get(v) {
                    Some(Obj::Pool(_, templates)) => Some((id.to_owned(), templates.clone())),
                    _ => None,
                }
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// A template pool, one object for an id.
    pub fn pool(&self, id: &str) -> Value {
        self.interned(&format!("pool:{id}"), || {
            obj(Obj::Pool(
                id.to_owned(),
                obj(Obj::List(Mutex::new(Vec::new()))),
            ))
        })
    }

    /// The id of an item (or a block, or a holder of one).
    pub fn item_id(&self, cx: &mut Context<'_>, v: &Value) -> Result<String> {
        let v = match get(v) {
            Some(Obj::Holder(h)) => {
                let h = h.clone();
                self.bind(cx, &h)?
            }
            _ => v.clone(),
        };
        let peer = self.peer(cx, &v)?;
        Ok(match get(&peer) {
            Some(Obj::Item(i)) => i.id.lock().unwrap().clone().unwrap_or_default(),
            Some(Obj::Block(b)) => b.id.lock().unwrap().clone().unwrap_or_default(),
            Some(Obj::Stack(id, _)) => id.clone(),
            Some(Obj::Loc(id)) | Some(Obj::Str(id)) => id.clone(),
            _ => String::new(),
        })
    }

    /// A call to a class of another jar running with this one: its export
    /// there (statics through the class's superclasses).
    fn foreign(
        &self,
        cx: &mut Context<'_>,
        import: &Import,
        args: &[Value],
    ) -> Result<Option<Value>> {
        let op = import.op.as_str();
        if !matches!(
            op,
            "getstatic"
                | "putstatic"
                | "invokestatic"
                | "new"
                | "extend"
                | "instanceof"
                | "getfield"
        ) {
            return Ok(None);
        }
        let owner = import.owner.as_str();
        let Some(jar) = cx.jar_of_class(owner) else {
            return Ok(None);
        };
        if jar == cx.jar() {
            return Ok(None);
        }
        let compiled = cx.compiled_of(jar).expect("a jar");
        let supers = |mut c: String| {
            let mut out = Vec::new();
            loop {
                out.push(c.clone());
                match compiled
                    .info
                    .iter()
                    .find(|i| i.name == c)
                    .and_then(|i| i.superclass.clone())
                {
                    Some(s) if compiled.classes.contains(&s) => c = s,
                    _ => break,
                }
            }
            out
        };
        let name = &import.name;
        let desc = &import.descriptor;
        let v = match op {
            "getstatic" | "putstatic" | "invokestatic" => {
                let mut found = None;
                for c in supers(owner.to_owned()) {
                    let export = match op {
                        "getstatic" => format!("get {c}.{name}"),
                        "putstatic" => format!("set {c}.{name}"),
                        _ => format!("{c}.{name}{desc}"),
                    };
                    if cx.in_jar(jar, |cx| cx.has(&export)) {
                        found = Some(export);
                        break;
                    }
                }
                let Some(export) = found else {
                    return Ok(None);
                };
                cx.in_jar(jar, |cx| cx.call(&export, args))?
            }
            "new" | "extend" => {
                let args = if op == "extend" { &args[1..] } else { args };
                cx.in_jar(jar, |cx| -> Result<Value> {
                    let object = cx.call(&format!("new {owner}"), &[])?;
                    let mut all = vec![object.clone()];
                    all.extend_from_slice(args);
                    cx.call(&format!("{owner}.<init>{desc}"), &all)?;
                    Ok(object)
                })?
            }
            "instanceof" => {
                let yes = match &args[0] {
                    Value::Mod(m) if m.jar == jar => {
                        let class = cx.class(&args[0])?;
                        let mut todo: Vec<String> = class.into_iter().collect();
                        let mut found = false;
                        while let Some(c) = todo.pop() {
                            if c == owner {
                                found = true;
                                break;
                            }
                            if let Some(i) = compiled.info.iter().find(|i| i.name == c) {
                                todo.extend(i.superclass.clone());
                                todo.extend(i.interfaces.clone());
                            }
                        }
                        found
                    }
                    _ => false,
                };
                Value::Int(yes as i32)
            }
            _ => {
                // getfield: the field of an object of that jar.
                let class = cx.class(&args[0])?.unwrap_or_default();
                let Some(index) = compiled
                    .info
                    .iter()
                    .find(|i| i.name == class)
                    .and_then(|i| i.fields.iter().rposition(|f| f == name))
                else {
                    return Ok(None);
                };
                cx.field(&args[0], index + 2)?
            }
        };
        Ok(Some(v))
    }

    /// An item by id: a mod's registered one (the object the mod has), else
    /// the game's.
    pub fn item_by_id(&self, id: &str) -> Value {
        let found = self
            .state
            .borrow()
            .items
            .iter()
            .find(|i| i.id.lock().unwrap().as_deref() == Some(id))
            .cloned();
        match found {
            Some(i) => i.this.clone().unwrap_or_else(|| obj(Obj::Item(i))),
            None => self.game_item(id),
        }
    }

    /// An item's food (`Item.Properties.food`).
    fn food_of(&self, cx: &mut Context<'_>, item: &Value) -> Result<Option<Food>> {
        let peer = self.peer(cx, item)?;
        Ok(match get(&peer) {
            Some(Obj::Item(i)) => i.props.food.clone(),
            _ => None,
        })
    }

    fn game_item(&self, id: &str) -> Value {
        self.interned(&format!("item:{id}"), || {
            obj(Obj::Item(Arc::new(ItemData {
                this: None,
                id: Mutex::new(Some(id.to_owned())),
                class: "Item".into(),
                props: ItemProps::default(),
                block: None,
            })))
        })
    }

    /// A property of the game's, one object for a name and values.
    fn property(&self, name: &str, values: Vec<String>) -> Arc<Prop> {
        let key = format!("prop:{name}:{}", values.join(","));
        let v = self.interned(&key, || {
            let kind = if values.iter().all(|v| v == "true" || v == "false") {
                PropKind::Bool
            } else if values.iter().all(|v| v.parse::<i32>().is_ok()) {
                PropKind::Int
            } else {
                PropKind::Enum
            };
            let objects = values
                .iter()
                .map(|v| match kind {
                    PropKind::Bool => obj(Obj::Boxed(Prim::Bool(v == "true"))),
                    PropKind::Int => obj(Obj::Boxed(Prim::Int(v.parse().unwrap_or(0)))),
                    PropKind::Enum => self.constant("", &v.to_uppercase()),
                })
                .collect();
            obj(Obj::Property(Arc::new(Prop {
                name: name.to_owned(),
                kind,
                values,
                objects,
            })))
        });
        match get(&v) {
            Some(Obj::Property(p)) => p.clone(),
            _ => unreachable!(),
        }
    }

    fn property_value(&self, p: &Arc<Prop>) -> Value {
        self.interned(&format!("propv:{}:{}", p.name, p.values.join(",")), || {
            obj(Obj::Property(p.clone()))
        })
    }

    /// A static field of the game's: blocks, items, properties, enum
    /// constants, registries.
    fn get_static(&self, cx: &mut Context<'_>, import: &Import) -> Result<Value> {
        let (owner, name) = (import.owner.as_str(), import.name.as_str());
        let id = || format!("minecraft:{}", name.to_lowercase());
        Ok(match owner {
            "net/minecraft/world/level/block/Blocks" => self.game_block(&id()),
            "net/minecraft/world/item/Items" => self.game_item(&id()),
            "net/minecraft/core/registries/BuiltInRegistries"
            | "net/minecraft/core/registries/Registries" => self
                .interned(&format!("registry:{name}"), || {
                    obj(Obj::Registry(name.to_lowercase()))
                }),
            "net/minecraft/world/level/material/MapColor" => self.constant(owner, name),
            "net/neoforged/neoforge/common/NeoForge" if name == "EVENT_BUS" => {
                self.interned("bus:game", || obj(Obj::Bus("game".into())))
            }
            "net/neoforged/fml/loading/FMLEnvironment" if name == "dist" => {
                self.constant("net/neoforged/api/distmarker/Dist", "DEDICATED_SERVER")
            }
            "net/minecraft/world/entity/npc/VillagerProfession" => self.game_profession(&id()),
            "net/minecraft/sounds/SoundEvents" => {
                let sound = self.game.sound_for_field(name);
                self.interned(&format!("sound:{sound}"), || obj(Obj::Sound(sound.clone())))
            }
            _ if import.descriptor.ends_with("Property;") => {
                let p = self.static_property(owner, name);
                match p {
                    Some(p) => self.property_value(&p),
                    None => self.unknown(import),
                }
            }
            _ if import.descriptor == format!("L{owner};") => self.constant(owner, name),
            _ => {
                let _ = cx;
                self.unknown(import)
            }
        })
    }

    /// `BlockStateProperties.X` or a block class's `X` (`CropBlock.AGE`).
    fn static_property(&self, owner: &str, field: &str) -> Option<Arc<Prop>> {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        let ints = |a: i32, b: i32| (a..=b).map(|v| v.to_string()).collect::<Vec<_>>();
        let bools = || s(&["true", "false"]);
        if simple(owner) == "BlockStateProperties" {
            let (name, values) = match field {
                "HORIZONTAL_FACING" => ("facing", s(&HORIZONTAL)),
                "FACING" => ("facing", s(&DIRECTIONS)),
                "FACING_HOPPER" => ("facing", s(&["down", "north", "south", "west", "east"])),
                "VERTICAL_DIRECTION" => ("vertical_direction", s(&["up", "down"])),
                "AXIS" => ("axis", s(&["x", "y", "z"])),
                "HORIZONTAL_AXIS" => ("axis", s(&["x", "z"])),
                "BED_PART" => ("part", s(&["head", "foot"])),
                "DOUBLE_BLOCK_HALF" => ("half", s(&["upper", "lower"])),
                "HALF" => ("half", s(&["top", "bottom"])),
                "BITES" => ("bites", ints(0, 6)),
                "MOISTURE" => ("moisture", ints(0, 7)),
                "LEVEL" => ("level", ints(0, 15)),
                "ROTATION_16" => ("rotation", ints(0, 15)),
                "POWER" => ("power", ints(0, 15)),
                "DISTANCE" => ("distance", ints(1, 7)),
                f if f.starts_with("AGE_") => ("age", ints(0, f[4..].parse().ok()?)),
                f if f.starts_with("LEVEL_") => ("level", ints(0, f[6..].parse().ok()?)),
                _ => {
                    let name = field.to_lowercase();
                    let values = self
                        .game
                        .blocks
                        .values()
                        .find_map(|b| b.properties.iter().find(|(n, _)| *n == name))
                        .map(|(_, v)| v.clone())?;
                    let values = if values.iter().all(|v| v == "true" || v == "false") {
                        bools()
                    } else {
                        values
                    };
                    return Some(self.property(&name, values));
                }
            };
            return Some(self.property(name, values));
        }
        let class = simple(owner);
        let name = field.to_lowercase();
        let values = self.game.class_property(class, &name)?;
        let values = if values.iter().all(|v| v == "true" || v == "false") {
            bools()
        } else {
            values
        };
        Some(self.property(&name, values))
    }

    /// The peer of a mod object (a game object passes through).
    fn peer(&self, cx: &mut Context<'_>, v: &Value) -> Result<Value> {
        if !matches!(v, Value::Mod(_)) {
            return Ok(v.clone());
        }
        let constructing = self.state.borrow().constructing.clone();
        for (o, p) in constructing.iter().rev() {
            if cx.same(o, v)? {
                return Ok(p.clone());
            }
        }
        match cx.peer(v) {
            Ok(Value::Null) | Err(_) => Ok(v.clone()),
            Ok(p) => Ok(p),
        }
    }

    /// Java's `equals` for the values registration compares.
    pub fn equal(&self, cx: &mut Context<'_>, a: &Value, b: &Value) -> Result<bool> {
        if cx.same(a, b)? {
            return Ok(true);
        }
        if let (Some(x), Some(y)) = (int(a), int(b))
            && (matches!(a, Value::Int(_) | Value::Long(_))
                || matches!(b, Value::Int(_) | Value::Long(_)))
        {
            return Ok(x == y);
        }
        Ok(match (get(a), get(b)) {
            (Some(Obj::Str(x)), Some(Obj::Str(y))) | (Some(Obj::Loc(x)), Some(Obj::Loc(y))) => {
                x == y
            }
            (Some(Obj::Boxed(x)), Some(Obj::Boxed(y))) => x == y,
            (Some(Obj::Const(_, x)), Some(Obj::Const(_, y))) => x == y,
            (Some(Obj::Key(r, x)), Some(Obj::Key(s, y))) => r == s && x == y,
            (Some(Obj::State(d, i)), Some(Obj::State(e, j))) => Arc::ptr_eq(d, e) && i == j,
            _ => false,
        })
    }

    /// The string a property value object stands for.
    fn value_name(&self, cx: &mut Context<'_>, p: &Prop, v: &Value) -> Result<Option<usize>> {
        for (i, o) in p.objects.iter().enumerate() {
            if self.equal(cx, o, v)? {
                return Ok(Some(i));
            }
        }
        let name = match get(v) {
            Some(Obj::Boxed(Prim::Bool(b))) => b.to_string(),
            Some(Obj::Boxed(Prim::Int(i))) => i.to_string(),
            Some(Obj::Const(_, n)) => n.to_lowercase(),
            _ => match v {
                Value::Int(i) => i.to_string(),
                _ => return Ok(None),
            },
        };
        Ok(p.values.iter().position(|x| *x == name))
    }

    /// Calls a lambda or a mod object's interface method.
    pub fn invoke(
        &self,
        cx: &mut Context<'_>,
        f: &Value,
        name: &str,
        desc: &str,
        args: &[Value],
    ) -> Result<Value> {
        if let Some(v) = cx.call_virtual(f, name, desc, args)? {
            return Ok(v);
        }
        cx.call_lambda(f, args)
    }

    /// A holder's value, made now if it is not yet.
    pub fn bind(&self, cx: &mut Context<'_>, h: &Arc<Holder>) -> Result<Value> {
        if let Some(v) = h.value.lock().unwrap().clone() {
            return Ok(v);
        }
        if h.binding.swap(true, Ordering::SeqCst) {
            bail!("{} needs itself to be made", h.id);
        }
        let supplier = h.supplier.lock().unwrap().clone();
        let made = match supplier {
            Some((f, None)) => self.invoke(cx, &f, "get", "()Ljava/lang/Object;", &[]),
            Some((f, Some(arg))) => self.invoke(
                cx,
                &f,
                "apply",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
                &[arg],
            ),
            None => bail!("{} has nothing to make it", h.id),
        };
        h.binding.store(false, Ordering::SeqCst);
        let v = made?;
        // The registry names what it holds.
        let peer = self.peer(cx, &v)?;
        match get(&peer) {
            Some(Obj::Block(b)) => {
                b.id.lock().unwrap().get_or_insert_with(|| h.id.clone());
            }
            Some(Obj::Item(i)) => {
                i.id.lock().unwrap().get_or_insert_with(|| h.id.clone());
            }
            _ => {}
        }
        *h.value.lock().unwrap() = Some(v.clone());
        Ok(v)
    }

    fn register(&self, registry: &str, namespace: &str) -> Value {
        let r = Arc::new(Register {
            registry: registry.to_owned(),
            namespace: namespace.to_owned(),
            entries: Mutex::new(Vec::new()),
            active: AtomicBool::new(false),
        });
        self.state.borrow_mut().registers.push(r.clone());
        obj(Obj::Register(r))
    }

    fn holder(&self, r: &Register, name: &str, supplier: Value, arg: Option<Value>) -> Value {
        let h = Arc::new(Holder {
            registry: r.registry.clone(),
            id: format!("{}:{name}", r.namespace),
            supplier: Mutex::new(Some((supplier, arg))),
            value: Mutex::new(None),
            binding: AtomicBool::new(false),
        });
        r.entries.lock().unwrap().push(h.clone());
        obj(Obj::Holder(h))
    }

    /// A game class's constructor for a mod object: its peer.
    fn extend(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value> {
        let this = args[0].clone();
        let owner = import.owner.as_str();
        let class = simple(owner).to_owned();
        // A block class of the game: known by its data, or by its package
        // and name.
        let block_class = self.game.is_block_class(&class)
            || class == "Block"
            || (owner.starts_with("net/minecraft/world/level/block/")
                && class.ends_with("Block")
                && !class.contains('$'));
        let peer = if block_class {
            let props = args[1..]
                .iter()
                .find_map(|a| match get(a) {
                    Some(Obj::BlockProps(p)) => Some(p.lock().unwrap().clone()),
                    _ => None,
                })
                .unwrap_or_else(BlockProps::new);
            let data = Arc::new(BlockData {
                this: Some(this.clone()),
                id: Mutex::new(None),
                class: class.clone(),
                props,
                def: Mutex::new(None),
                default: AtomicUsize::new(0),
                cross: Mutex::new(None),
            });
            // CrossCollisionBlock(nodeWidth, extensionWidth, nodeHeight,
            // extensionHeight, collisionHeight, properties).
            if class == "CrossCollisionBlock" {
                let n: Vec<f64> = args[1..].iter().filter_map(float).collect();
                if let [a, b, c, d, e, ..] = n[..] {
                    *data.cross.lock().unwrap() = Some([a, b, c, d, e]);
                }
            }
            let peer = obj(Obj::Block(data.clone()));
            self.state
                .borrow_mut()
                .constructing
                .push((this.clone(), peer.clone()));
            // Block's constructor: the state definition, from the mod's
            // override if it has one, then the class's default state.
            let builder = obj(Obj::Builder(Mutex::new(Vec::new())));
            let made = cx.call_virtual(
                &this,
                "createBlockStateDefinition",
                "(Lnet/minecraft/world/level/block/state/StateDefinition$Builder;)V",
                std::slice::from_ref(&builder),
            );
            if !matches!(made, Ok(Some(_))) {
                self.vanilla_definition(&class, &builder);
            }
            let props = match get(&builder) {
                Some(Obj::Builder(p)) => p.lock().unwrap().clone(),
                _ => Vec::new(),
            };
            let def = Arc::new(StateDef {
                props,
                block: Mutex::new(Some(this.clone())),
            });
            let rep = self
                .game
                .representative(&class)
                .cloned()
                .unwrap_or_default();
            data.default
                .store(default_index(&def, &rep.default), Ordering::Relaxed);
            *data.def.lock().unwrap() = Some(def);
            let mut st = self.state.borrow_mut();
            st.blocks.push(data);
            // The constructor returns: the object holds its peer from now.
            st.constructing.pop();
            drop(st);
            made?;
            peer
        } else if owner.starts_with("net/minecraft/world/item/") {
            let mut props = args[1..]
                .iter()
                .find_map(|a| match get(a) {
                    Some(Obj::ItemProps(p)) => Some(p.lock().unwrap().clone()),
                    _ => None,
                })
                .unwrap_or_default();
            // TieredItem's constructor: the tier's uses are its durability.
            if matches!(
                class.as_str(),
                "DiggerItem"
                    | "SwordItem"
                    | "TieredItem"
                    | "AxeItem"
                    | "PickaxeItem"
                    | "ShovelItem"
                    | "HoeItem"
            ) && props.durability.is_none()
                && let Some(tier) = args.get(1)
            {
                let uses = match get(tier) {
                    Some(Obj::Const(_, n)) => match n.as_str() {
                        "WOOD" => Some(59),
                        "STONE" => Some(131),
                        "IRON" => Some(250),
                        "DIAMOND" => Some(1561),
                        "GOLD" => Some(32),
                        "NETHERITE" => Some(2031),
                        _ => None,
                    },
                    _ => cx
                        .call_virtual(tier, "getUses", "()I", &[])?
                        .and_then(|v| int(&v))
                        .map(|v| v as u32),
                };
                props.durability = uses;
            }
            let block = args[1..]
                .iter()
                .find(|a| matches!(a, Value::Mod(_)) || matches!(get(a), Some(Obj::Block(_))))
                .cloned();
            let data = Arc::new(ItemData {
                this: Some(this.clone()),
                id: Mutex::new(None),
                class,
                props,
                block: if owner.ends_with("BlockItem") {
                    block
                } else {
                    None
                },
            });
            self.state.borrow_mut().items.push(data.clone());
            obj(Obj::Item(data))
        } else if owner == "net/neoforged/neoforge/registries/DeferredRegister" {
            let registry = match args.get(1).and_then(get) {
                Some(Obj::Key(_, id)) => id.trim_start_matches("minecraft:").to_owned(),
                Some(Obj::Registry(r)) => r.clone(),
                _ => "unknown".into(),
            };
            let ns = args.get(2).and_then(string).unwrap_or_default();
            self.register(&registry, &ns)
        } else if owner == "java/lang/Enum" {
            let name = args.get(1).and_then(string).unwrap_or_default();
            let ordinal = args.get(2).and_then(int).unwrap_or(0) as i32;
            obj(Obj::EnumPeer(name, ordinal))
        } else {
            *self
                .state
                .borrow_mut()
                .unknown
                .entry(import.to_string())
                .or_default() += 1;
            opaque(format!("{class} (extended)"))
        };
        Ok(peer)
    }

    /// The properties a game block class's `createBlockStateDefinition`
    /// adds: those of a block of that class.
    fn vanilla_definition(&self, class: &str, builder: &Value) {
        let Some(rep) = self.game.representative(class).cloned() else {
            return;
        };
        if let Some(Obj::Builder(list)) = get(builder) {
            for (name, values) in &rep.properties {
                let p = self.property(name, values.clone());
                let mut l = list.lock().unwrap();
                if !l.iter().any(|x| x.name == p.name) {
                    l.push(p);
                }
            }
        }
    }
}

/// The index of the state with these values (the rest first).
fn default_index(def: &StateDef, values: &BTreeMap<String, String>) -> usize {
    let idx: Vec<usize> = def
        .props
        .iter()
        .map(|p| {
            values
                .get(&p.name)
                .and_then(|v| p.values.iter().position(|x| x == v))
                .unwrap_or(0)
        })
        .collect();
    def.index(&idx)
}

fn shape(boxes: Vec<[f64; 6]>) -> Value {
    obj(Obj::Shape(boxes))
}

fn boxes(v: &Value) -> Vec<[f64; 6]> {
    match get(v) {
        Some(Obj::Shape(b)) => b.clone(),
        _ => Vec::new(),
    }
}

/// `Shapes.join`: the cells of the two shapes' grid that `op` keeps, as
/// boxes (each the largest run from its first cell).
pub fn join(a: &[[f64; 6]], b: &[[f64; 6]], op: &str) -> Vec<[f64; 6]> {
    let mut coords: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    for q in a.iter().chain(b) {
        for axis in 0..3 {
            coords[axis].push(q[axis]);
            coords[axis].push(q[axis + 3]);
        }
    }
    for c in &mut coords {
        c.sort_by(f64::total_cmp);
        c.dedup();
    }
    if coords.iter().any(|c| c.len() < 2) {
        return Vec::new();
    }
    let n: [usize; 3] = std::array::from_fn(|i| coords[i].len() - 1);
    let inside = |boxes: &[[f64; 6]], c: [f64; 3]| {
        boxes
            .iter()
            .any(|q| (0..3).all(|k| q[k] < c[k] && c[k] < q[k + 3]))
    };
    let mut cells = vec![false; n[0] * n[1] * n[2]];
    let at = |x: usize, y: usize, z: usize| (x * n[1] + y) * n[2] + z;
    for x in 0..n[0] {
        for y in 0..n[1] {
            for z in 0..n[2] {
                let c = [
                    (coords[0][x] + coords[0][x + 1]) / 2.0,
                    (coords[1][y] + coords[1][y + 1]) / 2.0,
                    (coords[2][z] + coords[2][z + 1]) / 2.0,
                ];
                let (p, q) = (inside(a, c), inside(b, c));
                cells[at(x, y, z)] = match op {
                    "OR" => p || q,
                    "AND" => p && q,
                    "ONLY_FIRST" => p && !q,
                    "ONLY_SECOND" => !p && q,
                    "NOT_SAME" => p != q,
                    "FIRST" => p,
                    "SECOND" => q,
                    "SAME" => p == q,
                    _ => false,
                };
            }
        }
    }
    let mut out = Vec::new();
    for x in 0..n[0] {
        for y in 0..n[1] {
            for z in 0..n[2] {
                if !cells[at(x, y, z)] {
                    continue;
                }
                // Grow along z, then y, then x while every cell is full.
                let mut z1 = z;
                while z1 + 1 < n[2] && cells[at(x, y, z1 + 1)] {
                    z1 += 1;
                }
                let mut y1 = y;
                while y1 + 1 < n[1] && (z..=z1).all(|k| cells[at(x, y1 + 1, k)]) {
                    y1 += 1;
                }
                let mut x1 = x;
                while x1 + 1 < n[0] && (y..=y1).all(|j| (z..=z1).all(|k| cells[at(x1 + 1, j, k)])) {
                    x1 += 1;
                }
                for i in x..=x1 {
                    for j in y..=y1 {
                        for k in z..=z1 {
                            cells[at(i, j, k)] = false;
                        }
                    }
                }
                out.push([
                    coords[0][x],
                    coords[1][y],
                    coords[2][z],
                    coords[0][x1 + 1],
                    coords[1][y1 + 1],
                    coords[2][z1 + 1],
                ]);
            }
        }
    }
    out
}

fn full() -> Vec<[f64; 6]> {
    vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]
}

impl Host for NeoForge {
    fn call(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value> {
        let op = import.op.as_str();
        if let Some(v) = self.foreign(cx, import, args)? {
            return Ok(v);
        }
        match op {
            "constant" => {
                let i = int(&args[0]).unwrap_or(-1) as i32;
                return Ok(match cx.constant(i) {
                    Some(HostConstant::String(s)) => self.string(&s),
                    Some(HostConstant::Class(c)) => {
                        self.interned(&format!("class:{c}"), || obj(Obj::Class(c.clone())))
                    }
                    None => Value::Null,
                });
            }
            "same" => return Ok(Value::Int(cx.same(&args[0], &args[1])? as i32)),
            "instanceof" => return self.instance_of(cx, &args[0], &import.owner),
            "concat" => {
                let mut out = String::new();
                let mut it = args.iter();
                for c in import.name.chars() {
                    if c == '\u{1}' {
                        let v = it.next().cloned().unwrap_or(Value::Null);
                        out.push_str(&self.to_string(cx, &v)?);
                    } else {
                        out.push(c);
                    }
                }
                return Ok(obj(Obj::Str(out)));
            }
            "record" => return Ok(Value::Int(0)),
            "getstatic" => return self.get_static(cx, import),
            "putstatic" => return Ok(Value::Void),
            "putfield" if import.name == "rawTemplates" => {
                let target = self.peer(cx, &args[0])?;
                if let Some(Obj::Pool(id, _)) = get(&target) {
                    self.state
                        .borrow_mut()
                        .raw_pools
                        .insert(id.clone(), args[1].clone());
                }
                return Ok(Value::Void);
            }
            "extend" => return self.extend(cx, import, args),
            "new" => return self.construct(cx, import, args),
            "invokestatic" => return self.static_call(cx, import, args),
            _ => {}
        }
        // A method on an object: a mod object's own first.
        let this = args.first().cloned().unwrap_or(Value::Null);
        if matches!(op, "invokevirtual" | "invokeinterface") && matches!(this, Value::Mod(_)) {
            // Of the jar the object is in, not the calling jar.
            let jar = match &this {
                Value::Mod(m) => m.jar,
                _ => cx.jar(),
            };
            let is_lambda = matches!(
                cx.id(&this)?.and_then(|id| cx
                    .compiled_of(jar)
                    .and_then(|c| c.object(id).map(|o| matches!(o, Object::Lambda(..))))),
                Some(true)
            );
            if let Some(v) = cx.call_virtual(&this, &import.name, &import.descriptor, &args[1..])? {
                return Ok(v);
            }
            if is_lambda {
                return cx.call_lambda(&this, &args[1..]);
            }
        }
        let target = self.peer(cx, &this)?;
        self.method(cx, import, &this, &target, &args[1..])
    }
}

impl NeoForge {
    fn to_string(&self, cx: &mut Context<'_>, v: &Value) -> Result<String> {
        Ok(match v {
            Value::Int(i) => i.to_string(),
            Value::Long(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Double(f) => f.to_string(),
            Value::Null => "null".into(),
            Value::Void => String::new(),
            Value::Mod(_) => match cx.call_virtual(v, "toString", "()Ljava/lang/String;", &[])? {
                Some(s) => string(&s).unwrap_or_default(),
                None => cx.class(v)?.unwrap_or_else(|| "object".into()),
            },
            Value::Host(_) => match get(v) {
                Some(Obj::Str(s) | Obj::Loc(s)) => s.clone(),
                Some(Obj::Boxed(Prim::Int(i))) => i.to_string(),
                Some(Obj::Boxed(Prim::Bool(b))) => b.to_string(),
                Some(Obj::Const(_, n)) => n.to_lowercase(),
                Some(Obj::Key(r, id)) => format!("ResourceKey[minecraft:{r} / {id}]"),
                _ => "?".into(),
            },
        })
    }

    fn instance_of(&self, cx: &mut Context<'_>, v: &Value, class: &str) -> Result<Value> {
        let peer = self.peer(cx, v)?;
        let yes = match get(&peer) {
            Some(Obj::Block(b)) => {
                class.ends_with("/Block")
                    || simple(class) == b.class
                    || self
                        .game
                        .representative(&b.class)
                        .is_some_and(|r| r.classes.iter().any(|c| c == simple(class)))
            }
            Some(Obj::Item(i)) => {
                class.ends_with("/Item")
                    || simple(class) == i.class
                    || (class.ends_with("/BlockItem") && i.block.is_some())
            }
            Some(Obj::Str(_)) => class == "java/lang/String",
            Some(Obj::List(_)) => class.ends_with("List") || class.ends_with("Collection"),
            _ => false,
        };
        Ok(Value::Int(yes as i32))
    }

    /// `new` of a game or JDK class.
    fn construct(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value> {
        let _ = cx;
        Ok(match import.owner.as_str() {
            "java/util/ArrayList"
            | "java/util/LinkedList"
            | "java/util/HashSet"
            | "java/util/LinkedHashSet"
            | "java/util/TreeSet"
            | "java/util/ArrayDeque" => {
                let init = match args.first().and_then(get) {
                    Some(Obj::List(l)) => l.lock().unwrap().clone(),
                    _ => Vec::new(),
                };
                obj(Obj::List(Mutex::new(init)))
            }
            "java/util/HashMap"
            | "java/util/LinkedHashMap"
            | "java/util/TreeMap"
            | "java/util/EnumMap"
            | "java/util/IdentityHashMap" => obj(Obj::Map(Mutex::new(Vec::new()))),
            "java/lang/StringBuilder" => obj(Obj::List(Mutex::new(Vec::new()))),
            "net/neoforged/neoforge/common/ModConfigSpec$Builder" => obj(Obj::ConfigBuilder),
            "net/minecraft/world/entity/npc/VillagerProfession" => {
                let a = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
                obj(Obj::Profession(Arc::new(Profession {
                    id: Mutex::new(None),
                    name: string(&a(0)).unwrap_or_default(),
                    held: a(1),
                    requested: a(3),
                    sound: a(5),
                })))
            }
            "net/minecraft/world/entity/ai/village/poi/PoiType" => obj(Obj::Poi {
                states: args.first().cloned().unwrap_or(Value::Null),
                tickets: args.get(1).and_then(int).unwrap_or(1) as i32,
                range: args.get(2).and_then(int).unwrap_or(1) as i32,
            }),
            "net/minecraft/world/item/trading/ItemCost" | "net/minecraft/world/item/ItemStack" => {
                let id = self.item_id(cx, args.first().unwrap_or(&Value::Null))?;
                let count = args.get(1).and_then(int).unwrap_or(1) as i32;
                obj(Obj::Stack(id, count))
            }
            "net/minecraft/world/item/trading/MerchantOffer" => {
                let stacks: Vec<(String, i32)> = args
                    .iter()
                    .filter_map(|a| match get(a) {
                        Some(Obj::Stack(i, n)) => Some((i.clone(), *n)),
                        Some(Obj::Optional(Some(v))) => match get(v) {
                            Some(Obj::Stack(i, n)) => Some((i.clone(), *n)),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
                let ints: Vec<i32> = args
                    .iter()
                    .filter_map(|a| match a {
                        Value::Int(i) => Some(*i),
                        _ => None,
                    })
                    .collect();
                let mult = args
                    .iter()
                    .find_map(|a| match a {
                        Value::Float(f) => Some(*f),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                let n = stacks.len();
                if n < 2 {
                    return Ok(self.unknown(import));
                }
                // (cost[, cost B], result, [uses,] max uses, xp, multiplier)
                let (max_uses, xp) = match ints.len() {
                    3.. => (ints[ints.len() - 2], ints[ints.len() - 1]),
                    2 => (ints[0], ints[1]),
                    _ => (12, 0),
                };
                obj(Obj::Offer(Offer {
                    wants: stacks[0].clone(),
                    also_wants: (n > 2).then(|| stacks[1].clone()),
                    gives: stacks[n - 1].clone(),
                    max_uses,
                    xp,
                    price_multiplier: mult,
                }))
            }
            "net/neoforged/neoforge/common/BasicItemListing" => {
                // (price, [price B,] for sale, max trades, xp[, multiplier]),
                // or (emeralds, for sale, max trades, xp[, multiplier]).
                let mut stacks: Vec<(String, i32)> = Vec::new();
                let mut ints: Vec<i32> = Vec::new();
                for a in args {
                    match (get(a), a) {
                        (Some(Obj::Stack(i, n)), _) => stacks.push((i.clone(), *n)),
                        (_, Value::Int(i)) => ints.push(*i),
                        _ => {}
                    }
                }
                let mult = args
                    .iter()
                    .find_map(|a| match a {
                        Value::Float(f) => Some(*f),
                        _ => None,
                    })
                    .unwrap_or(0.05);
                if stacks.len() == 1 && ints.len() >= 3 {
                    stacks.insert(0, ("minecraft:emerald".into(), ints.remove(0)));
                }
                if stacks.len() < 2 || ints.len() < 2 {
                    return Ok(self.unknown(import));
                }
                let n = stacks.len();
                obj(Obj::Offer(Offer {
                    wants: stacks[0].clone(),
                    also_wants: (n > 2).then(|| stacks[1].clone()),
                    gives: stacks[n - 1].clone(),
                    max_uses: ints[0],
                    xp: ints[1],
                    price_multiplier: mult,
                }))
            }
            "com/google/common/collect/ImmutableMap$Builder" => {
                obj(Obj::Map(Mutex::new(Vec::new())))
            }
            "com/google/common/collect/ImmutableList$Builder"
            | "com/google/common/collect/ImmutableSet$Builder" => {
                obj(Obj::List(Mutex::new(Vec::new())))
            }
            "net/minecraft/world/item/Item$Properties" => {
                obj(Obj::ItemProps(Mutex::new(ItemProps::default())))
            }
            "net/minecraft/world/food/FoodProperties$Builder" => {
                obj(Obj::Food(Mutex::new(Food::default())))
            }
            "net/minecraft/resources/ResourceLocation" => {
                let ns = args.first().and_then(string).unwrap_or_default();
                let path = args.get(1).and_then(string).unwrap_or_default();
                obj(Obj::Loc(format!("{ns}:{path}")))
            }
            owner
                if owner.starts_with("net/minecraft/world/level/block/")
                    && (self.game.is_block_class(simple(owner)) || simple(owner) == "Block") =>
            {
                let props = args
                    .iter()
                    .find(|a| matches!(get(a), Some(Obj::BlockProps(_))))
                    .cloned()
                    .unwrap_or(Value::Null);
                self.game_block_class(simple(owner), &props)?
            }
            owner if owner.starts_with("net/minecraft/world/item/") => {
                let props = args
                    .iter()
                    .find_map(|a| match get(a) {
                        Some(Obj::ItemProps(p)) => Some(p.lock().unwrap().clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let block = args
                    .iter()
                    .find(|a| matches!(a, Value::Mod(_)) || matches!(get(a), Some(Obj::Block(_))))
                    .cloned();
                let data = Arc::new(ItemData {
                    this: None,
                    id: Mutex::new(None),
                    class: simple(owner).to_owned(),
                    props,
                    block: if owner.ends_with("BlockItem") {
                        block
                    } else {
                        None
                    },
                });
                self.state.borrow_mut().items.push(data.clone());
                obj(Obj::Item(data))
            }
            _ => self.unknown(import),
        })
    }

    fn static_call(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value> {
        let member = format!("{}.{}", simple(&import.owner), import.name);
        let f = |i: usize| args.get(i).and_then(float).unwrap_or(0.0);
        Ok(match member.as_str() {
            "Pair.of" => obj(Obj::Pair(args[0].clone(), args[1].clone())),
            // A server's: success.
            "InteractionResult.sidedSuccess" => self.constant(&import.owner, "SUCCESS"),
            "SoundEvent.createVariableRangeEvent" | "SoundEvent.createFixedRangeEvent" => {
                obj(Obj::Sound(string(&args[0]).unwrap_or_default()))
            }
            "SinglePoolElement.single"
            | "SinglePoolElement.legacy"
            | "StructurePoolElement.single"
            | "StructurePoolElement.legacy" => {
                let location = string(&args[0]).unwrap_or_default();
                let processors = match args.get(1).and_then(get) {
                    Some(Obj::Key(_, id)) | Some(Obj::Loc(id)) | Some(Obj::Str(id)) => id.clone(),
                    _ => "minecraft:empty".into(),
                };
                obj(Obj::PoolFn(location, processors, import.name == "legacy"))
            }
            "Integer.valueOf" => obj(Obj::Boxed(Prim::Int(int(&args[0]).unwrap_or(0) as i32))),
            "Boolean.valueOf" => obj(Obj::Boxed(Prim::Bool(int(&args[0]).unwrap_or(0) != 0))),
            "Float.valueOf" => obj(Obj::Boxed(Prim::Float(f(0) as f32))),
            "Double.valueOf" => obj(Obj::Boxed(Prim::Double(f(0)))),
            "Long.valueOf" => obj(Obj::Boxed(Prim::Long(int(&args[0]).unwrap_or(0)))),
            "String.valueOf" => obj(Obj::Str(self.to_string(cx, &args[0])?)),
            "Math.max" => match (&args[0], &args[1]) {
                (Value::Int(a), Value::Int(b)) => Value::Int(*a.max(b)),
                _ => Value::Double(f(0).max(f(1))),
            },
            "Math.min" => match (&args[0], &args[1]) {
                (Value::Int(a), Value::Int(b)) => Value::Int(*a.min(b)),
                _ => Value::Double(f(0).min(f(1))),
            },
            // Block properties.
            "BlockBehaviour$Properties.of" => obj(Obj::BlockProps(Mutex::new(BlockProps::new()))),
            "BlockBehaviour$Properties.ofFullCopy" | "BlockBehaviour$Properties.ofLegacyCopy" => {
                let peer = self.peer(cx, &args[0])?;
                let props = match get(&peer) {
                    Some(Obj::Block(b)) => {
                        let mut p = b.props.clone();
                        if b.this.is_none() {
                            p.copy_of = b.id.lock().unwrap().clone();
                        }
                        p
                    }
                    _ => BlockProps::new(),
                };
                obj(Obj::BlockProps(Mutex::new(props)))
            }
            // Shapes, in sixteenths (Block.box) or blocks (Shapes.box).
            _ if import.name == "box"
                && import.descriptor == "(DDDDDD)Lnet/minecraft/world/phys/shapes/VoxelShape;" =>
            {
                shape(vec![[
                    f(0) / 16.0,
                    f(1) / 16.0,
                    f(2) / 16.0,
                    f(3) / 16.0,
                    f(4) / 16.0,
                    f(5) / 16.0,
                ]])
            }
            "Shapes.box" => shape(vec![[f(0), f(1), f(2), f(3), f(4), f(5)]]),
            "Shapes.block" => shape(full()),
            "Shapes.empty" => shape(Vec::new()),
            "Shapes.or" => {
                let mut all = boxes(&args[0]);
                for a in &args[1..] {
                    match cx.array(a)? {
                        Some(items) => items.iter().for_each(|s| all.extend(boxes(s))),
                        None => all.extend(boxes(a)),
                    }
                }
                shape(all)
            }
            "Shapes.join" | "Shapes.joinUnoptimized" => {
                let op = match get(&args[2]) {
                    Some(Obj::Const(_, n)) => n.clone(),
                    _ => String::new(),
                };
                shape(join(&boxes(&args[0]), &boxes(&args[1]), &op))
            }
            // Properties a mod makes.
            "BooleanProperty.create" => {
                let name = string(&args[0]).unwrap_or_default();
                self.property_value(&self.property(&name, vec!["true".into(), "false".into()]))
            }
            "IntegerProperty.create" => {
                let name = string(&args[0]).unwrap_or_default();
                let (a, b) = (int(&args[1]).unwrap_or(0), int(&args[2]).unwrap_or(0));
                self.property_value(&self.property(&name, (a..=b).map(|v| v.to_string()).collect()))
            }
            "DirectionProperty.create" => {
                let name = string(&args[0]).unwrap_or_default();
                let mut values: Vec<String> = Vec::new();
                for a in &args[1..] {
                    let list = match (cx.array(a)?, get(a)) {
                        (Some(items), _) => items,
                        (_, Some(Obj::List(l))) => l.lock().unwrap().clone(),
                        _ => vec![a.clone()],
                    };
                    for d in list {
                        if let Some(Obj::Const(_, n)) = get(&d) {
                            values.push(n.to_lowercase());
                        }
                    }
                }
                if values.is_empty() {
                    values = HORIZONTAL.iter().map(|s| (*s).to_owned()).collect();
                }
                self.property_value(&self.property(&name, values))
            }
            "EnumProperty.create" => self.enum_property(cx, args)?,
            // Registers.
            "DeferredRegister.create" => {
                let registry = match get(&args[0]) {
                    Some(Obj::Registry(r)) => r.clone(),
                    Some(Obj::Key(_, id)) => id.trim_start_matches("minecraft:").to_owned(),
                    _ => "unknown".into(),
                };
                self.register(&registry, &string(&args[1]).unwrap_or_default())
            }
            "DeferredRegister.createBlocks" => {
                self.register("block", &string(&args[0]).unwrap_or_default())
            }
            "DeferredRegister.createItems" => {
                self.register("item", &string(&args[0]).unwrap_or_default())
            }
            "DeferredRegister.createDataComponents" => self.register(
                "data_component_type",
                &string(&args[args.len() - 1]).unwrap_or_default(),
            ),
            "ResourceLocation.fromNamespaceAndPath" => {
                let ns = string(&args[0]).unwrap_or_default();
                obj(Obj::Loc(format!(
                    "{ns}:{}",
                    string(&args[1]).unwrap_or_default()
                )))
            }
            "ResourceLocation.parse" | "ResourceLocation.tryParse" => {
                let s = string(&args[0]).unwrap_or_default();
                obj(Obj::Loc(if s.contains(':') {
                    s
                } else {
                    format!("minecraft:{s}")
                }))
            }
            "ResourceLocation.withDefaultNamespace" => obj(Obj::Loc(format!(
                "minecraft:{}",
                string(&args[0]).unwrap_or_default()
            ))),
            "ResourceKey.create" => {
                let registry = match get(&args[0]) {
                    Some(Obj::Key(_, id)) | Some(Obj::Loc(id)) => id.clone(),
                    Some(Obj::Registry(r)) => r.clone(),
                    _ => String::new(),
                };
                let id = string(&args[1]).unwrap_or_default();
                obj(Obj::Key(
                    registry.trim_start_matches("minecraft:").to_owned(),
                    id,
                ))
            }
            // Collections.
            "List.of"
            | "Set.of"
            | "ImmutableList.of"
            | "ImmutableSet.of"
            | "Arrays.asList"
            | "Lists.newArrayList"
            | "Sets.newHashSet"
            | "Sets.newLinkedHashSet"
            | "List.copyOf"
            | "Set.copyOf"
            | "ImmutableList.copyOf"
            | "ImmutableSet.copyOf" => {
                let mut items = Vec::new();
                for a in args {
                    match (cx.array(a)?, get(a)) {
                        (Some(xs), _) => items.extend(xs),
                        (_, Some(Obj::List(l))) => items.extend(l.lock().unwrap().iter().cloned()),
                        _ => items.push(a.clone()),
                    }
                }
                obj(Obj::List(Mutex::new(items)))
            }
            "Map.of" | "ImmutableMap.of" => {
                let pairs = args
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| (c[0].clone(), c[1].clone()))
                    .collect();
                obj(Obj::Map(Mutex::new(pairs)))
            }
            "Maps.newHashMap" | "Maps.newLinkedHashMap" | "Maps.newEnumMap" => {
                obj(Obj::Map(Mutex::new(Vec::new())))
            }
            "Optional.of" | "Optional.ofNullable" => obj(Obj::Optional(match &args[0] {
                Value::Null => None,
                v => Some(v.clone()),
            })),
            "Optional.empty" => obj(Obj::Optional(None)),
            "ImmutableMap.builder" | "ImmutableSortedMap.naturalOrder" => {
                obj(Obj::Map(Mutex::new(Vec::new())))
            }
            "ImmutableList.builder" | "ImmutableSet.builder" => {
                obj(Obj::List(Mutex::new(Vec::new())))
            }
            "Util.make" => {
                self.invoke(
                    cx,
                    &args[1],
                    "accept",
                    "(Ljava/lang/Object;)V",
                    std::slice::from_ref(&args[0]),
                )?;
                args[0].clone()
            }
            "Suppliers.memoize"
            | "Maps.immutableEnumMap"
            | "Collections.unmodifiableMap"
            | "Collections.unmodifiableList"
            | "Collections.unmodifiableSet"
            | "ImmutableMap.copyOf" => args[0].clone(),
            "Stream.of" | "Arrays.stream" => {
                let mut items = Vec::new();
                for a in args {
                    match cx.array(a)? {
                        Some(xs) => items.extend(xs),
                        None => items.push(a.clone()),
                    }
                }
                obj(Obj::List(Mutex::new(items)))
            }
            "Direction.from2DDataValue" => {
                let i = int(&args[0]).unwrap_or(0).rem_euclid(4) as usize;
                self.constant(
                    "net/minecraft/core/Direction",
                    ["SOUTH", "WEST", "NORTH", "EAST"][i],
                )
            }
            "Objects.requireNonNull" | "Preconditions.checkNotNull" => args[0].clone(),
            "Objects.equals" => Value::Int(self.equal(cx, &args[0], &args[1])? as i32),
            _ => self.unknown(import),
        })
    }

    /// `EnumProperty.create(name, Class[, values or a predicate])`.
    fn enum_property(&self, cx: &mut Context<'_>, args: &[Value]) -> Result<Value> {
        let name = string(&args[0]).unwrap_or_default();
        let class = match get(&args[1]) {
            Some(Obj::Class(c)) => c.clone(),
            _ => String::new(),
        };
        // A mod's enum: its constants from `values()`.
        let constants = match cx.call(&format!("{class}.values()[L{class};"), &[]) {
            Ok(arr) => cx.array(&arr)?.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let mut chosen = constants.clone();
        if let Some(extra) = args.get(2) {
            match cx.array(extra)? {
                Some(only) if !only.is_empty() => chosen = only,
                _ if matches!(extra, Value::Mod(_)) => {
                    let mut keep = Vec::new();
                    for c in &constants {
                        let test = self.invoke(
                            cx,
                            extra,
                            "test",
                            "(Ljava/lang/Object;)Z",
                            std::slice::from_ref(c),
                        )?;
                        if int(&test).unwrap_or(0) != 0 {
                            keep.push(c.clone());
                        }
                    }
                    chosen = keep;
                }
                _ => {}
            }
        }
        let mut values = Vec::new();
        for c in &chosen {
            let serialized =
                cx.call_virtual(c, "getSerializedName", "()Ljava/lang/String;", &[])?;
            let v = match serialized.as_ref().and_then(string) {
                Some(s) => s,
                None => match get(&self.peer(cx, c)?) {
                    Some(Obj::EnumPeer(n, _)) => n.to_lowercase(),
                    _ => "?".into(),
                },
            };
            values.push(v);
        }
        let p = Arc::new(Prop {
            name,
            kind: PropKind::Enum,
            values,
            objects: chosen,
        });
        Ok(obj(Obj::Property(p)))
    }

    /// An instance method, on a game or JDK object (`target`; `this` is the
    /// object as the mod has it).
    fn method(
        &self,
        cx: &mut Context<'_>,
        import: &Import,
        this: &Value,
        target: &Value,
        args: &[Value],
    ) -> Result<Value> {
        let name = import.name.as_str();
        let a = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
        let a0 = a(0);
        // Builders return themselves for any setter they do not model.
        let returns_self = import
            .descriptor
            .rsplit_once(')')
            .is_some_and(|(_, r)| r == format!("L{};", import.owner));
        let Some(o) = get(target) else {
            if std::env::var_os("HORIZON_NEOFORGE_DEBUG").is_some() {
                eprintln!("{import} on {target:?}");
            }
            return Ok(self.unknown(import));
        };
        Ok(match o {
            Obj::BlockProps(p) => {
                let mut p = p.lock().unwrap();
                match name {
                    "strength" => {
                        let h = float(&a0).unwrap_or(0.0) as f32;
                        p.hardness = Some(h);
                        p.resistance = Some(args.get(1).and_then(float).map_or(h, |r| r as f32));
                    }
                    "destroyTime" => p.hardness = float(&a0).map(|v| v as f32),
                    "explosionResistance" => p.resistance = float(&a0).map(|v| v as f32),
                    "instabreak" => {
                        p.hardness = Some(0.0);
                        p.resistance = Some(0.0);
                    }
                    "sound" => {
                        if let Some(Obj::Const(_, n)) = get(&a0) {
                            p.sounds = self
                                .game
                                .block_with_sound(&n.to_lowercase())
                                .map(str::to_owned);
                        }
                    }
                    "lightLevel" => p.light = Some(a(0)),
                    "noOcclusion" => p.occludes = false,
                    "noCollission" | "noCollision" => {
                        p.collides = false;
                        p.occludes = false;
                    }
                    "requiresCorrectToolForDrops" => p.requires_tool = true,
                    "friction" => p.friction = float(&a0).map(|v| v as f32),
                    "mapColor" => {
                        if let Some(Obj::Const(_, n)) = get(&a0) {
                            p.map_color = MAP_COLORS.iter().position(|c| c == n).map(|i| i as u8);
                        }
                    }
                    _ => {}
                }
                target.clone()
            }
            Obj::ItemProps(p) => {
                let mut p = p.lock().unwrap();
                match name {
                    "stacksTo" => p.stack = int(&a0).map(|v| v as u32),
                    "durability" => p.durability = int(&a0).map(|v| v as u32),
                    "rarity" => {
                        if let Some(Obj::Const(_, n)) = get(&a0) {
                            p.rarity = Some(n.to_lowercase());
                        }
                    }
                    "food" => {
                        if let Some(Obj::Food(f)) = get(&a0) {
                            p.food = Some(f.lock().unwrap().clone());
                        }
                    }
                    "fireResistant" => p.fire_resistant = true,
                    "craftRemainder" => p.remainder = Some(a(0)),
                    _ => {}
                }
                target.clone()
            }
            // FoodProperties is a record: its accessors take nothing.
            Obj::Food(f) if args.is_empty() && name != "build" && !returns_self => {
                let f = f.lock().unwrap();
                match name {
                    "nutrition" | "getNutrition" => Value::Int(f.nutrition),
                    "saturation" | "getSaturationModifier" => {
                        Value::Float(f.nutrition as f32 * f.saturation_modifier * 2.0)
                    }
                    "canAlwaysEat" => Value::Int(f.always_edible as i32),
                    "eatSeconds" => Value::Float(if f.fast { 0.8 } else { 1.6 }),
                    _ => self.unknown(import),
                }
            }
            Obj::Food(f) => {
                if name == "build" {
                    return Ok(obj(Obj::Food(Mutex::new(f.lock().unwrap().clone()))));
                }
                let mut f = f.lock().unwrap();
                match name {
                    "nutrition" => f.nutrition = int(&a0).unwrap_or(0) as i32,
                    "saturationModifier" | "saturationMod" => {
                        f.saturation_modifier = float(&a0).unwrap_or(0.0) as f32
                    }
                    "alwaysEdible" | "alwaysEat" => f.always_edible = true,
                    "fast" => f.fast = true,
                    _ => {}
                }
                target.clone()
            }
            Obj::Builder(list) => {
                if name == "add" {
                    let mut l = list.lock().unwrap();
                    for v in cx.array(&a0)?.unwrap_or_else(|| args.to_vec()) {
                        if let Some(Obj::Property(p)) = get(&v)
                            && !l.iter().any(|x| x.name == p.name)
                        {
                            l.push(p.clone());
                        }
                    }
                }
                target.clone()
            }
            Obj::Property(p) => match name {
                "getName" => self.string(&p.name),
                "getPossibleValues" => obj(Obj::List(Mutex::new(p.objects.clone()))),
                "getValue" => {
                    let s = string(&a0).unwrap_or_default();
                    obj(Obj::Optional(
                        p.values
                            .iter()
                            .position(|v| *v == s)
                            .map(|i| p.objects[i].clone()),
                    ))
                }
                _ => self.unknown(import),
            },
            Obj::Block(b) => match name {
                // Block's constructor calls it; this is the game's.
                "createBlockStateDefinition" => {
                    let class = if import.op == "invokespecial" {
                        simple(&import.owner).to_owned()
                    } else {
                        b.class.clone()
                    };
                    self.vanilla_definition(&class, &a0);
                    Value::Void
                }
                "defaultBlockState" => match b.def() {
                    Some(d) => obj(Obj::State(d, b.default_index())),
                    None => self.unknown(import),
                },
                "registerDefaultState" => {
                    if let Some(Obj::State(_, i)) = get(&a0) {
                        b.default.store(*i, Ordering::Relaxed);
                    }
                    Value::Void
                }
                "stateDefinition" | "getStateDefinition" => match b.def() {
                    Some(d) => obj(Obj::StateDef(d)),
                    None => self.unknown(import),
                },
                // The game's own use: the block's game class decides.
                "useWithoutItem" | "useItemOn" => Value::Null,
                // CropBlock's age, for a crop that keeps it.
                "getAgeProperty" => {
                    match self.static_property("net/minecraft/world/level/block/CropBlock", "AGE") {
                        Some(p) => self.property_value(&p),
                        None => self.unknown(import),
                    }
                }
                "getMaxAge" => Value::Int(7),
                "asItem" => {
                    let items = self.state.borrow().items.clone();
                    let mut found = None;
                    for i in items {
                        if let Some(blk) = &i.block
                            && cx.same(blk, this)?
                        {
                            found =
                                Some(i.this.clone().unwrap_or_else(|| obj(Obj::Item(i.clone()))));
                        }
                    }
                    found.unwrap_or_else(|| self.game_item("minecraft:air"))
                }
                // The game's shapes for a block that does not override them.
                "getCollisionShape" | "getOcclusionShape"
                    if b.props.collides
                        && b.cross.lock().unwrap().is_none()
                        && !self.game.own_collision(&b.class) =>
                {
                    // BlockBehaviour.getCollisionShape: the state's own shape.
                    let desc = "(Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/BlockGetter;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/phys/shapes/CollisionContext;)Lnet/minecraft/world/phys/shapes/VoxelShape;";
                    match cx.call_virtual(this, "getShape", desc, args)? {
                        Some(s) => s,
                        None => shape(self.vanilla_shape(b, &a0, false)),
                    }
                }
                "getShape" | "getCollisionShape" | "getOcclusionShape" | "getInteractionShape" => {
                    let collision = name != "getShape" && name != "getInteractionShape";
                    shape(self.vanilla_shape(b, &a0, collision))
                }
                _ if returns_self => this.clone(),
                _ => self.unknown(import),
            },
            Obj::StateDef(d) => match name {
                "any" => obj(Obj::State(d.clone(), 0)),
                "getPossibleStates" => obj(Obj::List(Mutex::new(
                    (0..d.count())
                        .map(|i| obj(Obj::State(d.clone(), i)))
                        .collect(),
                ))),
                "getProperty" => {
                    let n = string(&a0).unwrap_or_default();
                    match d.prop(&n) {
                        Some(i) => self.property_value(&d.props[i]),
                        None => Value::Null,
                    }
                }
                "getOwner" => d.block.lock().unwrap().clone().unwrap_or(Value::Null),
                _ => self.unknown(import),
            },
            Obj::State(d, i) => match name {
                "setValue" | "trySetValue" => {
                    let Some(Obj::Property(p)) = get(&a0) else {
                        return Ok(target.clone());
                    };
                    let Some(pi) = d.prop(&p.name) else {
                        if name == "trySetValue" {
                            return Ok(target.clone());
                        }
                        bail!("{} has no property {}", import.owner, p.name);
                    };
                    let Some(vi) = self.value_name(cx, &d.props[pi], &a(1))? else {
                        bail!("not a value of {}", p.name);
                    };
                    let mut values = d.values(*i);
                    values[pi] = vi;
                    obj(Obj::State(d.clone(), d.index(&values)))
                }
                "getValue" => {
                    let Some(Obj::Property(p)) = get(&a0) else {
                        return Ok(self.unknown(import));
                    };
                    let Some(pi) = d.prop(&p.name) else {
                        bail!("no property {}", p.name);
                    };
                    d.props[pi].objects[d.values(*i)[pi]].clone()
                }
                "hasProperty" => {
                    let has =
                        matches!(get(&a0), Some(Obj::Property(p)) if d.prop(&p.name).is_some());
                    Value::Int(has as i32)
                }
                "getBlock" => d.block.lock().unwrap().clone().unwrap_or(Value::Null),
                "is" => {
                    let blk = d.block.lock().unwrap().clone().unwrap_or(Value::Null);
                    Value::Int(cx.same(&blk, &a0)? as i32)
                }
                "cycle" => {
                    let Some(Obj::Property(p)) = get(&a0) else {
                        return Ok(target.clone());
                    };
                    let Some(pi) = d.prop(&p.name) else {
                        return Ok(target.clone());
                    };
                    let mut values = d.values(*i);
                    values[pi] = (values[pi] + 1) % d.props[pi].values.len();
                    obj(Obj::State(d.clone(), d.index(&values)))
                }
                _ => self.unknown(import),
            },
            Obj::Shape(b) => match name {
                "move" => {
                    let (x, y, z) = (
                        float(&a0).unwrap_or(0.0),
                        float(&a(1)).unwrap_or(0.0),
                        float(&a(2)).unwrap_or(0.0),
                    );
                    shape(
                        b.iter()
                            .map(|q| [q[0] + x, q[1] + y, q[2] + z, q[3] + x, q[4] + y, q[5] + z])
                            .collect(),
                    )
                }
                "optimize" | "singleEncompassing" => target.clone(),
                "forAllBoxes" => {
                    for q in b.clone() {
                        let args: Vec<Value> = q.iter().map(|v| Value::Double(*v)).collect();
                        self.invoke(cx, &a0, "consume", "(DDDDDD)V", &args)?;
                    }
                    Value::Void
                }
                "toAabbs" => obj(Obj::List(Mutex::new(
                    b.iter().map(|q| shape(vec![*q])).collect(),
                ))),
                "isEmpty" => Value::Int(b.is_empty() as i32),
                _ => self.unknown(import),
            },
            Obj::Register(r) => match (name, import.descriptor.as_str()) {
                ("register", d) if d.starts_with("(Lnet/neoforged/bus/api/IEventBus;") => {
                    r.active.store(true, Ordering::Relaxed);
                    Value::Void
                }
                ("register", d) if d.contains("Ljava/util/function/Supplier;") => {
                    self.holder(r, &string(&a0).unwrap_or_default(), a(1), None)
                }
                ("register", _) => {
                    let n = string(&a0).unwrap_or_default();
                    let loc = obj(Obj::Loc(format!("{}:{n}", r.namespace)));
                    self.holder(r, &n, a(1), Some(loc))
                }
                ("registerBlock", _) | ("registerItem", _) => {
                    let n = string(&a0).unwrap_or_default();
                    self.holder(r, &n, a(1), Some(a(2)))
                }
                ("registerSimpleBlock", _) => {
                    let n = string(&a0).unwrap_or_default();
                    let props = a(1);
                    let block = self.game_block_class("Block", &props)?;
                    self.holder_value(r, &n, block)
                }
                ("registerSimpleItem", _) => {
                    let n = string(&a0).unwrap_or_default();
                    let props = match get(&a(1)) {
                        Some(Obj::ItemProps(p)) => p.lock().unwrap().clone(),
                        _ => ItemProps::default(),
                    };
                    let item = obj(Obj::Item(Arc::new(ItemData {
                        this: None,
                        id: Mutex::new(Some(format!("{}:{n}", r.namespace))),
                        class: "Item".into(),
                        props,
                        block: None,
                    })));
                    self.holder_value(r, &n, item)
                }
                ("registerSimpleBlockItem", _) => {
                    // (holder[, props]) or (name, block supplier[, props]).
                    let (n, block) = match get(&a0) {
                        Some(Obj::Holder(h)) => (
                            h.id.split_once(':')
                                .map_or(h.id.clone(), |x| x.1.to_owned()),
                            self.bind(cx, h)?,
                        ),
                        _ => {
                            let s = a(1);
                            let blk = self.invoke(cx, &s, "get", "()Ljava/lang/Object;", &[])?;
                            (string(&a0).unwrap_or_default(), blk)
                        }
                    };
                    let data = Arc::new(ItemData {
                        this: None,
                        id: Mutex::new(Some(format!("{}:{n}", r.namespace))),
                        class: "BlockItem".into(),
                        props: ItemProps::default(),
                        block: Some(block),
                    });
                    self.state.borrow_mut().items.push(data.clone());
                    self.holder_value(r, &n, obj(Obj::Item(data)))
                }
                ("getEntries", _) => obj(Obj::List(Mutex::new(
                    r.entries
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|h| obj(Obj::Holder(h.clone())))
                        .collect(),
                ))),
                ("getNamespace", _) => self.string(&r.namespace),
                _ => self.unknown(import),
            },
            Obj::Holder(h) => match name {
                "get" | "value" => self.bind(cx, h)?,
                "getId" => obj(Obj::Loc(h.id.clone())),
                "getKey" => obj(Obj::Key(h.registry.clone(), h.id.clone())),
                "isBound" => Value::Int(1),
                "asItem" | "toStack" => {
                    let v = self.bind(cx, h)?;
                    if h.registry == "item" {
                        v
                    } else {
                        let peer = self.peer(cx, &v)?;
                        let mut import = import.clone();
                        import.name = "asItem".into();
                        return self.method(cx, &import, &v, &peer, &[]);
                    }
                }
                _ => {
                    // Anything else is the held object's.
                    let v = self.bind(cx, h)?;
                    let mut all = vec![v];
                    all.extend_from_slice(args);
                    return self.call(cx, import, &all);
                }
            },
            Obj::Str(s) => match name {
                "equals" => Value::Int(self.equal(cx, target, &a0)? as i32),
                "hashCode" => Value::Int(
                    s.bytes()
                        .fold(0i32, |h, b| h.wrapping_mul(31).wrapping_add(i32::from(b))),
                ),
                "length" => Value::Int(s.chars().count() as i32),
                "isEmpty" => Value::Int(s.is_empty() as i32),
                "toLowerCase" => obj(Obj::Str(s.to_lowercase())),
                "toUpperCase" => obj(Obj::Str(s.to_uppercase())),
                "toString" | "intern" => target.clone(),
                "concat" => obj(Obj::Str(format!("{s}{}", string(&a0).unwrap_or_default()))),
                "startsWith" => Value::Int(s.starts_with(&string(&a0).unwrap_or_default()) as i32),
                "endsWith" => Value::Int(s.ends_with(&string(&a0).unwrap_or_default()) as i32),
                "contains" => Value::Int(s.contains(&string(&a0).unwrap_or_default()) as i32),
                "replace" => obj(Obj::Str(
                    s.replace(&self.to_string(cx, &a0)?, &self.to_string(cx, &a(1))?),
                )),
                _ => self.unknown(import),
            },
            Obj::Loc(s) => match name {
                "getNamespace" => self.string(s.split_once(':').map_or("minecraft", |x| x.0)),
                "getPath" => self.string(s.split_once(':').map_or(s.as_str(), |x| x.1)),
                "toString" => self.string(s),
                "equals" => Value::Int(self.equal(cx, target, &a0)? as i32),
                "withPrefix" | "withSuffix" => {
                    let (ns, p) = s.split_once(':').unwrap_or(("minecraft", s));
                    let x = string(&a0).unwrap_or_default();
                    obj(Obj::Loc(if name == "withPrefix" {
                        format!("{ns}:{x}{p}")
                    } else {
                        format!("{ns}:{p}{x}")
                    }))
                }
                _ => self.unknown(import),
            },
            Obj::Boxed(p) => match (name, p) {
                ("intValue" | "longValue", Prim::Int(i)) => Value::Int(*i),
                ("booleanValue", Prim::Bool(b)) => Value::Int(*b as i32),
                ("floatValue", Prim::Float(f)) => Value::Float(*f),
                ("doubleValue", Prim::Double(f)) => Value::Double(*f),
                ("equals", _) => Value::Int(self.equal(cx, target, &a0)? as i32),
                _ => self.unknown(import),
            },
            Obj::EnumPeer(n, ordinal) => match name {
                "name" | "toString" => self.string(n),
                "ordinal" => Value::Int(*ordinal),
                "equals" => Value::Int(cx.same(this, &a0)? as i32),
                "hashCode" => Value::Int(*ordinal),
                _ => self.unknown(import),
            },
            Obj::Const(_, n) => match name {
                "isClient" => Value::Int((n == "CLIENT") as i32),
                "isDedicatedServer" => Value::Int((n == "DEDICATED_SERVER") as i32),
                "name" | "toString" | "getSerializedName" | "getName" => {
                    self.string(&n.to_lowercase())
                }
                "ordinal" => Value::Int(
                    DIRECTIONS
                        .iter()
                        .position(|d| d.eq_ignore_ascii_case(n))
                        .unwrap_or(0) as i32,
                ),
                "getOpposite" => {
                    let opp = match n.as_str() {
                        "NORTH" => "SOUTH",
                        "SOUTH" => "NORTH",
                        "EAST" => "WEST",
                        "WEST" => "EAST",
                        "UP" => "DOWN",
                        "DOWN" => "UP",
                        other => other,
                    };
                    self.constant("net/minecraft/core/Direction", opp)
                }
                "equals" => Value::Int(cx.same(target, &a0)? as i32),
                "getAxis" => {
                    let axis = match n.as_str() {
                        "NORTH" | "SOUTH" => "Z",
                        "EAST" | "WEST" => "X",
                        _ => "Y",
                    };
                    self.constant("net/minecraft/core/Direction$Axis", axis)
                }
                "get2DDataValue" => Value::Int(match n.as_str() {
                    "SOUTH" => 0,
                    "WEST" => 1,
                    "NORTH" => 2,
                    "EAST" => 3,
                    _ => -1,
                }),
                "getClockWise" | "getCounterClockWise" => {
                    let order = ["NORTH", "EAST", "SOUTH", "WEST"];
                    match order.iter().position(|d| d == n) {
                        Some(i) => {
                            let j = if name == "getClockWise" {
                                (i + 1) % 4
                            } else {
                                (i + 3) % 4
                            };
                            self.constant("net/minecraft/core/Direction", order[j])
                        }
                        None => target.clone(),
                    }
                }
                "getStepX" => Value::Int(match n.as_str() {
                    "EAST" => 1,
                    "WEST" => -1,
                    _ => 0,
                }),
                "getStepY" => Value::Int(match n.as_str() {
                    "UP" => 1,
                    "DOWN" => -1,
                    _ => 0,
                }),
                "getStepZ" => Value::Int(match n.as_str() {
                    "SOUTH" => 1,
                    "NORTH" => -1,
                    _ => 0,
                }),
                "hashCode" => Value::Int(n.len() as i32),
                _ => self.unknown(import),
            },
            Obj::Iter(it) => match name {
                "hasNext" => {
                    let it = it.lock().unwrap();
                    Value::Int((it.1 < it.0.len()) as i32)
                }
                "next" => {
                    let mut it = it.lock().unwrap();
                    let v = it.0.get(it.1).cloned().unwrap_or(Value::Null);
                    it.1 += 1;
                    v
                }
                _ => self.unknown(import),
            },
            Obj::Entry(k, v) => match name {
                "getKey" => k.clone(),
                "getValue" => v.clone(),
                _ => self.unknown(import),
            },
            Obj::List(l) => match name {
                "add" if import.owner.ends_with("$Builder") => {
                    l.lock().unwrap().push(a0.clone());
                    target.clone()
                }
                "build" if import.owner.ends_with("$Builder") => target.clone(),
                "add" => {
                    l.lock()
                        .unwrap()
                        .push(args.last().cloned().unwrap_or(Value::Null));
                    Value::Int(1)
                }
                "addAll" => {
                    let more = match get(&a0) {
                        Some(Obj::List(m)) => m.lock().unwrap().clone(),
                        _ => Vec::new(),
                    };
                    l.lock().unwrap().extend(more);
                    Value::Int(1)
                }
                "append" => {
                    let s = self.to_string(cx, &a0)?;
                    l.lock().unwrap().push(obj(Obj::Str(s)));
                    target.clone()
                }
                "get" => {
                    let i = int(&a0).unwrap_or(0) as usize;
                    l.lock().unwrap().get(i).cloned().unwrap_or(Value::Null)
                }
                "size" => Value::Int(l.lock().unwrap().len() as i32),
                "isEmpty" => Value::Int(l.lock().unwrap().is_empty() as i32),
                "contains" => {
                    let items = l.lock().unwrap().clone();
                    let mut found = false;
                    for x in &items {
                        if self.equal(cx, x, &a0)? {
                            found = true;
                            break;
                        }
                    }
                    Value::Int(found as i32)
                }
                "toArray" => {
                    let items = l.lock().unwrap().clone();
                    cx.new_array("Ljava/lang/Object;", &items)?
                }
                "stream" => obj(Obj::List(Mutex::new(l.lock().unwrap().clone()))),
                "iterator" => obj(Obj::Iter(Mutex::new((l.lock().unwrap().clone(), 0)))),
                "forEach" => {
                    let items = l.lock().unwrap().clone();
                    for x in items {
                        self.invoke(cx, &a0, "accept", "(Ljava/lang/Object;)V", &[x])?;
                    }
                    Value::Void
                }
                "map" => {
                    let items = l.lock().unwrap().clone();
                    let mut out = Vec::new();
                    for x in items {
                        out.push(self.invoke(
                            cx,
                            &a0,
                            "apply",
                            "(Ljava/lang/Object;)Ljava/lang/Object;",
                            &[x],
                        )?);
                    }
                    obj(Obj::List(Mutex::new(out)))
                }
                "filter" => {
                    let items = l.lock().unwrap().clone();
                    let mut out = Vec::new();
                    for x in items {
                        let keep = self.invoke(
                            cx,
                            &a0,
                            "test",
                            "(Ljava/lang/Object;)Z",
                            std::slice::from_ref(&x),
                        )?;
                        if int(&keep).unwrap_or(0) != 0 {
                            out.push(x);
                        }
                    }
                    obj(Obj::List(Mutex::new(out)))
                }
                "reduce" => {
                    let items = l.lock().unwrap().clone();
                    // reduce(op) or reduce(identity, op).
                    let (mut acc, op, rest) = if args.len() == 1 {
                        (
                            items.first().cloned(),
                            a0.clone(),
                            items.iter().skip(1).cloned().collect::<Vec<_>>(),
                        )
                    } else {
                        (Some(a0.clone()), a(1), items.clone())
                    };
                    for x in rest {
                        let cur = acc.take().unwrap_or(Value::Null);
                        acc = Some(self.invoke(
                            cx,
                            &op,
                            "apply",
                            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                            &[cur, x],
                        )?);
                    }
                    if args.len() == 1 {
                        obj(Obj::Optional(acc))
                    } else {
                        acc.unwrap_or(Value::Null)
                    }
                }
                "toList" | "collect" | "distinct" | "sorted" => {
                    obj(Obj::List(Mutex::new(l.lock().unwrap().clone())))
                }
                "findFirst" | "findAny" => obj(Obj::Optional(l.lock().unwrap().first().cloned())),
                "toString" => {
                    let items = l.lock().unwrap().clone();
                    let mut s = String::new();
                    for x in &items {
                        s.push_str(&self.to_string(cx, x)?);
                    }
                    obj(Obj::Str(s))
                }
                _ => self.unknown(import),
            },
            Obj::Map(_)
                if import.owner.ends_with("$Builder")
                    && matches!(name, "build" | "buildOrThrow" | "buildKeepingLast") =>
            {
                target.clone()
            }
            Obj::Map(m) => match name {
                "put" if import.owner.ends_with("$Builder") => {
                    m.lock().unwrap().push((a0.clone(), a(1)));
                    target.clone()
                }
                "put" => {
                    let mut entries = m.lock().unwrap().clone();
                    let mut old = Value::Null;
                    let mut done = false;
                    for e in entries.iter_mut() {
                        if self.equal(cx, &e.0, &a0)? {
                            old = std::mem::replace(&mut e.1, a(1));
                            done = true;
                            break;
                        }
                    }
                    if !done {
                        entries.push((a(0), a(1)));
                    }
                    *m.lock().unwrap() = entries;
                    old
                }
                "get" | "getOrDefault" => {
                    let entries = m.lock().unwrap().clone();
                    let mut out = if name == "get" { Value::Null } else { a(1) };
                    for e in &entries {
                        if self.equal(cx, &e.0, &a0)? {
                            out = e.1.clone();
                            break;
                        }
                    }
                    out
                }
                "containsKey" => {
                    let entries = m.lock().unwrap().clone();
                    let mut found = false;
                    for e in &entries {
                        if self.equal(cx, &e.0, &a0)? {
                            found = true;
                        }
                    }
                    Value::Int(found as i32)
                }
                "size" => Value::Int(m.lock().unwrap().len() as i32),
                "values" => obj(Obj::List(Mutex::new(
                    m.lock().unwrap().iter().map(|e| e.1.clone()).collect(),
                ))),
                "entrySet" => obj(Obj::List(Mutex::new(
                    m.lock()
                        .unwrap()
                        .iter()
                        .map(|e| obj(Obj::Entry(e.0.clone(), e.1.clone())))
                        .collect(),
                ))),
                "computeIfAbsent" => {
                    let entries = m.lock().unwrap().clone();
                    for e in &entries {
                        if self.equal(cx, &e.0, &a0)? {
                            return Ok(e.1.clone());
                        }
                    }
                    let v = self.invoke(
                        cx,
                        &a(1),
                        "apply",
                        "(Ljava/lang/Object;)Ljava/lang/Object;",
                        std::slice::from_ref(&a0),
                    )?;
                    m.lock().unwrap().push((a0.clone(), v.clone()));
                    v
                }
                "forEach" => {
                    let entries = m.lock().unwrap().clone();
                    for (k, v) in entries {
                        self.invoke(
                            cx,
                            &a0,
                            "accept",
                            "(Ljava/lang/Object;Ljava/lang/Object;)V",
                            &[k, v],
                        )?;
                    }
                    Value::Void
                }
                "keySet" => obj(Obj::List(Mutex::new(
                    m.lock().unwrap().iter().map(|e| e.0.clone()).collect(),
                ))),
                _ => self.unknown(import),
            },
            Obj::Optional(v) => match name {
                "isPresent" => Value::Int(v.is_some() as i32),
                "isEmpty" => Value::Int(v.is_none() as i32),
                "get" | "orElseThrow" => v.clone().unwrap_or(Value::Null),
                "orElse" => v.clone().unwrap_or_else(|| a(0)),
                _ => self.unknown(import),
            },
            Obj::Level => {
                let Some(level) = self.level() else {
                    return Ok(self.unknown(import));
                };
                match name {
                    "isClientSide" => Value::Int(0),
                    "getBlockState" => {
                        let Some(pos) = Self::pos(&a0) else {
                            return Ok(self.unknown(import));
                        };
                        let id = level.block_state(pos);
                        self.java_state(id).unwrap_or(Value::Null)
                    }
                    "setBlock" | "setBlockAndUpdate" => {
                        let Some(pos) = Self::pos(&a0) else {
                            return Ok(self.unknown(import));
                        };
                        let Some(state) = self.game_state(cx, &a(1)) else {
                            return Ok(Value::Int(0));
                        };
                        let flags = if name == "setBlock" {
                            int(&a(2)).unwrap_or(3) as i32
                        } else {
                            3
                        };
                        let level = self.level().expect("the level");
                        Value::Int(level.set_block(pos, state, flags) as i32)
                    }
                    "removeBlock" | "destroyBlock" => {
                        let Some(pos) = Self::pos(&a0) else {
                            return Ok(self.unknown(import));
                        };
                        Value::Int(level.remove_block(pos) as i32)
                    }
                    "playSound" => {
                        // (player, pos, sound, source, volume, pitch)
                        let pos = args.iter().find_map(Self::pos);
                        let sound = args.iter().find_map(|v| match get(v) {
                            Some(Obj::Sound(s)) => Some(s.clone()),
                            Some(Obj::Holder(h)) => Some(h.id.clone()),
                            _ => None,
                        });
                        let floats: Vec<f32> = args
                            .iter()
                            .filter_map(|v| match v {
                                Value::Float(f) => Some(*f),
                                _ => None,
                            })
                            .collect();
                        if let (Some(pos), Some(sound)) = (pos, sound) {
                            let volume = floats.first().copied().unwrap_or(1.0);
                            let pitch = floats.get(1).copied().unwrap_or(1.0);
                            level.sound(pos, &sound, volume, pitch);
                        }
                        Value::Void
                    }
                    "gameEvent" => {
                        let pos = args.iter().find_map(Self::pos);
                        let event = args.iter().find_map(|v| match get(v) {
                            Some(Obj::Key(_, id)) | Some(Obj::Loc(id)) => Some(id.clone()),
                            Some(Obj::Const(_, n)) => {
                                Some(format!("minecraft:{}", n.to_lowercase()))
                            }
                            _ => None,
                        });
                        if let (Some(pos), Some(event)) = (pos, event) {
                            level.game_event(pos, &event);
                        }
                        Value::Void
                    }
                    "getRandom" => opaque("random"),
                    _ => self.unknown(import),
                }
            }
            Obj::Pos(x, y, z) => {
                let (x, y, z) = (*x, *y, *z);
                let by = |v: &Value| int(v).unwrap_or(1) as i32;
                match name {
                    "getX" => Value::Int(x),
                    "getY" => Value::Int(y),
                    "getZ" => Value::Int(z),
                    "above" => obj(Obj::Pos(
                        x,
                        y + if args.is_empty() { 1 } else { by(&a0) },
                        z,
                    )),
                    "below" => obj(Obj::Pos(
                        x,
                        y - if args.is_empty() { 1 } else { by(&a0) },
                        z,
                    )),
                    "north" => obj(Obj::Pos(
                        x,
                        y,
                        z - if args.is_empty() { 1 } else { by(&a0) },
                    )),
                    "south" => obj(Obj::Pos(
                        x,
                        y,
                        z + if args.is_empty() { 1 } else { by(&a0) },
                    )),
                    "west" => obj(Obj::Pos(
                        x - if args.is_empty() { 1 } else { by(&a0) },
                        y,
                        z,
                    )),
                    "east" => obj(Obj::Pos(
                        x + if args.is_empty() { 1 } else { by(&a0) },
                        y,
                        z,
                    )),
                    "relative" => {
                        let (dx, dy, dz) = match get(&a0) {
                            Some(Obj::Const(_, d)) => match d.as_str() {
                                "NORTH" => (0, 0, -1),
                                "SOUTH" => (0, 0, 1),
                                "WEST" => (-1, 0, 0),
                                "EAST" => (1, 0, 0),
                                "UP" => (0, 1, 0),
                                "DOWN" => (0, -1, 0),
                                _ => (0, 0, 0),
                            },
                            _ => (0, 0, 0),
                        };
                        let n = if args.len() > 1 { by(&a(1)) } else { 1 };
                        obj(Obj::Pos(x + dx * n, y + dy * n, z + dz * n))
                    }
                    "offset" => obj(Obj::Pos(
                        x + int(&a0).unwrap_or(0) as i32,
                        y + int(&a(1)).unwrap_or(0) as i32,
                        z + int(&a(2)).unwrap_or(0) as i32,
                    )),
                    "equals" => Value::Int(
                        matches!(get(&a0), Some(Obj::Pos(p, q, r)) if (*p, *q, *r) == (x, y, z))
                            as i32,
                    ),
                    "immutable" => target.clone(),
                    _ => self.unknown(import),
                }
            }
            Obj::Player { can_eat, creative } => match name {
                "getFoodData" => obj(Obj::FoodData { can_eat: *can_eat }),
                "canEat" => Value::Int((*can_eat || *creative) as i32),
                "isCreative" => Value::Int(*creative as i32),
                "isShiftKeyDown" | "isSpectator" => Value::Int(0),
                "level" => obj(Obj::Level),
                "getItemInHand" | "getMainHandItem" | "getOffhandItem" => {
                    obj(Obj::Stack("minecraft:air".into(), 0))
                }
                "awardStat" | "causeFoodExhaustion" | "displayClientMessage" => Value::Void,
                _ => self.unknown(import),
            },
            Obj::FoodData { can_eat } => match name {
                "eat" => {
                    // eat(nutrition, saturationModifier) or eat(FoodProperties)
                    let (nutrition, modifier) = match (get(&a0), &a0) {
                        (Some(Obj::Food(f)), _) => {
                            let f = f.lock().unwrap();
                            (f.nutrition, f.saturation_modifier)
                        }
                        (_, Value::Int(n)) => (*n, float(&a(1)).unwrap_or(0.0) as f32),
                        _ => (0, 0.0),
                    };
                    let using = self.world.borrow().as_ref().map(|w| w.using);
                    if let (Some(level), Some(pos)) = (self.level(), using) {
                        level.player_effect(
                            pos,
                            horizon_world::level::PlayerEffect::Eat {
                                nutrition,
                                saturation_modifier: modifier,
                            },
                        );
                    }
                    Value::Void
                }
                "needsFood" => Value::Int(*can_eat as i32),
                "getFoodLevel" => Value::Int(if *can_eat { 10 } else { 20 }),
                _ => self.unknown(import),
            },
            Obj::Hit(face, x, y, z) => match name {
                "getDirection" => self.constant("net/minecraft/core/Direction", face),
                "getBlockPos" => obj(Obj::Pos(*x, *y, *z)),
                "isInside" => Value::Int(0),
                _ => self.unknown(import),
            },
            Obj::Bus(bus) => match name {
                "addListener" => {
                    let listener = args.last().cloned().unwrap_or(Value::Null);
                    self.state
                        .borrow_mut()
                        .listeners
                        .push((bus.clone(), listener));
                    Value::Void
                }
                "register" => {
                    self.state
                        .borrow_mut()
                        .registered
                        .push((bus.clone(), a0.clone()));
                    Value::Void
                }
                _ => self.unknown(import),
            },
            Obj::ConfigBuilder => match name {
                n if n.starts_with("define") => {
                    // (name, default, ...): the default, boxed.
                    let default = match a(1) {
                        Value::Int(i) => obj(Obj::Boxed(Prim::Int(i))),
                        Value::Long(i) => obj(Obj::Boxed(Prim::Long(i))),
                        Value::Double(f) => obj(Obj::Boxed(Prim::Double(f))),
                        Value::Float(f) => obj(Obj::Boxed(Prim::Float(f))),
                        v => v,
                    };
                    let default = if import.descriptor.contains(";Z)")
                        || import.descriptor.ends_with(
                            "ZLnet/neoforged/neoforge/common/ModConfigSpec$BooleanValue;",
                        ) {
                        match a(1) {
                            Value::Int(i) => obj(Obj::Boxed(Prim::Bool(i != 0))),
                            _ => default,
                        }
                    } else {
                        default
                    };
                    obj(Obj::ConfigValue(default))
                }
                "build" | "configure" => opaque("ModConfigSpec"),
                _ => target.clone(),
            },
            Obj::ConfigValue(v) => match name {
                "get" | "getDefault" | "getRaw" => v.clone(),
                "getAsInt" | "getAsLong" => Value::Int(int(v).unwrap_or(0) as i32),
                "getAsBoolean" => Value::Int(match get(v) {
                    Some(Obj::Boxed(Prim::Bool(b))) => *b as i32,
                    _ => int(v).unwrap_or(0) as i32,
                }),
                "getAsDouble" => Value::Double(float(v).unwrap_or(0.0)),
                _ => Value::Void,
            },
            Obj::Pair(x, y) => match name {
                "getFirst" | "left" => x.clone(),
                "getSecond" | "right" => y.clone(),
                _ => self.unknown(import),
            },
            Obj::Sound(id) => match name {
                "getLocation" | "location" => obj(Obj::Loc(id.clone())),
                _ => self.unknown(import),
            },
            Obj::Profession(p) => match name {
                "name" => self.string(&p.name),
                "heldJobSite" => p.held.clone(),
                "workSound" => p.sound.clone(),
                "equals" => Value::Int(cx.same(target, &a0)? as i32),
                _ => self.unknown(import),
            },
            Obj::Offer(_) => match name {
                // An ItemListing that is its offer (BasicItemListing).
                "getOffer" => target.clone(),
                _ => self.unknown(import),
            },
            Obj::Event(kind, data) => match (kind.as_str(), name) {
                ("VillagerTradesEvent", "getType") => data[0].clone(),
                ("VillagerTradesEvent", "getTrades") => data[1].clone(),
                ("ServerAboutToStartEvent", "getServer") => opaque("server"),
                _ => self.unknown(import),
            },
            Obj::Registry(r) => match name {
                "get" | "getValue" | "getOptional" | "getHolder" | "getHolderOrThrow"
                | "getOrThrow" => {
                    let id = match get(&a0) {
                        Some(Obj::Loc(id)) | Some(Obj::Key(_, id)) | Some(Obj::Str(id)) => {
                            id.clone()
                        }
                        _ => return Ok(self.unknown(import)),
                    };
                    let v = match r.as_str() {
                        "template_pool" => self.pool(&id),
                        "item" => self.game_item(&id),
                        "block" => self.game_block(&id),
                        _ => obj(Obj::Key(r.clone(), id)),
                    };
                    if import.descriptor.ends_with(")Ljava/util/Optional;") {
                        obj(Obj::Optional(Some(v)))
                    } else {
                        v
                    }
                }
                "getKey" => {
                    let id = self.item_id(cx, &a0)?;
                    obj(Obj::Loc(id))
                }
                "key" => obj(Obj::Key("root".into(), r.clone())),
                _ => self.unknown(import),
            },
            Obj::Pool(id, templates) => match name {
                "templates" => templates.clone(),
                "rawTemplates" => {
                    let raw = self.state.borrow().raw_pools.get(id).cloned();
                    raw.unwrap_or_else(|| obj(Obj::List(Mutex::new(Vec::new()))))
                }
                _ => self.unknown(import),
            },
            Obj::PoolFn(location, processors, legacy) => match name {
                "apply" => obj(Obj::PoolElement(
                    location.clone(),
                    processors.clone(),
                    *legacy,
                )),
                _ => self.unknown(import),
            },
            Obj::Stack(id, n) => match name {
                "getCount" => Value::Int(*n),
                "getItem" | "item" => self.item_by_id(id),
                "copy" => target.clone(),
                "isEmpty" => Value::Int((*n <= 0 || id == "minecraft:air") as i32),
                // ItemStack.getFoodProperties (NeoForge) and get(FOOD).
                "getFoodProperties" | "get" => {
                    let item = self.item_by_id(id);
                    match self.food_of(cx, &item)? {
                        Some(f) => obj(Obj::Food(Mutex::new(f))),
                        None => Value::Null,
                    }
                }
                _ => self.unknown(import),
            },
            Obj::Item(i) => match name {
                "getFoodProperties" => match &i.props.food {
                    Some(f) => obj(Obj::Food(Mutex::new(f.clone()))),
                    None => Value::Null,
                },
                "getDefaultInstance" => obj(Obj::Stack(
                    i.id.lock().unwrap().clone().unwrap_or_default(),
                    1,
                )),
                _ if returns_self => target.clone(),
                _ => self.unknown(import),
            },
            // The server's registries, at ServerAboutToStartEvent.
            Obj::Opaque(_) if name == "registryAccess" => opaque("registry access"),
            Obj::Opaque(_)
                if matches!(
                    name,
                    "registryOrThrow" | "registry" | "lookupOrThrow" | "lookup"
                ) =>
            {
                let reg = match get(&a0) {
                    Some(Obj::Registry(r)) => obj(Obj::Registry(r.clone())),
                    _ => return Ok(self.unknown(import)),
                };
                if import.descriptor.ends_with(")Ljava/util/Optional;") {
                    obj(Obj::Optional(Some(reg)))
                } else {
                    reg
                }
            }
            Obj::Opaque(_) if returns_self => target.clone(),
            other => {
                if std::env::var_os("HORIZON_NEOFORGE_DEBUG").is_some() {
                    let kind = match other {
                        Obj::Opaque(l) => format!("placeholder {l}"),
                        Obj::Holder(h) => format!("holder {}", h.id),
                        Obj::Block(_) => "block".into(),
                        Obj::Item(_) => "item".into(),
                        _ => "another object".into(),
                    };
                    eprintln!("{import} on {kind}");
                }
                self.unknown(import)
            }
        })
    }

    /// A block of a game class made by the layer itself
    /// (`registerSimpleBlock`).
    fn game_block_class(&self, class: &str, props: &Value) -> Result<Value> {
        let props = match get(props) {
            Some(Obj::BlockProps(p)) => p.lock().unwrap().clone(),
            _ => BlockProps::new(),
        };
        let builder = obj(Obj::Builder(Mutex::new(Vec::new())));
        self.vanilla_definition(class, &builder);
        let list = match get(&builder) {
            Some(Obj::Builder(l)) => l.lock().unwrap().clone(),
            _ => Vec::new(),
        };
        let def = Arc::new(StateDef {
            props: list,
            block: Mutex::new(None),
        });
        let rep = self.game.representative(class).cloned().unwrap_or_default();
        let data = Arc::new(BlockData {
            this: None,
            id: Mutex::new(None),
            class: class.to_owned(),
            props,
            def: Mutex::new(Some(def.clone())),
            default: AtomicUsize::new(default_index(&def, &rep.default)),
            cross: Mutex::new(None),
        });
        self.state.borrow_mut().blocks.push(data.clone());
        let v = obj(Obj::Block(data));
        *def.block.lock().unwrap() = Some(v.clone());
        Ok(v)
    }

    /// A block's shape as its game class makes it: the shape of a game
    /// block of that class in the state with the same values.
    pub fn vanilla_shape(&self, b: &BlockData, state: &Value, collision: bool) -> Vec<[f64; 6]> {
        if collision && !b.props.collides {
            return Vec::new();
        }
        let values: BTreeMap<String, String> = match get(state) {
            Some(Obj::State(d, i)) => d
                .props
                .iter()
                .zip(d.values(*i))
                .map(|(p, v)| (p.name.clone(), p.values[v].clone()))
                .collect(),
            _ => BTreeMap::new(),
        };
        if let Some([node, ext, node_h, ext_h, coll_h]) = *b.cross.lock().unwrap() {
            // CrossCollisionBlock.makeShapes: the post, and an arm to each
            // connected side.
            let h = if collision { coll_h } else { ext_h };
            let nh = if collision { coll_h } else { node_h };
            let (f, f1, f2, f3) = (8.0 - node, 8.0 + node, 8.0 - ext, 8.0 + ext);
            let mut boxes = vec![[f, 0.0, f, f1, nh, f1]];
            let on = |k: &str| values.get(k).map(String::as_str) == Some("true");
            if on("north") {
                boxes.push([f2, 0.0, 0.0, f3, h, f3]);
            }
            if on("south") {
                boxes.push([f2, 0.0, f2, f3, h, 16.0]);
            }
            if on("west") {
                boxes.push([0.0, 0.0, f2, f3, h, f3]);
            }
            if on("east") {
                boxes.push([f2, 0.0, f2, 16.0, h, f3]);
            }
            let boxes: Vec<[f64; 6]> = boxes.iter().map(|q| q.map(|v| v / 16.0)).collect();
            return join(&boxes, &[], "FIRST");
        }
        let from_game = self
            .game
            .representative(&b.class)
            .and_then(|r| self.game.state_shapes(&r.id, &values));
        match from_game {
            Some((outline, coll)) if b.class != "Block" => {
                if collision {
                    if b.props.collides { coll } else { Vec::new() }
                } else {
                    outline
                }
            }
            _ if collision && !b.props.collides => Vec::new(),
            _ => full(),
        }
    }

    fn holder_value(&self, r: &Register, name: &str, value: Value) -> Value {
        let h = Arc::new(Holder {
            registry: r.registry.clone(),
            id: format!("{}:{name}", r.namespace),
            supplier: Mutex::new(None),
            value: Mutex::new(Some(value.clone())),
            binding: AtomicBool::new(false),
        });
        if let Some(Obj::Block(b)) = get(&value) {
            b.id.lock().unwrap().get_or_insert_with(|| h.id.clone());
        }
        if let Some(Obj::Item(i)) = get(&value) {
            i.id.lock().unwrap().get_or_insert_with(|| h.id.clone());
        }
        r.entries.lock().unwrap().push(h.clone());
        obj(Obj::Holder(h))
    }
}
