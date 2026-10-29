//! The native backend (docs/java-compat/compiler.md, step 6): Java bytecode
//! to machine code through Cranelift, with Horizon's own object model, for
//! the speed of the JVM. A prototype: the classes it gets are a closed world
//! (all known); what it does not compile yet (exceptions, strings, lambdas,
//! calls out of the classes it has) traps when it runs.
//!
//! Object model:
//! - an object: an 8-byte header (its class's vtable), then its fields, 8
//!   bytes each, the superclass's first;
//! - an array: its element kind (8 bytes), its length (8 bytes), then the
//!   elements at their natural size;
//! - a vtable: the class id, then a function pointer per virtual method
//!   selector (one selector per name and descriptor in the closed world);
//! - allocation: a pointer bumped in the code, in a zeroed arena (no garbage
//!   collector yet).
use crate::{
    bytecode::{Instruction, Operand},
    cfg::Cfg,
    classfile::{ClassFile, Constant, Member},
    decode,
    descriptor::Method,
    types::{Classes, Frame, Options, Stepper, THIS_SITE, Ty, infer_with, successors},
};
use cranelift_codegen::{
    ir::{
        AbiParam, Block, InstBuilder, MemFlagsData, Signature, TrapCode, Type, Value,
        condcodes::FloatCC, condcodes::IntCC, types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use std::collections::{BTreeMap, HashMap, HashSet};

const STATIC: u16 = 0x0008;
const INTERFACE: u16 = 0x0200;
const ABSTRACT: u16 = 0x0400;
const PRIVATE: u16 = 0x0002;
const OBJECT: &str = "java/lang/Object";
/// The trap of code the prototype does not compile.
const UNSUPPORTED: TrapCode = TrapCode::unwrap_user(1);
const NULL: TrapCode = TrapCode::unwrap_user(2);
const BOUNDS: TrapCode = TrapCode::unwrap_user(3);
const CAST: TrapCode = TrapCode::unwrap_user(4);
const ARENA: TrapCode = TrapCode::unwrap_user(5);

/// A class's layout.
struct Layout {
    /// Instance fields: name → (offset, kind of value).
    fields: HashMap<String, (i32, Type)>,
    size: i32,
    /// Its vtable (filled when the code is ready).
    vtable: *mut u64,
}

/// What the compiled code shares: the arena, the statics, the init flags.
#[repr(C)]
struct Globals {
    /// The next free byte of the arena, and its end.
    bump: u64,
    limit: u64,
}

pub struct Native {
    module: JITModule,
    exports: HashMap<String, FuncId>,
    /// Methods that compile to a trap, with why.
    pub unsupported: Vec<(String, String)>,
    // Memory the code points at, kept alive.
    _arena: Vec<u8>,
    _globals: Box<Globals>,
    _statics: Vec<u64>,
    _vtables: Vec<Vec<u64>>,
    /// Boxed: each one's address is baked into the code.
    #[allow(clippy::vec_box)]
    _singletons: Vec<Box<u64>>,
}

fn kind_type(t: &Ty) -> Option<Type> {
    Some(match t {
        Ty::Int => types::I32,
        Ty::Long => types::I64,
        Ty::Float => types::F32,
        Ty::Double => types::F64,
        Ty::Null | Ty::Ref(_) | Ty::Uninit(_) | Ty::UninitThis | Ty::Site(..) => types::I64,
        Ty::Top => return None,
    })
}

fn desc_type(d: &str) -> Option<Type> {
    kind_type(&Ty::of_descriptor(d)?)
}

/// An array's element size and load type, by its descriptor (`[I`...).
fn element(array: &str) -> (i32, Type) {
    match array.as_bytes().get(1) {
        Some(b'Z' | b'B') => (1, types::I8),
        Some(b'C' | b'S') => (2, types::I16),
        Some(b'I') => (4, types::I32),
        Some(b'F') => (4, types::F32),
        Some(b'J') => (8, types::I64),
        Some(b'D') => (8, types::F64),
        _ => (8, types::I64),
    }
}

fn export_name(owner: &str, name: &str, desc: &str) -> String {
    format!("{owner}.{name}{desc}")
}

extern "C" fn host_sin(x: f64) -> f64 {
    x.sin()
}
extern "C" fn host_cos(x: f64) -> f64 {
    x.cos()
}
extern "C" fn host_fmod(a: f64, b: f64) -> f64 {
    a % b
}
extern "C" fn host_fmodf(a: f32, b: f32) -> f32 {
    a % b
}
/// Whether the class with vtable `vt` is `target` or a subclass: the vtable
/// holds the class id, then the selectors; the ancestor sets are below.
extern "C" fn host_is_instance(obj: u64, target: u32) -> u32 {
    if obj == 0 {
        return 0;
    }
    // SAFETY: obj is an object the compiled code made: its first word is
    // its vtable, whose first word is its class id.
    let id = unsafe { *(*(obj as *const *const u64)) } as u32;
    ANCESTORS.with(|a| {
        a.borrow()
            .get(id as usize)
            .is_some_and(|set| set.contains(&target)) as u32
    })
}

thread_local! {
    /// Each class id's ancestors (itself, superclasses, interfaces), by id.
    static ANCESTORS: std::cell::RefCell<Vec<HashSet<u32>>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

impl Native {
    pub fn compile(classes: &[ClassFile]) -> Result<Native, String> {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
        let isa = cranelift_native::builder()
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
        jb.symbol("host_sin", host_sin as *const u8);
        jb.symbol("host_cos", host_cos as *const u8);
        jb.symbol("host_fmod", host_fmod as *const u8);
        jb.symbol("host_fmodf", host_fmodf as *const u8);
        jb.symbol("host_is_instance", host_is_instance as *const u8);
        let mut module = JITModule::new(jb);
        let ptr = module.target_config().pointer_type();

        let by_name: HashMap<&str, &ClassFile> = classes
            .iter()
            .filter_map(|c| Some((c.name().ok()?, c)))
            .collect();
        let hierarchy = Classes::new(classes);
        let mut names: Vec<&str> = by_name.keys().copied().collect();
        names.sort_unstable();
        let ids: HashMap<&str, u32> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, i as u32 + 1))
            .collect();
        let superclass = |c: &str| -> Option<&str> { by_name.get(c)?.super_name().ok().flatten() };

        let (lambdas, lambda_at, called) = prepass(classes);
        // Virtual selectors: every instance method that dispatches, every
        // method called virtually, every lambda's interface method.
        let mut selectors: BTreeMap<(String, String), usize> = BTreeMap::new();
        for key in called
            .into_iter()
            .chain(lambdas.iter().map(|l| (l.name.clone(), l.sam.clone())))
        {
            let n = selectors.len();
            selectors.entry(key).or_insert(n);
        }
        for c in classes {
            for m in &c.methods {
                if m.access & (STATIC | PRIVATE) == 0 && m.name != "<init>" {
                    let n = selectors.len();
                    selectors
                        .entry((m.name.clone(), m.descriptor.clone()))
                        .or_insert(n);
                }
            }
        }
        // Implementations per selector (for direct calls when only one).
        let mut implementations: HashMap<(String, String), usize> = HashMap::new();
        for c in classes {
            for m in &c.methods {
                if m.access & (STATIC | PRIVATE | ABSTRACT) == 0 && m.code().is_some() {
                    *implementations
                        .entry((m.name.clone(), m.descriptor.clone()))
                        .or_default() += 1;
                }
            }
        }
        for l in &lambdas {
            *implementations
                .entry((l.name.clone(), l.sam.clone()))
                .or_default() += 1;
        }

        // Layouts.
        let mut layouts: HashMap<String, Layout> = HashMap::new();
        let mut vtables: Vec<Vec<u64>> = Vec::new();
        fn layout_of<'a>(
            c: &'a str,
            by_name: &HashMap<&'a str, &'a ClassFile>,
            ids: &HashMap<&str, u32>,
            layouts: &mut HashMap<String, Layout>,
            vtables: &mut Vec<Vec<u64>>,
            selectors: usize,
        ) {
            if layouts.contains_key(c) {
                return;
            }
            let Some(class) = by_name.get(c) else { return };
            let (mut fields, mut size) = (HashMap::new(), 8);
            if let Some(s) = class.super_name().ok().flatten() {
                layout_of(s, by_name, ids, layouts, vtables, selectors);
                if let Some(l) = layouts.get(s) {
                    fields = l.fields.clone();
                    size = l.size;
                }
            }
            for f in &class.fields {
                if f.access & STATIC == 0 {
                    let t = desc_type(&f.descriptor).unwrap_or(types::I64);
                    fields.insert(f.name.clone(), (size, t));
                    size += 8;
                }
            }
            let mut vt = vec![0u64; 1 + selectors];
            vt[0] = u64::from(ids[c]);
            vtables.push(vt);
            let vtable = vtables.last_mut().expect("just pushed").as_mut_ptr();
            layouts.insert(
                c.to_owned(),
                Layout {
                    fields,
                    size,
                    vtable,
                },
            );
        }
        for n in &names {
            layout_of(
                n,
                &by_name,
                &ids,
                &mut layouts,
                &mut vtables,
                selectors.len(),
            );
        }
        // Each lambda site's class: an id after the classes', a vtable, and
        // (when it captures nothing) one object for every use.
        let mut lambda_vt: Vec<i64> = Vec::new();
        let mut singletons: Vec<Box<u64>> = Vec::new();
        let mut singleton_at: Vec<Option<i64>> = Vec::new();
        for (i, l) in lambdas.iter().enumerate() {
            let mut vt = vec![0u64; 1 + selectors.len()];
            vt[0] = (names.len() + 1 + i) as u64;
            vtables.push(vt);
            let p = vtables.last_mut().expect("just pushed").as_mut_ptr() as i64;
            lambda_vt.push(p);
            singleton_at.push(if l.captures.is_empty() {
                singletons.push(Box::new(p as u64));
                Some(&**singletons.last().expect("just pushed") as *const u64 as i64)
            } else {
                None
            });
        }
        // Ancestors, for instanceof and checkcast.
        let mut ancestors: Vec<HashSet<u32>> =
            vec![HashSet::new(); names.len() + 1 + lambdas.len()];
        for (i, l) in lambdas.iter().enumerate() {
            let id = (names.len() + 1 + i) as u32;
            let mut set: HashSet<u32> = ancestors_of(&l.interface, &by_name)
                .iter()
                .filter_map(|c| ids.get(c.as_str()).copied())
                .collect();
            set.insert(id);
            ancestors[id as usize] = set;
        }
        for n in &names {
            let mut set = HashSet::new();
            let mut todo = vec![*n];
            while let Some(c) = todo.pop() {
                if let Some(&id) = ids.get(c)
                    && set.insert(id)
                    && let Some(cf) = by_name.get(c)
                {
                    todo.extend(cf.super_name().ok().flatten());
                    todo.extend(cf.interfaces.iter().filter_map(|&i| cf.class_name(i).ok()));
                }
            }
            ancestors[ids[n] as usize] = set;
        }
        ANCESTORS.with(|a| *a.borrow_mut() = ancestors);

        // Statics: 8 bytes per static field, then an init flag per class.
        let mut statics_at: HashMap<(String, String), (i32, Type)> = HashMap::new();
        let mut n_statics = 0usize;
        for c in classes {
            let owner = c.name().map_err(|e| e.to_string())?;
            for f in &c.fields {
                if f.access & STATIC != 0 {
                    let t = desc_type(&f.descriptor).unwrap_or(types::I64);
                    statics_at.insert(
                        (owner.to_owned(), f.name.clone()),
                        ((n_statics * 8) as i32, t),
                    );
                    n_statics += 1;
                }
            }
        }
        let flags_at = n_statics;
        let mut statics = vec![0u64; n_statics + names.len() + 1];
        let statics_base = statics.as_mut_ptr() as i64;
        // A class needs an init test if it or a superclass has <clinit>.
        let has_clinit = |c: &str| -> bool {
            let mut x = Some(c);
            while let Some(n) = x {
                match by_name.get(n) {
                    Some(cf) => {
                        if cf.methods.iter().any(|m| m.name == "<clinit>") {
                            return true;
                        }
                        x = cf.super_name().ok().flatten();
                    }
                    None => return false,
                }
            }
            false
        };

        // The arena: 4 GiB, zeroed by the OS on first touch.
        let arena_size: usize = 4 << 30;
        let arena = vec![0u8; arena_size];
        let mut globals = Box::new(Globals {
            bump: arena.as_ptr() as u64,
            limit: arena.as_ptr() as u64 + arena_size as u64,
        });
        let globals_at = (&mut *globals) as *mut Globals as i64;

        // Declare every method.
        let mut funcs: HashMap<String, FuncId> = HashMap::new();
        let mut sigs: HashMap<String, Signature> = HashMap::new();
        for c in classes {
            let owner = c.name().map_err(|e| e.to_string())?;
            for m in &c.methods {
                if m.code().is_none() {
                    continue;
                }
                let Some(sig) = signature(&module, m.access & STATIC == 0, &m.descriptor) else {
                    continue;
                };
                let name = export_name(owner, &m.name, &m.descriptor);
                let id = module
                    .declare_function(&name, Linkage::Export, &sig)
                    .map_err(|e| e.to_string())?;
                funcs.insert(name.clone(), id);
                sigs.insert(name, sig);
            }
        }
        // The init function of each class with <clinit>: test, set, run.
        let mut inits: HashMap<String, FuncId> = HashMap::new();
        for n in &names {
            if has_clinit(n) {
                let sig = module.make_signature();
                let id = module
                    .declare_function(&format!("init {n}"), Linkage::Local, &sig)
                    .map_err(|e| e.to_string())?;
                inits.insert((*n).to_owned(), id);
            }
        }
        let mut bridges: Vec<Option<FuncId>> = Vec::new();
        for (i, l) in lambdas.iter().enumerate() {
            bridges.push(match signature(&module, true, &l.sam) {
                Some(sig) => Some(
                    module
                        .declare_function(&format!("lambda {i}"), Linkage::Local, &sig)
                        .map_err(|e| e.to_string())?,
                ),
                None => None,
            });
        }
        let mut host = HashMap::new();
        for (name, params, ret) in [
            ("host_sin", vec![types::F64], Some(types::F64)),
            ("host_cos", vec![types::F64], Some(types::F64)),
            ("host_fmod", vec![types::F64, types::F64], Some(types::F64)),
            ("host_fmodf", vec![types::F32, types::F32], Some(types::F32)),
            (
                "host_is_instance",
                vec![types::I64, types::I32],
                Some(types::I32),
            ),
        ] {
            let mut sig = module.make_signature();
            sig.params.extend(params.into_iter().map(AbiParam::new));
            sig.returns.extend(ret.map(AbiParam::new));
            let id = module
                .declare_function(name, Linkage::Import, &sig)
                .map_err(|e| e.to_string())?;
            host.insert(name, id);
        }

        let world = World {
            by_name: &by_name,
            ids: &ids,
            layouts: &layouts,
            selectors: &selectors,
            implementations: &implementations,
            funcs: &funcs,
            sigs: &sigs,
            inits: &inits,
            host: &host,
            statics_at: &statics_at,
            statics_base,
            flags_at,
            globals_at,
            ptr,
            lambdas: &lambdas,
            lambda_at: &lambda_at,
            lambda_vt: &lambda_vt,
            singleton_at: &singleton_at,
            stays: std::cell::RefCell::new(HashMap::new()),
        };
        let mut unsupported = Vec::new();
        let mut ctx = module.make_context();
        let mut fctx = FunctionBuilderContext::new();
        let mut bodies: HashMap<FuncId, (String, cranelift_codegen::ir::Function)> = HashMap::new();
        for c in classes {
            let owner = c.name().map_err(|e| e.to_string())?;
            for m in &c.methods {
                let name = export_name(owner, &m.name, &m.descriptor);
                let Some(&id) = funcs.get(&name) else {
                    continue;
                };
                ctx.func.signature = sigs[&name].clone();
                let r = {
                    let b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                    MethodGen::new(&world, &hierarchy, c, m, b, &mut module).and_then(|g| g.run())
                };
                if let Err(e) = r {
                    // Start over with a body that traps.
                    ctx.func.clear();
                    unsupported.push((name.clone(), e));
                    ctx.func.signature = sigs[&name].clone();
                    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                    let e = b.create_block();
                    b.append_block_params_for_function_params(e);
                    b.switch_to_block(e);
                    b.seal_block(e);
                    b.ins().trap(UNSUPPORTED);
                    b.finalize(module.target_config());
                }
                bodies.insert(id, (name, ctx.func.clone()));
                module.clear_context(&mut ctx);
            }
        }
        // The lambdas' bridges: captured values and the interface method's
        // arguments to the target.
        for (i, l) in lambdas.iter().enumerate() {
            let Some(id) = bridges[i] else { continue };
            ctx.func.signature = signature(&module, true, &l.sam).expect("declared");
            let r = {
                let b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                bridge(&world, l, b, &mut module)
            };
            if let Err(e) = r {
                ctx.func.clear();
                unsupported.push((format!("lambda {i} -> {}.{}", l.owner, l.target), e));
                ctx.func.signature = signature(&module, true, &l.sam).expect("declared");
                let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                let e = b.create_block();
                b.append_block_params_for_function_params(e);
                b.switch_to_block(e);
                b.seal_block(e);
                b.ins().trap(UNSUPPORTED);
                b.finalize(module.target_config());
            }
            bodies.insert(id, (format!("lambda {i}"), ctx.func.clone()));
            module.clear_context(&mut ctx);
        }
        // Small methods inlined into their callers, then every method defined.
        let sizes: HashMap<FuncId, usize> = bodies
            .iter()
            .map(|(&id, (_, f))| {
                (
                    id,
                    f.layout
                        .blocks()
                        .map(|b| f.layout.block_insts(b).count())
                        .sum(),
                )
            })
            .collect();
        for (&id, (name, func)) in &bodies {
            ctx.func = func.clone();
            ctx.inline(Inliner {
                bodies: &bodies,
                sizes: &sizes,
                caller: id,
            })
            .map_err(|e| format!("{name}: inlining: {e:?}"))?;
            module
                .define_function(id, &mut ctx)
                .map_err(|e| format!("{name}: {e:?}"))?;
            module.clear_context(&mut ctx);
        }
        // The init functions.
        for (class, &id) in &inits {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let e = b.create_block();
            b.switch_to_block(e);
            b.seal_block(e);
            let flag = statics_base + 8 * (flags_at + ids[class.as_str()] as usize) as i64;
            let at = b.ins().iconst(types::I64, flag);
            let one = b.ins().iconst(types::I64, 1);
            b.ins().store(MemFlagsData::trusted(), one, at, 0);
            if let Some(s) = superclass(class)
                && let Some(&sid) = inits.get(s)
            {
                let f = module.declare_func_in_func(sid, b.func);
                b.ins().call(f, &[]);
            }
            let clinit = export_name(class, "<clinit>", "()V");
            if let Some(&cid) = funcs.get(&clinit) {
                let f = module.declare_func_in_func(cid, b.func);
                b.ins().call(f, &[]);
            }
            b.ins().return_(&[]);
            b.finalize(module.target_config());
            module
                .define_function(id, &mut ctx)
                .map_err(|e| format!("init {class}: {e:?}"))?;
            module.clear_context(&mut ctx);
        }
        module.finalize_definitions().map_err(|e| e.to_string())?;

        // Fill the vtables: each selector's implementation in the class or
        // its superclasses (or an interface's default method).
        for n in &names {
            let l = &layouts[*n];
            for ((mname, mdesc), &sel) in &selectors {
                let mut x = Some(*n);
                let mut found = None;
                while let Some(c) = x {
                    let key = export_name(c, mname, mdesc);
                    if let Some(&f) = funcs.get(&key)
                        && by_name[c].methods.iter().any(|m| {
                            m.name == *mname && m.descriptor == *mdesc && m.access & ABSTRACT == 0
                        })
                    {
                        found = Some(f);
                        break;
                    }
                    x = superclass(c);
                }
                if found.is_none() {
                    // A default method of an interface it implements.
                    let defaults = ancestors_of(n, &by_name);
                    found = defaults.iter().find_map(|i| {
                        let cf = by_name.get(i.as_str())?;
                        (cf.access & INTERFACE != 0)
                            .then(|| funcs.get(&export_name(i, mname, mdesc)).copied())
                            .flatten()
                    });
                }
                if let Some(f) = found {
                    let p = module.get_finalized_function(f);
                    // SAFETY: the vtable has 1 + selectors entries.
                    unsafe { *l.vtable.add(1 + sel) = p as u64 };
                }
            }
        }
        for (i, l) in lambdas.iter().enumerate() {
            let vt = lambda_vt[i] as *mut u64;
            if let (Some(id), Some(&sel)) =
                (bridges[i], selectors.get(&(l.name.clone(), l.sam.clone())))
            {
                let p = module.get_finalized_function(id);
                // SAFETY: the vtable has 1 + selectors entries.
                unsafe { *vt.add(1 + sel) = p as u64 };
            }
            // The interface's default methods for its other selectors.
            for ((mname, mdesc), &sel) in &selectors {
                if *mname == l.name && *mdesc == l.sam {
                    continue;
                }
                let found = ancestors_of(&l.interface, &by_name)
                    .iter()
                    .find_map(|c| funcs.get(&export_name(c, mname, mdesc)).copied());
                if let Some(f) = found {
                    // SAFETY: as above.
                    unsafe { *vt.add(1 + sel) = module.get_finalized_function(f) as u64 };
                }
            }
        }
        let exports = funcs;
        Ok(Native {
            module,
            exports,
            unsupported,
            _arena: arena,
            _globals: globals,
            _statics: statics,
            _vtables: vtables,
            _singletons: singletons,
        })
    }

    /// The machine code of an exported method (`Class.name(desc)ret`).
    pub fn function(&self, export: &str) -> Option<*const u8> {
        Some(
            self.module
                .get_finalized_function(*self.exports.get(export)?),
        )
    }
}

