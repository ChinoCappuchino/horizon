//! Runs a compiled NeoForge mod against a recording stand-in for NeoForge
//! and Minecraft (docs/java-compat/plan.md, phase 0, step 2), in NeoForge's
//! order: the `@Mod` class's constructor, the registrations, the mod bus's
//! setup event, the villager trades event for every profession, and the
//! server's about-to-start event. It writes what the mod made, in the form
//! the NeoForge probe dumps (tools/java_compat/neoforge_compare.py
//! compares the two):
//!
//!     record <mod.jar> [out.json] [--trace]
//!
//! The stand-in knows only what the mods of phase 0 call; anything else
//! gives a placeholder object (or 0) and, with --trace, a line. Structure
//! only: no mod code is read.
use horizon_java::{
    ClassFile,
    classfile::ElementValue,
    compile::{HostConstant, Object, compile},
    descriptor::Method,
    runtime::{Context, Host, HostObject, Import, Runtime, Value},
};
use serde_json::{Value as Json, json};
use std::{
    cell::RefCell,
    io::Read,
    rc::Rc,
    sync::{Arc, Mutex},
};
use wasmtime::{Result, bail};

/// An object of the stand-in.
struct Sym {
    label: String,
    state: State,
}

enum State {
    Plain,
    Str(String),
    Int(i32),
    /// An item, block or sound, by registry id (or field name if unknown).
    Id(String),
    /// A `Supplier` of an `Id` (Farmer's Delight's registry fields).
    Supplies(String),
    List(Mutex<Vec<Value>>),
    /// An Int2ObjectMap of trade lists by level.
    Levels(Vec<(i32, Value)>),
    Register {
        registry: String,
        namespace: String,
    },
    Holder {
        id: String,
        value: Mutex<Option<Value>>,
    },
    /// A game object made by a constructor.
    Made {
        class: String,
        args: Vec<Value>,
    },
    Bus(String),
    Config {
        default: i32,
    },
    Optional(Value),
    Pool {
        templates: Value,
    },
    /// The `Function` that SinglePoolElement.single gives.
    Single(String),
}

fn sym(label: impl Into<String>, state: State) -> Value {
    Value::Host(Arc::new(Sym {
        label: label.into(),
        state,
    }) as HostObject)
}

fn get(v: &Value) -> Option<&Sym> {
    match v {
        Value::Host(o) => o.downcast_ref::<Sym>(),
        _ => None,
    }
}

fn int(v: &Value) -> i32 {
    match (v, get(v)) {
        (Value::Int(i), _) => *i,
        (
            _,
            Some(Sym {
                state: State::Int(i),
                ..
            }),
        ) => *i,
        _ => 0,
    }
}

fn text(v: &Value) -> String {
    match get(v) {
        Some(Sym {
            state: State::Str(s) | State::Id(s),
            ..
        }) => s.clone(),
        Some(s) => s.label.clone(),
        None => format!("{v:?}"),
    }
}

fn simple(class: &str) -> &str {
    class.rsplit(['/', '$']).next().unwrap_or(class)
}

/// The registry id a static field of the game or Farmer's Delight names.
fn field_id(owner: &str, name: &str) -> String {
    let namespace = if owner.starts_with("net/minecraft/") {
        "minecraft"
    } else if owner.starts_with("vectorwing/farmersdelight/") {
        "farmersdelight"
    } else {
        return format!("{}.{name}", simple(owner));
    };
    format!("{namespace}:{}", name.to_lowercase())
}

#[derive(Default)]
struct Loader {
    trace: bool,
    strings: RefCell<Vec<(String, Value)>>,
    /// DeferredRegisters given to the mod bus, in order: (register,
    /// [(holder, supplier)]).
    registers: RefCell<Vec<(Value, Vec<(Value, Value)>)>>,
    /// Every DeferredRegister's entries, registered to a bus or not.
    entries: RefCell<Vec<(usize, Value, Value)>>,
    all_registers: RefCell<Vec<Value>>,
    /// (bus, listener) as given to addListener or register.
    listeners: RefCell<Vec<(String, Value)>>,
    configs: RefCell<Vec<String>>,
    pools: RefCell<Vec<(String, Value)>>,
    statics: RefCell<Vec<(String, Value)>>,
    unknown: RefCell<Vec<String>>,
}

