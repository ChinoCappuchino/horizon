//! Running compiled jars (docs/java-compat/README.md section 6). A [`Host`]
//! answers every import: it is the compatibility layer, or in tests a
//! recording stand-in. The host gives its own objects as [`HostObject`]s
//! and gets the jars' objects as [`Value::Mod`], which it can call back
//! through a [`Context`].
//!
//! Several jars can run together (a mod and its addons), each its own
//! module in one store. An object of one jar that goes into another goes
//! as a host object there: a jar only ever sees its own structs as its
//! own objects.
use crate::compile::{Compiled, HostConstant, Object};
use std::{any::Any, rc::Rc, sync::Arc};
use wasmtime::{
    AnyRef, AsContextMut, Config, Engine, ExternRef, ExternType, Instance, Linker, Module,
    OwnedRooted, Result, RootScope, Store, StoreContextMut, Val, ValType, bail,
};

/// An object of the host: anything it likes, shared.
pub type HostObject = Arc<dyn Any + Send + Sync>;

#[derive(Clone)]
pub enum Value {
    /// What a method that returns nothing gives.
    Void,
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Null,
    Host(HostObject),
    /// An object of a jar: one of its classes, or a lambda.
    Mod(ModRef),
}

/// An object of a jar, and which jar.
#[derive(Clone)]
pub struct ModRef {
    pub jar: usize,
    pub r: OwnedRooted<AnyRef>,
}

/// An object of another jar, as a jar holds it.
struct Foreign(ModRef);

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Void => write!(f, "void"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Long(v) => write!(f, "{v}L"),
            Value::Float(v) => write!(f, "{v}f"),
            Value::Double(v) => write!(f, "{v}d"),
            Value::Null => write!(f, "null"),
            Value::Host(_) => write!(f, "<host>"),
            Value::Mod(_) => write!(f, "<mod>"),
        }
    }
}

/// An import, by its name (see [`crate::compile`]): `same` and `constant`
/// have only an operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub op: String,
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

impl Import {
    pub fn parse(name: &str) -> Import {
        let (op, rest) = name.split_once(' ').unwrap_or((name, ""));
        // `concat <descriptor> <recipe>`: the recipe (as `name`) may have
        // spaces. `instanceof <class>`: the class is the owner.
        if op == "concat" {
            let (descriptor, recipe) = rest.split_once(' ').unwrap_or((rest, ""));
            return Import {
                op: op.to_owned(),
                owner: String::new(),
                name: recipe.to_owned(),
                descriptor: descriptor.to_owned(),
            };
        }
        if op == "instanceof" {
            return Import {
                op: op.to_owned(),
                owner: rest.to_owned(),
                name: String::new(),
                descriptor: String::new(),
            };
        }
        let (member, descriptor) = rest.rsplit_once(' ').unwrap_or((rest, ""));
        let (owner, name) = member.rsplit_once('.').unwrap_or(("", member));
        Import {
            op: op.to_owned(),
            owner: owner.to_owned(),
            name: name.to_owned(),
            descriptor: descriptor.to_owned(),
        }
    }
}

impl std::fmt::Display for Import {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.owner.is_empty() {
            write!(f, "{}", self.op)
        } else {
            write!(
                f,
                "{} {}.{} {}",
                self.op, self.owner, self.name, self.descriptor
            )
        }
    }
}

/// The compatibility layer, as the jar sees it.
pub trait Host: 'static {
    /// Answers an import. A host may call back into the jar through `cx`.
    fn call(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value>;
}

pub struct State {
    host: Rc<dyn Host>,
    jars: Vec<(Rc<Compiled>, Option<Instance>)>,
}

/// Access to the running jars, from the host or from outside: calls go to
/// the current jar (the one whose import the host answers).
pub struct Context<'a> {
    cx: StoreContextMut<'a, State>,
    jar: usize,
}