/// A lambda site (`invokedynamic` through `LambdaMetafactory`): made into a
/// class at install, whose interface method is a bridge to the target.
#[derive(Clone, Debug)]
struct LambdaSite {
    /// The interface method's name and erased descriptor.
    name: String,
    sam: String,
    /// The interface (the site's return type).
    interface: String,
    /// The captured values' descriptors.
    captures: Vec<String>,
    /// The target method handle: kind (JVMS 5.4.3.5), owner, name, descriptor.
    kind: u8,
    owner: String,
    target: String,
    target_desc: String,
}

/// Every lambda site of `classes` (by class and position), and the method
/// names and descriptors called virtually (they need vtable selectors).
#[allow(clippy::type_complexity)]
fn prepass(
    classes: &[ClassFile],
) -> (
    Vec<LambdaSite>,
    HashMap<(String, u32), usize>,
    Vec<(String, String)>,
) {
    let mut sites = Vec::new();
    let mut at = HashMap::new();
    let mut called = Vec::new();
    for c in classes {
        let Ok(owner) = c.name() else { continue };
        for m in &c.methods {
            let Some(code) = m.code() else { continue };
            let Ok(insns) = decode(&code.code) else {
                continue;
            };
            for x in &insns {
                match (x.opcode, &x.operand) {
                    (0xB6 | 0xB9, Operand::Constant(i) | Operand::Interface(i, _)) => {
                        if let Ok((_, n, d)) = c.member_ref(*i) {
                            called.push((n.to_owned(), d.to_owned()));
                        }
                    }
                    (0xBA, Operand::Constant(i)) => {
                        if let Some(site) = lambda_site(c, *i) {
                            at.insert((owner.to_owned(), x.at), sites.len());
                            sites.push(site);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    (sites, at, called)
}

fn lambda_site(c: &ClassFile, index: u16) -> Option<LambdaSite> {
    let Some(Constant::InvokeDynamic(bsm, nat)) = c.constants.get(usize::from(index)) else {
        return None;
    };
    let (name, desc) = c.name_and_type(*nat).ok()?;
    let b = c.bootstrap_methods().get(usize::from(*bsm))?;
    let handle = |i: u16| match c.constants.get(usize::from(i)) {
        Some(Constant::MethodHandle(kind, r)) => {
            let (o, n, d) = c.member_ref(*r).ok()?;
            Some((*kind, o.to_owned(), n.to_owned(), d.to_owned()))
        }
        _ => None,
    };
    let (_, bo, bn, _) = handle(b.method_handle)?;
    if bo != "java/lang/invoke/LambdaMetafactory" || (bn != "metafactory" && bn != "altMetafactory")
    {
        return None;
    }
    let [sam, target, ..] = b.arguments[..] else {
        return None;
    };
    let Some(Constant::MethodType(sam)) = c.constants.get(usize::from(sam)) else {
        return None;
    };
    let sam = c.utf8(*sam).ok()?.to_owned();
    let (kind, owner, target, target_desc) = handle(target)?;
    let site = Method::parse(desc)?;
    let interface = site.ret.strip_prefix('L')?.strip_suffix(';')?.to_owned();
    Some(LambdaSite {
        name: name.to_owned(),
        sam,
        interface,
        captures: site.params,
        kind,
        owner,
        target,
        target_desc,
    })
}

/// Inlines calls to small methods (`INLINE` instructions or fewer), not
/// into themselves.
struct Inliner<'a> {
    bodies: &'a HashMap<FuncId, (String, cranelift_codegen::ir::Function)>,
    sizes: &'a HashMap<FuncId, usize>,
    caller: FuncId,
}

const INLINE: usize = 40;

impl cranelift_codegen::inline::Inline for Inliner<'_> {
    fn inline(
        &mut self,
        caller: &cranelift_codegen::ir::Function,
        _inst: cranelift_codegen::ir::Inst,
        _opcode: cranelift_codegen::ir::Opcode,
        callee: cranelift_codegen::ir::FuncRef,
        _args: &[Value],
    ) -> cranelift_codegen::inline::InlineCommand<'_> {
        use cranelift_codegen::inline::InlineCommand;
        use cranelift_codegen::ir::ExternalName;
        let ExternalName::User(r) = &caller.dfg.ext_funcs[callee].name else {
            return InlineCommand::KeepCall;
        };
        let name = &caller.params.user_named_funcs()[*r];
        if name.namespace != 0 {
            return InlineCommand::KeepCall;
        }
        let id = FuncId::from_u32(name.index);
        match (self.bodies.get(&id), self.sizes.get(&id)) {
            (Some((_, body)), Some(&size)) if size <= INLINE && id != self.caller => {
                InlineCommand::Inline {
                    callee: std::borrow::Cow::Borrowed(body),
                    visit_callee: false,
                }
            }
            _ => InlineCommand::KeepCall,
        }
    }
}

fn ancestors_of(c: &str, by_name: &HashMap<&str, &ClassFile>) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![c.to_owned()];
    while let Some(x) = todo.pop() {
        if out.contains(&x) {
            continue;
        }
        if let Some(cf) = by_name.get(x.as_str()) {
            todo.extend(cf.super_name().ok().flatten().map(str::to_owned));
            todo.extend(
                cf.interfaces
                    .iter()
                    .filter_map(|&i| cf.class_name(i).ok().map(str::to_owned)),
            );
        }
        out.push(x);
    }
    out
}

fn signature(module: &JITModule, instance: bool, desc: &str) -> Option<Signature> {
    let m = Method::parse(desc)?;
    let mut sig = module.make_signature();
    if instance {
        sig.params.push(AbiParam::new(types::I64));
    }
    for p in &m.params {
        sig.params.push(AbiParam::new(desc_type(p)?));
    }
    if m.ret != "V" {
        sig.returns.push(AbiParam::new(desc_type(&m.ret)?));
    }
    Some(sig)
}

/// What every method's code generation reads.
struct World<'a> {
    by_name: &'a HashMap<&'a str, &'a ClassFile>,
    ids: &'a HashMap<&'a str, u32>,
    layouts: &'a HashMap<String, Layout>,
    selectors: &'a BTreeMap<(String, String), usize>,
    implementations: &'a HashMap<(String, String), usize>,
    funcs: &'a HashMap<String, FuncId>,
    sigs: &'a HashMap<String, Signature>,
    inits: &'a HashMap<String, FuncId>,
    host: &'a HashMap<&'static str, FuncId>,
    statics_at: &'a HashMap<(String, String), (i32, Type)>,
    statics_base: i64,
    flags_at: usize,
    globals_at: i64,
    ptr: Type,
    lambdas: &'a [LambdaSite],
    lambda_at: &'a HashMap<(String, u32), usize>,
    lambda_vt: &'a [i64],
    singleton_at: &'a [Option<i64>],
    /// Whether a method keeps its own object inside it (by method and the
    /// inlining depth left).
    stays: std::cell::RefCell<HashMap<(String, u32), bool>>,
}

impl World<'_> {
    /// The class (of `owner` or a superclass) that declares method `name desc`.
    fn resolve(&self, owner: &str, name: &str, desc: &str) -> Option<String> {
        let mut x = Some(owner.to_owned());
        while let Some(c) = x {
            let cf = self.by_name.get(c.as_str())?;
            if cf
                .methods
                .iter()
                .any(|m| m.name == name && m.descriptor == desc && m.code().is_some())
            {
                return Some(c);
            }
            x = cf.super_name().ok().flatten().map(str::to_owned);
        }
        None
    }
    /// The class that declares static field `name`, from `owner` up.
    fn static_field(&self, owner: &str, name: &str) -> Option<(String, i32, Type)> {
        let mut x = Some(owner.to_owned());
        while let Some(c) = x {
            if let Some(&(at, t)) = self.statics_at.get(&(c.clone(), name.to_owned())) {
                return Some((c, at, t));
            }
            x = self
                .by_name
                .get(c.as_str())?
                .super_name()
                .ok()
                .flatten()
                .map(str::to_owned);
        }
        None
    }
}