impl Loader {
    fn interned(&self, s: &str) -> Value {
        let mut strings = self.strings.borrow_mut();
        if let Some((_, v)) = strings.iter().find(|(k, _)| k == s) {
            return v.clone();
        }
        let v = sym(format!("{s:?}"), State::Str(s.to_owned()));
        strings.push((s.to_owned(), v.clone()));
        v
    }

    /// The same object for the same static field each time.
    fn static_field(&self, owner: &str, name: &str, desc: &str) -> Value {
        let key = format!("{owner}.{name}");
        if let Some((_, v)) = self.statics.borrow().iter().find(|(k, _)| *k == key) {
            return v.clone();
        }
        let id = field_id(owner, name);
        let v = match desc {
            "Ljava/util/function/Supplier;" => sym(format!("{id} (supplier)"), State::Supplies(id)),
            "Lnet/neoforged/bus/api/IEventBus;" => {
                sym("NeoForge.EVENT_BUS", State::Bus("game".into()))
            }
            _ if owner.ends_with("BuiltInRegistries") || owner.ends_with("Registries") => sym(
                format!("{}.{name}", simple(owner)),
                State::Str(name.to_lowercase()),
            ),
            _ => sym(id.clone(), State::Id(id)),
        };
        self.statics.borrow_mut().push((key, v.clone()));
        v
    }

    fn pool(&self, id: &str) -> Value {
        if let Some((_, v)) = self.pools.borrow().iter().find(|(k, _)| k == id) {
            return v.clone();
        }
        let templates = sym(
            format!("{id}.templates"),
            State::List(Mutex::new(Vec::new())),
        );
        let v = sym(id, State::Pool { templates });
        self.pools.borrow_mut().push((id.to_owned(), v.clone()));
        v
    }

    /// A placeholder for what the stand-in does not model.
    fn unknown(&self, import: &Import) -> Value {
        let line = import.to_string();
        if !self.unknown.borrow().contains(&line) {
            self.unknown.borrow_mut().push(line);
        }
        let ret = if import.op == "new" {
            "L;".to_owned()
        } else if matches!(import.op.as_str(), "getstatic" | "getfield") {
            import.descriptor.clone()
        } else if import.op.starts_with("put") {
            "V".to_owned()
        } else {
            Method::parse(&import.descriptor)
                .map(|m| m.ret)
                .unwrap_or_default()
        };
        match ret.as_bytes().first() {
            Some(b'V') | None => Value::Void,
            Some(b'L' | b'[') => sym(
                format!("{}.{}(..)", simple(&import.owner), import.name),
                State::Plain,
            ),
            Some(b'J') => Value::Long(0),
            Some(b'F') => Value::Float(0.0),
            Some(b'D') => Value::Double(0.0),
            _ => Value::Int(0),
        }
    }
}

