//! The game's objects as the compatibility layer holds them while a mod
//! registers its content: Minecraft's block and item builders, state
//! definitions and shapes, NeoForge's registers and holders, and the JDK
//! values they pass around.
use horizon_java::runtime::{HostObject, Value};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub enum Obj {
    Str(String),
    /// `Integer`, `Boolean`, `Float`... boxed.
    Boxed(Prim),
    /// A class constant (`Foo.class`), by internal name.
    Class(String),
    /// `ResourceLocation`.
    Loc(String),
    /// `ResourceKey`: its registry and id.
    Key(String, String),
    /// A registry (`BuiltInRegistries.BLOCK`, `Registries.BLOCK`), by name.
    Registry(String),
    Register(Arc<Register>),
    Holder(Arc<Holder>),
    BlockProps(Mutex<BlockProps>),
    ItemProps(Mutex<ItemProps>),
    Food(Mutex<Food>),
    Block(Arc<BlockData>),
    Item(Arc<ItemData>),
    Property(Arc<Prop>),
    /// `StateDefinition.Builder`: the properties added.
    Builder(Mutex<Vec<Arc<Prop>>>),
    StateDef(Arc<StateDef>),
    /// A block state: its definition and index.
    State(Arc<StateDef>, usize),
    /// A `VoxelShape`: its boxes, in blocks.
    Shape(Vec<[f64; 6]>),
    /// A constant of a game enum (`Direction.NORTH`, `SoundType.WOOD`...).
    Const(String, String),
    /// The `java.lang.Enum` part of a mod's enum constant.
    EnumPeer(String, i32),
    List(Mutex<Vec<Value>>),
    Map(Mutex<Vec<(Value, Value)>>),
    Optional(Option<Value>),
    /// An iterator over a snapshot.
    Iter(Mutex<(Vec<Value>, usize)>),
    /// A map entry.
    Entry(Value, Value),
    /// An event bus (`mod`, `game`).
    Bus(String),
    /// `ModConfigSpec.Builder`.
    ConfigBuilder,
    /// A config value: its default (the config files are not read here).
    ConfigValue(Value),
    /// `Pair.of(a, b)`.
    Pair(Value, Value),
    /// A sound event, by id.
    Sound(String),
    /// A villager profession: the game's by id, or one a mod makes.
    Profession(Arc<Profession>),
    /// A POI type a mod makes: its states, tickets and range.
    Poi {
        states: Value,
        tickets: i32,
        range: i32,
    },
    /// `ItemCost` and `ItemStack`: an item (by id) and a count.
    Stack(String, i32),
    /// `MerchantOffer`.
    Offer(Offer),
    /// An event the layer fires: its class (simple name) and what it holds.
    Event(String, Vec<Value>),
    /// A template pool, by id: what mods add to its elements.
    Pool(String, Value),
    /// SinglePoolElement.single's `Function<Projection, element>`.
    PoolFn(String, String, bool),
    /// A pool element: location, processors, legacy.
    PoolElement(String, String, bool),
    /// The level a block's code runs in (the game's, while it runs).
    Level,
    /// `BlockPos`.
    Pos(i32, i32, i32),
    /// The player who uses a block: whether they can eat, and are in
    /// creative.
    Player {
        can_eat: bool,
        creative: bool,
    },
    /// `Player.getFoodData()`.
    FoodData {
        can_eat: bool,
    },
    /// `BlockHitResult`: the face (a Direction's name) and the block.
    Hit(String, i32, i32, i32),
    /// What the layer does not model: a placeholder, named for the log.
    Opaque(String),
}

pub struct Profession {
    pub id: Mutex<Option<String>>,
    pub name: String,
    /// `heldJobSite`: a predicate on POI type holders.
    pub held: Value,
    pub requested: Value,
    pub sound: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub wants: (String, i32),
    pub also_wants: Option<(String, i32)>,
    pub gives: (String, i32),
    pub max_uses: i32,
    pub xp: i32,
    pub price_multiplier: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prim {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Bool(bool),
}

pub fn obj(o: Obj) -> Value {
    Value::Host(Arc::new(o) as HostObject)
}

pub fn get(v: &Value) -> Option<&Obj> {
    match v {
        Value::Host(o) => o.downcast_ref::<Obj>(),
        _ => None,
    }
}

pub fn opaque(label: impl Into<String>) -> Value {
    obj(Obj::Opaque(label.into()))
}

pub fn string(v: &Value) -> Option<String> {
    match get(v)? {
        Obj::Str(s) | Obj::Loc(s) => Some(s.clone()),
        _ => None,
    }
}

/// An int, from a primitive or a box.
pub fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(i64::from(*i)),
        Value::Long(i) => Some(*i),
        _ => match get(v)? {
            Obj::Boxed(Prim::Int(i)) => Some(i64::from(*i)),
            Obj::Boxed(Prim::Long(i)) => Some(*i),
            _ => None,
        },
    }
}

