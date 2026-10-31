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
    rt,
    types::{Classes, Frame, Options, Stepper, THIS_SITE, Ty, infer_with, successors},
};
use cranelift_codegen::{
    ir::{
        AbiParam, Block, BlockArg, InstBuilder, MemFlagsData, Signature, TrapCode, Type, Value,
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
const SYNCHRONIZED: u16 = 0x0020;
const OBJECT: &str = "java/lang/Object";
/// The trap of code the prototype does not compile.
const UNSUPPORTED: TrapCode = TrapCode::unwrap_user(1);
const NULL: TrapCode = TrapCode::unwrap_user(2);
const ARENA: TrapCode = TrapCode::unwrap_user(5);

/// A class's layout.
struct Layout {
    /// Instance fields: name → (offset, kind of value).
    fields: HashMap<String, (i32, Type)>,
    size: i32,
    /// Its vtable (filled when the code is ready).
    vtable: *mut u64,
}

use crate::heap::{self, EXCEPTION};

/// The JDK's exception classes this backend knows (public API): each one's
/// superclass. A class of the closed world that extends one of them is
/// caught by a handler of any of its superclasses.
fn jdk_super(class: &str) -> Option<&'static str> {
    Some(match class {
        "java/lang/Exception" | "java/lang/Error" => "java/lang/Throwable",
        "java/lang/RuntimeException"
        | "java/io/IOException"
        | "java/lang/InterruptedException"
        | "java/lang/ReflectiveOperationException"
        | "java/lang/CloneNotSupportedException" => "java/lang/Exception",
        "java/lang/IllegalStateException"
        | "java/lang/IllegalArgumentException"
        | "java/lang/IllegalMonitorStateException"
        | "java/lang/NullPointerException"
        | "java/lang/UnsupportedOperationException"
        | "java/lang/IndexOutOfBoundsException"
        | "java/lang/ClassCastException"
        | "java/lang/ArithmeticException"
        | "java/lang/NegativeArraySizeException"
        | "java/lang/ArrayStoreException"
        | "java/lang/SecurityException"
        | "java/lang/MatchException"
        | "java/util/ConcurrentModificationException"
        | "java/util/concurrent/RejectedExecutionException"
        | "java/util/NoSuchElementException"
        | "java/io/UncheckedIOException" => "java/lang/RuntimeException",
        "java/lang/NumberFormatException"
        | "java/lang/IllegalThreadStateException"
        | "java/util/IllegalFormatException" => "java/lang/IllegalArgumentException",
        "java/lang/ArrayIndexOutOfBoundsException"
        | "java/lang/StringIndexOutOfBoundsException" => "java/lang/IndexOutOfBoundsException",
        "java/lang/ClassNotFoundException" | "java/lang/NoSuchMethodException" => {
            "java/lang/ReflectiveOperationException"
        }
        "java/lang/AssertionError" | "java/lang/VirtualMachineError" => "java/lang/Error",
        "java/lang/StackOverflowError"
        | "java/lang/OutOfMemoryError"
        | "java/lang/InternalError" => "java/lang/VirtualMachineError",
        "java/io/ObjectStreamException" => "java/io/IOException",
        "java/io/InvalidObjectException" => "java/io/ObjectStreamException",
        "java/nio/ReadOnlyBufferException" => "java/lang/UnsupportedOperationException",
        "java/lang/Throwable" => "java/lang/Object",
        _ => return None,
    })
}

/// The JDK exception classes of [`jdk_super`] (Throwable itself is in rt.rs).
const JDK_EXCEPTIONS: &[&str] = &[
    "java/lang/Exception",
    "java/lang/Error",
    "java/lang/RuntimeException",
    "java/io/IOException",
    "java/lang/InterruptedException",
    "java/lang/ReflectiveOperationException",
    "java/lang/CloneNotSupportedException",
    "java/lang/IllegalStateException",
    "java/lang/IllegalArgumentException",
    "java/lang/NullPointerException",
    "java/lang/UnsupportedOperationException",
    "java/lang/IndexOutOfBoundsException",
    "java/lang/ClassCastException",
    "java/lang/ArithmeticException",
    "java/lang/NegativeArraySizeException",
    "java/lang/ArrayStoreException",
    "java/lang/SecurityException",
    "java/lang/MatchException",
    "java/util/ConcurrentModificationException",
    "java/util/NoSuchElementException",
    "java/io/UncheckedIOException",
    "java/lang/NumberFormatException",
    "java/lang/ArrayIndexOutOfBoundsException",
    "java/lang/StringIndexOutOfBoundsException",
    "java/lang/ClassNotFoundException",
    "java/lang/NoSuchMethodException",
    "java/lang/AssertionError",
    "java/lang/VirtualMachineError",
    "java/lang/InternalError",
    "java/io/ObjectStreamException",
    "java/io/InvalidObjectException",
    "java/nio/ReadOnlyBufferException",
    "java/lang/StackOverflowError",
    "java/lang/OutOfMemoryError",
    "java/lang/IllegalMonitorStateException",
    "java/lang/IllegalThreadStateException",
    "java/util/concurrent/RejectedExecutionException",
    "java/util/IllegalFormatException",
];

/// The classes the runtime library has (Rust versions, and the JDK
/// exceptions it knows): a class of the JDK compiled into the program must
/// not stand on one of them (docs/java-compat/compiler.md 4.4, level 3).
pub fn runtime_classes() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = rt::builtins().iter().map(|b| b.name).collect();
    names.extend_from_slice(JDK_EXCEPTIONS);
    names
}

/// The program's type tables, for its type tests and catch clauses (the
/// compiled code passes their address: any thread may test).
struct TypeTables {
    /// Each class id's ancestors' ids (itself, superclasses, interfaces), sorted.
    ancestors: Vec<Vec<u32>>,
    /// Each class id's ancestors by name, the JDK's exception classes too.
    names: Vec<HashSet<String>>,
    /// The classes `catch` clauses name, by index.
    catches: Vec<String>,
    /// Each class id's bits: one per class the code tests against (its
    /// number in `World::tested`), set when the class is one of those.
    /// `bit_table` holds their addresses by class id, for the code.
    bits: Vec<Box<[u64]>>,
    bit_table: Box<[u64]>,
}

extern "C" fn host_catches(exc: u64, catch: u32, tables: *const TypeTables) -> u32 {
    if exc == 0 {
        return 0;
    }
    // SAFETY: the program's tables, alive while its code runs.
    let tables = unsafe { &*tables };
    // SAFETY: as in host_is_instance.
    let id = unsafe { *(*(exc as *const *const u64)) } as usize;
    let Some(name) = tables.catches.get(catch as usize) else {
        return 0;
    };
    tables.names.get(id).is_some_and(|set| set.contains(name)) as u32
}

extern "C" fn host_missing() {
    panic!("a virtual method with no implementation in the compiled classes was called");
}