impl Host for Loader {
    fn call(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value> {
        let r = self.answer(cx, import, args)?;
        if self.trace {
            let args: Vec<String> = args.iter().map(text).collect();
            eprintln!("{import} ({}) -> {}", args.join(", "), text(&r));
        }
        Ok(r)
    }
}

impl Loader {
    fn answer(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value> {
        let member = format!("{}.{}", simple(&import.owner), import.name);
        let this = args.first().and_then(get).map(|s| &s.state);
        Ok(match (import.op.as_str(), member.as_str(), this) {
            ("constant", ..) => match cx.constant(int(&args[0])) {
                Some(HostConstant::String(s)) => self.interned(&s),
                Some(HostConstant::Class(c)) => sym(format!("{}.class", simple(&c)), State::Str(c)),
                None => bail!("no constant {}", int(&args[0])),
            },
            ("same", ..) => Value::Int(cx.same(&args[0], &args[1])? as i32),
            ("getstatic", ..) => self.static_field(&import.owner, &import.name, &import.descriptor),

            // A lambda of the mod, through any interface: call it.
            ("invokeinterface" | "invokevirtual", ..)
                if matches!(args.first(), Some(Value::Mod(_))) =>
            {
                cx.call_lambda(&args[0], &args[1..])?
            }

            // Boxes.
            ("invokestatic", "Integer.valueOf", _) => sym("Integer", State::Int(int(&args[0]))),
            ("invokevirtual", "Integer.intValue", _) => Value::Int(int(&args[0])),

            // The mod's container and buses.
            ("invokevirtual", "ModContainer.registerConfig", _) => {
                self.configs.borrow_mut().push(text(&args[1]));
                Value::Void
            }
            (
                "invokeinterface",
                "IEventBus.addListener" | "IEventBus.register",
                Some(State::Bus(bus)),
            ) => {
                self.listeners
                    .borrow_mut()
                    .push((bus.clone(), args[1].clone()));
                Value::Void
            }

            // The config: defaults.
            ("new", "ModConfigSpec$Builder.<init>" | "Builder.<init>", _) => {
                sym("ModConfigSpec.Builder", State::Plain)
            }
            ("invokevirtual", "Builder.comment" | "Builder.push" | "Builder.pop", _) => {
                args[0].clone()
            }
            ("invokevirtual", "Builder.defineInRange", _) => sym(
                format!("config {}", text(&args[1])),
                State::Config {
                    default: int(&args[2]),
                },
            ),
            ("invokevirtual", "Builder.build", _) => sym("ModConfigSpec", State::Plain),
            ("invokevirtual", "IntValue.get", Some(State::Config { default })) => {
                sym("Integer", State::Int(*default))
            }

            // Registers.
            ("invokestatic", "DeferredRegister.create", _) => {
                let v = sym(
                    format!("DeferredRegister({}, {})", text(&args[0]), text(&args[1])),
                    State::Register {
                        registry: text(&args[0]),
                        namespace: text(&args[1]),
                    },
                );
                self.all_registers.borrow_mut().push(v.clone());
                v
            }
            (
                "invokevirtual",
                "DeferredRegister.register",
                Some(State::Register { namespace, .. }),
            ) => {
                let index = self
                    .all_registers
                    .borrow()
                    .iter()
                    .position(|r| cx_same(r, &args[0]))
                    .unwrap_or(usize::MAX);
                if import
                    .descriptor
                    .starts_with("(Lnet/neoforged/bus/api/IEventBus;")
                {
                    let entries: Vec<(Value, Value)> = self
                        .entries
                        .borrow()
                        .iter()
                        .filter(|(i, ..)| *i == index)
                        .map(|(_, h, s)| (h.clone(), s.clone()))
                        .collect();
                    self.registers.borrow_mut().push((args[0].clone(), entries));
                    Value::Void
                } else {
                    let id = format!("{namespace}:{}", text(&args[1]));
                    let holder = sym(
                        id.clone(),
                        State::Holder {
                            id,
                            value: Mutex::new(None),
                        },
                    );
                    self.entries
                        .borrow_mut()
                        .push((index, holder.clone(), args[2].clone()));
                    holder
                }
            }
            (
                "invokeinterface" | "invokevirtual",
                "Holder.value" | "Supplier.get" | "DeferredHolder.get" | "DeferredHolder.value",
                Some(State::Holder { id, value }),
            ) => match &*value.lock().unwrap() {
                Some(v) => v.clone(),
                None => bail!("{id} is used before it is registered"),
            },
            ("invokeinterface", "Supplier.get", Some(State::Supplies(id))) => {
                sym(id.clone(), State::Id(id.clone()))
            }

            // Game objects: record the constructor's arguments.
            ("new", ..) => sym(
                format!("new {}", simple(&import.owner)),
                State::Made {
                    class: simple(&import.owner).to_owned(),
                    args: args.to_vec(),
                },
            ),
            ("invokestatic", "ImmutableSet.of", _) => {
                sym("ImmutableSet.of()", State::List(Mutex::new(Vec::new())))
            }
            ("invokestatic", "ImmutableSet.copyOf", _) => args[0].clone(),
            ("invokevirtual", "Block.getStateDefinition", Some(State::Id(id))) => {
                sym(format!("{id} states"), State::Id(id.clone()))
            }
            ("invokevirtual", "StateDefinition.getPossibleStates", Some(State::Id(id))) => {
                sym(format!("{id} states"), State::Id(id.clone()))
            }

            // Villager trades.
            ("invokevirtual", "VillagerTradesEvent.getType", _) => match get(&args[0]) {
                Some(Sym {
                    state: State::Made { args, .. },
                    ..
                }) => args[0].clone(),
                _ => bail!("not a trades event"),
            },
            ("invokevirtual", "VillagerTradesEvent.getTrades", _) => match get(&args[0]) {
                Some(Sym {
                    state: State::Made { args, .. },
                    ..
                }) => args[1].clone(),
                _ => bail!("not a trades event"),
            },
            ("invokeinterface", "Int2ObjectMap.get", Some(State::Levels(levels))) => levels
                .iter()
                .find(|(l, _)| *l == int(&args[1]))
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Null),
            (
                "invokeinterface" | "invokevirtual",
                "List.add" | "ObjectArrayList.add",
                Some(State::List(l)),
            ) => {
                l.lock().unwrap().push(args[1].clone());
                Value::Int(1)
            }

            // Village pools, at server start.
            ("invokevirtual", "ServerAboutToStartEvent.getServer", _) => {
                sym("server", State::Plain)
            }
            ("invokevirtual", "MinecraftServer.registryAccess", _) => {
                sym("registryAccess", State::Plain)
            }
            ("invokeinterface", "Frozen.registry" | "RegistryAccess$Frozen.registry", _) => sym(
                format!("Optional({})", text(&args[1])),
                State::Optional(sym(
                    format!("registry {}", text(&args[1])),
                    State::Str(text(&args[1])),
                )),
            ),
            ("invokevirtual", "Optional.get", Some(State::Optional(v))) => v.clone(),
            ("invokestatic", "ResourceLocation.parse", _) => {
                let s = text(&args[0]);
                let id = if s.contains(':') {
                    s
                } else {
                    format!("minecraft:{s}")
                };
                sym(id.clone(), State::Id(id))
            }
            ("invokestatic", "ResourceLocation.withDefaultNamespace", _) => {
                let id = format!("minecraft:{}", text(&args[0]));
                sym(id.clone(), State::Id(id))
            }
            ("invokeinterface", "Registry.get", Some(State::Str(registry)))
                if registry == "template_pool" =>
            {
                self.pool(&text(&args[1]))
            }
            ("invokestatic", "ResourceKey.create", _) => {
                let id = text(&args[1]);
                sym(id.clone(), State::Id(id))
            }
            ("invokeinterface", "Registry.getHolderOrThrow", _) => sym(
                format!("holder {}", text(&args[1])),
                State::Id(text(&args[1])),
            ),
            ("invokestatic", "SinglePoolElement.single", _) => sym(
                format!("single {}", text(&args[0])),
                State::Single(text(&args[0])),
            ),
            ("invokeinterface", "Function.apply", Some(State::Single(name))) => {
                sym(format!("single {name}"), State::Id(name.clone()))
            }
            (
                "getfield",
                "StructureTemplatePool.templates",
                Some(State::Pool { templates, .. }),
            ) => templates.clone(),

            _ => self.unknown(import),
        })
    }
}

/// Reference identity of two host objects, outside a Context.
fn cx_same(a: &Value, b: &Value) -> bool {
    matches!((a, b), (Value::Host(a), Value::Host(b)) if Arc::ptr_eq(a, b))
}

/// What a value describes, for the output.
fn describe(cx: &mut Context<'_>, v: &Value) -> Json {
    match v {
        Value::Int(i) => json!(i),
        Value::Float(f) => json!(f),
        Value::Null => Json::Null,
        Value::Mod(_) => json!(format!(
            "<{}>",
            cx.class(v).ok().flatten().unwrap_or("lambda".into())
        )),
        _ => match get(v) {
            Some(Sym {
                state: State::Id(id) | State::Supplies(id),
                ..
            }) => json!(id),
            Some(Sym {
                state: State::Made { class, args },
                ..
            }) => {
                let args: Vec<Json> = args.iter().map(|a| describe(cx, a)).collect();
                json!({ "new": class, "args": args })
            }
            Some(Sym {
                state: State::Int(i),
                ..
            }) => json!(i),
            Some(Sym {
                state: State::Str(s),
                ..
            }) => json!(s),
            Some(s) => json!(s.label),
            None => json!(format!("{v:?}")),
        },
    }
}

/// An offer, as the probe writes it.
fn offer(cx: &mut Context<'_>, v: &Value) -> Json {
    let Some(Sym {
        state: State::Made { class, args },
        ..
    }) = get(v)
    else {
        return describe(cx, v);
    };
    if class != "MerchantOffer" || args.len() != 5 {
        return describe(cx, v);
    }
    let stack = |cx: &mut Context<'_>, v: &Value| match get(v) {
        Some(Sym {
            state: State::Made { args, .. },
            ..
        }) if args.len() == 2 => {
            json!({ "item": describe(cx, &args[0]), "count": int(&args[1]) })
        }
        _ => describe(cx, v),
    };
    json!({
        "cost": stack(cx, &args[0]),
        "result": stack(cx, &args[1]),
        "max_uses": int(&args[2]),
        "xp": int(&args[3]),
        "price_multiplier": match args[4] { Value::Float(f) => f, _ => 0.0 },
    })
}

