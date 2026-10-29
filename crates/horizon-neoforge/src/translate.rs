//! NeoForge mods' content, from their own jars: the layer runs the mods'
//! constructors and registers as NeoForge would, asks each block for its
//! states, shapes and light, fires the events that make content when the
//! game loads (config, setup, villager trades, server start), and writes it
//! all as Horizon content mods (docs/mods.md): block and item declarations,
//! POI types and professions, trades, and template pool additions.
use crate::{game::GameData, host::NeoForge, objects::*};
use horizon_java::{
    ClassFile,
    classfile::ElementValue,
    compile::{Object, compile},
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

/// What a mod registered, as content.
#[derive(Default)]
pub struct Translation {
    pub mod_id: String,
    /// Block id and its `data/<ns>/horizon/block/<path>.json`.
    pub blocks: Vec<(String, Json)>,
    pub items: Vec<(String, Json)>,
    /// Other data files, by path in the content mod (`data/...`).
    pub files: Vec<(String, Json)>,
    /// Calls the layer does not model, and how often.
    pub unknown: BTreeMap<String, usize>,
    /// Entries that failed to register, with why.
    pub errors: Vec<String>,
    /// Methods the compiler did not compile.
    pub skipped: usize,
}

const SHAPE: &str = "(Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/BlockGetter;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/phys/shapes/CollisionContext;)Lnet/minecraft/world/phys/shapes/VoxelShape;";

const GAME_PROFESSIONS: [&str; 15] = [
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

/// Translates a mod's classes against the game data of a prepared root.
pub fn translate(classes: &[ClassFile], game: Arc<GameData>) -> Result<Translation> {
    let mut all = translate_all(&[classes.to_vec()], game)?;
    Ok(all.remove(0))
}

/// A static method that subscribes to an event, or an object's.
struct Subscriber {
    jar: usize,
    class: String,
    method: String,
    desc: String,
    is_static: bool,
}

/// Mods running: constructed and registered as NeoForge does, kept for
/// what comes after (writing their content, running their blocks' code).
pub struct Session {
    pub rt: Runtime,
    pub host: Rc<NeoForge>,
    pub jars: Vec<Vec<ClassFile>>,
    /// Each jar's mod id (from `@Mod`).
    pub mod_ids: Vec<String>,
    /// Methods each jar's compiler did not compile.
    pub skipped: Vec<usize>,
    subscribers: Vec<Subscriber>,
    /// What the registers hold: blocks and items by id, POI types (their
    /// holders), professions by id.
    pub blocks: Vec<(String, Value)>,
    pub items: Vec<(String, Value)>,
    pois: Vec<(Arc<Holder>, Value)>,
    professions: Vec<(String, Value)>,
    pub errors: Vec<String>,
}

impl Session {
    /// Compiles and links the jars (a mod after the mods it needs),
    /// constructs their `@Mod` classes and binds their registers: blocks,
    /// then items, then the rest.
    pub fn start(jars: Vec<Vec<ClassFile>>, game: Arc<GameData>) -> Result<Session> {
        let mut compiled = Vec::new();
        let mut skipped = Vec::new();
        for classes in &jars {
            let c = compile(classes).map_err(wasmtime::Error::msg)?;
            skipped.push(c.skipped.len());
            compiled.push(c);
        }
        let host = Rc::new(NeoForge::new(game));
        let mut rt = Runtime::link(compiled, host.clone())?;
        let mut cx = rt.context();
        let mod_bus = obj(Obj::Bus("mod".into()));
        let mut errors: Vec<String> = Vec::new();
        let mut mod_ids = vec![String::new(); jars.len()];

        // The @Mod classes that run on a server, constructed; the event
        // subscribers' static methods.
        let mut subscribers = Vec::new();
        for (jar, classes) in jars.iter().enumerate() {
            for class in classes {
                let name = class.name()?.to_owned();
                for a in class.annotations() {
                    match a.type_descriptor.as_str() {
                        "Lnet/neoforged/fml/common/EventBusSubscriber;" => {
                            subscribers.extend(subscribed(jar, class, true));
                        }
                        "Lnet/neoforged/fml/common/Mod;" => {
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
                                    mod_ids[jar] = id.to_owned();
                                }
                            }
                            let Some(init) = class.methods.iter().find(|m| m.name == "<init>")
                            else {
                                continue;
                            };
                            let made = cx.in_jar(jar, |cx| -> Result<()> {
                                let object = cx.call(&format!("new {name}"), &[])?;
                                let mut args = vec![object];
                                for p in Method::parse(&init.descriptor)
                                    .map(|m| m.params)
                                    .unwrap_or_default()
                                {
                                    args.push(match p.as_str() {
                                        "Lnet/neoforged/bus/api/IEventBus;" => mod_bus.clone(),
                                        _ => opaque(p),
                                    });
                                }
                                cx.call(&format!("{name}.<init>{}", init.descriptor), &args)?;
                                Ok(())
                            });
                            if let Err(e) = made {
                                errors.push(format!("{name}: constructor: {e:#}"));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // RegisterEvent: blocks, then items, then the rest.
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
        let mut pois = Vec::new();
        let mut professions = Vec::new();
        for r in sorted {
            let entries = r.entries.lock().unwrap().clone();
            for h in entries {
                match host.bind(&mut cx, &h) {
                    Ok(v) => match r.registry.as_str() {
                        "block" => blocks.push((h.id.clone(), v)),
                        "item" => items.push((h.id.clone(), v)),
                        "point_of_interest_type" => pois.push((h.clone(), v)),
                        "villager_profession" => professions.push((h.id.clone(), v)),
                        _ => {}
                    },
                    Err(e) => errors.push(format!("{}: {e:#}", h.id)),
                }
            }
        }
        Ok(Session {
            rt,
            host,
            jars,
            mod_ids,
            skipped,
            subscribers,
            blocks,
            items,
            pois,
            professions,
            errors,
        })
    }
}

/// Translates mods that run together (a mod after the mods it needs), one
/// translation each, in order.
pub fn translate_all(jars: &[Vec<ClassFile>], game: Arc<GameData>) -> Result<Vec<Translation>> {
    let session = Session::start(jars.to_vec(), game.clone())?;
    translate_session(session, game)
}

/// What a running session's mods make, as content.
pub fn translate_session(mut session: Session, game: Arc<GameData>) -> Result<Vec<Translation>> {
    let host = session.host.clone();
    let jars = session.jars.clone();
    let subscribers = std::mem::take(&mut session.subscribers);
    let blocks = std::mem::take(&mut session.blocks);
    let items = std::mem::take(&mut session.items);
    let pois = std::mem::take(&mut session.pois);
    let professions = std::mem::take(&mut session.professions);
    let mut errors = std::mem::take(&mut session.errors);
    let mut out: Vec<Translation> = session
        .mod_ids
        .iter()
        .zip(&session.skipped)
        .map(|(id, skipped)| Translation {
            mod_id: id.clone(),
            skipped: *skipped,
            ..Translation::default()
        })
        .collect();
    let mut cx = session.rt.context();
    let jars = &jars;

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
    let by_ns = |id: &str| -> usize {
        let ns = id.split_once(':').map_or("", |x| x.0);
        out.iter().position(|t| t.mod_id == ns).unwrap_or(0)
    };
    let mut decls: Vec<(usize, &str, String, Json)> = Vec::new();
    for (id, v) in &blocks {
        let peer = peer_of(&mut cx, v);
        let Some(Obj::Block(b)) = get(&peer) else {
            errors.push(format!("{id}: not a block"));
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
                decls.push((by_ns(id), "block", id.clone(), decl));
            }
            Err(e) => errors.push(format!("{id}: {e:#}")),
        }
    }
    // 4. Items that are not their block's item.
    for (id, v) in &items {
        if item_blocks.get(id) == Some(id) {
            continue;
        }
        let peer = peer_of(&mut cx, v);
        let Some(Obj::Item(i)) = get(&peer) else {
            errors.push(format!("{id}: not an item"));
            continue;
        };
        let mut decl = item(i);
        if let Some(block) = item_blocks.get(id) {
            decl["places"] = json!(block);
        }
        decls.push((by_ns(id), "item", id.clone(), decl));
    }
    for (jar, kind, id, decl) in decls {
        match kind {
            "block" => out[jar].blocks.push((id, decl)),
            _ => out[jar].items.push((id, decl)),
        }
    }

    // 5. The events that make content as the game loads.
    let mut files: Vec<(String, Json)> = Vec::new();
    let fire = |cx: &mut Context<'_>, events: &[&str], event: &Value, errors: &mut Vec<String>| {
        fire_event(&host, cx, jars, &subscribers, events, event, errors)
    };
    let loading = opaque("ModConfigEvent.Loading");
    fire(
        &mut cx,
        &[
            "Lnet/neoforged/fml/event/config/ModConfigEvent$Loading;",
            "Lnet/neoforged/fml/event/config/ModConfigEvent;",
        ],
        &loading,
        &mut errors,
    );
    let setup = opaque("FMLCommonSetupEvent");
    fire(
        &mut cx,
        &["Lnet/neoforged/fml/event/lifecycle/FMLCommonSetupEvent;"],
        &setup,
        &mut errors,
    );

    // POI types and professions, to the game's registries.
    for (h, v) in &pois {
        if let Some(Obj::Poi {
            states,
            tickets,
            range,
        }) = get(v)
        {
            let mut blocks: Vec<String> = Vec::new();
            let list = match get(states) {
                Some(Obj::List(l)) => l.lock().unwrap().clone(),
                _ => Vec::new(),
            };
            for st in list {
                if let Some(Obj::State(d, _)) = get(&st) {
                    let blk = d.block.lock().unwrap().clone().unwrap_or(Value::Null);
                    if let Ok(id) = host.item_id(&mut cx, &blk)
                        && !id.is_empty()
                        && !blocks.contains(&id)
                    {
                        blocks.push(id);
                    }
                }
            }
            let (ns, path) = h.id.split_once(':').unwrap_or(("minecraft", &h.id));
            files.push((
                format!("data/{ns}/horizon/poi_type/{path}.json"),
                json!({"blocks": blocks, "max_tickets": tickets, "valid_range": range}),
            ));
        }
    }
    let mut trade_professions: Vec<(String, Value)> = GAME_PROFESSIONS
        .iter()
        .map(|p| {
            let id = format!("minecraft:{p}");
            (id.clone(), host.game_profession(&id))
        })
        .collect();
    for (id, v) in &professions {
        let Some(Obj::Profession(p)) = get(v) else {
            continue;
        };
        *p.id.lock().unwrap() = Some(id.clone());
        trade_professions.push((id.clone(), v.clone()));
        // Its job site: the POI type its heldJobSite accepts.
        let mut job_site = None;
        for (h, _) in &pois {
            let holder = obj(Obj::Holder(h.clone()));
            if let Ok(r) = host.invoke(&mut cx, &p.held, "test", "(Ljava/lang/Object;)Z", &[holder])
                && int(&r).unwrap_or(0) != 0
            {
                job_site = Some(h.id.clone());
                break;
            }
        }
        let sound = match get(&p.sound) {
            Some(Obj::Sound(s)) => Some(s.clone()),
            Some(Obj::Holder(h)) => Some(h.id.clone()),
            _ => None,
        };
        let requested: Vec<String> = match get(&p.requested) {
            Some(Obj::List(l)) => {
                let l = l.lock().unwrap().clone();
                l.iter()
                    .filter_map(|i| host.item_id(&mut cx, i).ok())
                    .filter(|i| !i.is_empty())
                    .collect()
            }
            _ => Vec::new(),
        };
        let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
        files.push((
            format!("data/{ns}/horizon/villager_profession/{path}.json"),
            json!({"job_site": job_site, "work_sound": sound, "requested_items": requested}),
        ));
    }

    // VillagerTradesEvent for every profession: the offers each level gets.
    for (pid, prof) in &trade_professions {
        let levels: Vec<(i32, Value)> = (1..=5)
            .map(|l| (l, obj(Obj::List(Mutex::new(Vec::new())))))
            .collect();
        let map = obj(Obj::Map(Mutex::new(
            levels
                .iter()
                .map(|(l, v)| (obj(Obj::Boxed(Prim::Int(*l))), v.clone()))
                .collect(),
        )));
        let event = obj(Obj::Event(
            "VillagerTradesEvent".into(),
            vec![prof.clone(), map],
        ));
        fire(
            &mut cx,
            &["Lnet/neoforged/neoforge/event/village/VillagerTradesEvent;"],
            &event,
            &mut errors,
        );
        let (pns, ppath) = pid.split_once(':').unwrap_or(("minecraft", pid));
        for (level, list) in &levels {
            let listings = match get(list) {
                Some(Obj::List(l)) => l.lock().unwrap().clone(),
                _ => Vec::new(),
            };
            if listings.is_empty() {
                continue;
            }
            let mut tag = Vec::new();
            for (n, listing) in listings.iter().enumerate() {
                let made = host.invoke(
                    &mut cx,
                    listing,
                    "getOffer",
                    "(Lnet/minecraft/world/entity/Entity;Lnet/minecraft/util/RandomSource;)Lnet/minecraft/world/item/trading/MerchantOffer;",
                    &[Value::Null, opaque("random")],
                );
                let offer = match made {
                    Ok(v) => match get(&v) {
                        Some(Obj::Offer(o)) => o.clone(),
                        _ => continue,
                    },
                    Err(e) => {
                        errors.push(format!("{pid} level {level} trade {n}: {e:#}"));
                        continue;
                    }
                };
                // A mod's profession's trades are its; one the mod adds to a
                // game profession, in the namespace of the mod whose item it
                // trades.
                let ns = if pns != "minecraft" {
                    pns.to_owned()
                } else {
                    [&offer.gives.0, &offer.wants.0]
                        .iter()
                        .filter_map(|i| i.split_once(':').map(|x| x.0))
                        .find(|n| *n != "minecraft")
                        .unwrap_or(pns)
                        .to_owned()
                };
                let name = format!("{ppath}/{level}/{n}");
                let stack = |(id, count): &(String, i32)| json!({"id": id, "count": count});
                let mut trade = json!({
                    "wants": stack(&offer.wants),
                    "gives": stack(&offer.gives),
                    "max_uses": offer.max_uses,
                    "xp": offer.xp,
                    "reputation_discount": offer.price_multiplier,
                });
                if let Some(b) = &offer.also_wants {
                    trade["additional_wants"] = stack(b);
                }
                files.push((format!("data/{ns}/villager_trade/{name}.json"), trade));
                tag.push(json!(format!("{ns}:{name}")));
            }
            if tag.is_empty() {
                continue;
            }
            // The level's trade tag (the game's professions' own tags gain
            // the mod's trades); a mod's profession gets its trade sets.
            files.push((
                format!("data/{pns}/tags/villager_trade/{ppath}/level_{level}.json"),
                json!({"values": tag}),
            ));
            if pns != "minecraft" {
                files.push((
                    format!("data/{pns}/trade_set/{ppath}/level_{level}.json"),
                    json!({
                        "amount": 2,
                        "random_sequence": format!("{pns}:trade_set/{ppath}/level_{level}"),
                        "trades": format!("#{pns}:{ppath}/level_{level}"),
                    }),
                ));
            }
        }
    }

    // ServerAboutToStartEvent: what mods add to template pools.
    let start = obj(Obj::Event("ServerAboutToStartEvent".into(), Vec::new()));
    fire(
        &mut cx,
        &["Lnet/neoforged/neoforge/event/server/ServerAboutToStartEvent;"],
        &start,
        &mut errors,
    );
    for (pool, templates) in host.pools() {
        let mut weights: BTreeMap<(String, String, bool), i64> = BTreeMap::new();
        if let Some(Obj::List(l)) = get(&templates) {
            for e in l.lock().unwrap().iter() {
                if let Some(Obj::PoolElement(loc, proc_, legacy)) = get(e) {
                    *weights
                        .entry((loc.clone(), proc_.clone(), *legacy))
                        .or_default() += 1;
                }
            }
        }
        let raw = host.state.borrow().raw_pools.get(&pool).cloned();
        if let Some(Obj::List(l)) = raw.as_ref().and_then(get) {
            for p in l.lock().unwrap().iter() {
                if let Some(Obj::Pair(e, w)) = get(p)
                    && let Some(Obj::PoolElement(loc, proc_, legacy)) = get(e)
                {
                    weights
                        .entry((loc.clone(), proc_.clone(), *legacy))
                        .or_insert(int(w).unwrap_or(1));
                }
            }
        }
        for ((loc, proc_, legacy), weight) in weights {
            let (ns, path) = loc.split_once(':').unwrap_or(("minecraft", &loc));
            let file = format!(
                "data/{ns}/worldgen/template_pool/horizon_add_to/{}/{}.json",
                pool.replace(':', "/"),
                path
            );
            files.push((
                file,
                json!({
                    "horizon:add_to": pool,
                    "elements": [{
                        "weight": weight,
                        "element": {
                            "element_type": if legacy { "minecraft:legacy_single_pool_element" } else { "minecraft:single_pool_element" },
                            "location": loc,
                            "processors": proc_,
                            "projection": "rigid",
                        }
                    }],
                }),
            ));
        }
    }
    // Each file to the mod whose namespace it is in.
    for (path, json) in files {
        let ns = path.split('/').nth(1).unwrap_or("");
        let jar = out.iter().position(|t| t.mod_id == ns).unwrap_or(0);
        out[jar].files.push((path, json));
    }
    let unknown = host.state.borrow().unknown.clone();
    for t in &mut out {
        t.unknown = unknown.clone();
    }
    out[0].errors = errors;
    Ok(out)
}

/// The methods of a class that subscribe to events (`@SubscribeEvent`).
fn subscribed(jar: usize, class: &ClassFile, statics: bool) -> Vec<Subscriber> {
    let Ok(name) = class.name() else {
        return Vec::new();
    };
    class
        .methods
        .iter()
        .filter(|m| (m.access & 0x0008 != 0) == statics)
        .filter(|m| {
            m.annotations()
                .any(|a| a.type_descriptor == "Lnet/neoforged/bus/api/SubscribeEvent;")
        })
        .map(|m| Subscriber {
            jar,
            class: name.to_owned(),
            method: m.name.clone(),
            desc: m.descriptor.clone(),
            is_static: statics,
        })
        .collect()
}

/// Fires an event: to the subscribers whose parameter is one of `events`
/// (the event's class, then its superclasses), the objects and classes
/// given to a bus, and the listeners whose lambda takes it.
fn fire_event(
    host: &NeoForge,
    cx: &mut Context<'_>,
    jars: &[Vec<ClassFile>],
    statics: &[Subscriber],
    events: &[&str],
    event: &Value,
    errors: &mut Vec<String>,
) {
    let takes = |desc: &str| {
        Method::parse(desc)
            .and_then(|m| m.params.first().cloned())
            .is_some_and(|p| events.contains(&p.as_str()))
    };
    let mut calls: Vec<(usize, String, Vec<Value>)> = Vec::new();
    for s in statics {
        if takes(&s.desc) {
            calls.push((
                s.jar,
                format!("{}.{}{}", s.class, s.method, s.desc),
                vec![event.clone()],
            ));
        }
    }
    let registered = host.state.borrow().registered.clone();
    for (_, target) in &registered {
        match (get(target), target) {
            (Some(Obj::Class(c)), _) => {
                if let Some(jar) = cx.jar_of_class(c)
                    && let Some(class) = jars[jar].iter().find(|k| k.name().is_ok_and(|n| n == c))
                {
                    for s in subscribed(jar, class, true) {
                        if takes(&s.desc) {
                            calls.push((
                                jar,
                                format!("{}.{}{}", s.class, s.method, s.desc),
                                vec![event.clone()],
                            ));
                        }
                    }
                }
            }
            (_, Value::Mod(m)) => {
                let Ok(Some(c)) = cx.class(target) else {
                    continue;
                };
                if let Some(class) = jars[m.jar].iter().find(|k| k.name().is_ok_and(|n| n == c)) {
                    for s in subscribed(m.jar, class, false) {
                        if takes(&s.desc) && !s.is_static {
                            calls.push((
                                m.jar,
                                format!("{}.{}{}", s.class, s.method, s.desc),
                                vec![target.clone(), event.clone()],
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (jar, export, args) in calls {
        if let Err(e) = cx.in_jar(jar, |cx| cx.call(&export, &args)) {
            errors.push(format!("{export}: {e:#}"));
        }
    }
    let listeners = host.state.borrow().listeners.clone();
    for (_, l) in &listeners {
        let Value::Mod(m) = l else { continue };
        let Ok(Some(id)) = cx.id(l) else { continue };
        let target = match cx.compiled_of(m.jar).and_then(|c| match c.object(id) {
            Some(Object::Lambda(_, lambda)) => Some(lambda.target.clone()),
            _ => None,
        }) {
            Some(t) => t,
            None => continue,
        };
        let desc = target.rsplit(' ').next().unwrap_or("");
        let last = Method::parse(desc)
            .and_then(|m| m.params.last().cloned())
            .unwrap_or_default();
        if events.contains(&last.as_str())
            && let Err(e) = cx.call_lambda(l, std::slice::from_ref(event))
        {
            errors.push(format!("listener {target}: {e:#}"));
        }
    }
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
    for (path, json) in &t.files {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, serde_json::to_vec_pretty(json)?)?;
    }
    Ok(())
}