pub struct Native {
    module: JITModule,
    exports: HashMap<String, FuncId>,
    /// Each class's init function (its `<clinit>`, once, after its
    /// superclass's), by class name.
    class_inits: HashMap<String, FuncId>,
    /// Methods that compile to a trap, with why.
    pub unsupported: Vec<(String, String)>,
    /// Instructions compiled as stops (code outside the compiled classes or
    /// not compiled yet), by why.
    pub stops: HashMap<String, usize>,
    // Memory the code points at, kept alive.
    /// The heap, and the context of the thread that compiled the code.
    main: Box<heap::Context>,
    _types: Box<TypeTables>,
    heap: Box<heap::Heap>,
    _statics: Vec<u64>,
    _vtables: Vec<Vec<u64>>,
    /// Boxed: each one's address is baked into the code.
    #[allow(clippy::vec_box)]
    _singletons: Vec<Box<u64>>,
    _strings: HashMap<String, Box<[u64]>>,
    /// The runtime library's view of the program (installed for this
    /// thread), and the cached boxes it hands out.
    _rt: Box<rt::Rt>,
    _boxes: Vec<Box<[u64]>>,
    _classes: Vec<Box<[u64]>>,
    _other_classes: Vec<(String, Box<[u64]>, Box<[u64]>)>,
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

/// A function's size for inlining: the instructions of the blocks from
/// which it returns normally. A path that only ends in a throw, a trap or
/// an exception's way out (cold blocks) is not what a caller pays for.
fn hot_size(f: &cranelift_codegen::ir::Function) -> usize {
    use cranelift_codegen::flowgraph::ControlFlowGraph;
    let cfg = ControlFlowGraph::with_function(f);
    let mut hot = std::collections::HashSet::new();
    let mut todo: Vec<Block> = f
        .layout
        .blocks()
        .filter(|&b| {
            !f.layout.is_cold(b)
                && f.layout
                    .last_inst(b)
                    .is_some_and(|i| f.dfg.insts[i].opcode().is_return())
        })
        .collect();
    while let Some(b) = todo.pop() {
        if !f.layout.is_cold(b) && hot.insert(b) {
            todo.extend(cfg.pred_iter(b).map(|p| p.block));
        }
    }
    hot.iter().map(|&b| f.layout.block_insts(b).count()).sum()
}

/// Code that cannot run: says why (stop reason `why`), then traps.
fn say_and_trap(b: &mut FunctionBuilder<'_>, why: u32) {
    let mut raw = Signature::new(b.func.signature.call_conv);
    raw.params.push(AbiParam::new(types::I32));
    let sr = b.import_signature(raw);
    let f = b.ins().iconst(types::I64, rt::stopped as *const () as i64);
    let id = b.ins().iconst(types::I32, i64::from(why));
    b.ins().call_indirect(sr, f, &[id]);
    b.ins().trap(UNSUPPORTED);
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
extern "C" fn host_is_instance(obj: u64, target: u32, tables: *const TypeTables) -> u32 {
    if obj == 0 {
        return 0;
    }
    // SAFETY: obj is an object the compiled code made: its first word is
    // its vtable, whose first word is its class id.
    let id = unsafe { *(*(obj as *const *const u64)) } as u32;
    // SAFETY: the program's tables, alive while its code runs.
    let tables = unsafe { &*tables };
    tables
        .ancestors
        .get(id as usize)
        .is_some_and(|a| a.binary_search(&target).is_ok()) as u32
}

impl Native {
    pub fn compile(classes: &[ClassFile]) -> Result<Native, String> {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
        // The verifier checks the compiler, not the program: debug builds only.
        if !cfg!(debug_assertions) {
            flags
                .set("enable_verifier", "false")
                .map_err(|e| e.to_string())?;
        }
        let isa = cranelift_native::builder()
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        // (The methods' workers compile with it while the module is busy.)
        let compile_isa = isa.clone();
        let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
        // All code in one reserved region: a call between two methods is a
        // 32-bit relative jump, which separate allocations (around the 4 GiB
        // object arena) can put out of reach.
        jb.memory_provider(Box::new(
            cranelift_jit::ArenaMemoryProvider::new_with_size(1 << 30)
                .map_err(|e| e.to_string())?,
        ));
        jb.symbol("host_sin", host_sin as *const u8);
        jb.symbol("host_cos", host_cos as *const u8);
        jb.symbol("host_fmod", host_fmod as *const u8);
        jb.symbol("host_fmodf", host_fmodf as *const u8);
        jb.symbol("host_is_instance", host_is_instance as *const u8);
        jb.symbol("host_catches", host_catches as *const u8);
        jb.symbol("host_missing", host_missing as *const u8);
        let mut module = JITModule::new(jb);
        let ptr = module.target_config().pointer_type();

        let by_name: HashMap<&str, &ClassFile> = classes
            .iter()
            .filter_map(|c| Some((c.name().ok()?, c)))
            .collect();
        let hierarchy = Classes::new(classes);
        let mut names: Vec<&str> = by_name.keys().copied().collect();
        names.sort_unstable();
        // Class ids in preorder of the superclass tree: a class's subclasses
        // follow it, so the classes that respond to a method are a few runs
        // of ids, which the dispatch table packs closely and quickly.
        names = {
            let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
            let mut roots = Vec::new();
            for &n in &names {
                match by_name[n]
                    .super_name()
                    .ok()
                    .flatten()
                    .filter(|s| by_name.contains_key(s))
                {
                    Some(s) => children.entry(s).or_default().push(n),
                    None => roots.push(n),
                }
            }
            let mut out = Vec::with_capacity(names.len());
            let mut seen: HashSet<&str> = HashSet::new();
            let mut stack: Vec<&str> = roots.into_iter().rev().collect();
            while let Some(n) = stack.pop() {
                if !seen.insert(n) {
                    continue;
                }
                out.push(n);
                if let Some(c) = children.get(n) {
                    stack.extend(c.iter().rev());
                }
            }
            // (A class in a superclass cycle, which no valid program has.)
            out.extend(names.iter().filter(|n| !seen.contains(*n)));
            out
        };
        let mut ids: HashMap<&str, u32> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, i as u32 + 1))
            .collect();
        let superclass = |c: &str| -> Option<&str> { by_name.get(c)?.super_name().ok().flatten() };

        // HORIZON_NATIVE_TIMES=1: how long each phase takes.
        let started = std::time::Instant::now();
        let timed = std::env::var_os("HORIZON_NATIVE_TIMES").is_some();
        let phase = |what: &str| {
            if timed {
                // (Resident memory, from /proc on Linux.)
                let rss_mb = std::fs::read_to_string("/proc/self/statm")
                    .ok()
                    .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
                    .map_or(0, |pages| pages * 4096 >> 20);
                eprintln!(
                    "{:8.1} s  {rss_mb:6} MB  {what}",
                    started.elapsed().as_secs_f64()
                );
            }
        };
        let (lambdas, lambda_at, called, created) = prepass(classes);
        phase("prepass");
        // The built-in classes (rt.rs), and the JDK's exception classes (all
        // with Throwable's fields), unless the program has its own.
        let mut builtin_list = rt::builtins();
        for &n in JDK_EXCEPTIONS {
            builtin_list.push(rt::Builtin {
                name: n,
                super_name: jdk_super(n),
                interfaces: &[],
                size: 24,
                is_final: false,
                is_interface: false,
                methods: rt::exception_methods(n),
            });
        }
        builtin_list.retain(|b| !by_name.contains_key(b.name));
        let builtin: HashMap<&'static str, &rt::Builtin> =
            builtin_list.iter().map(|b| (b.name, b)).collect();
        phase("built-ins");
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
        for b in &builtin_list {
            for m in &b.methods {
                if !m.is_static && m.name != "<init>" {
                    let n = selectors.len();
                    selectors
                        .entry((m.name.to_owned(), m.desc.to_owned()))
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
        for b in &builtin_list {
            for m in &b.methods {
                if !m.is_static && m.name != "<init>" {
                    *implementations
                        .entry((m.name.to_owned(), m.desc.to_owned()))
                        .or_default() += 1;
                }
            }
        }
        let builtin_size: HashMap<&str, i32> =
            builtin_list.iter().map(|b| (b.name, b.size)).collect();

        phase("selectors and implementations");
        // Layouts.
        let mut layouts: HashMap<String, Layout> = HashMap::new();
        let mut vtables: Vec<Vec<u64>> = Vec::new();
        fn layout_of<'a>(
            c: &'a str,
            by_name: &HashMap<&'a str, &'a ClassFile>,
            ids: &HashMap<&str, u32>,
            builtin_size: &HashMap<&str, i32>,
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
                layout_of(s, by_name, ids, builtin_size, layouts, vtables, selectors);
                if let Some(l) = layouts.get(s) {
                    fields = l.fields.clone();
                    size = l.size;
                } else if let Some(&b) = builtin_size.get(s) {
                    // A built-in superclass's fields come first.
                    size = b;
                }
            }
            for f in &class.fields {
                if f.access & STATIC == 0 {
                    let t = desc_type(&f.descriptor).unwrap_or(types::I64);
                    fields.insert(f.name.clone(), (size, t));
                    size += 8;
                }
            }
            // A class's record: its id (its methods are in the dispatch table).
            let vt = vec![u64::from(ids[c])];
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
                &builtin_size,
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
            let vt = vec![(names.len() + 1 + i) as u64];
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
        // The built-in classes: ids after the lambdas', vtables, layouts.
        let builtin_base = names.len() + 1 + lambdas.len();
        for (k, b) in builtin_list.iter().enumerate() {
            let id = (builtin_base + k) as u32;
            ids.insert(b.name, id);
            let vt = vec![u64::from(id)];
            vtables.push(vt);
            let vtable = vtables.last_mut().expect("just pushed").as_mut_ptr();
            layouts.insert(
                b.name.to_owned(),
                Layout {
                    fields: HashMap::new(),
                    size: b.size,
                    vtable,
                },
            );
        }
        let builtins: Vec<&str> = builtin_list.iter().map(|b| b.name).collect();
        phase("layouts");
        // Ancestors by name, for catch clauses.
        let mut names_of: Vec<HashSet<String>> =
            vec![HashSet::new(); builtin_base + builtins.len()];
        let chain = |start: &str| -> HashSet<String> {
            let mut set = HashSet::new();
            let mut todo = vec![start.to_owned()];
            while let Some(c) = todo.pop() {
                if !set.insert(c.clone()) {
                    continue;
                }
                match by_name.get(c.as_str()) {
                    Some(cf) => {
                        todo.extend(cf.super_name().ok().flatten().map(str::to_owned));
                        todo.extend(
                            cf.interfaces
                                .iter()
                                .filter_map(|&i| cf.class_name(i).ok().map(str::to_owned)),
                        );
                    }
                    None => match builtin.get(c.as_str()) {
                        Some(b) => {
                            todo.extend(b.super_name.map(str::to_owned));
                            todo.extend(b.interfaces.iter().map(|i| (*i).to_owned()));
                        }
                        None => todo.extend(jdk_super(&c).map(str::to_owned)),
                    },
                }
            }
            set
        };
        for n in &names {
            names_of[ids[n] as usize] = chain(n);
        }
        for (i, l) in lambdas.iter().enumerate() {
            names_of[names.len() + 1 + i] = chain(&l.interface);
        }
        for (k, n) in builtins.iter().enumerate() {
            names_of[builtin_base + k] = chain(n);
        }
        let names_by_id = names_of.clone();

        phase("ancestors");
        // The global dispatch table (row displacement): selector `s` of a
        // class with id `c` is entry `c + offsets[s]`, two words: its method
        // and `s + 1` (so the runtime can tell an entry is that selector's).
        // A class has entries for the selectors it responds to: the methods
        // it and its ancestors declare, and a lambda's interface method.
        let declared = |class: &str| -> Vec<usize> {
            let key = |n: &str, d: &str| selectors.get(&(n.to_owned(), d.to_owned())).copied();
            if let Some(cf) = by_name.get(class) {
                cf.methods
                    .iter()
                    .filter(|m| m.access & STATIC == 0 && !m.name.starts_with('<'))
                    .filter_map(|m| key(&m.name, &m.descriptor))
                    .collect()
            } else if let Some(b) = builtin.get(class) {
                b.methods
                    .iter()
                    .filter(|m| !m.is_static && !m.name.starts_with('<'))
                    .filter_map(|m| key(m.name, m.desc))
                    .collect()
            } else {
                Vec::new()
            }
        };
        let mut declared_by: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut responders: Vec<Vec<usize>> = vec![Vec::new(); builtin_base + builtins.len()];
        // Types with an interface ancestor outside the program (`Supplier`,
        // `List`...; a built-in interface lists only what the runtime
        // implements): what they declare is not all known here.
        let mut opaque: Vec<bool> = vec![false; responders.len()];
        for (id, ancestors) in names_by_id.iter().enumerate() {
            let mut sels: Vec<usize> = Vec::new();
            opaque[id] = ancestors.iter().any(|a| {
                !by_name.contains_key(a.as_str())
                    && builtin.get(a.as_str()).is_none_or(|b| b.is_interface)
            });
            for a in ancestors {
                if !declared_by.contains_key(a.as_str()) {
                    declared_by.insert(a.as_str(), declared(a));
                }
                sels.extend(&declared_by[a.as_str()]);
            }
            if let Some(i) = id
                .checked_sub(names.len() + 1)
                .filter(|&i| i < lambdas.len())
            {
                let l = &lambdas[i];
                sels.extend(selectors.get(&(l.name.clone(), l.sam.clone())));
            }
            sels.sort_unstable();
            sels.dedup();
            responders[id] = sels;
        }
        let mut by_selector: Vec<Vec<u32>> = vec![Vec::new(); selectors.len()];
        for (id, sels) in responders.iter().enumerate() {
            for &s in sels {
                by_selector[s].push(id as u32);
            }
        }
        // First fit, the selectors most classes respond to first.
        let mut order: Vec<usize> = (0..selectors.len()).collect();
        order.sort_by_key(|&s| std::cmp::Reverse(by_selector[s].len()));
        // Taken slots as bits. A selector's classes are runs of consecutive
        // ids (class ids follow the superclass tree), so a place is checked
        // a run at a time, and a run that meets a taken slot moves the
        // selector just past it: first fit, without trying every offset.
        let mut taken = SlotBits::default();
        let mut offsets: Vec<i64> = vec![i64::MIN; selectors.len()];
        let mut first_free = 0usize;
        for s in order {
            let cids = &by_selector[s];
            let Some(&lowest) = cids.first() else {
                continue;
            };
            // (start relative to the first class, length)
            let mut runs: Vec<(usize, usize)> = Vec::new();
            for &c in cids {
                let rel = (c - lowest) as usize;
                match runs.last_mut() {
                    Some((start, len)) if *start + *len == rel => *len += 1,
                    _ => runs.push((rel, 1)),
                }
            }
            let mut p = first_free;
            'place: loop {
                for &(start, len) in &runs {
                    if let Some(q) = taken.first_taken(p + start, p + start + len) {
                        // (Past the whole taken block, not one slot.)
                        p = taken.first_free(q) - start;
                        continue 'place;
                    }
                }
                break;
            }
            for &(start, len) in &runs {
                taken.set_range(p + start, p + start + len);
            }
            first_free = taken.first_free(first_free);
            offsets[s] = p as i64 - i64::from(lowest);
        }
        let mut dispatch = vec![0u64; 2 * taken.len.max(1)].into_boxed_slice();
        let dispatch_at = dispatch.as_ptr() as i64;
        if std::env::var_os("HORIZON_NATIVE_TRACE").is_some() {
            let entries: usize = by_selector.iter().map(Vec::len).sum();
            eprintln!(
                "dispatch table: {} selectors, {} classes, {entries} entries in {} slots ({} MB)",
                selectors.len(),
                responders.len(),
                taken.len,
                taken.len * 16 >> 20
            );
        }
        phase("dispatch table placed");
        // The type tables the compiled code tests against (their address
        // is baked into it).
        let mut types = Box::new(TypeTables {
            ancestors: Vec::new(),
            names: names_of,
            catches: Vec::new(),
            bits: Vec::new(),
            bit_table: vec![0u64; builtin_base + builtins.len()].into_boxed_slice(),
        });
        let bit_table_at = types.bit_table.as_ptr() as i64;
        let types_at = &*types as *const TypeTables as i64;
        // Ancestors, for instanceof and checkcast: by id, built-ins too.
        let mut ancestors: Vec<HashSet<u32>> = vec![HashSet::new(); builtin_base + builtins.len()];
        for b in &builtins {
            let set: HashSet<u32> = chain(b)
                .iter()
                .filter_map(|c| ids.get(c.as_str()).copied())
                .collect();
            ancestors[ids[b] as usize] = set;
        }
        for (i, l) in lambdas.iter().enumerate() {
            let id = (names.len() + 1 + i) as u32;
            // Its interface's ancestors, the runtime's interfaces too.
            let mut set: HashSet<u32> = chain(&l.interface)
                .iter()
                .filter_map(|c| ids.get(c.as_str()).copied())
                .collect();
            set.insert(id);
            ancestors[id as usize] = set;
        }
        for n in &names {
            let set: HashSet<u32> = chain(n)
                .iter()
                .filter_map(|c| ids.get(c.as_str()).copied())
                .collect();
            ancestors[ids[n] as usize] = set;
        }
        types.ancestors = ancestors
            .into_iter()
            .map(|set| {
                let mut ids: Vec<u32> = set.into_iter().collect();
                ids.sort_unstable();
                ids
            })
            .collect();

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

        // Each function's machine code size (HORIZON_NATIVE_PERFMAP).
        let mut code_sizes: Vec<(FuncId, u32)> = Vec::new();
        // The heap, shared by every thread that runs the code; each has a
        // context (this one's made at the end).
        let heap = Box::new(heap::Heap::reserve(heap::RESERVE));

        phase("type tables, statics");
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
                let mut sig = module.make_signature();
                sig.params.push(AbiParam::new(types::I64));
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
                        .declare_function(
                            &format!("lambda {i} -> {}.{}", l.owner, l.target),
                            Linkage::Local,
                            &sig,
                        )
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
                vec![types::I64, types::I32, types::I64],
                Some(types::I32),
            ),
            (
                "host_catches",
                vec![types::I64, types::I32, types::I64],
                Some(types::I32),
            ),
            ("host_missing", vec![], None),
        ] {
            let mut sig = module.make_signature();
            sig.params.extend(params.into_iter().map(AbiParam::new));
            sig.returns.extend(ret.map(AbiParam::new));
            let id = module
                .declare_function(name, Linkage::Import, &sig)
                .map_err(|e| e.to_string())?;
            host.insert(name, id);
        }