pub fn float(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(f64::from(*f)),
        Value::Double(f) => Some(*f),
        Value::Int(i) => Some(f64::from(*i)),
        _ => match get(v)? {
            Obj::Boxed(Prim::Float(f)) => Some(f64::from(*f)),
            Obj::Boxed(Prim::Double(f)) => Some(*f),
            Obj::Boxed(Prim::Int(i)) => Some(f64::from(*i)),
            _ => None,
        },
    }
}

/// A NeoForge `DeferredRegister`.
pub struct Register {
    /// The registry's name (`block`, `item`, `block_entity_type`...).
    pub registry: String,
    pub namespace: String,
    pub entries: Mutex<Vec<Arc<Holder>>>,
    /// Given to the mod bus (`register(IEventBus)`).
    pub active: AtomicBool,
}

/// A `DeferredHolder` (and `DeferredBlock`, `DeferredItem`): bound when its
/// registry registers, or the first time it is used.
pub struct Holder {
    pub registry: String,
    pub id: String,
    /// What makes the value: a `Supplier`, or a `Function` and its argument.
    pub supplier: Mutex<Option<(Value, Option<Value>)>>,
    pub value: Mutex<Option<Value>>,
    pub binding: AtomicBool,
}

/// `BlockBehaviour.Properties`.
#[derive(Clone, Default)]
pub struct BlockProps {
    /// `ofFullCopy` / `ofLegacyCopy` of a game block.
    pub copy_of: Option<String>,
    pub hardness: Option<f32>,
    pub resistance: Option<f32>,
    /// A game block with the same sounds.
    pub sounds: Option<String>,
    /// `lightLevel`: a function of the state.
    pub light: Option<Value>,
    /// The light of a copied game block.
    pub light_level: Option<u8>,
    pub occludes: bool,
    pub collides: bool,
    pub requires_tool: bool,
    pub map_color: Option<u8>,
    pub friction: Option<f32>,
}

impl BlockProps {
    /// `Properties.of()`: Java's defaults (no map colour, breaks at once).
    pub fn new() -> Self {
        BlockProps {
            occludes: true,
            collides: true,
            hardness: Some(0.0),
            resistance: Some(0.0),
            map_color: Some(0),
            ..BlockProps::default()
        }
    }
}

/// `Item.Properties`.
#[derive(Clone, Default)]
pub struct ItemProps {
    pub stack: Option<u32>,
    pub durability: Option<u32>,
    pub rarity: Option<String>,
    pub food: Option<Food>,
    pub fire_resistant: bool,
    pub remainder: Option<Value>,
}

/// `FoodProperties` (and its builder).
#[derive(Clone, Default)]
pub struct Food {
    pub nutrition: i32,
    pub saturation_modifier: f32,
    pub always_edible: bool,
    pub fast: bool,
}

/// A block: a game block, or the peer of a mod's.
pub struct BlockData {
    /// The mod's object; none for a game block.
    pub this: Option<Value>,
    /// A game block's id; a mod block's once registered.
    pub id: Mutex<Option<String>>,
    /// The game block class it extends (simple name: `CropBlock`).
    pub class: String,
    pub props: BlockProps,
    pub def: Mutex<Option<Arc<StateDef>>>,
    pub default: AtomicUsize,
    /// CrossCollisionBlock's constructor numbers: node width, extension
    /// width, node height, extension height, collision height (sixteenths).
    pub cross: Mutex<Option<[f64; 5]>>,
}

impl BlockData {
    pub fn def(&self) -> Option<Arc<StateDef>> {
        self.def.lock().unwrap().clone()
    }
    pub fn default_index(&self) -> usize {
        self.default.load(Ordering::Relaxed)
    }
}

/// An item: a game item, or the peer of a mod's.
pub struct ItemData {
    pub this: Option<Value>,
    pub id: Mutex<Option<String>>,
    /// The game item class it extends (simple name: `BlockItem`).
    pub class: String,
    pub props: ItemProps,
    /// A block item's block.
    pub block: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PropKind {
    Bool,
    Int,
    Enum,
}

/// A block state property: its name and values, as strings and as the
/// objects Java code sees.
pub struct Prop {
    pub name: String,
    pub kind: PropKind,
    pub values: Vec<String>,
    pub objects: Vec<Value>,
}

/// A `StateDefinition`: states in its order, the last property changing
/// fastest.
pub struct StateDef {
    pub props: Vec<Arc<Prop>>,
    pub block: Mutex<Option<Value>>,
}

impl StateDef {
    pub fn count(&self) -> usize {
        self.props.iter().map(|p| p.values.len()).product()
    }
    /// Each property's value index in state `i`.
    pub fn values(&self, mut i: usize) -> Vec<usize> {
        let mut out = vec![0; self.props.len()];
        for (j, p) in self.props.iter().enumerate().rev() {
            out[j] = i % p.values.len();
            i /= p.values.len();
        }
        out
    }
    pub fn index(&self, values: &[usize]) -> usize {
        values
            .iter()
            .zip(&self.props)
            .fold(0, |acc, (v, p)| acc * p.values.len() + v)
    }
    pub fn prop(&self, name: &str) -> Option<usize> {
        self.props.iter().position(|p| p.name == name)
    }
}