/// Bump allocation of `size` bytes (zeroed), the header set to `header`.
fn alloc_in(b: &mut FunctionBuilder<'_>, w: &World<'_>, size: Value, header: Value) -> Value {
    let g = b.ins().iconst(types::I64, w.globals_at);
    let cur = b.ins().load(types::I64, MemFlagsData::trusted(), g, 0);
    let limit = b.ins().load(types::I64, MemFlagsData::trusted(), g, 8);
    let size = b.ins().iadd_imm_s(size, 7);
    let size = b.ins().band_imm_s(size, !7);
    let next = b.ins().iadd(cur, size);
    let over = b.ins().icmp(IntCC::UnsignedGreaterThan, next, limit);
    b.ins().trapnz(over, ARENA);
    b.ins().store(MemFlagsData::trusted(), next, g, 0);
    b.ins().store(MemFlagsData::trusted(), header, cur, 0);
    cur
}

/// Runs the static initializer of `class` if it has not run.
fn init_in(b: &mut FunctionBuilder<'_>, module: &mut JITModule, w: &World<'_>, class: &str) {
    let Some(&id) = w.inits.get(class) else {
        return;
    };
    let flag = w.statics_base + 8 * (w.flags_at + w.ids[class] as usize) as i64;
    let at = b.ins().iconst(types::I64, flag);
    let v = b.ins().load(types::I64, MemFlagsData::trusted(), at, 0);
    let run = b.create_block();
    let done = b.create_block();
    b.ins().brif(v, done, &[], run, &[]);
    b.switch_to_block(run);
    b.seal_block(run);
    let f = module.declare_func_in_func(id, b.func);
    b.ins().call(f, &[]);
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
    b.seal_block(done);
}

/// Calls instance method `name desc` of `owner` on `args[0]`: directly when
/// the closed world has one implementation, else through the vtable.
fn call_virtual(
    b: &mut FunctionBuilder<'_>,
    module: &mut JITModule,
    w: &World<'_>,
    owner: &str,
    name: &str,
    desc: &str,
    args: &[Value],
) -> Result<Option<Value>, String> {
    b.ins().trapz(args[0], NULL);
    let key = (name.to_owned(), desc.to_owned());
    let direct = (w.implementations.get(&key) == Some(&1))
        .then(|| w.resolve(owner, name, desc))
        .flatten()
        .filter(|c| {
            w.by_name
                .get(c.as_str())
                .is_some_and(|cf| cf.access & INTERFACE == 0)
        });
    let call = match (direct, w.selectors.get(&key)) {
        (Some(declaring), _) => {
            let id = w.funcs[&export_name(&declaring, name, desc)];
            let fr = module.declare_func_in_func(id, b.func);
            b.ins().call(fr, args)
        }
        (None, Some(&sel)) => {
            let vt = b
                .ins()
                .load(types::I64, MemFlagsData::trusted(), args[0], 0);
            let fp = b.ins().load(
                types::I64,
                MemFlagsData::trusted(),
                vt,
                (8 * (1 + sel)) as i32,
            );
            let sig = signature(module, true, desc).ok_or("a bad signature")?;
            let sr = b.import_signature(sig);
            b.ins().call_indirect(sr, fp, args)
        }
        (None, None) => return Err(format!("no implementation of {owner}.{name}{desc}")),
    };
    Ok(b.inst_results(call).first().copied())
}

