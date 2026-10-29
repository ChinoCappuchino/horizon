//! Running a compiled jar (docs/java-compat/README.md section 6). A [`Host`]
//! answers every import: it is the compatibility layer, or in tests a
//! recording stand-in. The host gives its own objects as [`HostObject`]s
//! and gets the jar's objects as [`Value::Mod`], which it can call back
//! through a [`Context`].
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
    /// An object of the jar: one of its classes, or a lambda.
    Mod(OwnedRooted<AnyRef>),
}

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
    compiled: Rc<Compiled>,
    instance: Option<Instance>,
}

/// Access to the running jar, from the host or from outside.
pub struct Context<'a> {
    cx: StoreContextMut<'a, State>,
}

impl Context<'_> {
    pub fn compiled(&self) -> Rc<Compiled> {
        self.cx.data().compiled.clone()
    }

    pub fn constant(&self, index: i32) -> Option<HostConstant> {
        let compiled = &self.cx.data().compiled;
        usize::try_from(index)
            .ok()
            .and_then(|i| compiled.constants.get(i).cloned())
    }

    /// Calls an export of the jar.
    pub fn call(&mut self, export: &str, args: &[Value]) -> Result<Value> {
        let Some(instance) = self.cx.data().instance else {
            bail!("the jar is not running");
        };
        let Some(func) = instance.get_func(&mut self.cx, export) else {
            bail!("no export {export}");
        };
        let ty = func.ty(&self.cx);
        let mut scope = RootScope::new(&mut self.cx);
        let mut params = Vec::new();
        for (v, t) in args.iter().zip(ty.params()) {
            params.push(to_val(&mut scope, v, &t)?);
        }
        if params.len() != ty.params().len() {
            bail!("{export} takes {} arguments", ty.params().len());
        }
        let mut results: Vec<Val> = ty.results().map(|t| t.default_value().unwrap()).collect();
        func.call(&mut scope, &params, &mut results)?;
        match results.first() {
            Some(v) => from_val(&mut scope, v),
            None => Ok(Value::Void),
        }
    }

    /// The id of an object of the jar (see [`Compiled::object`]).
    pub fn id(&mut self, v: &Value) -> Result<Option<i32>> {
        let Value::Mod(r) = v else { return Ok(None) };
        let mut scope = RootScope::new(&mut self.cx);
        let r = r.to_rooted(&mut scope);
        let s = r.unwrap_struct(&scope)?;
        Ok(s.field(&mut scope, 0)?.i32())
    }

    /// The class of an object of the jar, if it is one of a class.
    pub fn class(&mut self, v: &Value) -> Result<Option<String>> {
        let Some(id) = self.id(v)? else {
            return Ok(None);
        };
        Ok(match self.compiled().object(id) {
            Some(Object::Class(c)) => Some(c.to_owned()),
            _ => None,
        })
    }

    /// Calls a lambda of the jar: its interface method, with `args`.
    pub fn call_lambda(&mut self, lambda: &Value, args: &[Value]) -> Result<Value> {
        let Some(id) = self.id(lambda)? else {
            bail!("not an object of the jar");
        };
        let n = match self.compiled().object(id) {
            Some(Object::Lambda(n, _)) => n,
            _ => bail!("not a lambda"),
        };
        let mut all = vec![lambda.clone()];
        all.extend_from_slice(args);
        self.call(&format!("lambda {n}"), &all)
    }

    /// Java's `==` on references.
    pub fn same(&mut self, a: &Value, b: &Value) -> Result<bool> {
        Ok(match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Host(a), Value::Host(b)) => Arc::ptr_eq(a, b),
            (Value::Mod(a), Value::Mod(b)) => OwnedRooted::ref_eq(&self.cx, a, b)?,
            _ => false,
        })
    }
}

fn to_val(mut cx: impl AsContextMut<Data = State>, v: &Value, ty: &ValType) -> Result<Val> {
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
        Value::Mod(r) => Val::AnyRef(Some(r.to_rooted(&mut cx))),
    })
}

fn from_val(mut cx: impl AsContextMut<Data = State>, v: &Val) -> Result<Value> {
    Ok(match v {
        Val::I32(i) => Value::Int(*i),
        Val::I64(i) => Value::Long(*i),
        Val::F32(f) => Value::Float(f32::from_bits(*f)),
        Val::F64(f) => Value::Double(f64::from_bits(*f)),
        Val::AnyRef(None) => Value::Null,
        Val::AnyRef(Some(a)) => {
            if a.is_struct(&cx)? {
                Value::Mod(a.to_owned_rooted(&mut cx)?)
            } else {
                let e = ExternRef::convert_any(&mut cx, *a)?;
                match e.data(&cx)?.and_then(|d| d.downcast_ref::<HostObject>()) {
                    Some(o) => Value::Host(o.clone()),
                    None => bail!("a reference that is neither the host's nor the jar's"),
                }
            }
        }
        _ => bail!("a value of an unexpected type"),
    })
}

/// A compiled jar, running.
pub struct Runtime {
    store: Store<State>,
}

impl Runtime {
    pub fn new(compiled: Compiled, host: Rc<dyn Host>) -> Result<Runtime> {
        let mut config = Config::new();
        config
            .wasm_gc(true)
            .wasm_function_references(true)
            .wasm_exceptions(true);
        let engine = Engine::new(&config)?;
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
                    };
                    let mut args = Vec::with_capacity(params.len());
                    for p in params {
                        args.push(from_val(&mut cx.cx, p)?);
                    }
                    let r = host.call(&mut cx, &parsed, &args)?;
                    if let Some(slot) = results.first_mut() {
                        let t = ty.results().next().unwrap();
                        *slot = to_val(&mut cx.cx, &r, &t)?;
                    }
                    Ok(())
                },
            )?;
        }
        let state = State {
            host,
            compiled: Rc::new(compiled),
            instance: None,
        };
        let mut store = Store::new(&engine, state);
        let instance = linker.instantiate(&mut store, &module)?;
        store.data_mut().instance = Some(instance);
        Ok(Runtime { store })
    }

    pub fn context(&mut self) -> Context<'_> {
        Context {
            cx: self.store.as_context_mut(),
        }
    }

    pub fn compiled(&self) -> Rc<Compiled> {
        self.store.data().compiled.clone()
    }
}