impl Context<'_> {
    /// The current jar.
    pub fn jar(&self) -> usize {
        self.jar
    }

    /// Runs `f` with another jar current.
    pub fn in_jar<R>(&mut self, jar: usize, f: impl FnOnce(&mut Self) -> R) -> R {
        let before = std::mem::replace(&mut self.jar, jar);
        let r = f(self);
        self.jar = before;
        r
    }

    pub fn jars(&self) -> usize {
        self.cx.data().jars.len()
    }

    pub fn compiled(&self) -> Rc<Compiled> {
        self.cx.data().jars[self.jar].0.clone()
    }

    pub fn compiled_of(&self, jar: usize) -> Option<Rc<Compiled>> {
        self.cx.data().jars.get(jar).map(|j| j.0.clone())
    }

    /// The jar a class is in, if one of them has it.
    pub fn jar_of_class(&self, class: &str) -> Option<usize> {
        self.cx
            .data()
            .jars
            .iter()
            .position(|(c, _)| c.classes.iter().any(|n| n == class))
    }

    pub fn constant(&self, index: i32) -> Option<HostConstant> {
        let compiled = &self.cx.data().jars[self.jar].0;
        usize::try_from(index)
            .ok()
            .and_then(|i| compiled.constants.get(i).cloned())
    }

    /// Whether the current jar has an export.
    pub fn has(&mut self, export: &str) -> bool {
        match self.cx.data().jars[self.jar].1 {
            Some(instance) => instance.get_func(&mut self.cx, export).is_some(),
            None => false,
        }
    }

    /// Calls an export of the current jar.
    pub fn call(&mut self, export: &str, args: &[Value]) -> Result<Value> {
        let jar = self.jar;
        let Some(instance) = self.cx.data().jars[jar].1 else {
            bail!("the jar is not running");
        };
        let Some(func) = instance.get_func(&mut self.cx, export) else {
            bail!("no export {export}");
        };
        let ty = func.ty(&self.cx);
        let mut scope = RootScope::new(&mut self.cx);
        let mut params = Vec::new();
        for (v, t) in args.iter().zip(ty.params()) {
            params.push(to_val(&mut scope, v, &t, jar)?);
        }
        if params.len() != ty.params().len() {
            bail!("{export} takes {} arguments", ty.params().len());
        }
        let mut results: Vec<Val> = ty.results().map(|t| t.default_value().unwrap()).collect();
        func.call(&mut scope, &params, &mut results)?;
        match results.first() {
            Some(v) => from_val(&mut scope, v, jar),
            None => Ok(Value::Void),
        }
    }

    /// The id of an object of a jar (see [`Compiled::object`]).
    pub fn id(&mut self, v: &Value) -> Result<Option<i32>> {
        let Value::Mod(m) = v else { return Ok(None) };
        let mut scope = RootScope::new(&mut self.cx);
        let r = m.r.to_rooted(&mut scope);
        let Some(s) = r.as_struct(&scope)? else {
            return Ok(None);
        };
        Ok(s.field(&mut scope, 0)?.i32())
    }

    /// The class of an object of a jar, if it is one of a class.
    pub fn class(&mut self, v: &Value) -> Result<Option<String>> {
        let Value::Mod(m) = v else { return Ok(None) };
        let jar = m.jar;
        let Some(id) = self.id(v)? else {
            return Ok(None);
        };
        Ok(self.compiled_of(jar).and_then(|c| match c.object(id) {
            Some(Object::Class(c)) => Some(c.to_owned()),
            _ => None,
        }))
    }

    /// Calls a lambda of a jar: its interface method, with `args`.
    pub fn call_lambda(&mut self, lambda: &Value, args: &[Value]) -> Result<Value> {
        let Value::Mod(m) = lambda else {
            bail!("not an object of a jar");
        };
        let jar = m.jar;
        let Some(id) = self.id(lambda)? else {
            bail!("not an object of a jar");
        };
        let n = match self.compiled_of(jar).and_then(|c| match c.object(id) {
            Some(Object::Lambda(n, _)) => Some(n),
            _ => None,
        }) {
            Some(n) => n,
            None => bail!("not a lambda"),
        };
        let mut all = vec![lambda.clone()];
        all.extend_from_slice(args);
        self.in_jar(jar, |cx| cx.call(&format!("lambda {n}"), &all))
    }

    /// The peer of an object of the jar: the game object that stands for
    /// it (null if its class does not extend a game class).
    pub fn peer(&mut self, v: &Value) -> Result<Value> {
        self.field(v, 1)
    }

    /// Field `index` of an object of the jar (0 the class id, 1 the peer,
    /// then [`crate::compile::ClassInfo::fields`]).
    pub fn field(&mut self, v: &Value, index: usize) -> Result<Value> {
        let Value::Mod(m) = v else {
            bail!("not an object of a jar")
        };
        let jar = m.jar;
        let mut scope = RootScope::new(&mut self.cx);
        let r = m.r.to_rooted(&mut scope);
        let s = r.unwrap_struct(&scope)?;
        let f = s.field(&mut scope, index)?;
        from_val(&mut scope, &f, jar)
    }

    /// Calls the method `name desc` of an object of the jar, through its
    /// dispatcher; None if no class of the jar has such a method.
    pub fn call_virtual(
        &mut self,
        object: &Value,
        name: &str,
        desc: &str,
        args: &[Value],
    ) -> Result<Option<Value>> {
        let export = format!("virtual {name} {desc}");
        let Value::Mod(m) = object else {
            return Ok(None);
        };
        let jar = m.jar;
        self.in_jar(jar, |cx| {
            if !cx.has(&export) {
                return Ok(None);
            }
            let mut all = vec![object.clone()];
            all.extend_from_slice(args);
            cx.call(&export, &all).map(Some)
        })
    }

    /// The elements of an array (of the jar or made by the host).
    pub fn array(&mut self, v: &Value) -> Result<Option<Vec<Value>>> {
        let Value::Mod(m) = v else { return Ok(None) };
        let jar = m.jar;
        let mut scope = RootScope::new(&mut self.cx);
        let r = m.r.to_rooted(&mut scope);
        let Some(a) = r.as_array(&scope)? else {
            return Ok(None);
        };
        let mut out = Vec::new();
        for i in 0..a.len(&scope)? {
            let e = a.get(&mut scope, i)?;
            out.push(from_val(&mut scope, &e, jar)?);
        }
        Ok(Some(out))
    }

    /// A new array for the jar, of a component descriptor (`I`, `B`,
    /// `Ljava/lang/String;`...).
    pub fn new_array(&mut self, component: &str, values: &[Value]) -> Result<Value> {
        use wasmtime::{ArrayRef, ArrayRefPre, ArrayType, FieldType, Mutability, StorageType};
        let (storage, ty) = match component.as_bytes().first() {
            Some(b'Z' | b'B') => (StorageType::I8, ValType::I32),
            Some(b'C' | b'S') => (StorageType::I16, ValType::I32),
            Some(b'I') => (StorageType::ValType(ValType::I32), ValType::I32),
            Some(b'J') => (StorageType::ValType(ValType::I64), ValType::I64),
            Some(b'F') => (StorageType::ValType(ValType::F32), ValType::F32),
            Some(b'D') => (StorageType::ValType(ValType::F64), ValType::F64),
            _ => (StorageType::ValType(ValType::ANYREF), ValType::ANYREF),
        };
        let engine = self.cx.engine().clone();
        let array_ty = ArrayType::new(&engine, FieldType::new(Mutability::Var, storage));
        let mut scope = RootScope::new(&mut self.cx);
        let pre = ArrayRefPre::new(&mut scope, array_ty);
        let jar = self.jar;
        let mut elems = Vec::new();
        for v in values {
            elems.push(to_val(&mut scope, v, &ty, jar)?);
        }
        let a = ArrayRef::new_fixed(&mut scope, &pre, &elems)?;
        Ok(Value::Mod(ModRef {
            jar,
            r: a.to_anyref().to_owned_rooted(&mut scope)?,
        }))
    }

    /// Java's `==` on references.
    pub fn same(&mut self, a: &Value, b: &Value) -> Result<bool> {
        Ok(match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Host(a), Value::Host(b)) => Arc::ptr_eq(a, b),
            (Value::Mod(a), Value::Mod(b)) => OwnedRooted::ref_eq(&self.cx, &a.r, &b.r)?,
            _ => false,
        })
    }
}