        let mut class_names = vec![String::new(); builtin_base + builtins.len()];
        for n in &names {
            class_names[ids[n] as usize] = (*n).to_owned();
        }
        for (i, l) in lambdas.iter().enumerate() {
            class_names[names.len() + 1 + i] =
                format!("{}$$Lambda{i} -> {}.{}", l.interface, l.owner, l.target);
        }
        for b in &builtins {
            class_names[ids[b] as usize] = (*b).to_owned();
        }
        phase("methods declared, init functions");
        // A Class object for each class: [Class vtable][id][its name].
        let string_vt = layouts["java/lang/String"].vtable as u64;
        let class_vt = layouts["java/lang/Class"].vtable as u64;
        let mut class_boxes: Vec<Box<[u64]>> = Vec::new();
        let mut class_at: HashMap<String, i64> = HashMap::new();
        let mut class_objects = vec![0u64; class_names.len()];
        for (id, name) in class_names.iter().enumerate().skip(1) {
            let text: Vec<u16> = name.replace('/', ".").encode_utf16().collect();
            let s = rt::literal(&text, string_vt);
            let c: Box<[u64]> = vec![class_vt, id as u64, s.as_ptr() as u64].into_boxed_slice();
            class_objects[id] = c.as_ptr() as u64;
            class_at.insert(name.clone(), c.as_ptr() as i64);
            class_boxes.push(s);
            class_boxes.push(c);
        }
        // Who declares what, found once: the questions below are asked for
        // every call site, and scanning every class each time made the
        // compile grow with the square of the program.
        let index = {
            let mut with_code: HashMap<&str, HashSet<(&str, &str)>> = HashMap::new();
            let mut declarers: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
            for (&class, cf) in &by_name {
                for m in &cf.methods {
                    let key = (m.name.as_str(), m.descriptor.as_str());
                    if m.code().is_some() {
                        with_code.entry(class).or_default().insert(key);
                    }
                    if m.access & ABSTRACT == 0 {
                        declarers.entry(key).or_default().push(class);
                    }
                }
            }
            let library = builtin
                .values()
                .flat_map(|b| {
                    b.methods
                        .iter()
                        .filter(|m| !m.is_static)
                        .map(|m| (m.name, m.desc))
                })
                .collect();
            let mut under: HashMap<&str, Vec<(&str, usize)>> = HashMap::new();
            for (c, &n) in &created {
                let Some(cf) = by_name.get(c.as_str()) else {
                    continue;
                };
                if cf.access & (INTERFACE | ABSTRACT) != 0 {
                    continue;
                }
                let mut x = Some(c.as_str());
                while let Some(a) = x {
                    let Some(acf) = by_name.get(a) else {
                        break;
                    };
                    under.entry(a).or_default().push((c.as_str(), n));
                    x = acf.super_name().ok().flatten();
                }
            }
            MethodIndex {
                with_code,
                declarers,
                library,
                under,
            }
        };
        let world = World {
            index: &index,
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
            dispatch_at,
            offsets: &offsets,
            responders: &responders,
            opaque: &opaque,
            types_at,
            bit_table_at,
            tested: std::cell::RefCell::new(HashMap::new()),
            ptr,
            lambdas: &lambdas,
            lambda_at: &lambda_at,
            lambda_vt: &lambda_vt,
            singleton_at: &singleton_at,
            stays: std::cell::RefCell::new(HashMap::new()),
            catches: std::cell::RefCell::new(Vec::new()),
            strings: std::cell::RefCell::new(HashMap::new()),
            string_vt: layouts["java/lang/String"].vtable as i64,
            builtin: &builtin,
            class_at: &class_at,
            class_vt,
            other_classes: std::cell::RefCell::new(Vec::new()),
            stops: std::cell::RefCell::new(HashMap::new()),
            nothrow: std::cell::RefCell::new(HashMap::new()),
            guards: std::cell::RefCell::new(HashMap::new()),
        };
        let mut unsupported = Vec::new();
        let mut ctx = module.make_context();
        let mut fctx = FunctionBuilderContext::new();
        // A method body: built from its bytecode, or one that traps saying
        // why it could not be (then with a clean context).
        macro_rules! build_method {
            ($owner:expr, $c:expr, $m:expr) => {{
                let name = export_name($owner, &$m.name, &$m.descriptor);
                match funcs.get(&name) {
                    None => None,
                    Some(&id) => {
                        ctx.func.signature = sigs[&name].clone();
                        let r = {
                            let b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                            MethodGen::new(&world, &hierarchy, $c, $m, b, &mut module)
                                .and_then(|g| g.run())
                        };
                        if let Err(e) = r {
                            ctx.func.clear();
                            fctx = FunctionBuilderContext::new();
                            let why = rt::stop_reason(format!("{name}, not compiled: {e}"));
                            unsupported.push((name.clone(), e));
                            ctx.func.signature = sigs[&name].clone();
                            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                            let e = b.create_block();
                            b.append_block_params_for_function_params(e);
                            b.switch_to_block(e);
                            b.seal_block(e);
                            say_and_trap(&mut b, why);
                            b.finalize(module.target_config());
                        }
                        let f = ctx.func.clone();
                        module.clear_context(&mut ctx);
                        Some((id, name, f))
                    }
                }
            }};
        }
        // Methods are built in two passes. First the small ones (by bytecode
        // length), which their callers may inline: those stay, read-only.
        // Then every other method, each compiled by a worker as soon as it
        // is built and then dropped: the program's code is never all in
        // memory at once (it was 5 GB of 12 for Create, Sable and NeoForge
        // with Minecraft).
        let small =
            |m: &crate::classfile::Member| m.code().is_some_and(|c| c.code.len() <= SMALL_BYTECODE);
        let mut inlinable: HashMap<FuncId, (String, cranelift_codegen::ir::Function)> =
            HashMap::new();
        let mut sizes: HashMap<FuncId, usize> = HashMap::new();
        // (The small bodies not inlined, to compile; those inlined are
        // compiled from `inlinable`, with no second copy.)
        let mut first: Vec<(FuncId, String, cranelift_codegen::ir::Function)> = Vec::new();
        let mut first_inlinable: Vec<FuncId> = Vec::new();
        for c in classes {
            let owner = c.name().map_err(|e| e.to_string())?;
            for m in c.methods.iter().filter(|m| small(m)) {
                if let Some((id, name, f)) = build_method!(owner, c, m) {
                    let size = hot_size(&f);
                    if size <= INLINE {
                        sizes.insert(id, size);
                        inlinable.insert(id, (name, f));
                        first_inlinable.push(id);
                    } else {
                        first.push((id, name, f));
                    }
                }
            }
        }
        phase("small method bodies built");
        let trace = std::env::var_os("HORIZON_NATIVE_TRACE").is_some();
        // HORIZON_NATIVE_CLIF=<text>: the code of each method whose name has
        // it, after inlining.
        let clif = std::env::var("HORIZON_NATIVE_CLIF").ok();
        // Compiled on every core: each worker inlines and compiles methods
        // with its own context, and this thread puts their code in the
        // module between bodies. A method Cranelift fails on (code it panics
        // on, in a release build without its verifier) is one not compiled,
        // not the end: the verifier says why, and a stand-in traps saying so.
        enum Done {
            Code {
                id: FuncId,
                name: String,
                align: u64,
                bytes: Vec<u8>,
                relocs: Vec<cranelift_module::ModuleReloc>,
                size: u32,
            },
            Failed {
                id: FuncId,
                name: String,
                signature: Signature,
                why: String,
            },
        }
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
        let isa: &dyn cranelift_codegen::isa::TargetIsa = &*compile_isa;
        // (Work is bounded: the bodies waiting are a few per worker.
        // Results are not, so a worker never waits on this thread.)
        let (work_tx, work_rx) =
            std::sync::mpsc::sync_channel::<(FuncId, String, cranelift_codegen::ir::Function)>(
                2 * workers,
            );
        let work_rx = std::sync::Mutex::new(work_rx);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Done>();
        std::thread::scope(|scope| -> Result<(), String> {
            for _ in 0..workers {
                let tx = done_tx.clone();
                let (work_rx, inlinable, sizes, clif) = (&work_rx, &inlinable, &sizes, &clif);
                scope.spawn(move || {
                    let mut ctx = cranelift_codegen::Context::new();
                    let inliner = |caller| Inliner {
                        bodies: inlinable,
                        sizes,
                        caller,
                    };
                    loop {
                        let next = work_rx.lock().unwrap().recv();
                        let Ok((id, name, func)) = next else {
                            break;
                        };
                        if trace {
                            eprintln!("defining {name}");
                        }
                        let failed = |why: String| Done::Failed {
                            id,
                            name: name.clone(),
                            signature: func.signature.clone(),
                            why,
                        };
                        ctx.clear();
                        ctx.func = func.clone();
                        let done = match ctx.inline(inliner(id)) {
                            Err(e) => failed(format!("inlining: {e:?}")),
                            Ok(_) => {
                                if clif.as_ref().is_some_and(|f| name.contains(f.as_str())) {
                                    eprintln!("{name}:\n{}", ctx.func.display());
                                }
                                let compiled =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        let align = ctx
                                            .compile(isa, &mut Default::default())
                                            .map_err(|e| format!("{:?}", e.inner))?
                                            .buffer
                                            .alignment
                                            as u64;
                                        let code = ctx.compiled_code().expect("compiled");
                                        let relocs = code
                                            .buffer
                                            .relocs()
                                            .iter()
                                            .map(|r| {
                                                cranelift_module::ModuleReloc::from_mach_reloc(
                                                    r, &ctx.func, id,
                                                )
                                            })
                                            .collect();
                                        Ok::<_, String>((
                                            align,
                                            code.code_buffer().to_vec(),
                                            relocs,
                                            code.code_info().total_size,
                                        ))
                                    }));
                                match compiled {
                                    Ok(Ok((align, bytes, relocs, size))) => Done::Code {
                                        id,
                                        name: name.clone(),
                                        align,
                                        bytes,
                                        relocs,
                                        size,
                                    },
                                    Ok(Err(why)) => failed(why),
                                    Err(_) => {
                                        ctx.clear();
                                        ctx.func = func.clone();
                                        let _ = ctx.inline(inliner(id));
                                        let mut why =
                                            cranelift_codegen::verify_function(&ctx.func, isa)
                                                .err()
                                                .map_or_else(
                                                    || "a compiler panic".to_owned(),
                                                    |e| e.to_string(),
                                                );
                                        why.truncate(why.floor_char_boundary(400));
                                        failed(why)
                                    }
                                }
                            }
                        };
                        if tx.send(done).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(done_tx);
            // A compiled method into the module (or its stand-in).
            macro_rules! define {
                ($done:expr) => {
                    match $done {
                        Done::Code {
                            id,
                            name,
                            align,
                            bytes,
                            relocs,
                            size,
                        } => {
                            module
                                .define_function_bytes(id, align, &bytes, &relocs)
                                .map_err(|e| format!("{name}: {e:?}"))?;
                            code_sizes.push((id, size));
                        }
                        Done::Failed {
                            id,
                            name,
                            signature,
                            why: e,
                        } => {
                            let why = rt::stop_reason(format!("{name}, not compiled: {e}"));
                            unsupported.push((name.clone(), e));
                            module.clear_context(&mut ctx);
                            ctx.func.signature = signature;
                            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                            let e = b.create_block();
                            b.append_block_params_for_function_params(e);
                            b.switch_to_block(e);
                            b.seal_block(e);
                            say_and_trap(&mut b, why);
                            b.finalize(module.target_config());
                            module
                                .define_function(id, &mut ctx)
                                .map_err(|e| format!("{name} (stand-in): {e:?}"))?;
                            code_sizes.push((
                                id,
                                ctx.compiled_code().map_or(0, |c| c.code_info().total_size),
                            ));
                            module.clear_context(&mut ctx);
                        }
                    }
                };
            }
            // A body to the workers, and the code that is ready meanwhile.
            macro_rules! submit {
                ($item:expr) => {
                    work_tx
                        .send($item)
                        .map_err(|_| "the compile workers stopped")?;
                    while let Ok(done) = done_rx.try_recv() {
                        define!(done);
                    }
                };
            }
            for id in std::mem::take(&mut first_inlinable) {
                let (name, f) = &inlinable[&id];
                submit!((id, name.clone(), f.clone()));
            }
            for item in std::mem::take(&mut first) {
                submit!(item);
            }
            for c in classes {
                let owner = c.name().map_err(|e| e.to_string())?;
                for m in c.methods.iter().filter(|m| !small(m)) {
                    if let Some(item) = build_method!(owner, c, m) {
                        submit!(item);
                    }
                }
            }
            types.catches = world.catches.borrow().clone();
            phase("method bodies built");
            // The bits of the classes the code tests against, now all known.
            let tested = world.tested.borrow().clone();
            let words = tested.len().div_ceil(64).max(1);
            for id in 0..types.bit_table.len() {
                let mut bits = vec![0u64; words].into_boxed_slice();
                for (&target, &k) in &tested {
                    if types
                        .ancestors
                        .get(id)
                        .is_some_and(|a| a.binary_search(&target).is_ok())
                    {
                        bits[k as usize / 64] |= 1 << (k % 64);
                    }
                }
                types.bit_table[id] = bits.as_ptr() as u64;
                types.bits.push(bits);
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
                if trace {
                    eprintln!(
                        "bridge {i} ({}.{}): {:?}, entry {:?}",
                        l.owner,
                        l.target,
                        r.as_ref().err(),
                        ctx.func.layout.entry_block()
                    );
                }
                if let Err(e) = r {
                    ctx.func.clear();
                    // The failed builder left its context half used.
                    fctx = FunctionBuilderContext::new();
                    let why = rt::stop_reason(format!(
                        "lambda {i} -> {}.{}, not compiled: {e}",
                        l.owner, l.target
                    ));
                    unsupported.push((format!("lambda {i} -> {}.{}", l.owner, l.target), e));
                    ctx.func.signature = signature(&module, true, &l.sam).expect("declared");
                    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                    let e = b.create_block();
                    b.append_block_params_for_function_params(e);
                    b.switch_to_block(e);
                    b.seal_block(e);
                    say_and_trap(&mut b, why);
                    b.finalize(module.target_config());
                }
                let f = ctx.func.clone();
                module.clear_context(&mut ctx);
                submit!((id, format!("lambda {i}"), f));
            }
            phase("lambda bridges");
            drop(work_tx);
            for done in done_rx {
                define!(done);
            }
            Ok(())
        })?;
        phase("methods compiled");
        // The init functions.
        for (class, &id) in &inits {
            ctx.func.signature.params = vec![AbiParam::new(types::I64)];
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let e = b.create_block();
            b.append_block_params_for_function_params(e);
            b.switch_to_block(e);
            b.seal_block(e);
            let thread = b.block_params(e)[0];
            let flag = statics_base + 8 * (flags_at + ids[class.as_str()] as usize) as i64;
            let at = b.ins().iconst(types::I64, flag);
            // One thread runs it, the others wait until it has (the flag is
            // set by `class_init_end`); the thread running it passes.
            let raw = |module: &JITModule, params: &[Type], ret: Option<Type>| {
                let mut sig = module.make_signature();
                sig.params.extend(params.iter().map(|&t| AbiParam::new(t)));
                sig.returns.extend(ret.map(AbiParam::new));
                sig
            };
            let begin_sig = b.import_signature(raw(&module, &[types::I64], Some(types::I32)));
            let begin = b.ins().iconst(
                types::I64,
                rt::threads::class_init_begin as *const () as i64,
            );
            let call = b.ins().call_indirect(begin_sig, begin, &[at]);
            let mine = b.inst_results(call)[0];
            let run = b.create_block();
            let skip = b.create_block();
            b.ins().brif(mine, run, &[], skip, &[]);
            b.switch_to_block(skip);
            b.seal_block(skip);
            b.ins().return_(&[]);
            b.switch_to_block(run);
            b.seal_block(run);
            if let Some(s) = superclass(class)
                && let Some(&sid) = inits.get(s)
            {
                let f = module.declare_func_in_func(sid, b.func);
                b.ins().call(f, &[thread]);
            }
            let clinit = export_name(class, "<clinit>", "()V");
            if let Some(&cid) = funcs.get(&clinit) {
                let f = module.declare_func_in_func(cid, b.func);
                b.ins().call(f, &[thread]);
            }
            let end_sig = b.import_signature(raw(&module, &[types::I64], None));
            let end = b
                .ins()
                .iconst(types::I64, rt::threads::class_init_end as *const () as i64);
            b.ins().call_indirect(end_sig, end, &[at]);
            b.ins().return_(&[]);
            b.finalize(module.target_config());
            module
                .define_function(id, &mut ctx)
                .map_err(|e| format!("init {class}: {e:?}"))?;
            code_sizes.push((
                id,
                ctx.compiled_code().map_or(0, |c| c.code_info().total_size),
            ));
            module.clear_context(&mut ctx);
        }
        // The runtime library's instance methods as vtable entries: compiled
        // callers pass the thread's context first, which they don't take.
        let mut thunks: HashMap<(usize, &'static str), FuncId> = HashMap::new();
        for b in &builtin_list {
            for m in b.methods.iter().filter(|m| !m.is_static) {
                if thunks.contains_key(&(m.f, m.desc)) {
                    continue;
                }
                let (Some(sig), Some(raw)) = (
                    signature(&module, true, m.desc),
                    raw_signature(&module, true, m.desc),
                ) else {
                    continue;
                };
                let id = module
                    .declare_function(
                        &format!("thunk {} {}.{}{}", thunks.len(), b.name, m.name, m.desc),
                        Linkage::Local,
                        &sig,
                    )
                    .map_err(|e| e.to_string())?;
                ctx.func.signature = sig;
                let mut fb = FunctionBuilder::new(&mut ctx.func, &mut fctx);
                let e = fb.create_block();
                fb.append_block_params_for_function_params(e);
                fb.switch_to_block(e);
                fb.seal_block(e);
                let args = fb.block_params(e)[1..].to_vec();
                let sr = fb.import_signature(raw);
                let fp = fb.ins().iconst(types::I64, m.f as i64);
                let call = fb.ins().call_indirect(sr, fp, &args);
                let rets = fb.inst_results(call).to_vec();
                fb.ins().return_(&rets);
                fb.finalize(module.target_config());
                module
                    .define_function(id, &mut ctx)
                    .map_err(|e| format!("thunk {}.{}: {e:?}", b.name, m.name))?;
                code_sizes.push((
                    id,
                    ctx.compiled_code().map_or(0, |c| c.code_info().total_size),
                ));
                module.clear_context(&mut ctx);
                thunks.insert((m.f, m.desc), id);
            }
        }
        // Each selector's stand-in for the classes without an
        // implementation: it says which method was called, then traps.
        let mut absent: HashMap<usize, FuncId> = HashMap::new();
        for ((mname, mdesc), &sel) in &selectors {
            // (Only a selector some class responds to can reach one.)
            if by_selector[sel].is_empty() {
                continue;
            }
            // (The thread's context and the receiver; the other arguments
            // are left alone.)
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            let id = module
                .declare_function(&format!("absent {sel}"), Linkage::Local, &sig)
                .map_err(|e| e.to_string())?;
            ctx.func.signature = sig;
            let mut fb = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let e = fb.create_block();
            fb.append_block_params_for_function_params(e);
            fb.switch_to_block(e);
            fb.seal_block(e);
            let why = rt::stop_reason(format!(
                "a virtual call to {mname}{mdesc}, which the object's class does not implement"
            ));
            let receiver = fb.block_params(e)[1];
            let mut raw = module.make_signature();
            raw.params.push(AbiParam::new(types::I32));
            raw.params.push(AbiParam::new(types::I64));
            let sr = fb.import_signature(raw);
            let f = fb
                .ins()
                .iconst(types::I64, rt::stopped_on as *const () as i64);
            let id_value = fb.ins().iconst(types::I32, i64::from(why));
            fb.ins().call_indirect(sr, f, &[id_value, receiver]);
            fb.ins().trap(UNSUPPORTED);
            fb.finalize(module.target_config());
            module
                .define_function(id, &mut ctx)
                .map_err(|e| format!("absent {mname}: {e:?}"))?;
            code_sizes.push((
                id,
                ctx.compiled_code().map_or(0, |c| c.code_info().total_size),
            ));
            module.clear_context(&mut ctx);
            absent.insert(sel, id);
        }
        module.finalize_definitions().map_err(|e| e.to_string())?;
        // HORIZON_NATIVE_PERFMAP=1: /tmp/perf-<pid>.map, where `perf`
        // finds the compiled functions' names.
        if std::env::var_os("HORIZON_NATIVE_PERFMAP").is_some() {
            use std::io::Write;
            let path = format!("/tmp/perf-{}.map", std::process::id());
            if let Ok(f) = std::fs::File::create(&path) {
                let mut f = std::io::BufWriter::new(f);
                for &(id, size) in &code_sizes {
                    let at = module.get_finalized_function(id) as usize;
                    let name = module.declarations().get_function_decl(id).linkage_name(id);
                    let _ = writeln!(f, "{at:x} {size:x} {name}");
                }
            }
        }
        let thunk = |m: &rt::Method| -> u64 {
            thunks
                .get(&(m.f, m.desc))
                .map_or(m.f as u64, |&id| module.get_finalized_function(id) as u64)
        };

        // A default method of a built-in interface among `ancestors`.
        let interface_default =
            |ancestors: &HashSet<String>, name: &str, desc: &str| -> Option<u64> {
                let mut list: Vec<&String> = ancestors.iter().collect();
                list.sort();
                list.into_iter().find_map(|a| {
                    let b = builtin.get(a.as_str())?;
                    if !b.is_interface {
                        return None;
                    }
                    b.methods
                        .iter()
                        .find(|m| !m.is_static && m.name == name && m.desc == desc)
                        .map(thunk)
                })
            };
        // A built-in implementation of `name desc` from built-in class `start`
        // up.
        let builtin_impl = |start: &str, name: &str, desc: &str| -> Option<u64> {
            let mut x = Some(start);
            while let Some(c) = x {
                let b = builtin.get(c)?;
                if let Some(m) = b
                    .methods
                    .iter()
                    .find(|m| !m.is_static && m.name == name && m.desc == desc)
                {
                    return Some(thunk(m));
                }
                x = b.super_name;
            }
            None
        };
        // The first ancestor of a class of the program that is not one.
        let first_outside = |start: &str| -> String {
            let mut c = start.to_owned();
            while let Some(cf) = by_name.get(c.as_str()) {
                match cf.super_name().ok().flatten() {
                    Some(s) => c = s.to_owned(),
                    None => return OBJECT.to_owned(),
                }
            }
            c
        };
        // Fill the dispatch table: each selector a class responds to, its
        // implementation in the class or its superclasses (or an interface's
        // default method, or a built-in superclass's); none, the selector's
        // stand-in (it says which method was called, then traps).
        let by_sel_name: Vec<(&str, &str)> = {
            let mut v = vec![("", ""); selectors.len()];
            for ((n, d), &i) in &selectors {
                v[i] = (n.as_str(), d.as_str());
            }
            v
        };
        let mut put = |cid: usize, sel: usize, p: u64| {
            let at = (cid as i64 + offsets[sel]) as usize;
            dispatch[2 * at] = p;
            dispatch[2 * at + 1] = sel as u64 + 1;
        };
        let report = std::env::var("HORIZON_NATIVE_MISSING").ok();
        let absent_at: HashMap<usize, u64> = absent
            .iter()
            .map(|(&sel, &id)| (sel, module.get_finalized_function(id) as u64))
            .collect();
        for cid in 0..responders.len() {
            let class = class_names.get(cid).cloned().unwrap_or_default();
            let lambda = cid
                .checked_sub(names.len() + 1)
                .filter(|&i| i < lambdas.len());
            // (Class ids start at 1.)
            let compiled = cid
                .checked_sub(1)
                .and_then(|i| names.get(i))
                .filter(|n| ids.get(**n) == Some(&(cid as u32)));
            for &sel in &responders[cid] {
                let (mname, mdesc) = by_sel_name[sel];
                let found: Option<u64> = if let Some(i) = lambda {
                    let l = &lambdas[i];
                    if mname == l.name && mdesc == l.sam {
                        bridges[i].map(|id| module.get_finalized_function(id) as u64)
                    } else {
                        ancestors_of(&l.interface, &by_name)
                            .iter()
                            .find_map(|c| funcs.get(&export_name(c, mname, mdesc)).copied())
                            .map(|f| module.get_finalized_function(f) as u64)
                            .or_else(|| interface_default(&names_by_id[cid], mname, mdesc))
                            .or_else(|| builtin_impl(OBJECT, mname, mdesc))
                    }
                } else if let Some(n) = compiled {
                    let mut x = Some(*n);
                    let mut f = None;
                    while let Some(c) = x {
                        if let Some(&id) = funcs.get(&export_name(c, mname, mdesc))
                            && by_name[c].methods.iter().any(|m| {
                                m.name == mname && m.descriptor == mdesc && m.access & ABSTRACT == 0
                            })
                        {
                            f = Some(id);
                            break;
                        }
                        x = superclass(c);
                    }
                    f.or_else(|| {
                        ancestors_of(n, &by_name).iter().find_map(|i| {
                            let cf = by_name.get(i.as_str())?;
                            (cf.access & INTERFACE != 0)
                                .then(|| funcs.get(&export_name(i, mname, mdesc)).copied())
                                .flatten()
                        })
                    })
                    .map(|f| module.get_finalized_function(f) as u64)
                    .or_else(|| builtin_impl(&first_outside(n), mname, mdesc))
                    .or_else(|| interface_default(&names_by_id[cid], mname, mdesc))
                } else {
                    // A built-in class.
                    builtins
                        .get(cid.wrapping_sub(builtin_base))
                        .and_then(|b| builtin_impl(b, mname, mdesc))
                        .or_else(|| interface_default(&names_by_id[cid], mname, mdesc))
                };
                match found {
                    Some(p) => put(cid, sel, p),
                    None => {
                        put(cid, sel, absent_at[&sel]);
                        // HORIZON_NATIVE_MISSING=<text>: those left to it.
                        if report.as_ref().is_some_and(|r| class.contains(r.as_str())) {
                            eprintln!("{class}: no {mname}{mdesc}");
                        }
                    }
                }
            }
        }
        let strings = world.strings.take();
        let other_classes = world.other_classes.take();
        let stops = world.stops.take();
        // The runtime library: the class names, the built-in vtables, the
        // selectors it calls back through, the cached boxes.
        let builtin_vtables: HashMap<&'static str, u64> = builtin_list
            .iter()
            .map(|b| (b.name, layouts[b.name].vtable as u64))
            .collect();
        let mut boxes = Vec::new();
        let cache = rt::make_caches(&builtin_vtables, &mut boxes);
        let sel = |n: &str, d: &str| selectors.get(&(n.to_owned(), d.to_owned())).copied();
        let class_ids: HashMap<String, u32> = class_names
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.is_empty() && !n.contains("$$Lambda"))
            .map(|(i, n)| (n.clone(), i as u32))
            .collect();
        let super_of = |n: &str| -> Option<&str> {
            if n.contains("$$Lambda") {
                return Some(OBJECT);
            }
            match by_name.get(n) {
                Some(cf) => cf.super_name().ok().flatten(),
                None => builtin.get(n).and_then(|b| b.super_name),
            }
        };
        let supers: Vec<u32> = class_names
            .iter()
            .map(|n| {
                super_of(n)
                    .and_then(|s| class_ids.get(s))
                    .copied()
                    .unwrap_or(0)
            })
            .collect();
        let interface_flags: Vec<bool> = class_names
            .iter()
            .map(|n| match by_name.get(n.as_str()) {
                Some(cf) => cf.access & INTERFACE != 0,
                None => builtin.get(n.as_str()).is_some_and(|b| b.is_interface),
            })
            .collect();
        let sizes: Vec<u32> = class_names
            .iter()
            .map(|n| layouts.get(n.as_str()).map_or(0, |l| l.size.max(0) as u32))
            .collect();
        let runtime = Box::new(rt::Rt {
            vtables: builtin_vtables,
            names: class_names,
            to_string: sel("toString", "()Ljava/lang/String;"),
            hash_code: sel("hashCode", "()I"),
            equals: sel("equals", "(Ljava/lang/Object;)Z"),
            cache,
            selectors: selectors
                .iter()
                .map(|((n, d), &i)| (format!("{n} {d}"), i))
                .collect(),
            dispatch,
            offsets: offsets.clone(),
            sizes,
            ids: class_ids,
            supers,
            interfaces: interface_flags,
            outside_interfaces: classes
                .iter()
                .flat_map(|c| c.interfaces.iter().filter_map(|&i| c.class_name(i).ok()))
                .filter(|i| !by_name.contains_key(*i))
                .map(str::to_owned)
                .collect(),
            classes: class_objects,
            ancestors: names_by_id,
            enum_values: names
                .iter()
                .filter(|n| by_name[*n].super_name().ok().flatten() == Some("java/lang/Enum"))
                .filter_map(|n| {
                    let (at, _) = statics_at.get(&((*n).to_owned(), "$VALUES".to_owned()))?;
                    Some((ids[n], (statics_base + i64::from(*at)) as u64))
                })
                .collect(),
            once: std::sync::Mutex::new(HashMap::new()),
        });
        rt::install(&runtime);
        let main = heap.context();
        rt::install_context(&main);
        let exports = funcs;
        let class_inits = inits.clone();
        Ok(Native {
            module,
            exports,
            class_inits,
            unsupported,
            main,
            heap,
            _types: types,
            _statics: statics,
            _vtables: vtables,
            _singletons: singletons,
            _strings: strings,
            _rt: runtime,
            _boxes: boxes,
            _classes: class_boxes,
            _other_classes: other_classes,
            stops,
        })
    }