/// The body of a lambda's interface method: its captured values and
/// arguments to the target method handle.
fn bridge(
    w: &World<'_>,
    l: &LambdaSite,
    mut b: FunctionBuilder<'_>,
    module: &mut JITModule,
) -> Result<(), String> {
    let e = b.create_block();
    b.append_block_params_for_function_params(e);
    b.switch_to_block(e);
    b.seal_block(e);
    let params = b.block_params(e).to_vec();
    let sam = Method::parse(&l.sam).ok_or("a bad interface descriptor")?;
    let target = Method::parse(&l.target_desc).ok_or("a bad target descriptor")?;
    let mut args = Vec::new();
    for (k, c) in l.captures.iter().enumerate() {
        let t = desc_type(c).ok_or("a bad capture")?;
        args.push(
            b.ins()
                .load(t, MemFlagsData::trusted(), params[0], 8 + 8 * k as i32),
        );
    }
    args.extend_from_slice(&params[1..]);
    let mut want: Vec<Type> = Vec::new();
    if matches!(l.kind, 5 | 7 | 9) {
        want.push(types::I64);
    }
    for p in &target.params {
        want.push(desc_type(p).ok_or("a bad parameter")?);
    }
    if args.len() != want.len() {
        return Err("arguments do not match the target".into());
    }
    if args
        .iter()
        .zip(&want)
        .any(|(a, t)| b.func.dfg.value_type(*a) != *t)
    {
        return Err("boxing between the interface and the target (not in the prototype)".into());
    }
    let result = match l.kind {
        6 | 7 => {
            let declaring = w
                .resolve(&l.owner, &l.target, &l.target_desc)
                .ok_or("no target")?;
            if l.kind == 6 {
                init_in(&mut b, module, w, &declaring);
            } else {
                b.ins().trapz(args[0], NULL);
            }
            let id = w.funcs[&export_name(&declaring, &l.target, &l.target_desc)];
            let fr = module.declare_func_in_func(id, b.func);
            let call = b.ins().call(fr, &args);
            b.inst_results(call).first().copied()
        }
        5 | 9 => call_virtual(
            &mut b,
            module,
            w,
            &l.owner,
            &l.target,
            &l.target_desc,
            &args,
        )?,
        8 => {
            let layout = w
                .layouts
                .get(&l.owner)
                .ok_or("a constructor outside the classes")?;
            let (size, vt) = (layout.size, layout.vtable as i64);
            init_in(&mut b, module, w, &l.owner);
            let size = b.ins().iconst(types::I64, i64::from(size));
            let header = b.ins().iconst(types::I64, vt);
            let obj = alloc_in(&mut b, w, size, header);
            let declaring = w
                .resolve(&l.owner, "<init>", &l.target_desc)
                .ok_or("no constructor")?;
            let id = w.funcs[&export_name(&declaring, "<init>", &l.target_desc)];
            let fr = module.declare_func_in_func(id, b.func);
            let mut all = vec![obj];
            all.extend_from_slice(&args);
            b.ins().call(fr, &all);
            Some(obj)
        }
        k => return Err(format!("method handle kind {k}")),
    };
    if sam.ret == "V" {
        b.ins().return_(&[]);
    } else {
        let want = desc_type(&sam.ret).ok_or("a bad return")?;
        match result {
            Some(v) if b.func.dfg.value_type(v) == want => {
                b.ins().return_(&[v]);
            }
            _ => return Err("boxing of the result (not in the prototype)".into()),
        }
    }
    b.finalize(module.target_config());
    Ok(())
}

/// Inlining depth for objects kept in variables, and the largest method
/// inlined for them.
const ESCAPE_DEPTH: u32 = 4;
const ESCAPE_SIZE: usize = 400;

/// The objects of a method (its `new` sites, and [`THIS_SITE`]) and which of
/// them escape it: stored in a field, an array or a static, passed to a
/// call other than one that keeps them (see [`stays`]), returned, thrown,
/// compared by identity, locked, merged with another value, or made again
/// while one of them is still held.
fn escapes(
    w: &World<'_>,
    h: &Classes,
    class: &ClassFile,
    insns: &[Instruction],
    frames: &[Option<Frame>],
    opts: &Options,
    depth: u32,
) -> (HashSet<u32>, HashSet<u32>) {
    let mut seen = HashSet::new();
    let mut out = HashSet::new();
    // An object not yet constructed is its site too.
    let site = |t: &Ty| match t {
        Ty::Site(x, _) | Ty::Uninit(x) => Some(*x),
        Ty::UninitThis if opts.this_site => Some(THIS_SITE),
        _ => None,
    };
    let at: HashMap<u32, usize> = insns.iter().enumerate().map(|(i, x)| (x.at, i)).collect();
    let stepper = Stepper::new(class, insns);
    for (i, insn) in insns.iter().enumerate() {
        let Some(f) = &frames[i] else { continue };
        for t in f.locals.iter().chain(&f.stack) {
            seen.extend(site(t));
        }
        let d = f.stack.len();
        let s = |k: usize| f.stack.get(d.wrapping_sub(1 + k)).and_then(site);
        let mut escape = |x: Option<u32>| out.extend(x);
        match insn.opcode {
            0xBB => {
                // A second one while the first is held: two objects, one name.
                if f.locals
                    .iter()
                    .chain(&f.stack)
                    .any(|t| site(t) == Some(insn.at))
                {
                    escape(Some(insn.at));
                }
                seen.insert(insn.at);
            }
            0xB5 => escape(s(0)),
            0xB3 | 0xB0 | 0xBF | 0xC2 | 0xC3 | 0xC1 => escape(s(0)),
            0x53 => {
                escape(s(0));
                escape(s(2));
            }
            0xA5 | 0xA6 => {
                escape(s(0));
                escape(s(1));
            }
            0xB6..=0xBA => {
                let call = invoked_member(class, insn);
                let Some((owner, name, desc)) = call else {
                    for k in 0..d {
                        escape(s(k));
                    }
                    continue;
                };
                let n = Method::parse(&desc).map_or(0, |m| m.params.len());
                for k in 0..n {
                    escape(s(k));
                }
                if insn.opcode != 0xB8 && insn.opcode != 0xBA {
                    let receiver = f.stack.get(d - n - 1);
                    let followed = match receiver {
                        Some(Ty::Site(x, c)) => Some((*x, c.to_string())),
                        // A constructor call: the class it names.
                        Some(t) => site(t).map(|x| (x, owner.clone())),
                        None => None,
                    };
                    if let Some((x, c)) = followed {
                        let x = &x;
                        // Its class is exactly known: the method it runs too.
                        let target = if insn.opcode == 0xB7 {
                            w.resolve(&owner, &name, &desc)
                        } else {
                            w.resolve(&c, &name, &desc)
                        };
                        let keeps = match target {
                            _ if owner == "java/lang/Object" && name == "<init>" => true,
                            Some(t) => depth > 0 && stays(w, h, &t, &name, &desc, depth - 1),
                            None => false,
                        };
                        if !keeps {
                            escape(Some(*x));
                        }
                    }
                }
                if insn.opcode == 0xBA {
                    for k in 0..d {
                        escape(s(k));
                    }
                }
            }
            _ => {}
        }
        // Across an edge, a held object must arrive as itself, or as Top
        // (its slot is dead there).
        let Ok(after) = stepper.after(class, opts, insn, f) else {
            continue;
        };
        for t in successors(insns, i, &at) {
            let Some(g) = &frames[t] else { continue };
            for (k, v) in after.locals.iter().enumerate() {
                if let Some(x) = site(v) {
                    let there = g.locals.get(k).unwrap_or(&Ty::Top);
                    if site(there) != Some(x) && *there != Ty::Top {
                        out.insert(x);
                    }
                }
            }
            for (k, v) in after.stack.iter().enumerate() {
                if let Some(x) = site(v)
                    && g.stack.get(k).and_then(site) != Some(x)
                {
                    out.insert(x);
                }
            }
        }
    }
    // An object of a class outside the closed world has no known fields.
    for (i, insn) in insns.iter().enumerate() {
        if insn.opcode == 0xBB
            && let Operand::Constant(c) = insn.operand
            && class
                .class_name(c)
                .map_or(true, |n| !w.layouts.contains_key(n))
        {
            out.insert(insn.at);
        }
        let _ = i;
    }
    (seen, out)
}

/// The method a call instruction names.
fn invoked_member(class: &ClassFile, insn: &Instruction) -> Option<(String, String, String)> {
    let (Operand::Constant(c) | Operand::Interface(c, _)) = insn.operand else {
        return None;
    };
    if insn.opcode == 0xBA {
        return None;
    }
    let (o, n, d) = class.member_ref(c).ok()?;
    Some((o.to_owned(), n.to_owned(), d.to_owned()))
}

/// Whether method `name desc` of `class` keeps its own object inside it
/// (so a caller can keep that object in variables and inline the method).
fn stays(w: &World<'_>, h: &Classes, class: &str, name: &str, desc: &str, depth: u32) -> bool {
    let key = (export_name(class, name, desc), depth);
    if let Some(&v) = w.stays.borrow().get(&key) {
        return v;
    }
    // While it is decided (recursion): no.
    w.stays.borrow_mut().insert(key.clone(), false);
    let v = (|| {
        let cf = w.by_name.get(class)?;
        let m = cf
            .methods
            .iter()
            .find(|m| m.name == name && m.descriptor == desc)?;
        let code = m.code()?;
        if !code.handlers.is_empty() || m.access & STATIC != 0 {
            return None;
        }
        let insns = decode(&code.code).ok()?;
        if insns.len() > ESCAPE_SIZE {
            return None;
        }
        let opts = Options {
            sites: true,
            this_site: true,
        };
        let frames = infer_with(h, cf, m, &insns, &opts).ok()?;
        let (_, out) = escapes(w, h, cf, &insns, &frames, &opts, depth);
        Some(!out.contains(&THIS_SITE))
    })()
    .unwrap_or(false);
    w.stays.borrow_mut().insert(key, v);
    v
}

/// An object kept in variables: the body it was made in (its id) and its
/// `new` position.
type SiteKey = (u32, u32);