/// A value as jar `jar` takes it: another jar's object as a host object.
fn to_val(
    mut cx: impl AsContextMut<Data = State>,
    v: &Value,
    ty: &ValType,
    jar: usize,
) -> Result<Val> {
    Ok(match v {
        Value::Void => ty.default_value().unwrap(),
        Value::Int(i) => Val::I32(*i),
        Value::Long(i) => Val::I64(*i),
        Value::Float(f) => Val::F32(f.to_bits()),
        Value::Double(f) => Val::F64(f.to_bits()),
        Value::Null => Val::AnyRef(None),
        Value::Host(o) => {
            let e = ExternRef::new(&mut cx, o.clone())?;
            Val::AnyRef(Some(AnyRef::convert_extern(&mut cx, e)?))
        }
        Value::Mod(m) if m.jar == jar => Val::AnyRef(Some(m.r.to_rooted(&mut cx))),
        Value::Mod(m) => {
            let o: HostObject = Arc::new(Foreign(m.clone()));
            let e = ExternRef::new(&mut cx, o)?;
            Val::AnyRef(Some(AnyRef::convert_extern(&mut cx, e)?))
        }
    })
}

/// A value jar `jar` gives: its structs and arrays are its objects.
fn from_val(mut cx: impl AsContextMut<Data = State>, v: &Val, jar: usize) -> Result<Value> {
    Ok(match v {
        Val::I32(i) => Value::Int(*i),
        Val::I64(i) => Value::Long(*i),
        Val::F32(f) => Value::Float(f32::from_bits(*f)),
        Val::F64(f) => Value::Double(f64::from_bits(*f)),
        Val::AnyRef(None) => Value::Null,
        Val::AnyRef(Some(a)) => {
            if a.is_struct(&cx)? || a.is_array(&cx)? {
                Value::Mod(ModRef {
                    jar,
                    r: a.to_owned_rooted(&mut cx)?,
                })
            } else {
                let e = ExternRef::convert_any(&mut cx, *a)?;
                match e.data(&cx)?.and_then(|d| d.downcast_ref::<HostObject>()) {
                    Some(o) => match o.downcast_ref::<Foreign>() {
                        Some(f) => Value::Mod(f.0.clone()),
                        None => Value::Host(o.clone()),
                    },
                    None => bail!("a reference that is neither the host's nor a jar's"),
                }
            }
        }
        _ => bail!("a value of an unexpected type"),
    })
}