const PROFESSIONS: [&str; 15] = [
    "none",
    "armorer",
    "butcher",
    "cartographer",
    "cleric",
    "farmer",
    "fisherman",
    "fletcher",
    "leatherworker",
    "librarian",
    "mason",
    "nitwit",
    "shepherd",
    "toolsmith",
    "weaponsmith",
];

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let trace = args.iter().any(|a| a == "--trace");
    args.retain(|a| a != "--trace");
    let Some(path) = args.first().cloned() else {
        bail!("usage: record <mod.jar> [out.json] [--trace]");
    };
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path)?)?;
    let mut classes = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.name().ends_with(".class") {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            classes.push(ClassFile::parse(&data)?);
        }
    }
    let compiled = compile(&classes).map_err(wasmtime::Error::msg)?;
    for (m, why) in &compiled.skipped {
        eprintln!("not compiled: {m}: {why}");
    }
    let loader = Rc::new(Loader {
        trace,
        ..Loader::default()
    });
    let mut rt = Runtime::new(compiled, loader.clone())?;
    let mut cx = rt.context();
    let mod_bus = sym("mod bus", State::Bus("mod".into()));
    let container = sym("ModContainer", State::Plain);

    // 1. The @Mod classes that run on a server, constructed.
    let enum_value = |a: &horizon_java::classfile::Annotation, key: &str| -> Vec<String> {
        a.values
            .iter()
            .filter(|(k, _)| k == key)
            .flat_map(|(_, v)| match v {
                ElementValue::Enum { name, .. } => vec![name.clone()],
                ElementValue::Array(vs) => vs
                    .iter()
                    .filter_map(|v| match v {
                        ElementValue::Enum { name, .. } => Some(name.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => vec![],
            })
            .collect()
    };
    let mut subscribers = Vec::new();
    for class in &classes {
        let name = class.name()?.to_owned();
        for a in class.annotations() {
            let dist = enum_value(a, "dist");
            let server = dist.is_empty() || dist.iter().any(|d| d == "DEDICATED_SERVER");
            match a.type_descriptor.as_str() {
                "Lnet/neoforged/fml/common/Mod;" if server => {
                    let init = class
                        .methods
                        .iter()
                        .find(|m| m.name == "<init>")
                        .ok_or_else(|| wasmtime::Error::msg("no constructor"))?;
                    let object = cx.call(&format!("new {name}"), &[])?;
                    let mut call = vec![object];
                    for p in Method::parse(&init.descriptor).unwrap().params {
                        call.push(match p.as_str() {
                            "Lnet/neoforged/bus/api/IEventBus;" => mod_bus.clone(),
                            "Lnet/neoforged/fml/ModContainer;" => container.clone(),
                            _ => sym(p, State::Plain),
                        });
                    }
                    cx.call(&format!("{name}.<init>{}", init.descriptor), &call)?;
                }
                "Lnet/neoforged/fml/common/EventBusSubscriber;" if server => {
                    let bus = enum_value(a, "bus").pop().unwrap_or("GAME".into());
                    for m in &class.methods {
                        if m.annotations()
                            .any(|a| a.type_descriptor == "Lnet/neoforged/bus/api/SubscribeEvent;")
                        {
                            subscribers.push((
                                bus.to_lowercase(),
                                name.clone(),
                                m.name.clone(),
                                m.descriptor.clone(),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // 2. RegisterEvent: each register's suppliers, in order.
    let mut registered = Vec::new();
    for (register, entries) in loader.registers.borrow().iter() {
        let registry = match get(register) {
            Some(Sym {
                state: State::Register { registry, .. },
                ..
            }) => registry.clone(),
            _ => continue,
        };
        for (holder, supplier) in entries {
            let value = cx.call_lambda(supplier, &[])?;
            if let Some(Sym {
                state: State::Holder { id, value: slot },
                ..
            }) = get(holder)
            {
                *slot.lock().unwrap() = Some(value.clone());
                registered.push((registry.clone(), id.clone(), holder.clone(), value));
            }
        }
    }

    // The event subscribers and listeners, by event class.
    // An event reaches the listeners of its class and its superclasses
    // (`event` lists them, the event's own first).
    let fire = |cx: &mut Context<'_>, event: &[&str], value: &Value| -> Result<usize> {
        let mut n = 0;
        for (_, class, method, desc) in &subscribers {
            let param = Method::parse(desc)
                .unwrap()
                .params
                .first()
                .cloned()
                .unwrap_or_default();
            if event.contains(&param.as_str()) {
                cx.call(
                    &format!("{class}.{method}{desc}"),
                    std::slice::from_ref(value),
                )?;
                n += 1;
            }
        }
        let listeners = loader.listeners.borrow().clone();
        for (_, l) in &listeners {
            let id = cx.id(l)?;
            let target = match id.and_then(|id| {
                cx.compiled().object(id).map(|o| match o {
                    Object::Lambda(_, l) => l.target.clone(),
                    Object::Class(c) => c.to_owned(),
                })
            }) {
                Some(t) => t,
                None => continue,
            };
            let desc = target.rsplit(' ').next().unwrap_or("");
            let param = Method::parse(desc)
                .and_then(|m| m.params.last().cloned())
                .unwrap_or_default();
            if event.contains(&param.as_str()) {
                cx.call_lambda(l, std::slice::from_ref(value))?;
                n += 1;
            }
        }
        Ok(n)
    };

    if trace {
        for (bus, class, method, desc) in &subscribers {
            eprintln!("subscriber ({bus} bus): {class}.{method}{desc}");
        }
    }

    // 3. The common config loads (the values are their defaults), then
    // setup.
    let loading = sym("ModConfigEvent.Loading", State::Plain);
    fire(
        &mut cx,
        &[
            "Lnet/neoforged/fml/event/config/ModConfigEvent$Loading;",
            "Lnet/neoforged/fml/event/config/ModConfigEvent;",
        ],
        &loading,
    )?;
    let setup = sym("FMLCommonSetupEvent", State::Plain);
    fire(
        &mut cx,
        &["Lnet/neoforged/fml/event/lifecycle/FMLCommonSetupEvent;"],
        &setup,
    )?;

    // 4. Trades, for the game's professions and the mod's.
    let mut professions: Vec<(String, Value)> = PROFESSIONS
        .iter()
        .map(|p| {
            (
                format!("minecraft:{p}"),
                sym(
                    format!("minecraft:{p}"),
                    State::Id(format!("minecraft:{p}")),
                ),
            )
        })
        .collect();
    for (registry, id, _, value) in &registered {
        if registry == "villager_profession" {
            professions.push((id.clone(), value.clone()));
        }
    }
    let mut trades = serde_json::Map::new();
    for (id, profession) in &professions {
        let levels: Vec<(i32, Value)> = (1..=5)
            .map(|l| {
                (
                    l,
                    sym(
                        format!("{id} level {l}"),
                        State::List(Mutex::new(Vec::new())),
                    ),
                )
            })
            .collect();
        let map = sym(format!("{id} trades"), State::Levels(levels.clone()));
        let event = sym(
            "VillagerTradesEvent",
            State::Made {
                class: "VillagerTradesEvent".into(),
                args: vec![profession.clone(), map],
            },
        );
        fire(
            &mut cx,
            &["Lnet/neoforged/neoforge/event/village/VillagerTradesEvent;"],
            &event,
        )?;
        let mut by_level = serde_json::Map::new();
        for (level, list) in &levels {
            let Some(Sym {
                state: State::List(l),
                ..
            }) = get(list)
            else {
                continue;
            };
            let listings = l.lock().unwrap().clone();
            if listings.is_empty() {
                continue;
            }
            let mut offers = Vec::new();
            for listing in &listings {
                let entity = sym("villager", State::Plain);
                let random = sym("random", State::Plain);
                let made = cx.call_lambda(listing, &[entity, random])?;
                offers.push(offer(&mut cx, &made));
            }
            by_level.insert(level.to_string(), Json::Array(offers));
        }
        if !by_level.is_empty() {
            trades.insert(id.clone(), Json::Object(by_level));
        }
    }

    // 5. Server start: village pools.
    let start = sym("ServerAboutToStartEvent", State::Plain);
    fire(
        &mut cx,
        &["Lnet/neoforged/neoforge/event/server/ServerAboutToStartEvent;"],
        &start,
    )?;
    let mut pools = serde_json::Map::new();
    for (id, pool) in loader.pools.borrow().iter() {
        let Some(Sym {
            state: State::Pool { templates, .. },
            ..
        }) = get(pool)
        else {
            continue;
        };
        let Some(Sym {
            state: State::List(l),
            ..
        }) = get(templates)
        else {
            continue;
        };
        let mut counts = serde_json::Map::new();
        for e in l.lock().unwrap().iter() {
            let n = counts.entry(text(e)).or_insert(json!(0));
            *n = json!(n.as_i64().unwrap() + 1);
        }
        pools.insert(id.clone(), Json::Object(counts));
    }

    // What was registered: professions with the job site their predicate
    // accepts, and POI types.
    let mut pois = serde_json::Map::new();
    let mut poi_holders = Vec::new();
    for (registry, id, holder, value) in &registered {
        if registry == "point_of_interest_type" {
            let Some(Sym {
                state: State::Made { args, .. },
                ..
            }) = get(value)
            else {
                continue;
            };
            pois.insert(
                id.clone(),
                json!({
                    "blocks": [describe(&mut cx, &args[0])],
                    "max_tickets": int(&args[1]),
                    "valid_range": int(&args[2]),
                }),
            );
            let h = sym(
                format!("holder {id}"),
                State::Holder {
                    id: id.clone(),
                    value: Mutex::new(Some(value.clone())),
                },
            );
            poi_holders.push((id.clone(), h));
            let _ = holder;
        }
    }
    let mut out_professions = serde_json::Map::new();
    for (registry, id, _, value) in &registered {
        if registry != "villager_profession" {
            continue;
        }
        let Some(Sym {
            state: State::Made { args, .. },
            ..
        }) = get(value)
        else {
            continue;
        };
        let mut sites = Vec::new();
        for (poi, h) in &poi_holders {
            if int(&cx.call_lambda(&args[1], std::slice::from_ref(h))?) != 0 {
                sites.push(json!(poi));
            }
        }
        out_professions.insert(
            id.clone(),
            json!({
                "name": text(&args[0]),
                "job_sites": sites,
                "work_sound": describe(&mut cx, &args[5]),
            }),
        );
    }
    let out = json!({
        "mod": path.rsplit('/').next(),
        "professions": out_professions,
        "poi_types": pois,
        "trades": trades,
        "pools": pools,
        "configs": *loader.configs.borrow(),
        "not_modelled": *loader.unknown.borrow(),
    });
    let text = serde_json::to_string_pretty(&out)?;
    match args.get(1) {
        Some(file) => std::fs::write(file, text)?,
        None => println!("{text}"),
    }
    Ok(())
}