    /// The context of the thread that compiled the code: the first argument
    /// of every compiled method it calls.
    pub fn context(&self) -> *mut heap::Context {
        &*self.main as *const heap::Context as *mut heap::Context
    }

    /// Lets the calling thread run the code: its own context (allocation
    /// buffer, exception), registered with the heap until dropped.
    pub fn attach(&self) -> Attached<'_> {
        self.shared().attach()
    }

    /// What other threads need to run the code (the compiled code itself
    /// is finished and read-only): they attach through it.
    pub fn shared(&self) -> Shared<'_> {
        Shared {
            heap: &self.heap,
            rt: &self._rt,
        }
    }

    pub fn heap(&self) -> &heap::Heap {
        &self.heap
    }

    /// The machine code of class `name`'s init function, if it has one: it
    /// takes a thread's context. (Compiled code initializes the classes it
    /// uses; a call into the code from outside does it first with this.)
    pub fn class_init(&self, name: &str) -> Option<*const u8> {
        Some(
            self.module
                .get_finalized_function(*self.class_inits.get(name)?),
        )
    }

    /// The machine code of an exported method (`Class.name(desc)ret`): it
    /// takes a thread's context first.
    pub fn function(&self, export: &str) -> Option<*const u8> {
        Some(
            self.module
                .get_finalized_function(*self.exports.get(export)?),
        )
    }
}

/// The compiled program as threads share it (`Native::shared`).
#[derive(Clone, Copy)]
pub struct Shared<'n> {
    heap: &'n heap::Heap,
    rt: &'n rt::Rt,
}

impl<'n> Shared<'n> {
    /// Lets the calling thread run the code (`Native::attach`).
    pub fn attach(self) -> Attached<'n> {
        let ctx = self.heap.context();
        rt::install(self.rt);
        rt::install_context(&ctx);
        Attached {
            heap: self.heap,
            ctx,
        }
    }
}

/// A thread attached to compiled code (`Native::attach`).
pub struct Attached<'n> {
    heap: &'n heap::Heap,
    ctx: Box<heap::Context>,
}

impl Attached<'_> {
    /// This thread's context: the first argument of every compiled method.
    pub fn context(&self) -> *mut heap::Context {
        &*self.ctx as *const heap::Context as *mut heap::Context
    }
}