/// Compiled jars, running.
/// The engine compiled jars run in: WASM GC with exceptions, the copying
/// collector, inlining (env overrides to compare: see below).
pub fn engine() -> Result<Engine> {
    let mut config = Config::new();
    config
        .wasm_gc(true)
        .wasm_function_references(true)
        .wasm_exceptions(true);
    // A perf map (/tmp/perf-<pid>.map) names the compiled methods for a
    // profiler (perf, samply).
    if std::env::var_os("HORIZON_JAVA_PERFMAP").is_some() {
        config.profiler(wasmtime::ProfilingStrategy::PerfMap);
    }
    // The copying collector allocates by bumping a pointer: 3.5 times
    // the reference-counting one on allocation (SimplexNoise bench). The
    // other stays to compare: HORIZON_JAVA_COLLECTOR=drc.
    config.collector(match std::env::var("HORIZON_JAVA_COLLECTOR").as_deref() {
        Ok("drc") => wasmtime::Collector::DeferredReferenceCounting,
        _ => wasmtime::Collector::Copying,
    });
    // Small methods inlined into their callers (static calls 3 times
    // faster); HORIZON_JAVA_INLINE=0 to compare.
    if std::env::var("HORIZON_JAVA_INLINE").as_deref() != Ok("0") {
        config.compiler_inlining(wasmtime::Inlining::Yes);
    }
    Engine::new(&config)
}

/// A compiled jar as native code for this machine (an ELF file), ready to
/// load without compiling: the install-time cache (docs/java-compat/
/// compiler.md, step 8), and a way to read the machine code (`objdump`).
pub fn precompile(compiled: &Compiled) -> Result<Vec<u8>> {
    engine()?.precompile_module(&compiled.wasm)
}

pub struct Runtime {
    store: Store<State>,
}

impl Runtime {
    /// One jar.
    pub fn new(compiled: Compiled, host: Rc<dyn Host>) -> Result<Runtime> {
        Self::link(vec![compiled], host)
    }

    /// Jars that run together (a mod and the mods it needs), as jars 0, 1...
    pub fn link(jars: Vec<Compiled>, host: Rc<dyn Host>) -> Result<Runtime> {
        let engine = engine()?;
        let state = State {
            host,
            jars: Vec::new(),
        };
        let mut store = Store::new(&engine, state);
        for (jar, compiled) in jars.into_iter().enumerate() {
            let module = Module::new(&engine, &compiled.wasm)?;
            let mut linker: Linker<State> = Linker::new(&engine);
            for import in module.imports() {
                let ExternType::Func(ty) = import.ty() else {
                    bail!("the jar imports a non-function");
                };
                let parsed = Arc::new(Import::parse(import.name()));
                linker.func_new(
                    import.module(),
                    import.name(),
                    ty.clone(),
                    move |mut caller, params, results| {
                        let host = caller.data().host.clone();
                        let mut cx = Context {
                            cx: caller.as_context_mut(),
                            jar,
                        };
                        let mut args = Vec::with_capacity(params.len());
                        for p in params {
                            args.push(from_val(&mut cx.cx, p, jar)?);
                        }
                        let r = host.call(&mut cx, &parsed, &args)?;
                        if let Some(slot) = results.first_mut() {
                            let t = ty.results().next().unwrap();
                            *slot = to_val(&mut cx.cx, &r, &t, jar)?;
                        }
                        Ok(())
                    },
                )?;
            }
            store.data_mut().jars.push((Rc::new(compiled), None));
            let instance = linker.instantiate(&mut store, &module)?;
            store.data_mut().jars[jar].1 = Some(instance);
        }
        Ok(Runtime { store })
    }

    /// Jar 0's context.
    pub fn context(&mut self) -> Context<'_> {
        self.context_of(0)
    }

    pub fn context_of(&mut self, jar: usize) -> Context<'_> {
        Context {
            cx: self.store.as_context_mut(),
            jar,
        }
    }

    pub fn compiled(&self) -> Rc<Compiled> {
        self.store.data().jars[0].0.clone()
    }
}