struct MethodGen<'a, 'b, 'f> {
    w: &'a World<'a>,
    h: &'a Classes,
    class: &'a ClassFile,
    owner: &'a str,
    method: &'a Member,
    insns: Vec<Instruction>,
    frames: Vec<Option<Frame>>,
    cfg: Cfg,
    b: FunctionBuilder<'f>,
    module: &'b mut JITModule,
    /// Locals and stack slots, by body (the method or one inlined in it).
    vars: HashMap<(u32, bool, usize, Type), Variable>,
    blocks: Vec<Option<Block>>,
    /// Classes whose init test this method already made.
    inited: HashSet<String>,
    /// The body being generated, and the next body's id.
    body: u32,
    next_body: u32,
    /// This body's objects kept in variables (by `new` position).
    scalar: HashSet<u32>,
    /// In an inlined body: the object its `this` is.
    this_key: Option<SiteKey>,
    /// In an inlined body: where its returns go, with the value's type.
    ret: Option<(Block, Option<Type>)>,
    /// Inlining depth left for objects kept in variables.
    depth: u32,
    /// The fields of objects kept in variables.
    fields: HashMap<(SiteKey, i32, Type), Variable>,
}

impl<'a, 'b, 'f> MethodGen<'a, 'b, 'f> {
    fn new(
        w: &'a World<'a>,
        h: &'a Classes,
        class: &'a ClassFile,
        method: &'a Member,
        b: FunctionBuilder<'f>,
        module: &'b mut JITModule,
    ) -> Result<Self, String> {
        let code = method.code().ok_or("no code")?;
        let insns = decode(&code.code).map_err(|e| e.to_string())?;
        if !code.handlers.is_empty() {
            return Err("exception handlers (not in the prototype)".into());
        }
        let opts = Options {
            sites: true,
            this_site: false,
        };
        let frames = infer_with(h, class, method, &insns, &opts)?;
        let cfg = Cfg::new(&insns, &code.handlers);
        let (seen, out) = escapes(w, h, class, &insns, &frames, &opts, ESCAPE_DEPTH);
        let scalar = seen
            .difference(&out)
            .copied()
            .filter(|&x| x != THIS_SITE)
            .collect();
        Ok(MethodGen {
            w,
            h,
            class,
            owner: class.name().map_err(|e| e.to_string())?,
            method,
            insns,
            frames,
            cfg,
            b,
            module,
            vars: HashMap::new(),
            blocks: Vec::new(),
            inited: HashSet::new(),
            body: 0,
            next_body: 1,
            scalar,
            this_key: None,
            ret: None,
            depth: ESCAPE_DEPTH,
            fields: HashMap::new(),
        })
    }

    fn var(&mut self, local: bool, n: usize, t: Type) -> Variable {
        let key = (self.body, local, n, t);
        if let Some(&v) = self.vars.get(&key) {
            return v;
        }
        let v = self.b.declare_var(t);
        self.vars.insert(key, v);
        v
    }

    /// The object a value of type `t` is, if it is kept in variables.
    fn site_key(&self, t: &Ty) -> Option<SiteKey> {
        match t {
            Ty::Site(THIS_SITE, _) | Ty::UninitThis => self.this_key,
            Ty::Site(x, _) | Ty::Uninit(x) if self.scalar.contains(x) => Some((self.body, *x)),
            _ => None,
        }
    }

    /// Field variable `at` (of type `t`) of object `key`.
    fn field_var(&mut self, key: SiteKey, at: i32, t: Type) -> Variable {
        if let Some(&v) = self.fields.get(&(key, at, t)) {
            return v;
        }
        let v = self.b.declare_var(t);
        self.fields.insert((key, at, t), v);
        v
    }
    fn get(&mut self, d: usize, t: Type) -> Value {
        let v = self.var(false, d, t);
        self.b.use_var(v)
    }
    fn set(&mut self, d: usize, t: Type, x: Value) {
        let v = self.var(false, d, t);
        self.b.def_var(v, x);
    }

    fn run(mut self) -> Result<(), String> {
        let n = self.cfg.blocks.len();
        self.blocks = (0..n)
            .map(|b| {
                self.frames[self.cfg.blocks[b].start]
                    .is_some()
                    .then(|| self.b.create_block())
            })
            .collect();
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        // Parameters into their locals.
        let params: Vec<Value> = self.b.block_params(entry).to_vec();
        let sig = Method::parse(&self.method.descriptor).ok_or("a bad descriptor")?;
        let mut slot = 0usize;
        let mut k = 0usize;
        if self.method.access & STATIC == 0 {
            let v = self.var(true, 0, types::I64);
            self.b.def_var(v, params[0]);
            slot = 1;
            k = 1;
        }
        for p in &sig.params {
            let t = desc_type(p).ok_or("a bad parameter")?;
            let v = self.var(true, slot, t);
            self.b.def_var(v, params[k]);
            slot += if matches!(p.as_bytes()[0], b'J' | b'D') {
                2
            } else {
                1
            };
            k += 1;
        }
        // A static method makes sure its class is initialized.
        if self.method.access & STATIC != 0 && self.method.name != "<clinit>" {
            let owner = self.owner.to_owned();
            self.init(&owner);
        }
        let first = self.blocks[0].ok_or("no entry")?;
        self.b.ins().jump(first, &[]);
        self.b.seal_block(entry);
        self.emit_blocks()?;
        self.b.seal_all_blocks();
        let config = self.module.target_config();
        self.b.finalize(config);
        Ok(())
    }

    /// Method `name desc` of `class` inlined on object `key`, with `args`:
    /// its result.
    fn inline(
        &mut self,
        key: SiteKey,
        class: &str,
        name: &str,
        desc: &str,
        args: &[Value],
        ret: Option<Type>,
    ) -> Result<Option<Value>, String> {
        let w = self.w;
        let cf: &'a ClassFile = w.by_name.get(class).copied().ok_or("no class to inline")?;
        let m: &'a Member = cf
            .methods
            .iter()
            .find(|m| m.name == name && m.descriptor == desc)
            .ok_or("no method to inline")?;
        let code = m.code().ok_or("no code to inline")?;
        let insns = decode(&code.code).map_err(|e| e.to_string())?;
        let opts = Options {
            sites: true,
            this_site: true,
        };
        let frames = infer_with(self.h, cf, m, &insns, &opts)?;
        let cfg = Cfg::new(&insns, &code.handlers);
        let depth = self.depth.checked_sub(1).ok_or("inlined too deep")?;
        let (seen, out) = escapes(w, self.h, cf, &insns, &frames, &opts, depth);
        let scalar: HashSet<u32> = seen
            .difference(&out)
            .copied()
            .filter(|&x| x != THIS_SITE)
            .collect();
        let cont = self.b.create_block();
        if let Some(t) = ret {
            self.b.append_block_param(cont, t);
        }
        let owner: &'a str = cf.name().map_err(|e| e.to_string())?;
        let id = self.next_body;
        self.next_body += 1;
        // The caller's body, put back after.
        let saved = (
            self.class,
            self.owner,
            self.method,
            std::mem::replace(&mut self.insns, insns),
            std::mem::replace(&mut self.frames, frames),
            std::mem::replace(&mut self.cfg, cfg),
            std::mem::take(&mut self.blocks),
            std::mem::replace(&mut self.scalar, scalar),
            self.this_key,
            self.ret,
            self.body,
            self.depth,
        );
        self.class = cf;
        self.owner = owner;
        self.method = m;
        self.this_key = Some(key);
        self.ret = Some((cont, ret));
        self.body = id;
        self.depth = depth;
        let r = (|| -> Result<(), String> {
            let n = self.cfg.blocks.len();
            self.blocks = (0..n)
                .map(|b| {
                    self.frames[self.cfg.blocks[b].start]
                        .is_some()
                        .then(|| self.b.create_block())
                })
                .collect();
            // Its own object is the kept one: local 0 is a stand-in.
            let one = self.b.ins().iconst(types::I64, 1);
            let v = self.var(true, 0, types::I64);
            self.b.def_var(v, one);
            let sig = Method::parse(desc).ok_or("a bad descriptor")?;
            let mut slot = 1usize;
            for (p, a) in sig.params.iter().zip(args) {
                let t = desc_type(p).ok_or("a bad parameter")?;
                let v = self.var(true, slot, t);
                self.b.def_var(v, *a);
                slot += if matches!(p.as_bytes()[0], b'J' | b'D') {
                    2
                } else {
                    1
                };
            }
            let first = self.blocks[0].ok_or("no entry to inline")?;
            self.b.ins().jump(first, &[]);
            self.emit_blocks()
        })();
        (
            self.class,
            self.owner,
            self.method,
            self.insns,
            self.frames,
            self.cfg,
            self.blocks,
            self.scalar,
            self.this_key,
            self.ret,
            self.body,
            self.depth,
        ) = saved;
        r?;
        self.b.switch_to_block(cont);
        self.inited.clear();
        Ok(ret.map(|_| self.b.block_params(cont)[0]))
    }

    /// The code of the current body's blocks.
    fn emit_blocks(&mut self) -> Result<(), String> {
        for x in self.cfg.rpo.clone() {
            let Some(blk) = self.blocks[x] else { continue };
            self.b.switch_to_block(blk);
            self.inited.clear();
            let (start, end) = (self.cfg.blocks[x].start, self.cfg.blocks[x].end);
            let mut ended = false;
            for i in start..end {
                let f = self.frames[i].clone().ok_or("unreachable within a block")?;
                let insn = self.insns[i].clone();
                ended = self.insn(&insn, &f, i)?;
                if ended {
                    break;
                }
            }
            if !ended {
                // Falls through.
                let next = self
                    .cfg
                    .block_of
                    .get(end)
                    .and_then(|&b| self.blocks[b])
                    .ok_or("falls off the end")?;
                self.b.ins().jump(next, &[]);
            }
        }
        Ok(())
    }

    fn target(&self, pc: u32) -> Result<Block, String> {
        let i = self
            .insns
            .iter()
            .position(|x| x.at == pc)
            .ok_or("a bad target")?;
        self.blocks[self.cfg.block_of[i]].ok_or_else(|| "a jump into dead code".into())
    }
    fn next_block(&self, i: usize) -> Result<Block, String> {
        self.cfg
            .block_of
            .get(i + 1)
            .and_then(|&b| self.blocks[b])
            .ok_or_else(|| "no block after a branch".into())
    }

    /// Calls the init function of `class` once in this block.
    fn init(&mut self, class: &str) {
        if !self.inited.insert(class.to_owned()) {
            return;
        }
        let Some(&id) = self.w.inits.get(class) else {
            return;
        };
        if self.method.name == "<clinit>" && class == self.owner {
            return;
        }
        let flag = self.w.statics_base + 8 * (self.w.flags_at + self.w.ids[class] as usize) as i64;
        let at = self.b.ins().iconst(types::I64, flag);
        let v = self
            .b
            .ins()
            .load(types::I64, MemFlagsData::trusted(), at, 0);
        let run = self.b.create_block();
        let done = self.b.create_block();
        self.b.ins().brif(v, done, &[], run, &[]);
        self.b.switch_to_block(run);
        self.b.seal_block(run);
        let f = self.module.declare_func_in_func(id, self.b.func);
        self.b.ins().call(f, &[]);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
    }

    fn null_check(&mut self, obj: Value) {
        self.b.ins().trapz(obj, NULL);
    }