impl Drop for Attached<'_> {
    fn drop(&mut self) {
        self.heap.release(&self.ctx);
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
    HashMap<(String, String, u32), usize>,
    Vec<(String, String)>,
    HashMap<String, usize>,
) {
    let mut sites = Vec::new();
    let mut at = HashMap::new();
    let mut called = Vec::new();
    // How many places create each class (`new`, constructor references).
    let mut created: HashMap<String, usize> = HashMap::new();
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
                    (0xBB, Operand::Constant(i)) => {
                        if let Ok(class) = c.class_name(*i) {
                            *created.entry(class.to_owned()).or_default() += 1;
                        }
                    }
                    (0xBA, Operand::Constant(i)) => {
                        if let Some(site) = lambda_site(c, *i) {
                            if site.kind == 8 {
                                *created.entry(site.owner.clone()).or_default() += 1;
                            }
                            // By method too: an offset is its method's own.
                            let method = format!("{}{}", m.name, m.descriptor);
                            at.insert((owner.to_owned(), method, x.at), sites.len());
                            sites.push(site);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    (sites, at, called, created)
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
/// Methods with at most this much bytecode are built first and kept, as
/// callees to inline (one within `INLINE` instructions once built).
const SMALL_BYTECODE: usize = 80;

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

/// A compiled method's signature: its thread's context first (hidden), then
/// the receiver and the descriptor's parameters.
fn signature(module: &JITModule, instance: bool, desc: &str) -> Option<Signature> {
    let mut sig = raw_signature(module, instance, desc)?;
    sig.params.insert(0, AbiParam::new(types::I64));
    Some(sig)
}

/// A runtime library function's signature: no context.
fn raw_signature(module: &JITModule, instance: bool, desc: &str) -> Option<Signature> {
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
/// The dispatch table's taken slots, as bits.
#[derive(Default)]
struct SlotBits {
    words: Vec<u64>,
    /// One past the last taken slot.
    len: usize,
}

impl SlotBits {
    /// The first taken slot in `from..to`, if any.
    fn first_taken(&self, from: usize, to: usize) -> Option<usize> {
        let mut i = from;
        while i < to {
            let w = i / 64;
            let Some(&word) = self.words.get(w) else {
                return None;
            };
            let bits = word >> (i % 64);
            if bits != 0 {
                let at = i + bits.trailing_zeros() as usize;
                return (at < to).then_some(at);
            }
            i = (w + 1) * 64;
        }
        None
    }

    fn set_range(&mut self, from: usize, to: usize) {
        if to.div_ceil(64) > self.words.len() {
            self.words.resize(to.div_ceil(64), 0);
        }
        for i in from..to {
            self.words[i / 64] |= 1 << (i % 64);
        }
        self.len = self.len.max(to);
    }

    /// The first free slot from `from` on.
    fn first_free(&self, from: usize) -> usize {
        let mut i = from;
        loop {
            let w = i / 64;
            let Some(&word) = self.words.get(w) else {
                return i;
            };
            let free = !word >> (i % 64);
            if free != 0 {
                return i + free.trailing_zeros() as usize;
            }
            i = (w + 1) * 64;
        }
    }
}

/// Who declares which method (see `Native::compile`).
struct MethodIndex<'a> {
    /// Each class's methods with code, by name and descriptor.
    with_code: HashMap<&'a str, HashSet<(&'a str, &'a str)>>,
    /// The classes declaring a method that is not abstract.
    declarers: HashMap<(&'a str, &'a str), Vec<&'a str>>,
    /// The runtime library's instance methods.
    library: HashSet<(&'static str, &'static str)>,
    /// Each class's concrete classes with `new` sites (itself and its
    /// subclasses), and how many sites.
    under: HashMap<&'a str, Vec<(&'a str, usize)>>,
}

struct World<'a> {
    index: &'a MethodIndex<'a>,
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
    /// The dispatch table's address, each selector's offset in it
    /// (`i64::MIN`: no class responds to it), and each class id's selectors.
    dispatch_at: i64,
    offsets: &'a [i64],
    responders: &'a [Vec<usize>],
    opaque: &'a [bool],
    /// The program's type tables (`TypeTables`), by address, and their
    /// table of each class id's bits.
    types_at: i64,
    bit_table_at: i64,
    /// The classes the code tests against, by id: their bit numbers.
    tested: std::cell::RefCell<HashMap<u32, u32>>,
    ptr: Type,
    lambdas: &'a [LambdaSite],
    /// Each lambda site, by class, method and offset.
    lambda_at: &'a HashMap<(String, String, u32), usize>,
    lambda_vt: &'a [i64],
    singleton_at: &'a [Option<i64>],
    /// Whether a method keeps its own object inside it (by method and the
    /// inlining depth left).
    stays: std::cell::RefCell<HashMap<(String, u32), bool>>,
    /// The classes `catch` clauses name (indices for `host_catches`).
    catches: std::cell::RefCell<Vec<String>>,
    /// String literals, one object each (interned), made at compile time:
    /// the String vtable, the length, the UTF-16 units.
    strings: std::cell::RefCell<HashMap<String, Box<[u64]>>>,
    string_vt: i64,
    /// The built-in classes, by name.
    builtin: &'a HashMap<&'static str, &'a rt::Builtin>,
    /// Each class's Class object, by name; others (classes outside the
    /// program, array classes) made when code names them.
    class_at: &'a HashMap<String, i64>,
    class_vt: u64,
    other_classes: std::cell::RefCell<Vec<(String, Box<[u64]>, Box<[u64]>)>>,
    /// The stops compiled, by why.
    stops: std::cell::RefCell<HashMap<String, usize>>,
    /// Methods that cannot throw (`World::cannot_throw`), by export name.
    nothrow: std::cell::RefCell<HashMap<String, bool>>,
    /// How many places create each class.
    /// The implementations a virtual call tries first (`World::guards`).
    guards: std::cell::RefCell<HashMap<(String, String), Vec<String>>>,
}

impl World<'_> {
    /// The class (of `owner` or a superclass) that declares method `name desc`.
    /// Whether `class.name desc` can never throw, so its callers need not
    /// test for an exception after calling it. Conservative: its code has
    /// no `athrow`, array access, integer division, cast, allocation, field
    /// access but reading `this`'s and its own class's statics, nor calls
    /// but static ones to methods that cannot throw either.
    fn cannot_throw(&self, class: &str, name: &str, desc: &str) -> bool {
        let key = export_name(class, name, desc);
        if let Some(&known) = self.nothrow.borrow().get(&key) {
            return known;
        }
        // (A method calling itself, directly or not: it may throw.)
        self.nothrow.borrow_mut().insert(key.clone(), false);
        let answer = (|| -> Option<bool> {
            let cf = self.by_name.get(class)?;
            let m = cf
                .methods
                .iter()
                .find(|m| m.name == name && m.descriptor == desc)?;
            let insns = decode(&m.code()?.code).ok()?;
            let instance = m.access & STATIC == 0;
            for (k, insn) in insns.iter().enumerate() {
                let ok = match insn.opcode {
                    // Constants, local loads and stores, stack shuffles.
                    0x00..=0x2D | 0x36..=0x4E | 0x57..=0x5F => true,
                    // Integer division and remainder (by zero).
                    0x6C | 0x6D | 0x70 | 0x71 => false,
                    // Arithmetic, conversions, comparisons, branches,
                    // switches, returns.
                    0x60..=0xA7 | 0xAA..=0xB1 => true,
                    // Its own class's statics (initialized before it runs).
                    0xB2 | 0xB3 => {
                        let Operand::Constant(c) = insn.operand else {
                            return Some(false);
                        };
                        cf.member_ref(c).ok()?.0 == class
                    }
                    // A field of `this`.
                    0xB4 => instance && k > 0 && insns[k - 1].opcode == 0x2A,
                    0xB8 => {
                        let Operand::Constant(c) = insn.operand else {
                            return Some(false);
                        };
                        let (owner, callee, cdesc) = cf.member_ref(c).ok()?;
                        let declaring = self.resolve(owner, callee, cdesc)?;
                        (declaring == class || !self.inits.contains_key(&declaring))
                            && self.cannot_throw(&declaring, callee, cdesc)
                    }
                    _ => false,
                };
                if !ok {
                    return Some(false);
                }
            }
            Some(true)
        })()
        .unwrap_or(false);
        self.nothrow.borrow_mut().insert(key, answer);
        answer
    }

    /// Whether every object of type `class` (a receiver's type) responds to
    /// selector `sel`; `None` for a type outside the program (its objects'
    /// classes are the program's, each may or may not).
    fn responds(&self, class: &str, sel: usize) -> Option<bool> {
        if self.offsets[sel] == i64::MIN {
            return Some(false);
        }
        let id = *self.ids.get(class)? as usize;
        let r = self.responders.get(id)?;
        if r.binary_search(&sel).is_ok() {
            Some(true)
        } else if self.opaque.get(id) == Some(&true) {
            None
        } else {
            Some(false)
        }
    }

    /// Whether `class` is `Enum` or one of the program's enums.
    fn is_enum(&self, class: &str) -> bool {
        let mut c = Some(class);
        while let Some(name) = c {
            if name == "java/lang/Enum" {
                return true;
            }
            c = self
                .by_name
                .get(name)
                .and_then(|cf| cf.super_name().ok().flatten());
        }
        false
    }

    /// The classes declaring the implementations of `name desc` that a
    /// virtual call tries directly before its vtable: the ones the classes
    /// the program creates most use (at most two). A guess: the call tests
    /// the vtable's method against each, so a wrong one costs only time.
    fn guards(&self, name: &str, desc: &str) -> Vec<String> {
        let key = (name.to_owned(), desc.to_owned());
        if let Some(g) = self.guards.borrow().get(&key) {
            return g.clone();
        }
        // Each instance method with code of that name, weighted by the
        // `new` sites of the classes that reach it.
        let mut weight: HashMap<String, usize> = HashMap::new();
        let declarers = self
            .index
            .declarers
            .get(&(name, desc))
            .map_or(&[][..], |v| v);
        for &d in declarers {
            let instance = self.by_name.get(d).is_some_and(|cf| {
                cf.methods.iter().any(|m| {
                    m.name == name
                        && m.descriptor == desc
                        && m.access & (STATIC | ABSTRACT) == 0
                        && m.code().is_some()
                })
            });
            if !instance || !self.funcs.contains_key(&export_name(d, name, desc)) {
                continue;
            }
            for &(c, n) in self.index.under.get(d).map_or(&[][..], |v| v) {
                if self.resolve(c, name, desc).as_deref() == Some(d) {
                    *weight.entry(d.to_owned()).or_default() += n;
                }
            }
        }
        let mut ranked: Vec<(String, usize)> = weight.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if std::env::var_os("HORIZON_NATIVE_GUARDS").is_some() {
            eprintln!("guards {name}{desc}: {ranked:?}");
        }
        let picked: Vec<String> = ranked.into_iter().take(2).map(|(c, _)| c).collect();
        self.guards.borrow_mut().insert(key, picked.clone());
        picked
    }

    /// Whether no implementation of `name desc` in the program can throw
    /// (a virtual call to it needs no test after it). The runtime
    /// library's own implementations count as able to.
    fn no_implementation_throws(&self, name: &str, desc: &str) -> bool {
        let key = format!("* {name}{desc}");
        if let Some(&known) = self.nothrow.borrow().get(&key) {
            return known;
        }
        let in_library = self.index.library.contains(&(name, desc));
        let answer = !in_library
            && self
                .index
                .declarers
                .get(&(name, desc))
                .is_none_or(|v| v.iter().all(|class| self.cannot_throw(class, name, desc)));
        self.nothrow.borrow_mut().insert(key, answer);
        answer
    }

    /// Whether `class`'s own method `name desc` is static.
    fn is_static(&self, class: &str, name: &str, desc: &str) -> bool {
        self.by_name.get(class).is_some_and(|cf| {
            cf.methods
                .iter()
                .any(|m| m.name == name && m.descriptor == desc && m.access & STATIC != 0)
        })
    }

    fn resolve(&self, owner: &str, name: &str, desc: &str) -> Option<String> {
        let mut x = Some(owner);
        while let Some(c) = x {
            let cf = self.by_name.get(c)?;
            if self
                .index
                .with_code
                .get(c)
                .is_some_and(|m| m.contains(&(name, desc)))
            {
                return Some(c.to_owned());
            }
            x = cf.super_name().ok().flatten();
        }
        None
    }
    /// The built-in method `name desc` that a call on `owner` reaches (its
    /// address), when no class of the program has it: (address, static,
    /// the class it is in is final).
    fn builtin_method(&self, owner: &str, name: &str, desc: &str) -> Option<(usize, bool, bool)> {
        let mut x = owner.to_owned();
        // Up through the program's classes to a built-in one.
        while let Some(cf) = self.by_name.get(x.as_str()) {
            x = cf.super_name().ok().flatten()?.to_owned();
        }
        let mut y = Some(x.as_str());
        let mut first = true;
        let mut fin = false;
        while let Some(c) = y {
            let b = self.builtin.get(c)?;
            if first {
                fin = b.is_final;
                first = false;
            }
            if let Some(m) = b.methods.iter().find(|m| m.name == name && m.desc == desc) {
                return Some((m.f, m.is_static, fin));
            }
            y = b.super_name;
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
/// The method of `obj`'s class for selector `sel`: its entry in the
/// dispatch table, at its class id plus the selector's offset.
fn dispatch_fp(b: &mut FunctionBuilder<'_>, w: &World<'_>, obj: Value, sel: usize) -> Value {
    let at = dispatch_entry(b, w, obj, sel);
    b.ins().load(types::I64, MemFlagsData::trusted(), at, 0)
}

/// The address of `obj`'s entry for selector `sel`: `[method, selector + 1]`.
fn dispatch_entry(b: &mut FunctionBuilder<'_>, w: &World<'_>, obj: Value, sel: usize) -> Value {
    let vt = b.ins().load(types::I64, MemFlagsData::trusted(), obj, 0);
    let class_id = b.ins().load(types::I64, MemFlagsData::trusted(), vt, 0);
    let slot = b.ins().imul_imm_s(class_id, 16);
    let row = b
        .ins()
        .iconst(types::I64, w.dispatch_at + 16 * w.offsets[sel]);
    b.ins().iadd(row, slot)
}

/// In this thread's buffer (`ctx`'s); when it is out, a new one from the
/// heap (`heap::refill`).
fn alloc_in(b: &mut FunctionBuilder<'_>, ctx: Value, size: Value, header: Value) -> Value {
    let cur = b
        .ins()
        .load(types::I64, MemFlagsData::trusted(), ctx, heap::BUMP);
    let limit = b
        .ins()
        .load(types::I64, MemFlagsData::trusted(), ctx, heap::LIMIT);
    let size = b.ins().iadd_imm_s(size, 7);
    let size = b.ins().band_imm_s(size, !7);
    let next = b.ins().iadd(cur, size);
    let over = b.ins().icmp(IntCC::UnsignedGreaterThan, next, limit);
    let fast = b.create_block();
    let slow = b.create_block();
    let done = b.create_block();
    b.append_block_param(done, types::I64);
    b.set_cold_block(slow);
    b.ins().brif(over, slow, &[], fast, &[]);
    b.switch_to_block(fast);
    b.seal_block(fast);
    b.ins()
        .store(MemFlagsData::trusted(), next, ctx, heap::BUMP);
    b.ins().jump(done, &[cur.into()]);
    b.switch_to_block(slow);
    b.seal_block(slow);
    let mut sig = Signature::new(b.func.signature.call_conv);
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    let sr = b.import_signature(sig);
    let f = b.ins().iconst(types::I64, heap::refill as *const () as i64);
    let call = b.ins().call_indirect(sr, f, &[ctx, size]);
    let got = b.inst_results(call)[0];
    b.ins().trapz(got, ARENA);
    b.ins().jump(done, &[got.into()]);
    b.switch_to_block(done);
    b.seal_block(done);
    let obj = b.block_params(done)[0];
    b.ins().store(MemFlagsData::trusted(), header, obj, 0);
    obj
}

/// Runs the static initializer of `class` if it has not run.
fn init_in(
    b: &mut FunctionBuilder<'_>,
    module: &mut JITModule,
    w: &World<'_>,
    ctx: Value,
    class: &str,
) {
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
    b.ins().call(f, &[ctx]);
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
    b.seal_block(done);
}

/// Calls instance method `name desc` of `owner` on `args[0]`: directly when
/// the closed world has one implementation, else through the vtable.
#[allow(clippy::too_many_arguments)]
fn call_virtual(
    b: &mut FunctionBuilder<'_>,
    module: &mut JITModule,
    w: &World<'_>,
    ctx: Value,
    owner: &str,
    name: &str,
    desc: &str,
    args: &[Value],
) -> Result<Option<Value>, String> {
    b.ins().trapz(args[0], NULL);
    let args: Vec<Value> = std::iter::once(ctx).chain(args.iter().copied()).collect();
    let args = &args[..];
    let key = (name.to_owned(), desc.to_owned());
    let direct = (w.implementations.get(&key) == Some(&1))
        .then(|| w.resolve(owner, name, desc))
        .flatten()
        .filter(|c| {
            w.by_name
                .get(c.as_str())
                .is_some_and(|cf| cf.access & INTERFACE == 0)
                && !w.is_static(c, name, desc)
        });
    let call = match (direct, w.selectors.get(&key)) {
        (Some(declaring), _) => {
            let id = w.funcs[&export_name(&declaring, name, desc)];
            let fr = module.declare_func_in_func(id, b.func);
            b.ins().call(fr, args)
        }
        (None, Some(&sel)) if w.offsets[sel] != i64::MIN => {
            let fp = dispatch_fp(b, w, args[1], sel);
            let sig = signature(module, true, desc).ok_or("a bad signature")?;
            let sr = b.import_signature(sig);
            b.ins().call_indirect(sr, fp, args)
        }
        _ => return Err(format!("no implementation of {owner}.{name}{desc}")),
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
    // The thread's context, the lambda object, the interface's arguments.
    let ctx = params[0];
    let sam = Method::parse(&l.sam).ok_or("a bad interface descriptor")?;
    let target = Method::parse(&l.target_desc).ok_or("a bad target descriptor")?;
    let mut args = Vec::new();
    for (k, c) in l.captures.iter().enumerate() {
        let t = desc_type(c).ok_or("a bad capture")?;
        args.push(
            b.ins()
                .load(t, MemFlagsData::trusted(), params[1], 8 + 8 * k as i32),
        );
    }
    args.extend_from_slice(&params[2..]);
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
    // The interface's arguments as the target takes them: boxed, unboxed or
    // widened, as LambdaMetafactory adapts them.
    let mut have: Vec<String> = l.captures.clone();
    have.extend(sam.params.iter().cloned());
    let mut need: Vec<String> = Vec::new();
    if matches!(l.kind, 5 | 7 | 9) {
        need.push(format!("L{};", l.owner));
    }
    need.extend(target.params.iter().cloned());
    for k in 0..args.len() {
        args[k] = adapt(&mut b, module, args[k], &have[k], &need[k])?;
    }
    // A runtime library target (Integer::sum, ArrayList::new...).
    let builtin_call = |b: &mut FunctionBuilder<'_>,
                        module: &mut JITModule,
                        f: usize,
                        instance: bool,
                        args: &[Value]|
     -> Result<Option<Value>, String> {
        let sig = raw_signature(module, instance, &l.target_desc).ok_or("a bad signature")?;
        let sr = b.import_signature(sig);
        let fp = b.ins().iconst(types::I64, f as i64);
        let call = b.ins().call_indirect(sr, fp, args);
        Ok(b.inst_results(call).first().copied())
    };
    // A private instance target (javac's `lambda$...` bodies) is never
    // overridden: called directly, as `invokespecial` would.
    let private = matches!(l.kind, 5 | 9)
        && w.by_name.get(l.owner.as_str()).is_some_and(|cf| {
            cf.methods.iter().any(|m| {
                m.name == l.target && m.descriptor == l.target_desc && m.access & PRIVATE != 0
            })
        });
    let kind = if private { 7 } else { l.kind };
    let result = match kind {
        6 | 7 if w.resolve(&l.owner, &l.target, &l.target_desc).is_none() => {
            let (f, is_static, _) = w
                .builtin_method(&l.owner, &l.target, &l.target_desc)
                .ok_or("no target")?;
            if is_static != (kind == 6) {
                return Err("a target of the wrong kind".into());
            }
            if kind == 7 {
                b.ins().trapz(args[0], NULL);
            }
            builtin_call(&mut b, module, f, kind == 7, &args)?
        }
        8 if !w.by_name.contains_key(l.owner.as_str()) => {
            let layout = w
                .layouts
                .get(&l.owner)
                .ok_or("a constructor outside the classes")?;
            let (size, vt) = (layout.size, layout.vtable as i64);
            let size = b.ins().iconst(types::I64, i64::from(size));
            let header = b.ins().iconst(types::I64, vt);
            let obj = alloc_in(&mut b, ctx, size, header);
            let (f, _, _) = w
                .builtin_method(&l.owner, "<init>", &l.target_desc)
                .ok_or("no constructor")?;
            let mut all = vec![obj];
            all.extend_from_slice(&args);
            builtin_call(&mut b, module, f, true, &all)?;
            Some(obj)
        }
        6 | 7 => {
            let declaring = w
                .resolve(&l.owner, &l.target, &l.target_desc)
                .ok_or("no target")?;
            if kind == 6 {
                init_in(&mut b, module, w, ctx, &declaring);
            } else {
                b.ins().trapz(args[0], NULL);
            }
            let id = w.funcs[&export_name(&declaring, &l.target, &l.target_desc)];
            let fr = module.declare_func_in_func(id, b.func);
            let all: Vec<Value> = std::iter::once(ctx).chain(args.iter().copied()).collect();
            let call = b.ins().call(fr, &all);
            b.inst_results(call).first().copied()
        }
        5 | 9 => call_virtual(
            &mut b,
            module,
            w,
            ctx,
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
            init_in(&mut b, module, w, ctx, &l.owner);
            let size = b.ins().iconst(types::I64, i64::from(size));
            let header = b.ins().iconst(types::I64, vt);
            let obj = alloc_in(&mut b, ctx, size, header);
            let declaring = w
                .resolve(&l.owner, "<init>", &l.target_desc)
                .ok_or("no constructor")?;
            let id = w.funcs[&export_name(&declaring, "<init>", &l.target_desc)];
            let fr = module.declare_func_in_func(id, b.func);
            let mut all = vec![ctx, obj];
            all.extend_from_slice(&args);
            b.ins().call(fr, &all);
            Some(obj)
        }
        k => return Err(format!("method handle kind {k}")),
    };
    let made = if l.kind == 8 {
        format!("L{};", l.owner)
    } else {
        target.ret.clone()
    };
    if sam.ret == "V" {
        b.ins().return_(&[]);
    } else {
        let v = result.ok_or("no result for the interface")?;
        let v = adapt(&mut b, module, v, &made, &sam.ret)?;
        b.ins().return_(&[v]);
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
#[allow(clippy::too_many_arguments)]
fn escapes(
    w: &World<'_>,
    h: &Classes,
    class: &ClassFile,
    insns: &[Instruction],
    handlers: &[crate::classfile::Handler],
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
        // An object held in a local where an exception can arrive at a
        // handler: kept in memory.
        if handlers
            .iter()
            .any(|h| u32::from(h.start) <= insn.at && insn.at < u32::from(h.end))
        {
            for t in &f.locals {
                out.extend(site(t));
            }
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
        let (_, out) = escapes(w, h, cf, &insns, &code.handlers, &frames, &opts, depth);
        Some(!out.contains(&THIS_SITE))
    })()
    .unwrap_or(false);
    w.stays.borrow_mut().insert(key, v);
    v
}

/// An object kept in variables: the body it was made in (its id) and its
/// `new` position.
type SiteKey = (u32, u32);

/// Value `v` of descriptor `from` as descriptor `to`: boxed, unboxed or
/// widened, or as it is.
fn adapt(
    b: &mut FunctionBuilder<'_>,
    module: &mut JITModule,
    v: Value,
    from: &str,
    to: &str,
) -> Result<Value, String> {
    let is_ref = |d: &str| d.starts_with('L') || d.starts_with('[');
    let call =
        |b: &mut FunctionBuilder<'_>, module: &mut JITModule, f: usize, desc: &str, v: Value| {
            let sig = raw_signature(module, false, desc).expect("a runtime signature");
            let sr = b.import_signature(sig);
            let fp = b.ins().iconst(types::I64, f as i64);
            let call = b.ins().call_indirect(sr, fp, &[v]);
            b.inst_results(call)[0]
        };
    Ok(match (is_ref(from), is_ref(to)) {
        (true, true) => v,
        (false, true) => {
            let (_, of, _) = rt::boxing(from).ok_or("boxing of an unknown type")?;
            call(b, module, of, &format!("({from})Ljava/lang/Object;"), v)
        }
        (true, false) => {
            let (_, _, un) = rt::boxing(to).ok_or("unboxing to an unknown type")?;
            b.ins().trapz(v, NULL);
            call(b, module, un, &format!("(Ljava/lang/Object;){to}"), v)
        }
        (false, false) if from == to => v,
        (false, false) => {
            let (ft, tt) = (
                desc_type(from).ok_or("a bad type")?,
                desc_type(to).ok_or("a bad type")?,
            );
            match (ft, tt) {
                (a, c) if a == c => v,
                (types::I32, types::I64) => b.ins().sextend(types::I64, v),
                (types::I32 | types::I64, types::F32 | types::F64) => b.ins().fcvt_from_sint(tt, v),
                (types::F32, types::F64) => b.ins().fpromote(types::F64, v),
                _ => return Err(format!("no conversion from {from} to {to}")),
            }
        }
    })
}

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
    /// In an inlined body: where an exception it does not catch goes.
    unwind: Option<Block>,
    /// Exception dispatch blocks, by body and covering handlers.
    dispatches: HashMap<(u32, Vec<usize>), Block>,
    /// The instruction being compiled (where implicit exceptions go from).
    cur: usize,
    /// This thread's context: the function's hidden first parameter.
    ctx: Value,
    /// A synchronized method: the object whose monitor it holds.
    sync_on: Option<Value>,
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
        let opts = Options {
            sites: true,
            this_site: false,
        };
        let frames = infer_with(h, class, method, &insns, &opts)?;
        let cfg = Cfg::new(&insns, &code.handlers);
        let (seen, out) = escapes(
            w,
            h,
            class,
            &insns,
            &code.handlers,
            &frames,
            &opts,
            ESCAPE_DEPTH,
        );
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
            unwind: None,
            dispatches: HashMap::new(),
            cur: 0,
            // (Set to the entry block's first parameter by `run`.)
            ctx: Value::from_u32(u32::MAX),
            sync_on: None,
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
        // Parameters into their locals, after the thread's context.
        let params: Vec<Value> = self.b.block_params(entry).to_vec();
        self.ctx = params[0];
        let sig = Method::parse(&self.method.descriptor).ok_or("a bad descriptor")?;
        let mut slot = 0usize;
        let mut k = 1usize;
        if self.method.access & STATIC == 0 {
            let v = self.var(true, 0, types::I64);
            self.b.def_var(v, params[1]);
            slot = 1;
            k = 2;
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
        // A synchronized method holds its object's (or class's) monitor
        // until it returns or throws.
        if self.method.access & SYNCHRONIZED != 0 {
            let o = if self.method.access & STATIC != 0 {
                let class = self.owner.to_owned();
                let at = self.class_object(&class);
                self.b.ins().iconst(types::I64, at)
            } else {
                params[1]
            };
            let enter = rt::threads::monitor_enter as *const () as usize;
            self.call_builtin(enter, "(Ljava/lang/Object;)V", false, &[o])?;
            self.sync_on = Some(o);
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
        at: usize,
    ) -> Result<Option<Value>, String> {
        let w = self.w;
        // What it does not catch goes to this call's handlers.
        let unwind = self.dispatch(at)?;
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
        let (seen, out) = escapes(w, self.h, cf, &insns, &code.handlers, &frames, &opts, depth);
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
            self.unwind,
        );
        self.unwind = Some(unwind);
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
            self.unwind,
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
                self.cur = i;
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

    /// The block that sends the exception being thrown at instruction `i` to
    /// the first handler covering `i` that catches it, else out (to the
    /// caller, or to the caller's handlers for an inlined body).
    fn dispatch(&mut self, i: usize) -> Result<Block, String> {
        let at = self.insns[i].at;
        let handlers: Vec<crate::classfile::Handler> = self
            .method
            .code()
            .map(|c| c.handlers.clone())
            .unwrap_or_default();
        let covering: Vec<usize> = handlers
            .iter()
            .enumerate()
            .filter(|(_, h)| u32::from(h.start) <= at && at < u32::from(h.end))
            .map(|(k, _)| k)
            .collect();
        if let Some(&b) = self.dispatches.get(&(self.body, covering.clone())) {
            return Ok(b);
        }
        let here = self.b.current_block();
        let d = self.b.create_block();
        // Exceptions are the rare path (and so not counted for inlining).
        self.b.set_cold_block(d);
        self.dispatches.insert((self.body, covering.clone()), d);
        self.b.switch_to_block(d);
        let g = self.ctx;
        let exc = self
            .b
            .ins()
            .load(types::I64, MemFlagsData::trusted(), g, EXCEPTION);
        for k in covering {
            let h = handlers[k];
            let target = self
                .insns
                .iter()
                .position(|x| x.at == u32::from(h.handler))
                .and_then(|j| self.blocks[self.cfg.block_of[j]])
                .ok_or("a handler at no block")?;
            let catch = (h.catch_type != 0)
                .then(|| self.class.class_name(h.catch_type).map(str::to_owned))
                .transpose()
                .map_err(|e| e.to_string())?
                .filter(|c| c != "java/lang/Throwable");
            let take = self.b.create_block();
            match catch {
                None => {
                    self.b.ins().jump(take, &[]);
                }
                Some(c) => {
                    let index = {
                        let mut list = self.w.catches.borrow_mut();
                        match list.iter().position(|x| *x == c) {
                            Some(p) => p,
                            None => {
                                list.push(c);
                                list.len() - 1
                            }
                        }
                    };
                    let idx = self.b.ins().iconst(types::I32, index as i64);
                    let types = self.b.ins().iconst(types::I64, self.w.types_at);
                    let yes = self
                        .host("host_catches", &[exc, idx, types])
                        .ok_or("host_catches")?;
                    let next = self.b.create_block();
                    self.b.ins().brif(yes, take, &[], next, &[]);
                    self.b.switch_to_block(take);
                    self.take(g, exc, target);
                    self.b.switch_to_block(next);
                    continue;
                }
            }
            self.b.switch_to_block(take);
            self.take(g, exc, target);
            // A catch-all: nothing after it.
            let dead = self.b.create_block();
            self.b.switch_to_block(dead);
        }
        // Not caught here.
        match self.unwind {
            Some(u) => {
                self.b.ins().jump(u, &[]);
            }
            None => {
                let rets: Vec<Type> = self
                    .b
                    .func
                    .signature
                    .returns
                    .iter()
                    .map(|r| r.value_type)
                    .collect();
                let zeros: Vec<Value> = rets.into_iter().map(|t| zero(&mut self.b, t)).collect();
                self.leave_monitor()?;
                self.b.ins().return_(&zeros);
            }
        }
        if let Some(h) = here {
            self.b.switch_to_block(h);
        }
        Ok(d)
    }

    /// Takes the exception `exc` into handler block `target`: it is the
    /// handler's stack, and no longer being thrown.
    fn take(&mut self, g: Value, exc: Value, target: Block) {
        let z = self.b.ins().iconst(types::I64, 0);
        self.b.ins().store(MemFlagsData::trusted(), z, g, EXCEPTION);
        let v = self.var(false, 0, types::I64);
        self.b.def_var(v, exc);
        self.b.ins().jump(target, &[]);
    }

    /// After a call at instruction `i`: on to the handlers if it threw.
    fn after_call(&mut self, i: usize) -> Result<(), String> {
        let g = self.ctx;
        let exc = self
            .b
            .ins()
            .load(types::I64, MemFlagsData::trusted(), g, EXCEPTION);
        let go = self.dispatch(i)?;
        let on = self.b.create_block();
        self.b.ins().brif(exc, go, &[], on, &[]);
        self.b.switch_to_block(on);
        Ok(())
    }

    /// String concatenation (`invokedynamic` through StringConcatFactory):
    /// a StringBuilder, one append per piece, its string. Whether it was one.
    fn concat(&mut self, insn: &Instruction, f: &Frame, i: usize) -> Result<bool, String> {
        let me = |e: crate::classfile::Error| e.to_string();
        let Operand::Constant(c) = insn.operand else {
            return Ok(false);
        };
        let Some(Constant::InvokeDynamic(bsm, nat)) = self.class.constants.get(usize::from(c))
        else {
            return Ok(false);
        };
        let (_, desc) = self.class.name_and_type(*nat).map_err(me)?;
        let bootstrap = self
            .class
            .bootstrap_methods()
            .get(usize::from(*bsm))
            .ok_or("a bad bootstrap method")?;
        let Some(Constant::MethodHandle(_, r)) = self
            .class
            .constants
            .get(usize::from(bootstrap.method_handle))
        else {
            return Ok(false);
        };
        let (bo, bn, _) = self.class.member_ref(*r).map_err(me)?;
        if bo != "java/lang/invoke/StringConcatFactory" {
            return Ok(false);
        }
        let m = Method::parse(desc).ok_or("a bad concatenation")?;
        let n = m.params.len();
        let class = self.class;
        let constant = |index: u16| -> Result<String, String> {
            match class.constants.get(usize::from(index)) {
                Some(Constant::String(u)) => Ok(class.utf8(*u).map_err(me)?.to_owned()),
                Some(Constant::Integer(v)) => Ok(v.to_string()),
                Some(Constant::Long(v)) => Ok(v.to_string()),
                Some(Constant::Float(v)) => Ok(rt::java_float(*v)),
                Some(Constant::Double(v)) => Ok(rt::java_double(*v)),
                _ => Err("a bad concatenation constant".into()),
            }
        };
        // The recipe: text, \u{1} for each argument, \u{2} for a constant.
        let recipe = if bn == "makeConcatWithConstants" {
            let mut args = bootstrap.arguments.iter();
            let first = *args.next().ok_or("a concatenation without a recipe")?;
            let mut text = String::new();
            for ch in constant(first)?.chars() {
                match ch {
                    '\u{2}' => text.push_str(&constant(*args.next().ok_or("a bad recipe")?)?),
                    ch => text.push(ch),
                }
            }
            text
        } else {
            "\u{1}".repeat(n)
        };
        let d = f.stack.len();
        let base = d - n;
        let sb = self
            .call_builtin(
                rt::concat::new as *const () as usize,
                "()Ljava/lang/Object;",
                false,
                &[],
            )?
            .ok_or("no builder")?;
        let mut text = String::new();
        let mut k = 0;
        for ch in recipe.chars().chain(std::iter::once('\u{1}')) {
            if ch != '\u{1}' {
                text.push(ch);
                continue;
            }
            if !text.is_empty() {
                let lit = self.string(&text);
                let lit = self.b.ins().iconst(types::I64, lit);
                self.call_builtin(
                    rt::concat::append_string as *const () as usize,
                    "(Ljava/lang/String;)Ljava/lang/Object;",
                    true,
                    &[sb, lit],
                )?;
                text.clear();
            }
            if k == n {
                break;
            }
            let p = m.params[k].as_str();
            let t = kind_type(&f.stack[base + k]).unwrap_or(types::I64);
            let v = self.get(base + k, t);
            let (fp, pdesc) = match p {
                "I" | "S" | "B" => (
                    rt::concat::append_int as *const () as usize,
                    "(I)Ljava/lang/Object;",
                ),
                "Z" => (
                    rt::concat::append_bool as *const () as usize,
                    "(Z)Ljava/lang/Object;",
                ),
                "C" => (
                    rt::concat::append_char as *const () as usize,
                    "(C)Ljava/lang/Object;",
                ),
                "J" => (
                    rt::concat::append_long as *const () as usize,
                    "(J)Ljava/lang/Object;",
                ),
                "F" => (
                    rt::concat::append_float as *const () as usize,
                    "(F)Ljava/lang/Object;",
                ),
                "D" => (
                    rt::concat::append_double as *const () as usize,
                    "(D)Ljava/lang/Object;",
                ),
                "Ljava/lang/String;" => (
                    rt::concat::append_string as *const () as usize,
                    "(Ljava/lang/String;)Ljava/lang/Object;",
                ),
                _ => (
                    rt::concat::append_object as *const () as usize,
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                ),
            };
            self.call_builtin(fp, pdesc, true, &[sb, v])?;
            k += 1;
        }
        let out = self
            .call_builtin(
                rt::concat::finish as *const () as usize,
                "()Ljava/lang/String;",
                true,
                &[sb],
            )?
            .ok_or("no string")?;
        // An argument's toString() may have thrown.
        self.after_call(i)?;
        self.set(base, types::I64, out);
        Ok(true)
    }

    /// A record's `equals`, `hashCode` or `toString` (`invokedynamic`
    /// through ObjectMethods), as the JDK defines them, over its fields.
    /// Whether it was one.
    fn record_method(&mut self, insn: &Instruction, f: &Frame, i: usize) -> Result<bool, String> {
        let me = |e: crate::classfile::Error| e.to_string();
        let Operand::Constant(c) = insn.operand else {
            return Ok(false);
        };
        let Some(Constant::InvokeDynamic(bsm, nat)) = self.class.constants.get(usize::from(c))
        else {
            return Ok(false);
        };
        let (method, desc) = self.class.name_and_type(*nat).map_err(me)?;
        let (method, desc) = (method.to_owned(), desc.to_owned());
        let bootstrap = self
            .class
            .bootstrap_methods()
            .get(usize::from(*bsm))
            .ok_or("a bad bootstrap method")?
            .clone();
        let Some(Constant::MethodHandle(_, r)) = self
            .class
            .constants
            .get(usize::from(bootstrap.method_handle))
        else {
            return Ok(false);
        };
        let (bo, _, _) = self.class.member_ref(*r).map_err(me)?;
        if bo != "java/lang/runtime/ObjectMethods" {
            return Ok(false);
        }
        let mut args = bootstrap.arguments.iter();
        let record = self
            .class
            .class_name(*args.next().ok_or("a record without its class")?)
            .map_err(me)?
            .to_owned();
        let names = match self
            .class
            .constants
            .get(usize::from(*args.next().ok_or("no names")?))
        {
            Some(Constant::String(u)) => self.class.utf8(*u).map_err(me)?.to_owned(),
            _ => return Err("a record without its names".into()),
        };
        // The components: name, field offset, descriptor (from the getters).
        let mut parts: Vec<(String, i32, String)> = Vec::new();
        for (name, h) in names.split(';').filter(|n| !n.is_empty()).zip(args) {
            let Some(Constant::MethodHandle(_, fr)) = self.class.constants.get(usize::from(*h))
            else {
                return Err("a record component without its getter".into());
            };
            let (owner, field, fdesc) = self.class.member_ref(*fr).map_err(me)?;
            let (at, _) = self
                .w
                .layouts
                .get(owner)
                .and_then(|l| l.fields.get(field))
                .copied()
                .ok_or("a record field outside the classes")?;
            parts.push((name.to_owned(), at, fdesc.to_owned()));
        }
        let d = f.stack.len();
        let n = Method::parse(&desc).ok_or("a bad descriptor")?.params.len();
        let base = d - n;
        let this = self.get(base, types::I64);
        self.null_check(this);
        let flags = MemFlagsData::trusted();
        let load = |s: &mut Self, obj: Value, at: i32, fdesc: &str| -> Value {
            let t = desc_type(fdesc).unwrap_or(types::I64);
            s.b.ins().load(t, flags, obj, at)
        };
        let result = match method.as_str() {
            "toString" => {
                // Name[a=1, b=x]: the simple name (after `/` and `$`).
                let simple = record
                    .rsplit(['/', '$'])
                    .next()
                    .unwrap_or(&record)
                    .to_owned();
                let sb = self
                    .call_builtin(
                        rt::concat::new as *const () as usize,
                        "()Ljava/lang/Object;",
                        false,
                        &[],
                    )?
                    .ok_or("no builder")?;
                let lit = |s: &mut Self, text: &str| -> Result<(), String> {
                    let l = s.string(text);
                    let l = s.b.ins().iconst(types::I64, l);
                    s.call_builtin(
                        rt::concat::append_string as *const () as usize,
                        "(Ljava/lang/String;)Ljava/lang/Object;",
                        true,
                        &[sb, l],
                    )?;
                    Ok(())
                };
                lit(self, &format!("{simple}["))?;
                for (k, (name, at, fdesc)) in parts.iter().enumerate() {
                    lit(self, &format!("{}{name}=", if k > 0 { ", " } else { "" }))?;
                    let v = load(self, this, *at, fdesc);
                    let (fp, pdesc) = match fdesc.as_str() {
                        "I" | "S" | "B" => (
                            rt::concat::append_int as *const () as usize,
                            "(I)Ljava/lang/Object;",
                        ),
                        "Z" => (
                            rt::concat::append_bool as *const () as usize,
                            "(Z)Ljava/lang/Object;",
                        ),
                        "C" => (
                            rt::concat::append_char as *const () as usize,
                            "(C)Ljava/lang/Object;",
                        ),
                        "J" => (
                            rt::concat::append_long as *const () as usize,
                            "(J)Ljava/lang/Object;",
                        ),
                        "F" => (
                            rt::concat::append_float as *const () as usize,
                            "(F)Ljava/lang/Object;",
                        ),
                        "D" => (
                            rt::concat::append_double as *const () as usize,
                            "(D)Ljava/lang/Object;",
                        ),
                        _ => (
                            rt::concat::append_object as *const () as usize,
                            "(Ljava/lang/Object;)Ljava/lang/Object;",
                        ),
                    };
                    self.call_builtin(fp, pdesc, true, &[sb, v])?;
                }
                lit(self, "]")?;
                self.call_builtin(
                    rt::concat::finish as *const () as usize,
                    "()Ljava/lang/String;",
                    true,
                    &[sb],
                )?
                .ok_or("no string")?
            }
            "hashCode" => {
                // 31 * h + each component's hash, from 0.
                let mut h = self.b.ins().iconst(types::I32, 0);
                for (_, at, fdesc) in &parts {
                    let v = load(self, this, *at, fdesc);
                    let ph = self.primitive_hash(v, fdesc)?;
                    let m = self.b.ins().imul_imm_s(h, 31);
                    h = self.b.ins().iadd(m, ph);
                }
                h
            }
            "equals" => {
                let other = self.get(base + 1, types::I64);
                let id = self
                    .w
                    .ids
                    .get(record.as_str())
                    .copied()
                    .ok_or("an unknown record")?;
                let same_class = self.instance_test(other, &record, id)?;
                let out = self.b.create_block();
                self.b.append_block_param(out, types::I32);
                let mut ok = same_class;
                // Each component, while all are equal so far.
                for (_, at, fdesc) in &parts {
                    let next = self.b.create_block();
                    let fail = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().brif(ok, next, &[], out, &[fail.into()]);
                    self.b.switch_to_block(next);
                    let a = load(self, this, *at, fdesc);
                    let b2 = load(self, other, *at, fdesc);
                    ok = self.component_equals(a, b2, fdesc)?;
                }
                self.b.ins().jump(out, &[ok.into()]);
                self.b.switch_to_block(out);
                self.b.block_params(out)[0]
            }
            _ => return Err(format!("record method {method}")),
        };
        self.after_call(i)?;
        let t =
            desc_type(&Method::parse(&desc).ok_or("a bad descriptor")?.ret).unwrap_or(types::I32);
        self.set(base, t, result);
        Ok(true)
    }

    /// The hash of a component, as its wrapper's `hashCode` (or
    /// `Objects.hashCode` for a reference).
    fn primitive_hash(&mut self, v: Value, fdesc: &str) -> Result<Value, String> {
        let ins = self.b.ins();
        Ok(match fdesc {
            "I" | "S" | "B" | "C" => v,
            "Z" => {
                let t = ins.iconst(types::I32, 1231);
                let f = self.b.ins().iconst(types::I32, 1237);
                self.b.ins().select(v, t, f)
            }
            "J" => {
                let hi = ins.ushr_imm_s(v, 32);
                let x = self.b.ins().bxor(v, hi);
                self.b.ins().ireduce(types::I32, x)
            }
            "F" => self.float_bits(v, types::F32),
            "D" => {
                let bits = self.float_bits(v, types::F64);
                let hi = self.b.ins().ushr_imm_s(bits, 32);
                let x = self.b.ins().bxor(bits, hi);
                self.b.ins().ireduce(types::I32, x)
            }
            _ => self
                .call_builtin(
                    rt::objects_hash_code as *const () as usize,
                    "(Ljava/lang/Object;)I",
                    false,
                    &[v],
                )?
                .ok_or("no hash")?,
        })
    }

    /// `floatToIntBits` / `doubleToLongBits`: the bits, every NaN the same.
    fn float_bits(&mut self, v: Value, t: Type) -> Value {
        let (it, canon) = if t == types::F32 {
            (types::I32, 0x7fc0_0000i64)
        } else {
            (types::I64, 0x7ff8_0000_0000_0000i64)
        };
        let bits = self.b.ins().bitcast(it, MemFlagsData::new(), v);
        let nan = self.b.ins().fcmp(FloatCC::Unordered, v, v);
        let c = self.b.ins().iconst(it, canon);
        self.b.ins().select(nan, c, bits)
    }

    /// Whether two component values are equal as a record compares them.
    fn component_equals(&mut self, a: Value, b: Value, fdesc: &str) -> Result<Value, String> {
        Ok(match fdesc {
            "F" | "D" => {
                let t = if fdesc == "F" { types::F32 } else { types::F64 };
                let x = self.float_bits(a, t);
                let y = self.float_bits(b, t);
                let e = self.b.ins().icmp(IntCC::Equal, x, y);
                self.b.ins().uextend(types::I32, e)
            }
            "I" | "S" | "B" | "C" | "Z" | "J" => {
                let e = self.b.ins().icmp(IntCC::Equal, a, b);
                self.b.ins().uextend(types::I32, e)
            }
            _ => self
                .call_builtin(
                    rt::objects_equals as *const () as usize,
                    "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                    false,
                    &[a, b],
                )?
                .ok_or("no equals")?,
        })
    }

    /// The Class object of class `name` (made for a class outside the program).
    fn class_object(&mut self, name: &str) -> i64 {
        if let Some(&at) = self.w.class_at.get(name) {
            return at;
        }
        let mut others = self.w.other_classes.borrow_mut();
        if let Some((_, c, _)) = others.iter().find(|(n, _, _)| n == name) {
            return c.as_ptr() as i64;
        }
        let text: Vec<u16> = name.replace('/', ".").encode_utf16().collect();
        let s = rt::literal(&text, self.w.string_vt as u64);
        let c: Box<[u64]> = vec![self.w.class_vt, 0, s.as_ptr() as u64].into_boxed_slice();
        let at = c.as_ptr() as i64;
        others.push((name.to_owned(), c, s));
        at
    }

    /// The object of string literal `text` (the same for each use).
    fn string(&mut self, text: &str) -> i64 {
        let mut strings = self.w.strings.borrow_mut();
        if let Some(o) = strings.get(text) {
            return o.as_ptr() as i64;
        }
        let units: Vec<u16> = text.encode_utf16().collect();
        let o = rt::literal(&units, self.w.string_vt as u64);
        let at = o.as_ptr() as i64;
        strings.insert(text.to_owned(), o);
        at
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
        self.b.ins().call(f, &[self.ctx]);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
    }

    fn null_check(&mut self, obj: Value) {
        let null = self.b.ins().icmp_imm_s(IntCC::Equal, obj, 0);
        self.throw_if(null, "java/lang/NullPointerException");
    }

    /// Code the prototype does not compile: a stop (a trap), counted by why.
    fn stop(&mut self, why: String) {
        *self.w.stops.borrow_mut().entry(why.clone()).or_default() += 1;
        self.trap_with(why);
    }

    /// A guard's trap: like a stop, but not counted (the code it guards is
    /// compiled; it traps only on what the program could not have meant).
    fn trap_with(&mut self, why: String) {
        // Reached, it says where and why before the trap.
        let id = rt::stop_reason(format!(
            "{}.{}{}: {why}",
            self.owner, self.method.name, self.method.descriptor
        ));
        let id = self.b.ins().iconst(types::I32, i64::from(id));
        let _ = self.call_builtin(rt::stopped as *const () as usize, "(I)V", false, &[id]);
        self.b.ins().trap(UNSUPPORTED);
    }

    /// Throws a new `class` (a built-in exception) when `cond` is nonzero.
    fn throw_if(&mut self, cond: Value, class: &str) {
        let Some(vt) = self.w.layouts.get(class).map(|l| l.vtable as i64) else {
            self.b.ins().trapnz(cond, UNSUPPORTED);
            return;
        };
        let throw = self.b.create_block();
        let on = self.b.create_block();
        self.b.set_cold_block(throw);
        self.b.ins().brif(cond, throw, &[], on, &[]);
        self.b.switch_to_block(throw);
        let v = self.b.ins().iconst(types::I64, vt);
        let _ = self.call_builtin(rt::throw_new as *const () as usize, "(J)V", false, &[v]);
        match self.dispatch(self.cur) {
            Ok(go) => {
                self.b.ins().jump(go, &[]);
            }
            Err(_) => {
                self.stop("exception dispatch".into());
            }
        }
        self.b.switch_to_block(on);
    }

    /// A call to a runtime method at address `f` with descriptor `desc`.
    fn call_builtin(
        &mut self,
        f: usize,
        desc: &str,
        instance: bool,
        args: &[Value],
    ) -> Result<Option<Value>, String> {
        let sig = raw_signature(self.module, instance, desc).ok_or("a bad signature")?;
        let sr = self.b.import_signature(sig);
        let fp = self.b.ins().iconst(types::I64, f as i64);
        let call = self.b.ins().call_indirect(sr, fp, args);
        Ok(self.b.inst_results(call).first().copied())
    }

    fn host(&mut self, name: &str, args: &[Value]) -> Option<Value> {
        let id = self.w.host[name];
        let f = self.module.declare_func_in_func(id, self.b.func);
        let call = self.b.ins().call(f, args);
        self.b.inst_results(call).first().copied()
    }

    /// Bump allocation of `size` bytes (zeroed), the header set to `header`.
    /// Whether `obj` is an instance of `class` (id `id`), 0 for null: its
    /// vtable compared with the class's own first, then its class's bit for
    /// `class` (no call either way).
    fn instance_test(&mut self, obj: Value, class: &str, id: u32) -> Result<Value, String> {
        let i32t = types::I32;
        let zero = self.b.ins().iconst(i32t, 0);
        let one = self.b.ins().iconst(i32t, 1);
        let done = self.b.create_block();
        self.b.append_block_param(done, i32t);
        let present = self.b.create_block();
        self.b.ins().brif(obj, present, &[], done, &[zero.into()]);
        self.b.switch_to_block(present);
        self.b.seal_block(present);
        if let Some(layout) = self.w.layouts.get(class) {
            let vt = self
                .b
                .ins()
                .load(types::I64, MemFlagsData::trusted(), obj, 0);
            let same = self
                .b
                .ins()
                .icmp_imm_s(IntCC::Equal, vt, layout.vtable as i64);
            let slow = self.b.create_block();
            self.b.set_cold_block(slow);
            self.b.ins().brif(same, done, &[one.into()], slow, &[]);
            self.b.switch_to_block(slow);
            self.b.seal_block(slow);
        }
        // Else the class's bit for `class`: its id's bits, from the table.
        let k = {
            let mut tested = self.w.tested.borrow_mut();
            let n = tested.len() as u32;
            *tested.entry(id).or_insert(n)
        };
        let i64t = types::I64;
        let vt = self.b.ins().load(i64t, MemFlagsData::trusted(), obj, 0);
        let class_id = self.b.ins().load(i64t, MemFlagsData::trusted(), vt, 0);
        let slot = self.b.ins().imul_imm_s(class_id, 8);
        let table = self.b.ins().iconst(i64t, self.w.bit_table_at);
        let slot = self.b.ins().iadd(table, slot);
        let bits = self.b.ins().load(i64t, MemFlagsData::trusted(), slot, 0);
        let word = self
            .b
            .ins()
            .load(i64t, MemFlagsData::trusted(), bits, (8 * (k / 64)) as i32);
        let bit = self.b.ins().ushr_imm_s(word, i64::from(k % 64));
        let bit = self.b.ins().band_imm_s(bit, 1);
        let r = self.b.ins().ireduce(i32t, bit);
        self.b.ins().jump(done, &[r.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        Ok(self.b.block_params(done)[0])
    }

    /// A synchronized method lets go of its monitor (before it returns).
    fn leave_monitor(&mut self) -> Result<(), String> {
        if let Some(o) = self.sync_on {
            let exit = rt::threads::monitor_exit as *const () as usize;
            self.call_builtin(exit, "(Ljava/lang/Object;)V", false, &[o])?;
        }
        Ok(())
    }

    /// A compiled method's arguments: this thread's context first.
    fn with_ctx(&self, args: &[Value]) -> Vec<Value> {
        std::iter::once(self.ctx)
            .chain(args.iter().copied())
            .collect()
    }

    fn alloc(&mut self, size: Value, header: Value) -> Value {
        alloc_in(&mut self.b, self.ctx, size, header)
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
                    Some(Constant::String(u)) => {
                        let text = self.class.utf8(*u).map_err(me)?.to_owned();
                        let at = self.string(&text);
                        (self.b.ins().iconst(i64t, at), i64t)
                    }
                    Some(Constant::Class(_)) => {
                        let name = self.class.class_name(c).map_err(me)?.to_owned();
                        let at = self.class_object(&name);
                        (self.b.ins().iconst(i64t, at), i64t)
                    }
                    _ => {
                        self.stop("ldc of a method handle, method type or dynamic constant".into());
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
                        self.leave_monitor()?;
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
                        self.leave_monitor()?;
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
                    // A JDK constant the runtime library provides.
                    if op == 0xB2
                        && let Some((f, desc)) = rt::static_getter(owner, name)
                    {
                        let t = Method::parse(desc)
                            .and_then(|m| desc_type(&m.ret))
                            .ok_or("a bad getter")?;
                        let v = self.call_builtin(f, desc, false, &[])?.ok_or("no value")?;
                        self.set(d, t, v);
                        return Ok(false);
                    }
                    self.stop(format!("static field of {owner}.{name}"));
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
                    self.stop(format!("field of {owner}"));
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
            0xB6..=0xB9 => return self.invoke(insn, f, i),
            0xBA if self.concat(insn, f, i)? => {}
            0xBA if self.record_method(insn, f, i)? => {}
            0xBA => {
                let site = self
                    .w
                    .lambda_at
                    .get(&(
                        self.owner.to_owned(),
                        format!("{}{}", self.method.name, self.method.descriptor),
                        insn.at,
                    ))
                    .copied();
                let Some(n) = site else {
                    self.stop("invokedynamic (another bootstrap)".into());
                    return Ok(true);
                };
                let l = &self.w.lambdas[n];
                let obj = match self.w.singleton_at[n] {
                    Some(at) => self.b.ins().iconst(i64t, at),
                    None => {
                        let k = l.captures.len();
                        let size = self.b.ins().iconst(i64t, 8 + 8 * k as i64);
                        let header = self.b.ins().iconst(i64t, self.w.lambda_vt[n]);
                        let obj = alloc_in(&mut self.b, self.ctx, size, header);
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
                    self.stop(format!("new {class}"));
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
                self.throw_if(neg, "java/lang/NegativeArraySizeException");
                let len64 = self.b.ins().uextend(i64t, len);
                let bytes = self.b.ins().imul_imm_s(len64, elem);
                let size = self.b.ins().iadd_imm_s(bytes, 16);
                // An array's header: its element size.
                let header = self.b.ins().iconst(i64t, elem);
                let arr = self.alloc(size, header);
                self.b.ins().store(MemFlagsData::trusted(), len64, arr, 8);
                self.set(d - 1, i64t, arr);
            }
            0xC5 => {
                let Operand::Dimensions(c, dims) = insn.operand else {
                    unreachable!()
                };
                let desc = self.class.class_name(c).map_err(me)?.to_owned();
                let dims = usize::from(dims);
                if !(1..=4).contains(&dims) || desc.len() < dims + 1 {
                    self.stop(format!("multianewarray of {dims} dimensions"));
                    return Ok(true);
                }
                let (inner, _) = element(&desc[dims - 1..]);
                let mut args = vec![
                    self.b.ins().iconst(i64t, i64::from(inner)),
                    self.b.ins().iconst(i32t, dims as i64),
                ];
                for k in 0..4 {
                    args.push(if k < dims {
                        self.get(d - dims + k, i32t)
                    } else {
                        self.b.ins().iconst(i32t, 0)
                    });
                }
                let f = rt::multi_array as *const () as usize;
                let arr = self
                    .call_builtin(f, "(JIIIII)Ljava/lang/Object;", false, &args)?
                    .ok_or("no array")?;
                self.after_call(i)?;
                self.set(d - dims, i64t, arr);
            }
            0xBE => {
                let arr = self.get(d - 1, i64t);
                self.null_check(arr);
                let len = self.b.ins().load(i32t, MemFlagsData::trusted(), arr, 8);
                self.set(d - 1, i32t, len);
            }
            0xBF => {
                let exc = self.get(d - 1, i64t);
                self.null_check(exc);
                let g = self.ctx;
                self.b
                    .ins()
                    .store(MemFlagsData::trusted(), exc, g, EXCEPTION);
                let go = self.dispatch(i)?;
                self.b.ins().jump(go, &[]);
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
                        Some(self.instance_test(obj, &class, id)?)
                    }
                    _ => None,
                };
                if op == 0xC0 {
                    if let Some(ok) = r {
                        // null passes a checkcast.
                        let null = self.b.ins().icmp_imm_s(IntCC::Equal, obj, 0);
                        let null = self.b.ins().uextend(i32t, null);
                        let pass = self.b.ins().bor(ok, null);
                        let fail = self.b.ins().icmp_imm_s(IntCC::Equal, pass, 0);
                        self.throw_if(fail, "java/lang/ClassCastException");
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
            // synchronized blocks: the object's monitor.
            0xC2 | 0xC3 => {
                let o = self.get(d - 1, i64t);
                self.null_check(o);
                let f = if op == 0xC2 {
                    rt::threads::monitor_enter as *const () as usize
                } else {
                    rt::threads::monitor_exit as *const () as usize
                };
                self.call_builtin(f, "(Ljava/lang/Object;)V", false, &[o])?;
                if op == 0xC3 {
                    self.after_call(i)?;
                }
            }
            _ => {
                self.stop(format!("opcode {op:#x}"));
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
        self.throw_if(out, "java/lang/ArrayIndexOutOfBoundsException");
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
            // dup2_x2's four forms, by which of the top values are wide.
            0x5E if wide(0) && wide(1) => vec![(d - 1, d), (d - 2, d - 1), (d, d - 2)],
            0x5E if wide(0) => vec![(d - 1, d), (d - 2, d - 1), (d - 3, d - 2), (d, d - 3)],
            0x5E if wide(2) => vec![
                (d - 1, d + 1),
                (d - 2, d),
                (d - 3, d - 1),
                (d + 1, d - 2),
                (d, d - 3),
            ],
            0x5E => vec![
                (d - 1, d + 1),
                (d - 2, d),
                (d - 3, d - 1),
                (d - 4, d - 2),
                (d + 1, d - 3),
                (d, d - 4),
            ],
            _ => return Err(format!("stack opcode {op:#x}")),
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
                let zero_div = self.b.ins().icmp_imm_s(IntCC::Equal, b, 0);
                self.throw_if(zero_div, "java/lang/ArithmeticException");
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

    fn invoke(&mut self, insn: &Instruction, f: &Frame, i: usize) -> Result<bool, String> {
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
        // An enum's ordinal() and name() (final in Enum): its field, no call.
        if matches!(op, 0xB6 | 0xB9)
            && ((name == "ordinal" && desc == "()I")
                || (name == "name" && desc == "()Ljava/lang/String;"))
            && self.w.is_enum(&owner)
        {
            self.null_check(args[0]);
            let (t, at) = if name == "ordinal" {
                (types::I32, 16)
            } else {
                (types::I64, 8)
            };
            let v = self.b.ins().load(t, MemFlagsData::trusted(), args[0], at);
            self.set(base, t, v);
            return Ok(false);
        }
        // A direct call to a method that cannot throw: no test after it.
        let mut nothrow = false;
        // An array's clone: a copy; its other methods are Object's.
        if owner.starts_with('[') && name == "clone" {
            self.null_check(args[0]);
            let r = self
                .call_builtin(
                    rt::array_clone as *const () as usize,
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    false,
                    &[args[0]],
                )?
                .ok_or("no clone")?;
            self.set(base, types::I64, r);
            return Ok(false);
        }
        let owner = if owner.starts_with('[') {
            OBJECT.to_owned()
        } else {
            owner
        };
        // JDK methods as instructions.
        if op == 0xB8
            && let Some(v) = self.intrinsic(&owner, &name, &desc, &args)
        {
            if let (Some(t), Some(v)) = (ret, v) {
                self.set(base, t, v);
            }
            return Ok(false);
        }
        // Object's constructor does nothing; nor (for now) a JDK exception's.
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
            let result = self.inline(key, &declaring, &name, &desc, &args[1..], ret, i)?;
            if let (Some(t), Some(v)) = (ret, result) {
                self.set(base, t, v);
            }
            return Ok(false);
        }
        let result = match op {
            0xB8 | 0xB7 => {
                let Some(declaring) = self.w.resolve(&owner, &name, &desc) else {
                    // A runtime library method (String.valueOf, a constructor
                    // of a JDK class, super() into one).
                    match self.w.builtin_method(&owner, &name, &desc) {
                        Some((f, is_static, _)) if is_static == (op == 0xB8) => {
                            if op == 0xB7 {
                                self.null_check(args[0]);
                            }
                            let r = self.call_builtin(f, &desc, op == 0xB7, &args)?;
                            self.after_call(i)?;
                            if let (Some(t), Some(v)) = (ret, r) {
                                self.set(base, t, v);
                            }
                            return Ok(false);
                        }
                        _ => {
                            self.stop(format!("call to {owner}.{name}"));
                            return Ok(true);
                        }
                    }
                };
                // Classes compiled against another version of their parent
                // (a method of it made static, or not): the JVM's
                // IncompatibleClassChangeError.
                if self.w.is_static(&declaring, &name, &desc) != (op == 0xB8) {
                    self.stop(format!(
                        "call to {owner}.{name}{desc}, static in one class and not the other"
                    ));
                    return Ok(true);
                }
                if op == 0xB8 {
                    self.init(&declaring);
                } else {
                    self.null_check(args[0]);
                }
                let id = self.w.funcs[&export_name(&declaring, &name, &desc)];
                let fr = self.module.declare_func_in_func(id, self.b.func);
                let all = self.with_ctx(&args);
                let call = self.b.ins().call(fr, &all);
                nothrow = self.w.cannot_throw(&declaring, &name, &desc);
                self.b.inst_results(call).first().copied()
            }
            _ if self.w.resolve(&owner, &name, &desc).is_none()
                && matches!(
                    self.w.builtin_method(&owner, &name, &desc),
                    Some((_, false, true))
                ) =>
            {
                // A method of a final runtime class (String, a box): direct.
                let (f, _, _) = self
                    .w
                    .builtin_method(&owner, &name, &desc)
                    .expect("matched");
                self.null_check(args[0]);
                self.call_builtin(f, &desc, true, &args)?
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
                            && !self.w.is_static(c, &name, &desc)
                    });
                match (direct, self.w.selectors.get(&key)) {
                    (Some(declaring), _) => {
                        let id = self.w.funcs[&export_name(&declaring, &name, &desc)];
                        let fr = self.module.declare_func_in_func(id, self.b.func);
                        let all = self.with_ctx(&args);
                        let call = self.b.ins().call(fr, &all);
                        nothrow = self.w.cannot_throw(&declaring, &name, &desc);
                        self.b.inst_results(call).first().copied()
                    }
                    (None, Some(&sel)) => {
                        // Through the dispatch table: only for a receiver
                        // whose type responds to it (a method declared only
                        // by a JDK class outside the program has no entries).
                        // For a type outside the program, checked as called:
                        // the entry is the method's when tagged with it.
                        let fp = match self.w.responds(&owner, sel) {
                            Some(false) => {
                                self.stop(format!(
                                    "virtual call to {owner}.{name}{desc}, outside the program"
                                ));
                                return Ok(true);
                            }
                            Some(true) => dispatch_fp(&mut self.b, self.w, args[0], sel),
                            None => {
                                let at = dispatch_entry(&mut self.b, self.w, args[0], sel);
                                let tag =
                                    self.b
                                        .ins()
                                        .load(types::I64, MemFlagsData::trusted(), at, 8);
                                let bad =
                                    self.b
                                        .ins()
                                        .icmp_imm_s(IntCC::NotEqual, tag, sel as i64 + 1);
                                let stop = self.b.create_block();
                                let on = self.b.create_block();
                                self.b.set_cold_block(stop);
                                self.b.ins().brif(bad, stop, &[], on, &[]);
                                self.b.switch_to_block(stop);
                                self.trap_with(format!("virtual call to {owner}.{name}{desc}, not implemented by the receiver's class"));
                                self.b.switch_to_block(on);
                                self.b
                                    .ins()
                                    .load(types::I64, MemFlagsData::trusted(), at, 0)
                            }
                        };
                        let sig = signature(self.module, true, &desc).ok_or("a bad signature")?;
                        let all = self.with_ctx(&args);
                        nothrow = self.w.no_implementation_throws(&name, &desc);
                        // The likely implementations called directly (and so
                        // inlined) when the vtable holds them.
                        let join = self.b.create_block();
                        if let Some(t) = ret {
                            self.b.append_block_param(join, t);
                        }
                        for declaring in self.w.guards(&name, &desc) {
                            let id = self.w.funcs[&export_name(&declaring, &name, &desc)];
                            let fr = self.module.declare_func_in_func(id, self.b.func);
                            let at = self.b.ins().func_addr(types::I64, fr);
                            let hit = self.b.ins().icmp(IntCC::Equal, fp, at);
                            let direct = self.b.create_block();
                            let next = self.b.create_block();
                            self.b.ins().brif(hit, direct, &[], next, &[]);
                            self.b.switch_to_block(direct);
                            self.b.seal_block(direct);
                            let call = self.b.ins().call(fr, &all);
                            let out: Vec<BlockArg> = self
                                .b
                                .inst_results(call)
                                .iter()
                                .map(|&v| v.into())
                                .collect();
                            self.b.ins().jump(join, &out);
                            self.b.switch_to_block(next);
                            self.b.seal_block(next);
                        }
                        let sr = self.b.import_signature(sig);
                        let call = self.b.ins().call_indirect(sr, fp, &all);
                        let out: Vec<BlockArg> = self
                            .b
                            .inst_results(call)
                            .iter()
                            .map(|&v| v.into())
                            .collect();
                        self.b.ins().jump(join, &out);
                        self.b.switch_to_block(join);
                        self.b.seal_block(join);
                        self.b.block_params(join).first().copied()
                    }
                    (None, None) => {
                        self.stop(format!("virtual call to {owner}.{name}"));
                        return Ok(true);
                    }
                }
            }
        };
        // It may have thrown.
        if !nothrow {
            self.after_call(i)?;
        }
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