    fn host(&mut self, name: &str, args: &[Value]) -> Option<Value> {
        let id = self.w.host[name];
        let f = self.module.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(f, args);
        self.b.inst_results(call).first().copied()
    }

    /// Bump allocation of `size` bytes (zeroed), the header set to `header`.
    fn alloc(&mut self, size: Value, header: Value) -> Value {
        let g = self.b.ins().iconst(types::I64, self.w.globals_at);
        let cur = self.b.ins().load(types::I64, MemFlagsData::trusted(), g, 0);
        let limit = self.b.ins().load(types::I64, MemFlagsData::trusted(), g, 8);
        let size = self.b.ins().iadd_imm_s(size, 7);
        let size = self.b.ins().band_imm_s(size, !7);
        let next = self.b.ins().iadd(cur, size);
        let over = self.b.ins().icmp(IntCC::UnsignedGreaterThan, next, limit);
        self.b.ins().trapnz(over, ARENA);
        self.b.ins().store(MemFlagsData::trusted(), next, g, 0);
        self.b.ins().store(MemFlagsData::trusted(), header, cur, 0);
        cur
    }

    /// One instruction; whether it ended the block.
    fn insn(&mut self, insn: &Instruction, f: &Frame, i: usize) -> Result<bool, String> {
        let op = insn.opcode;
        let d = f.stack.len();
        let st = |k: usize| kind_type(&f.stack[d - 1 - k]).unwrap_or(types::I64);
        let me = |e: crate::classfile::Error| e.to_string();
        let (i32t, i64t, f32t, f64t) = (types::I32, types::I64, types::F32, types::F64);
        macro_rules! pop2 {
            ($t:expr) => {{
                let b = self.get(d - 1, $t);
                let a = self.get(d - 2, $t);
                (a, b)
            }};
        }
        match op {
            0x00 => {}
            0x01 => {
                let z = self.b.ins().iconst(i64t, 0);
                self.set(d, i64t, z);
            }
            0x02..=0x08 => {
                let v = self.b.ins().iconst(i32t, i64::from(op) - 3);
                self.set(d, i32t, v);
            }
            0x09 | 0x0A => {
                let v = self.b.ins().iconst(i64t, i64::from(op) - 9);
                self.set(d, i64t, v);
            }
            0x0B..=0x0D => {
                let v = self.b.ins().f32const(f32::from(op - 0x0B));
                self.set(d, f32t, v);
            }
            0x0E | 0x0F => {
                let v = self.b.ins().f64const(f64::from(op - 0x0E));
                self.set(d, f64t, v);
            }
            0x10 | 0x11 => {
                let Operand::Value(v) = insn.operand else {
                    unreachable!()
                };
                let v = self.b.ins().iconst(i32t, i64::from(v));
                self.set(d, i32t, v);
            }
            0x12..=0x14 => {
                let Operand::Constant(c) = insn.operand else {
                    unreachable!()
                };
                let (v, t) = match self.class.constants.get(usize::from(c)) {
                    Some(Constant::Integer(x)) => (self.b.ins().iconst(i32t, i64::from(*x)), i32t),
                    Some(Constant::Long(x)) => (self.b.ins().iconst(i64t, *x), i64t),
                    Some(Constant::Float(x)) => (self.b.ins().f32const(*x), f32t),
                    Some(Constant::Double(x)) => (self.b.ins().f64const(*x), f64t),
                    _ => {
                        self.b.ins().trap(UNSUPPORTED);
                        return Ok(true);
                    }
                };
                self.set(d, t, v);
            }
            0x15..=0x2D => {
                let (n, t) = match (op, &insn.operand) {
                    (0x15..=0x19, Operand::Local(n)) => (
                        usize::from(*n),
                        [i32t, i64t, f32t, f64t, i64t][usize::from(op - 0x15)],
                    ),
                    _ => {
                        let o = op - 0x1A;
                        (
                            usize::from(o % 4),
                            [i32t, i64t, f32t, f64t, i64t][usize::from(o / 4)],
                        )
                    }
                };
                let v = self.var(true, n, t);
                let x = self.b.use_var(v);
                self.set(d, t, x);
            }
            0x36..=0x4E => {
                let (n, t) = match (op, &insn.operand) {
                    (0x36..=0x3A, Operand::Local(n)) => (
                        usize::from(*n),
                        [i32t, i64t, f32t, f64t, i64t][usize::from(op - 0x36)],
                    ),
                    _ => {
                        let o = op - 0x3B;
                        (
                            usize::from(o % 4),
                            [i32t, i64t, f32t, f64t, i64t][usize::from(o / 4)],
                        )
                    }
                };
                let x = self.get(d - 1, t);
                let v = self.var(true, n, t);
                self.b.def_var(v, x);
            }
            0x2E..=0x35 => {
                let array_ty = match &f.stack[d - 2] {
                    Ty::Ref(a) => a.to_string(),
                    _ => {
                        self.b.ins().trap(NULL);
                        return Ok(true);
                    }
                };
                let idx = self.get(d - 1, i32t);
                let arr = self.get(d - 2, i64t);
                let addr = self.element_addr(arr, idx, &array_ty);
                let (_, et) = element(&array_ty);
                let flags = MemFlagsData::trusted();
                let (v, t) = match op {
                    0x33 => (self.b.ins().sload8(i32t, flags, addr, 0), i32t),
                    0x34 => (self.b.ins().uload16(i32t, flags, addr, 0), i32t),
                    0x35 => (self.b.ins().sload16(i32t, flags, addr, 0), i32t),
                    _ => (self.b.ins().load(et, flags, addr, 0), et),
                };
                self.set(d - 2, t, v);
            }
            0x4F..=0x56 => {
                let array_ty = match &f.stack[d - 3] {
                    Ty::Ref(a) => a.to_string(),
                    _ => {
                        self.b.ins().trap(NULL);
                        return Ok(true);
                    }
                };
                let vt = st(0);
                let v = self.get(d - 1, vt);
                let idx = self.get(d - 2, i32t);
                let arr = self.get(d - 3, i64t);
                let addr = self.element_addr(arr, idx, &array_ty);
                let flags = MemFlagsData::trusted();
                match op {
                    0x54 => {
                        self.b.ins().istore8(flags, v, addr, 0);
                    }
                    0x55 | 0x56 => {
                        self.b.ins().istore16(flags, v, addr, 0);
                    }
                    _ => {
                        self.b.ins().store(flags, v, addr, 0);
                    }
                }
            }
            0x57 | 0x58 => {}
            0x59..=0x5F => self.stack_op(op, f)?,
            0x60..=0x83 => self.arith(op, d, &st)?,
            0x84 => {
                let Operand::Increment(n, by) = insn.operand else {
                    unreachable!()
                };
                let v = self.var(true, usize::from(n), i32t);
                let x = self.b.use_var(v);
                let y = self.b.ins().iadd_imm_s(x, i64::from(by));
                self.b.def_var(v, y);
            }
            0x85..=0x93 => self.convert(op, d)?,
            0x94 => {
                let (a, b) = pop2!(i64t);
                let gt = self.b.ins().icmp(IntCC::SignedGreaterThan, a, b);
                let lt = self.b.ins().icmp(IntCC::SignedLessThan, a, b);
                let gt = self.b.ins().uextend(i32t, gt);
                let lt = self.b.ins().uextend(i32t, lt);
                let r = self.b.ins().isub(gt, lt);
                self.set(d - 2, i32t, r);
            }
            0x95..=0x98 => {
                let t = if op <= 0x96 { f32t } else { f64t };
                let (a, b) = pop2!(t);
                let gt = self.b.ins().fcmp(FloatCC::GreaterThan, a, b);
                let lt = self.b.ins().fcmp(FloatCC::LessThan, a, b);
                let uno = self.b.ins().fcmp(FloatCC::Unordered, a, b);
                let gt = self.b.ins().uextend(i32t, gt);
                let lt = self.b.ins().uextend(i32t, lt);
                let r = self.b.ins().isub(gt, lt);
                let nan = self
                    .b
                    .ins()
                    .iconst(i32t, if op == 0x95 || op == 0x97 { -1 } else { 1 });
                let r = self.b.ins().select(uno, nan, r);
                self.set(d - 2, i32t, r);
            }
            0xC6 | 0xC7 if self.site_key(&f.stack[d - 1]).is_some() => {
                // An object kept in variables is never null.
                let Operand::Target(t) = insn.operand else {
                    unreachable!()
                };
                let to = if op == 0xC7 {
                    self.target(t)?
                } else {
                    self.next_block(i)?
                };
                self.b.ins().jump(to, &[]);
                return Ok(true);
            }
            0xC0 if self.site_key(&f.stack[d - 1]).is_some() => {}
            0x99..=0xA6 | 0xC6 | 0xC7 => {
                let Operand::Target(t) = insn.operand else {
                    unreachable!()
                };
                let cond = match op {
                    0x99..=0x9E => {
                        let a = self.get(d - 1, i32t);
                        self.b.ins().icmp_imm_s(int_cc(op - 0x99), a, 0)
                    }
                    0x9F..=0xA4 => {
                        let (a, b) = pop2!(i32t);
                        self.b.ins().icmp(int_cc(op - 0x9F), a, b)
                    }
                    0xA5 | 0xA6 => {
                        let (a, b) = pop2!(i64t);
                        self.b.ins().icmp(
                            if op == 0xA5 {
                                IntCC::Equal
                            } else {
                                IntCC::NotEqual
                            },
                            a,
                            b,
                        )
                    }
                    _ => {
                        let a = self.get(d - 1, i64t);
                        self.b.ins().icmp_imm_s(
                            if op == 0xC6 {
                                IntCC::Equal
                            } else {
                                IntCC::NotEqual
                            },
                            a,
                            0,
                        )
                    }
                };
                let to = self.target(t)?;
                let next = self.next_block(i)?;
                self.b.ins().brif(cond, to, &[], next, &[]);
                return Ok(true);
            }
            0xA7 | 0xC8 => {
                let Operand::Target(t) = insn.operand else {
                    unreachable!()
                };
                let to = self.target(t)?;
                self.b.ins().jump(to, &[]);
                return Ok(true);
            }
            0xAA | 0xAB => {
                let key = self.get(d - 1, i32t);
                let (default, pairs): (u32, Vec<(i32, u32)>) = match &insn.operand {
                    Operand::Table {
                        default,
                        low,
                        targets,
                    } => (
                        *default,
                        targets
                            .iter()
                            .enumerate()
                            .map(|(k, t)| (low + k as i32, *t))
                            .collect(),
                    ),
                    Operand::Lookup { default, pairs } => (*default, pairs.clone()),
                    _ => unreachable!(),
                };
                let mut sw = cranelift_frontend::Switch::new();
                for (k, t) in pairs {
                    sw.set_entry(u128::from(k as u32), self.target(t)?);
                }
                let def = self.target(default)?;
                sw.emit(&mut self.b, key, def);
                return Ok(true);
            }
            0xAC..=0xB0 => {
                let v = self.get(d - 1, st(0));
                match self.ret {
                    Some((to, _)) => {
                        self.b.ins().jump(to, &[v.into()]);
                    }
                    None => {
                        self.b.ins().return_(&[v]);
                    }
                }
                return Ok(true);
            }
            0xB1 => {
                match self.ret {
                    Some((to, _)) => {
                        self.b.ins().jump(to, &[]);
                    }
                    None => {
                        self.b.ins().return_(&[]);
                    }
                }
                return Ok(true);
            }
            0xB2 | 0xB3 => {
                let Operand::Constant(c) = insn.operand else {
                    unreachable!()
                };
                let (owner, name, _) = self.class.member_ref(c).map_err(me)?;
                let Some((declaring, at, t)) = self.w.static_field(owner, name) else {
                    self.b.ins().trap(UNSUPPORTED);
                    return Ok(true);
                };
                self.init(&declaring);
                let addr = self
                    .b
                    .ins()
                    .iconst(i64t, self.w.statics_base + i64::from(at));
                if op == 0xB2 {
                    let v = self.b.ins().load(t, MemFlagsData::trusted(), addr, 0);
                    self.set(d, t, v);
                } else {
                    let v = self.get(d - 1, t);
                    self.b.ins().store(MemFlagsData::trusted(), v, addr, 0);
                }
            }
            0xB4 | 0xB5 => {
                let Operand::Constant(c) = insn.operand else {
                    unreachable!()
                };
                let (owner, name, _) = self.class.member_ref(c).map_err(me)?;
                let Some(&(at, t)) = self.w.layouts.get(owner).and_then(|l| l.fields.get(name))
                else {
                    self.b.ins().trap(UNSUPPORTED);
                    return Ok(true);
                };
                let object = if op == 0xB4 { d - 1 } else { d - 2 };
                if let Some(key) = self.site_key(&f.stack[object]) {
                    let v = self.field_var(key, at, t);
                    if op == 0xB4 {
                        let x = self.b.use_var(v);
                        self.set(d - 1, t, x);
                    } else {
                        let x = self.get(d - 1, t);
                        self.b.def_var(v, x);
                    }
                    return Ok(false);
                }
                if op == 0xB4 {
                    let obj = self.get(d - 1, i64t);
                    self.null_check(obj);
                    let v = self.b.ins().load(t, MemFlagsData::trusted(), obj, at);
                    self.set(d - 1, t, v);
                } else {
                    let v = self.get(d - 1, t);
                    let obj = self.get(d - 2, i64t);
                    self.null_check(obj);
                    self.b.ins().store(MemFlagsData::trusted(), v, obj, at);
                }
            }
            0xB6..=0xB9 => return self.invoke(insn, f),
            0xBA => {
                let site = self
                    .w
                    .lambda_at
                    .get(&(self.owner.to_owned(), insn.at))
                    .copied();
                let Some(n) = site else {
                    self.b.ins().trap(UNSUPPORTED);
                    return Ok(true);
                };
                let l = &self.w.lambdas[n];
                let obj = match self.w.singleton_at[n] {
                    Some(at) => self.b.ins().iconst(i64t, at),
                    None => {
                        let k = l.captures.len();
                        let size = self.b.ins().iconst(i64t, 8 + 8 * k as i64);
                        let header = self.b.ins().iconst(i64t, self.w.lambda_vt[n]);
                        let obj = alloc_in(&mut self.b, self.w, size, header);
                        for (j, c) in l.captures.clone().iter().enumerate() {
                            let t = desc_type(c).ok_or("a bad capture")?;
                            let v = self.get(d - k + j, t);
                            self.b
                                .ins()
                                .store(MemFlagsData::trusted(), v, obj, 8 + 8 * j as i32);
                        }
                        obj
                    }
                };
                let k = l.captures.len();
                self.set(d - k, i64t, obj);
            }
            0xBB => {
                let Operand::Constant(c) = insn.operand else {
                    unreachable!()
                };
                let class = self.class.class_name(c).map_err(me)?.to_owned();
                let Some(l) = self.w.layouts.get(&class) else {
                    self.b.ins().trap(UNSUPPORTED);
                    return Ok(true);
                };
                let (size, vt) = (l.size, l.vtable as i64);
                self.init(&class);
                if self.scalar.contains(&insn.at) {
                    // Kept in variables: its fields start at zero.
                    let key = (self.body, insn.at);
                    let fields: Vec<(i32, Type)> = l.fields.values().copied().collect();
                    for (at, t) in fields {
                        let v = self.field_var(key, at, t);
                        let z = zero(&mut self.b, t);
                        self.b.def_var(v, z);
                    }
                    let one = self.b.ins().iconst(i64t, 1);
                    self.set(d, i64t, one);
                    return Ok(false);
                }
                let size = self.b.ins().iconst(i64t, i64::from(size));
                let header = self.b.ins().iconst(i64t, vt);
                let obj = self.alloc(size, header);
                self.set(d, i64t, obj);
            }
            0xBC | 0xBD => {
                let elem = if op == 0xBC {
                    let Operand::ArrayType(t) = insn.operand else {
                        unreachable!()
                    };
                    match t {
                        4 | 8 => 1,
                        5 | 9 => 2,
                        6 | 10 => 4,
                        _ => 8,
                    }
                } else {
                    8
                };
                let len = self.get(d - 1, i32t);
                let neg = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, len, 0);
                self.b.ins().trapnz(neg, BOUNDS);
                let len64 = self.b.ins().uextend(i64t, len);
                let bytes = self.b.ins().imul_imm_s(len64, elem);
                let size = self.b.ins().iadd_imm_s(bytes, 16);
                let header = self.b.ins().iconst(i64t, 0);
                let arr = self.alloc(size, header);
                self.b.ins().store(MemFlagsData::trusted(), len64, arr, 8);
                self.set(d - 1, i64t, arr);
            }
            0xBE => {
                let arr = self.get(d - 1, i64t);
                self.null_check(arr);
                let len = self.b.ins().load(i32t, MemFlagsData::trusted(), arr, 8);
                self.set(d - 1, i32t, len);
            }
            0xBF => {
                self.b.ins().trap(UNSUPPORTED);
                return Ok(true);
            }
            0xC0 | 0xC1 => {
                let Operand::Constant(c) = insn.operand else {
                    unreachable!()
                };
                let class = self.class.class_name(c).map_err(me)?.to_owned();
                let obj = self.get(d - 1, i64t);
                // Arrays and classes outside the closed world: not tested yet.
                let r = match self.w.ids.get(class.as_str()) {
                    Some(&id) if !class.starts_with('[') => {
                        let id = self.b.ins().iconst(i32t, i64::from(id));
                        self.host("host_is_instance", &[obj, id])
                    }
                    _ => None,
                };
                if op == 0xC0 {
                    if let Some(ok) = r {
                        // null passes a checkcast.
                        let null = self.b.ins().icmp_imm_s(IntCC::Equal, obj, 0);
                        let null = self.b.ins().uextend(i32t, null);
                        let pass = self.b.ins().bor(ok, null);
                        self.b.ins().trapz(pass, CAST);
                    }
                } else {
                    let r = match r {
                        Some(r) => r,
                        None => {
                            let nn = self.b.ins().icmp_imm_s(IntCC::NotEqual, obj, 0);
                            self.b.ins().uextend(i32t, nn)
                        }
                    };
                    self.set(d - 1, i32t, r);
                }
            }
            0xC2 | 0xC3 => {}
            _ => {
                self.b.ins().trap(UNSUPPORTED);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The address of element `idx` of array `arr`, after the null and
    /// bounds tests.
    fn element_addr(&mut self, arr: Value, idx: Value, array_ty: &str) -> Value {
        self.null_check(arr);
        let len = self
            .b
            .ins()
            .load(types::I32, MemFlagsData::trusted(), arr, 8);
        let out = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        self.b.ins().trapnz(out, BOUNDS);
        let (size, _) = element(array_ty);
        let i = self.b.ins().uextend(types::I64, idx);
        let off = self.b.ins().imul_imm_s(i, i64::from(size));
        let base = self.b.ins().iadd(arr, off);
        self.b.ins().iadd_imm_s(base, 16)
    }

    fn stack_op(&mut self, op: u8, f: &Frame) -> Result<(), String> {
        let d = f.stack.len();
        let t = |k: usize| kind_type(&f.stack[d - 1 - k]).unwrap_or(types::I64);
        let wide = |k: usize| f.stack[d - 1 - k].wide();
        // Copies stack entries: (from depth, to depth) by value type.
        let moves: Vec<(usize, usize)> = match op {
            0x59 => vec![(d - 1, d)],
            0x5A => vec![(d - 1, d), (d - 2, d - 1), (d, d - 2)],
            0x5B if wide(1) => vec![(d - 1, d), (d - 2, d - 1), (d, d - 2)],
            0x5B => vec![(d - 1, d), (d - 2, d - 1), (d - 3, d - 2), (d, d - 3)],
            0x5C if wide(0) => vec![(d - 1, d)],
            0x5C => vec![(d - 2, d), (d - 1, d + 1)],
            0x5D if wide(0) => vec![(d - 1, d), (d - 2, d - 1), (d, d - 2)],
            0x5D => vec![
                (d - 1, d + 1),
                (d - 2, d),
                (d - 3, d - 1),
                (d + 1, d - 2),
                (d, d - 3),
            ],
            0x5F => vec![(d - 1, d), (d - 2, d - 1), (d, d - 2)],
            _ => return Err("dup2_x2 (not in the prototype)".into()),
        };
        // Types by original position; copies above the stack take the type
        // of what they copy.
        let mut ty: HashMap<usize, Type> = (0..d).map(|k| (d - 1 - k, t(k))).collect();
        let mut vals: HashMap<usize, Value> = HashMap::new();
        for &(from, _) in &moves {
            if from < d && !vals.contains_key(&from) {
                let tt = ty[&from];
                let v = self.get(from, tt);
                vals.insert(from, v);
            }
        }
        for &(from, to) in &moves {
            let v = vals[&from];
            let tt = ty[&from];
            vals.insert(to, v);
            ty.insert(to, tt);
            self.set(to, tt, v);
        }
        Ok(())
    }

    fn arith(&mut self, op: u8, d: usize, st: &dyn Fn(usize) -> Type) -> Result<(), String> {
        let t = st(0);
        // Negation.
        if (0x74..=0x77).contains(&op) {
            let a = self.get(d - 1, t);
            let r = if t.is_int() {
                self.b.ins().ineg(a)
            } else {
                self.b.ins().fneg(a)
            };
            self.set(d - 1, t, r);
            return Ok(());
        }
        // Shifts: the count is an int.
        if (0x78..=0x7D).contains(&op) {
            let vt = st(1);
            let n = self.get(d - 1, types::I32);
            let a = self.get(d - 2, vt);
            let n = if vt == types::I64 {
                self.b.ins().uextend(types::I64, n)
            } else {
                n
            };
            let r = match (op - 0x78) / 2 {
                0 => self.b.ins().ishl(a, n),
                1 => self.b.ins().sshr(a, n),
                _ => self.b.ins().ushr(a, n),
            };
            self.set(d - 2, vt, r);
            return Ok(());
        }
        let b = self.get(d - 1, t);
        let a = self.get(d - 2, t);
        let r = match op {
            0x60..=0x63 => {
                if t.is_int() {
                    self.b.ins().iadd(a, b)
                } else {
                    self.b.ins().fadd(a, b)
                }
            }
            0x64..=0x67 => {
                if t.is_int() {
                    self.b.ins().isub(a, b)
                } else {
                    self.b.ins().fsub(a, b)
                }
            }
            0x68..=0x6B => {
                if t.is_int() {
                    self.b.ins().imul(a, b)
                } else {
                    self.b.ins().fmul(a, b)
                }
            }
            0x6C..=0x73 if !t.is_int() => {
                if op <= 0x6F {
                    self.b.ins().fdiv(a, b)
                } else if t == types::F64 {
                    self.host("host_fmod", &[a, b]).ok_or("fmod")?
                } else {
                    self.host("host_fmodf", &[a, b]).ok_or("fmodf")?
                }
            }
            0x6C..=0x73 => {
                // Java: division by zero throws; MIN / -1 is MIN, MIN % -1 is 0.
                self.b.ins().trapz(b, TrapCode::INTEGER_DIVISION_BY_ZERO);
                let minus = self.b.ins().icmp_imm_s(IntCC::Equal, b, -1);
                let one = self.b.ins().iconst(t, 1);
                let safe = self.b.ins().select(minus, one, b);
                if op <= 0x6F {
                    let q = self.b.ins().sdiv(a, safe);
                    let neg = self.b.ins().ineg(a);
                    self.b.ins().select(minus, neg, q)
                } else {
                    let r = self.b.ins().srem(a, safe);
                    let zero = self.b.ins().iconst(t, 0);
                    self.b.ins().select(minus, zero, r)
                }
            }
            0x7E | 0x7F => self.b.ins().band(a, b),
            0x80 | 0x81 => self.b.ins().bor(a, b),
            0x82 | 0x83 => self.b.ins().bxor(a, b),
            _ => return Err(format!("arithmetic {op:#x}")),
        };
        self.set(d - 2, t, r);
        Ok(())
    }

    fn convert(&mut self, op: u8, d: usize) -> Result<(), String> {
        use types::{F32, F64, I8, I16, I32, I64};
        let (from, to) = match op {
            0x85 => (I32, I64),
            0x86 => (I32, F32),
            0x87 => (I32, F64),
            0x88 => (I64, I32),
            0x89 => (I64, F32),
            0x8A => (I64, F64),
            0x8B => (F32, I32),
            0x8C => (F32, I64),
            0x8D => (F32, F64),
            0x8E => (F64, I32),
            0x8F => (F64, I64),
            0x90 => (F64, F32),
            _ => (I32, I32),
        };
        let a = self.get(d - 1, from);
        let ins = self.b.ins();
        let r = match op {
            0x85 => ins.sextend(I64, a),
            0x86 | 0x87 | 0x89 | 0x8A => ins.fcvt_from_sint(to, a),
            0x88 => ins.ireduce(I32, a),
            0x8B | 0x8C | 0x8E | 0x8F => ins.fcvt_to_sint_sat(to, a),
            0x8D => ins.fpromote(F64, a),
            0x90 => ins.fdemote(F32, a),
            0x91 => {
                let x = ins.ireduce(I8, a);
                self.b.ins().sextend(I32, x)
            }
            0x92 => {
                let x = ins.ireduce(I16, a);
                self.b.ins().uextend(I32, x)
            }
            0x93 => {
                let x = ins.ireduce(I16, a);
                self.b.ins().sextend(I32, x)
            }
            _ => return Err(format!("conversion {op:#x}")),
        };
        self.set(d - 1, to, r);
        Ok(())
    }

    fn invoke(&mut self, insn: &Instruction, f: &Frame) -> Result<bool, String> {
        let op = insn.opcode;
        let d = f.stack.len();
        let (Operand::Constant(c) | Operand::Interface(c, _)) = insn.operand else {
            unreachable!()
        };
        let (owner, name, desc) = self.class.member_ref(c).map_err(|e| e.to_string())?;
        let (owner, name, desc) = (owner.to_owned(), name.to_owned(), desc.to_owned());
        let m = Method::parse(&desc).ok_or("a bad descriptor")?;
        let n = m.params.len();
        let receiver = op != 0xB8;
        let base = d - n - usize::from(receiver);
        let mut args = Vec::new();
        for k in 0..usize::from(receiver) + n {
            let t = kind_type(&f.stack[base + k]).unwrap_or(types::I64);
            args.push(self.get(base + k, t));
        }
        let ret = (m.ret != "V").then(|| desc_type(&m.ret)).flatten();
        // JDK methods as instructions.
        if op == 0xB8
            && let Some(v) = self.intrinsic(&owner, &name, &desc, &args)
        {
            if let (Some(t), Some(v)) = (ret, v) {
                self.set(base, t, v);
            }
            return Ok(false);
        }
        // Object's constructor does nothing.
        if op == 0xB7 && name == "<init>" && owner == OBJECT {
            return Ok(false);
        }
        // A call on an object kept in variables: its code, here.
        if receiver && let Some(key) = self.site_key(&f.stack[base]) {
            let class = match &f.stack[base] {
                Ty::Site(_, c) if op != 0xB7 => c.to_string(),
                _ => owner.clone(),
            };
            let declaring = self
                .w
                .resolve(&class, &name, &desc)
                .ok_or("no method for a kept object")?;
            let result = self.inline(key, &declaring, &name, &desc, &args[1..], ret)?;
            if let (Some(t), Some(v)) = (ret, result) {
                self.set(base, t, v);
            }
            return Ok(false);
        }
        let result = match op {
            0xB8 | 0xB7 => {
                let Some(declaring) = self.w.resolve(&owner, &name, &desc) else {
                    self.b.ins().trap(UNSUPPORTED);
                    return Ok(true);
                };
                if op == 0xB8 {
                    self.init(&declaring);
                } else {
                    self.null_check(args[0]);
                }
                let id = self.w.funcs[&export_name(&declaring, &name, &desc)];
                let fr = self.module.declare_func_in_func(id, self.b.func);
                let call = self.b.ins().call(fr, &args);
                self.b.inst_results(call).first().copied()
            }
            _ => {
                self.null_check(args[0]);
                let key = (name.clone(), desc.clone());
                let single = self.w.implementations.get(&key) == Some(&1);
                let direct = single
                    .then(|| self.w.resolve(&owner, &name, &desc))
                    .flatten()
                    .filter(|c| {
                        self.w
                            .by_name
                            .get(c.as_str())
                            .is_some_and(|cf| cf.access & INTERFACE == 0)
                    });
                match (direct, self.w.selectors.get(&key)) {
                    (Some(declaring), _) => {
                        let id = self.w.funcs[&export_name(&declaring, &name, &desc)];
                        let fr = self.module.declare_func_in_func(id, self.b.func);
                        let call = self.b.ins().call(fr, &args);
                        self.b.inst_results(call).first().copied()
                    }
                    (None, Some(&sel)) => {
                        let vt = self
                            .b
                            .ins()
                            .load(types::I64, MemFlagsData::trusted(), args[0], 0);
                        let fp = self.b.ins().load(
                            types::I64,
                            MemFlagsData::trusted(),
                            vt,
                            (8 * (1 + sel)) as i32,
                        );
                        let sig = signature(self.module, true, &desc).ok_or("a bad signature")?;
                        let sr = self.b.import_signature(sig);
                        let call = self.b.ins().call_indirect(sr, fp, &args);
                        self.b.inst_results(call).first().copied()
                    }
                    (None, None) => {
                        self.b.ins().trap(UNSUPPORTED);
                        return Ok(true);
                    }
                }
            }
        };
        if let (Some(t), Some(v)) = (ret, result) {
            self.set(base, t, v);
        }
        let _ = (self.h, self.w.sigs, self.w.ptr);
        Ok(false)
    }

    /// A JDK static method as instructions: `Some(result)` when it is one.
    fn intrinsic(
        &mut self,
        owner: &str,
        name: &str,
        desc: &str,
        a: &[Value],
    ) -> Option<Option<Value>> {
        let math = matches!(owner, "java/lang/Math" | "java/lang/StrictMath");
        let ins = self.b.ins();
        Some(Some(match (owner, name, desc) {
            (_, "floor", "(D)D") if math => ins.floor(a[0]),
            (_, "ceil", "(D)D") if math => ins.ceil(a[0]),
            (_, "sqrt", "(D)D") if math => ins.sqrt(a[0]),
            (_, "rint", "(D)D") if math => ins.nearest(a[0]),
            (_, "abs", "(D)D" | "(F)F") if math => ins.fabs(a[0]),
            (_, "abs", "(I)I" | "(J)J") if math => ins.iabs(a[0]),
            (_, "max", "(DD)D" | "(FF)F") if math => ins.fmax(a[0], a[1]),
            (_, "min", "(DD)D" | "(FF)F") if math => ins.fmin(a[0], a[1]),
            (_, "max", "(II)I" | "(JJ)J") if math => ins.smax(a[0], a[1]),
            (_, "min", "(II)I" | "(JJ)J") if math => ins.smin(a[0], a[1]),
            (_, "sin", "(D)D") if math => return Some(self.host("host_sin", a)),
            (_, "cos", "(D)D") if math => return Some(self.host("host_cos", a)),
            ("java/lang/Double", "doubleToRawLongBits", "(D)J") => {
                ins.bitcast(types::I64, MemFlagsData::new(), a[0])
            }
            ("java/lang/Double", "longBitsToDouble", "(J)D") => {
                ins.bitcast(types::F64, MemFlagsData::new(), a[0])
            }
            ("java/lang/Float", "floatToRawIntBits", "(F)I") => {
                ins.bitcast(types::I32, MemFlagsData::new(), a[0])
            }
            ("java/lang/Float", "intBitsToFloat", "(I)F") => {
                ins.bitcast(types::F32, MemFlagsData::new(), a[0])
            }
            ("java/lang/Double" | "java/lang/Float", "isNaN", _) => {
                let c = ins.fcmp(FloatCC::Unordered, a[0], a[0]);
                self.b.ins().uextend(types::I32, c)
            }
            _ => return None,
        }))
    }
}

fn zero(b: &mut FunctionBuilder<'_>, t: Type) -> Value {
    if t == types::F32 {
        b.ins().f32const(0.0)
    } else if t == types::F64 {
        b.ins().f64const(0.0)
    } else {
        b.ins().iconst(t, 0)
    }
}

fn int_cc(n: u8) -> IntCC {
    [
        IntCC::Equal,
        IntCC::NotEqual,
        IntCC::SignedLessThan,
        IntCC::SignedGreaterThanOrEqual,
        IntCC::SignedGreaterThan,
        IntCC::SignedLessThanOrEqual,
    ][usize::from(n)]
}
