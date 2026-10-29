//! The compiler (docs/java-compat/README.md section 6): the classes of one
//! jar become one WASM module.
//!
//! The JVM's operand stack and local variables both become WASM locals: one
//! for each stack depth or variable slot, per kind. Every push and pop is a
//! local set or get (Cranelift keeps them in registers), so values never have
//! to cross a block boundary on WASM's own stack. A method's basic blocks are
//! cases of one dispatch loop: any jump the JVM allows compiles.
//!
//! References are `anyref`. An object of a class in the jar is a GC struct:
//! field 0 is its class id, field 1 its peer, then the instance fields, the
//! superclass's first. A lambda is a struct too: its site's id, no peer,
//! then the captured values. An array is a GC array (`i8` for boolean and
//! byte, `i16` for char and short, `anyref` for every reference type). Every
//! other object belongs to the host (the game and the JDK, given by the
//! compatibility layer) and is an `externref` converted to `anyref`.
//!
//! A class of the jar can extend a game class (a block, an item). Its
//! superclass constructor is the import `extend`: the host makes a game
//! object, the peer, that stands for the mod object in the game, and the
//! peer goes in field 1. The game's fields and methods of the object are
//! the peer's.
//!
//! A virtual call goes through a dispatcher, one for each method name and
//! descriptor: by the object's class id it calls the method of the jar that
//! the class has, or else the game's method of its superclass (import
//! `super`), or for an object of the host, the host's method. Each
//! dispatcher is exported as `virtual <name> <descriptor>`: the game calls
//! the overrides of a mod's objects through it.
//!
//! Whatever is outside the jar is an import of module `host`, named
//! `<operation> <owner>.<member> <descriptor>`: `invokestatic`,
//! `invokevirtual`, `invokeinterface`, `invokespecial` (a game method of the
//! superclass, on a mod object), `new` (a constructor: the arguments in, the
//! object out), `extend` (the superclass constructor of a mod object: the
//! object and the arguments in, the peer out), `super` (from a dispatcher),
//! `getstatic`, `putstatic`, `getfield`, `putfield` and `instanceof <class>`.
//! And: `constant` (an `ldc` string or class, by index into
//! [`Compiled::constants`]), `same` (`==` on references), `concat
//! <descriptor> <recipe>` (string concatenation, StringConcatFactory's
//! recipe) and `record <class>.<method> <descriptor>` (a record's
//! `toString`, `equals` or `hashCode`; [`ClassInfo::fields`] names the
//! fields).
//!
//! Each method is exported as `<class>.<name><descriptor>`, each lambda as
//! `lambda <n>`, each class's allocator as `new <class>`, and each static
//! field's getter and setter as `get <class>.<field>` and `set ...`.
use crate::{
    ClassFile, Constant,
    bytecode::{Instruction, Operand, decode},
    classfile::{Attribute, Member},
    descriptor::{Kind, Method},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
};
use wasm_encoder::{
    BlockType, CodeSection, CompositeInnerType, CompositeType, ConstExpr, EntityType, ExportKind,
    ExportSection, FieldType, Function, FunctionSection, GlobalSection, GlobalType, HeapType,
    ImportSection, Instruction as W, Module, RefType, StorageType, StructType, SubType,
    TypeSection, ValType,
};

pub type Result<T> = std::result::Result<T, String>;

const ANY: ValType = ValType::Ref(RefType::ANYREF);
const STATIC: u16 = 0x0008;
/// Calls to imports carry this bit until the imports are counted.
const IMPORT: u32 = 1 << 31;
/// Calls to dispatchers and type tests, made after the methods.
const LATE: u32 = 1 << 30;
/// The struct type all objects share: the class id and the peer.
const OBJECT: u32 = 0;
/// The array types, by [`array_kind`]: i8, i16, i32, i64, f32, f64, anyref.
const ARRAYS: [u32; 7] = [1, 2, 3, 4, 5, 6, 7];

/// The array type of a component descriptor.
fn array_kind(component: &str) -> usize {
    match component.as_bytes().first() {
        Some(b'Z' | b'B') => 0,
        Some(b'C' | b'S') => 1,
        Some(b'I') => 2,
        Some(b'J') => 3,
        Some(b'F') => 4,
        Some(b'D') => 5,
        _ => 6,
    }
}

fn storage(kind: usize) -> StorageType {
    match kind {
        0 => StorageType::I8,
        1 => StorageType::I16,
        2 => StorageType::Val(ValType::I32),
        3 => StorageType::Val(ValType::I64),
        4 => StorageType::Val(ValType::F32),
        5 => StorageType::Val(ValType::F64),
        _ => StorageType::Val(ANY),
    }
}

/// Every struct of the module is an object of the jar: testing for a struct
/// (not for the concrete type) also keeps wasmtime's subtype check away
/// from host objects, which it does not expect there.
const STRUCT_REF: HeapType = HeapType::Abstract {
    shared: false,
    ty: wasm_encoder::AbstractHeapType::Struct,
};

const ARRAY_REF: HeapType = HeapType::Abstract {
    shared: false,
    ty: wasm_encoder::AbstractHeapType::Array,
};

fn val(kind: Kind) -> ValType {
    match kind {
        Kind::Int => ValType::I32,
        Kind::Long => ValType::I64,
        Kind::Float => ValType::F32,
        Kind::Double => ValType::F64,
        Kind::Ref => ANY,
    }
}

/// The JDK classes a class of the jar can extend as if it extended nothing.
fn base_class(class: &str) -> bool {
    matches!(class, "java/lang/Object" | "java/lang/Record")
}

fn zero(kind: Kind) -> W<'static> {
    match kind {
        Kind::Int => W::I32Const(0),
        Kind::Long => W::I64Const(0),
        Kind::Float => W::F32Const(0.0.into()),
        Kind::Double => W::F64Const(0.0.into()),
        Kind::Ref => W::RefNull(HeapType::ANY),
    }
}

/// The export name of a method: `owner.name(descriptor)`.
pub fn export_name(owner: &str, name: &str, descriptor: &str) -> String {
    format!("{owner}.{name}{descriptor}")
}

/// A constant the host makes: an `ldc` of a string or a class.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HostConstant {
    String(String),
    Class(String),
}

/// A lambda site (an `invokedynamic` through LambdaMetafactory). Its
/// objects implement `interface`; the host calls `method` on one through
/// the export `lambda <n>`, with the object and the method's arguments as
/// `descriptor` gives them (the erased form).
#[derive(Clone, Debug)]
pub struct Lambda {
    pub interface: String,
    pub method: String,
    pub descriptor: String,
    /// The method it calls: `owner.name descriptor`. NeoForge reads a
    /// listener's event type from its parameter.
    pub target: String,
}

/// A compiled jar.
pub struct Compiled {
    pub wasm: Vec<u8>,
    /// The methods not compiled, with the reason: each one traps when
    /// called (docs/java-compat/plan.md: stub what is missing, and say so).
    pub skipped: Vec<(String, String)>,
    /// The classes; an object of class `classes[i]` has id `i + 1`.
    pub classes: Vec<String>,
    /// The lambda sites; an object of site `n` has id `classes.len() + 1 + n`.
    pub lambdas: Vec<Lambda>,
    pub constants: Vec<HostConstant>,
    /// The classes, as the host needs them, in the order of `classes`.
    pub info: Vec<ClassInfo>,
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub name: String,
    pub superclass: Option<String>,
    pub interfaces: Vec<String>,
    /// The game class it extends, through the jar's own classes: its
    /// objects have a peer of this class.
    pub game_superclass: Option<String>,
    /// The instance fields, as the struct has them: field `i` of the list
    /// is struct field `i + 2`.
    pub fields: Vec<String>,
}

impl Compiled {
    /// What an object id is: a class, or a lambda site.
    pub fn object(&self, id: i32) -> Option<Object<'_>> {
        let i = usize::try_from(id).ok()?.checked_sub(1)?;
        match self.classes.get(i) {
            Some(c) => Some(Object::Class(c)),
            None => self
                .lambdas
                .get(i - self.classes.len())
                .map(|l| Object::Lambda(i - self.classes.len(), l)),
        }
    }
}

pub enum Object<'a> {
    Class(&'a str),
    Lambda(usize, &'a Lambda),
}

enum TypeDef {
    Func(Vec<ValType>, Vec<ValType>),
    /// A struct type: its supertype, and its fields after the class id and
    /// the peer.
    Struct(u32, Vec<(ValType, bool)>),
    Array(StorageType),
}

/// A function made after the methods, when all classes and lambdas are
/// known.
#[derive(Clone, PartialEq, Eq)]
enum Late {
    /// The dispatcher of a method name and descriptor.
    Virtual(String, String),
    /// `instanceof` a class or interface of the jar.
    InstanceOf(String),
    /// `multianewarray` of an array descriptor, with this many lengths.
    MultiArray(String, u32),
}

struct Site {
    sam: String,
    captures: Vec<String>,
    /// The method handle's kind (JVMS 5.4.3.5), owner, name, descriptor.
    target: (u8, String, String, String),
    ty: u32,
}

/// How a call is made, by the instruction or method handle.
#[derive(Clone, Copy, PartialEq)]
enum Call {
    Static,
    Virtual,
    Interface,
    Special,
}

struct Plan<'c> {
    classes: BTreeMap<String, &'c ClassFile>,
    ids: BTreeMap<String, u32>,
    /// (class, method, descriptor) → function index.
    functions: BTreeMap<(String, String, String), u32>,
    /// (class, field) → (global index, kind).
    statics: BTreeMap<(String, String), (u32, Kind)>,
    /// class → (init flag global, its initializer function).
    inits: BTreeMap<String, (u32, Option<u32>)>,
    /// class → the function that runs its initializer once.
    ensure: BTreeMap<String, u32>,
    /// class → (struct type, fields: (declaring class, name, kind)).
    structs: BTreeMap<String, (u32, Vec<(String, String, Kind)>)>,
    types: RefCell<Vec<TypeDef>>,
    imports: RefCell<Vec<(String, u32)>>,
    globals: RefCell<Vec<(Kind, Option<ConstExpr>)>>,
    constants: RefCell<Vec<(HostConstant, u32)>>,
    sites: RefCell<Vec<(Lambda, Site)>>,
    late: RefCell<Vec<Late>>,
    /// The (name, descriptor) of every instance method the jar declares.
    virtuals: BTreeSet<(String, String)>,
    /// class → (name, descriptor) → the function its objects run: its own
    /// method, else its superclass's, else a default method of an
    /// interface.
    vtables: HashMap<String, HashMap<(String, String), u32>>,
    /// (name, descriptor) → function → the ids of the objects that run it
    /// (classes that can have objects, and lambdas, added later).
    implementations: HashMap<(String, String), BTreeMap<u32, Vec<i32>>>,
    /// class → itself and every class and interface above it.
    ancestors: HashMap<String, BTreeSet<String>>,
}

impl Plan<'_> {
    fn type_index(&self, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        let mut types = self.types.borrow_mut();
        let found = types
            .iter()
            .position(|t| matches!(t, TypeDef::Func(p, r) if *p == params && *r == results));
        if let Some(i) = found {
            return i as u32;
        }
        types.push(TypeDef::Func(params, results));
        (types.len() - 1) as u32
    }
    fn struct_type(&self, supertype: u32, fields: Vec<(ValType, bool)>) -> u32 {
        let mut types = self.types.borrow_mut();
        types.push(TypeDef::Struct(supertype, fields));
        (types.len() - 1) as u32
    }

    /// The function index of an import, numbered once the imports are known.
    fn import(&self, name: String, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        let ty = self.type_index(params, results);
        let mut imports = self.imports.borrow_mut();
        if let Some(i) = imports.iter().position(|(n, _)| *n == name) {
            return IMPORT | i as u32;
        }
        imports.push((name, ty));
        IMPORT | (imports.len() - 1) as u32
    }
    fn host_method(&self, op: &str, owner: &str, name: &str, desc: &str) -> Result<u32> {
        let m = Method::parse(desc).ok_or("a bad method descriptor")?;
        let mut params = Vec::new();
        if matches!(
            op,
            "invokevirtual" | "invokeinterface" | "invokespecial" | "extend" | "super"
        ) {
            params.push(ANY);
        }
        params.extend(m.param_kinds().into_iter().map(val));
        let results = if op == "new" || op == "extend" {
            vec![ANY]
        } else {
            m.return_kind().map(val).into_iter().collect()
        };
        Ok(self.import(format!("{op} {owner}.{name} {desc}"), params, results))
    }
    fn host_field(&self, op: &str, owner: &str, name: &str, kind: Kind, desc: &str) -> u32 {
        let (params, results) = match op {
            "getstatic" => (vec![], vec![val(kind)]),
            "putstatic" => (vec![val(kind)], vec![]),
            "getfield" => (vec![ANY], vec![val(kind)]),
            _ => (vec![ANY, val(kind)], vec![]),
        };
        self.import(format!("{op} {owner}.{name} {desc}"), params, results)
    }

    /// The global that caches a host constant, and its index.
    fn constant(&self, c: HostConstant) -> (u32, u32) {
        let mut constants = self.constants.borrow_mut();
        if let Some(i) = constants.iter().position(|(k, _)| *k == c) {
            return (i as u32, constants[i].1);
        }
        let mut globals = self.globals.borrow_mut();
        globals.push((Kind::Ref, None));
        let g = (globals.len() - 1) as u32;
        constants.push((c, g));
        ((constants.len() - 1) as u32, g)
    }

    fn superclass(&self, class: &str) -> Option<&str> {
        self.classes.get(class)?.super_name().ok().flatten()
    }

    fn interfaces(&self, class: &str) -> Vec<String> {
        let Some(c) = self.classes.get(class) else {
            return Vec::new();
        };
        c.interfaces
            .iter()
            .filter_map(|i| c.class_name(*i).ok().map(str::to_owned))
            .collect()
    }

    /// The game class a class of the jar extends through the jar's classes,
    /// unless it is Object or Record.
    fn game_superclass(&self, class: &str) -> Option<String> {
        let mut c = class;
        loop {
            let s = self.superclass(c)?;
            if !self.classes.contains_key(s) {
                return (!base_class(s)).then(|| s.to_owned());
            }
            c = s;
        }
    }

    /// The class to name in an import for a member that `owner` (perhaps a
    /// class of the jar) inherits from the game.
    fn game_owner(&self, owner: &str) -> String {
        if self.classes.contains_key(owner) {
            self.game_superclass(owner)
                .unwrap_or_else(|| "java/lang/Object".to_owned())
        } else {
            owner.to_owned()
        }
    }

    /// Whether `class` is `target` or below it, as far as the jar knows.
    fn is_subtype(&self, class: &str, target: &str) -> bool {
        class == target
            || self
                .ancestors
                .get(class)
                .is_some_and(|a| a.contains(target))
    }

    /// Builds the vtables, the implementations and the ancestors.
    fn tables(&mut self) {
        fn ancestors(
            plan: &Plan,
            class: &str,
            memo: &mut HashMap<String, BTreeSet<String>>,
        ) -> BTreeSet<String> {
            if let Some(a) = memo.get(class) {
                return a.clone();
            }
            let mut a = BTreeSet::from([class.to_owned()]);
            if plan.classes.contains_key(class) {
                // A placeholder stops a cycle (a bad jar).
                memo.insert(class.to_owned(), a.clone());
                let mut above: Vec<String> = plan.interfaces(class);
                above.extend(plan.superclass(class).map(str::to_owned));
                for s in above {
                    a.extend(ancestors(plan, &s, memo));
                }
            }
            memo.insert(class.to_owned(), a.clone());
            a
        }
        fn vtable(
            plan: &Plan,
            class: &str,
            memo: &mut HashMap<String, HashMap<(String, String), u32>>,
        ) -> HashMap<(String, String), u32> {
            if let Some(v) = memo.get(class) {
                return v.clone();
            }
            memo.insert(class.to_owned(), HashMap::new());
            let mut v = HashMap::new();
            for i in plan.interfaces(class) {
                if plan.classes.contains_key(&i) {
                    v.extend(vtable(plan, &i, memo));
                }
            }
            if let Some(s) = plan.superclass(class).map(str::to_owned)
                && plan.classes.contains_key(&s)
            {
                v.extend(vtable(plan, &s, memo));
            }
            let c = plan.classes[class];
            for m in &c.methods {
                if m.access & STATIC == 0 && m.name != "<init>" && m.code().is_some() {
                    let key = (class.to_owned(), m.name.clone(), m.descriptor.clone());
                    v.insert((m.name.clone(), m.descriptor.clone()), plan.functions[&key]);
                }
            }
            memo.insert(class.to_owned(), v.clone());
            v
        }
        let names: Vec<String> = self.classes.keys().cloned().collect();
        let mut memo = HashMap::new();
        let mut vt = HashMap::new();
        for n in &names {
            ancestors(self, n, &mut memo);
            vtable(self, n, &mut vt);
        }
        let mut implementations: HashMap<(String, String), BTreeMap<u32, Vec<i32>>> =
            HashMap::new();
        for n in &names {
            if self.classes[n.as_str()].access & 0x0600 != 0 {
                continue;
            }
            for (sig, f) in &vt[n] {
                implementations
                    .entry(sig.clone())
                    .or_default()
                    .entry(*f)
                    .or_default()
                    .push(self.ids[n] as i32);
            }
        }
        self.ancestors = memo;
        self.vtables = vt;
        self.implementations = implementations;
    }

    /// A static field as `owner` sees it: its own, or one it inherits from a
    /// class or interface of the jar. (declaring class, global, kind).
    fn static_field(&self, owner: &str, name: &str) -> Option<(String, u32, Kind)> {
        let mut todo = vec![owner.to_owned()];
        while let Some(c) = todo.pop() {
            if let Some(&(g, k)) = self.statics.get(&(c.clone(), name.to_owned())) {
                return Some((c, g, k));
            }
            todo.extend(self.interfaces(&c));
            if let Some(s) = self.superclass(&c) {
                todo.push(s.to_owned());
            }
        }
        None
    }

    /// A late function's index, numbered once the methods are compiled.
    fn late(&self, l: Late) -> u32 {
        let mut late = self.late.borrow_mut();
        if let Some(i) = late.iter().position(|x| *x == l) {
            return LATE | i as u32;
        }
        late.push(l);
        LATE | (late.len() - 1) as u32
    }

    /// The method a call to `owner.name desc` reaches, searching the
    /// classes of the jar from `owner` up.
    fn resolve(&self, owner: &str, name: &str, desc: &str) -> Option<(String, u32)> {
        let mut c = Some(owner);
        while let Some(class) = c {
            let key = (class.to_owned(), name.to_owned(), desc.to_owned());
            if let Some(&f) = self.functions.get(&key) {
                return Some((class.to_owned(), f));
            }
            c = self.superclass(class);
        }
        None
    }

    /// Whether objects of the jar run more than one method for `name desc`
    /// (so a call needs the dispatcher).
    fn overridden(&self, name: &str, desc: &str) -> bool {
        self.implementations
            .get(&(name.to_owned(), desc.to_owned()))
            .is_some_and(|f| f.len() > 1)
    }

    /// Calls `owner.name desc`, its receiver (if any) and arguments on the
    /// WASM stack. `within` is the class whose initializer is running, so
    /// it is not started again.
    fn call(
        &self,
        out: &mut Vec<W<'static>>,
        call: Call,
        owner: &str,
        name: &str,
        desc: &str,
        within: Option<&str>,
    ) -> Result<()> {
        let in_jar = self.classes.contains_key(owner);
        match call {
            Call::Static if in_jar => match self.resolve(owner, name, desc) {
                Some((declaring, f)) => {
                    if within != Some(declaring.as_str()) {
                        out.push(W::Call(self.ensure[&declaring]));
                    }
                    out.push(W::Call(f));
                }
                // A static method of the game that the class inherits
                // (Block.simpleCodec through a mod's block).
                None => out.push(W::Call(self.host_method(
                    "invokestatic",
                    &self.game_owner(owner),
                    name,
                    desc,
                )?)),
            },
            Call::Static => out.push(W::Call(self.host_method(
                "invokestatic",
                owner,
                name,
                desc,
            )?)),
            Call::Special if in_jar => match self.resolve(owner, name, desc) {
                Some((_, f)) => out.push(W::Call(f)),
                // A game method the class inherits (`super.m()`).
                None => out.push(W::Call(self.host_method(
                    "invokespecial",
                    &self.game_owner(owner),
                    name,
                    desc,
                )?)),
            },
            Call::Virtual | Call::Interface if in_jar => {
                let direct = match self.resolve(owner, name, desc) {
                    Some((_, f)) if call == Call::Virtual => {
                        (!self.overridden(name, desc)).then_some(f)
                    }
                    _ => None,
                };
                if let Some(f) = direct {
                    out.push(W::Call(f));
                } else if self.virtuals.contains(&(name.to_owned(), desc.to_owned())) {
                    out.push(W::Call(
                        self.late(Late::Virtual(name.to_owned(), desc.to_owned())),
                    ));
                } else {
                    // Only the game has it (a Block method on a mod block).
                    let op = if call == Call::Virtual {
                        "invokevirtual"
                    } else {
                        "invokeinterface"
                    };
                    out.push(W::Call(self.host_method(
                        op,
                        &self.game_owner(owner),
                        name,
                        desc,
                    )?));
                }
            }
            Call::Special if base_class(owner) && name == "<init>" => {
                // The superclass constructor of a class that extends Object
                // (or Record, whose constructor does nothing either).
                out.push(W::Drop);
            }
            Call::Special if name == "<init>" => {
                return Err(format!("{owner}.<init>: a game constructor out of place"));
            }
            Call::Special => {
                // `super.m()` of a game method, on a mod object.
                out.push(W::Call(self.host_method(
                    "invokespecial",
                    owner,
                    name,
                    desc,
                )?));
            }
            Call::Virtual | Call::Interface => {
                let op = if call == Call::Virtual {
                    "invokevirtual"
                } else {
                    "invokeinterface"
                };
                out.push(W::Call(self.host_method(op, owner, name, desc)?));
            }
        }
        Ok(())
    }

    /// A new object of a class of the jar, its fields zero.
    fn alloc(&self, out: &mut Vec<W<'static>>, class: &str) -> Result<()> {
        let (ty, fields) = &self.structs[class];
        let c = self.classes[class];
        if c.access & 0x0600 != 0 {
            return Err(format!("{class} is abstract or an interface"));
        }
        out.push(W::I32Const(self.ids[class] as i32));
        // The peer: set by the superclass constructor, if it is the game's.
        out.push(W::RefNull(HeapType::ANY));
        for (_, _, k) in fields {
            out.push(zero(*k));
        }
        out.push(W::StructNew(*ty));
        Ok(())
    }

    /// A field of an object of a class of the jar: (struct type, field index).
    fn field(&self, owner: &str, name: &str) -> Option<(u32, u32)> {
        let (ty, fields) = self.structs.get(owner)?;
        // The field the name means from `owner`: its own, else its
        // superclass's (the nearest one).
        let mut c = Some(owner);
        while let Some(class) = c {
            if let Some(i) = fields.iter().position(|(d, n, _)| d == class && n == name) {
                return Some((*ty, i as u32 + 2));
            }
            c = self.superclass(class);
        }
        None
    }
}

/// Compiles the classes of one jar into a WASM module.
pub fn compile(classes: &[ClassFile]) -> Result<Compiled> {
    let mut plan = Plan {
        classes: BTreeMap::new(),
        ids: BTreeMap::new(),
        functions: BTreeMap::new(),
        statics: BTreeMap::new(),
        inits: BTreeMap::new(),
        ensure: BTreeMap::new(),
        structs: BTreeMap::new(),
        types: RefCell::new(
            std::iter::once(TypeDef::Struct(u32::MAX, Vec::new()))
                .chain((0..7).map(|k| TypeDef::Array(storage(k))))
                .collect(),
        ),
        imports: RefCell::new(Vec::new()),
        globals: RefCell::new(Vec::new()),
        constants: RefCell::new(Vec::new()),
        sites: RefCell::new(Vec::new()),
        late: RefCell::new(Vec::new()),
        virtuals: BTreeSet::new(),
        vtables: HashMap::new(),
        implementations: HashMap::new(),
        ancestors: HashMap::new(),
    };
    let mut names = Vec::new();
    for class in classes {
        let name = class.name().map_err(|e| e.to_string())?.to_owned();
        plan.ids.insert(name.clone(), names.len() as u32 + 1);
        plan.classes.insert(name.clone(), class);
        names.push(name);
    }
    // Functions: every method with code, then for each class its `ensure`
    // and its allocator, then the lambdas, then the late functions.
    let mut bodies = Vec::new();
    for class in classes {
        let owner = class.name().map_err(|e| e.to_string())?.to_owned();
        for m in &class.methods {
            if m.access & STATIC == 0 && m.name != "<init>" {
                plan.virtuals.insert((m.name.clone(), m.descriptor.clone()));
            }
            if m.code().is_none() {
                continue;
            }
            let index = bodies.len() as u32;
            plan.functions
                .insert((owner.clone(), m.name.clone(), m.descriptor.clone()), index);
            bodies.push((class, m));
        }
    }
    for class in classes {
        let owner = class.name().map_err(|e| e.to_string())?.to_owned();
        for f in &class.fields {
            if f.access & STATIC == 0 {
                continue;
            }
            let kind = Kind::of(&f.descriptor).ok_or("a field with a bad descriptor")?;
            // A constant (`static final int X = 3`) has its value here, not
            // in the initializer.
            let constant = f.attributes.iter().find_map(|a| match a {
                Attribute::ConstantValue(i) => match class.constants.get(usize::from(*i)) {
                    Some(Constant::Integer(v)) => Some(ConstExpr::i32_const(*v)),
                    Some(Constant::Float(v)) => Some(ConstExpr::f32_const((*v).into())),
                    Some(Constant::Long(v)) => Some(ConstExpr::i64_const(*v)),
                    Some(Constant::Double(v)) => Some(ConstExpr::f64_const((*v).into())),
                    _ => None,
                },
                _ => None,
            });
            let mut globals = plan.globals.borrow_mut();
            plan.statics.insert(
                (owner.clone(), f.name.clone()),
                (globals.len() as u32, kind),
            );
            globals.push((kind, constant));
        }
        let clinit = plan
            .functions
            .get(&(owner.clone(), "<clinit>".to_owned(), "()V".to_owned()))
            .copied();
        let mut globals = plan.globals.borrow_mut();
        plan.inits
            .insert(owner.clone(), (globals.len() as u32, clinit));
        globals.push((Kind::Int, None));
    }
    let base = bodies.len() as u32;
    for (i, owner) in names.iter().enumerate() {
        plan.ensure.insert(owner.clone(), base + i as u32);
    }
    plan.tables();
    // Struct types, each class after its superclass.
    fn layout(plan: &mut Plan, class: &str) -> Result<()> {
        if plan.structs.contains_key(class) {
            return Ok(());
        }
        let c = plan.classes[class];
        let (supertype, mut fields) = match plan.superclass(class).map(str::to_owned) {
            Some(s) if plan.classes.contains_key(&s) => {
                layout(plan, &s)?;
                plan.structs[&s].clone()
            }
            _ => (OBJECT, Vec::new()),
        };
        for f in &c.fields {
            if f.access & STATIC == 0 {
                let kind = Kind::of(&f.descriptor).ok_or("a field with a bad descriptor")?;
                fields.push((class.to_owned(), f.name.clone(), kind));
            }
        }
        let ty = plan.struct_type(
            supertype,
            fields.iter().map(|(_, _, k)| (val(*k), true)).collect(),
        );
        plan.structs.insert(class.to_owned(), (ty, fields));
        Ok(())
    }
    for name in &names {
        layout(&mut plan, name)?;
    }

    let mut skipped = Vec::new();
    let mut functions: Vec<(u32, Vec<ValType>, Vec<W<'static>>)> = Vec::new();
    let mut exports: Vec<(String, u32)> = Vec::new();
    for (class, m) in &bodies {
        let owner = class.name().map_err(|e| e.to_string())?;
        let sig = Method::parse(&m.descriptor).ok_or("a bad method descriptor")?;
        let mut params: Vec<ValType> = Vec::new();
        if m.access & STATIC == 0 {
            params.push(ANY);
        }
        params.extend(sig.param_kinds().into_iter().map(val));
        let results: Vec<ValType> = sig.return_kind().map(val).into_iter().collect();
        let ty = plan.type_index(params, results);
        let name = export_name(owner, &m.name, &m.descriptor);
        let (locals, body) = match MethodCompiler::new(&plan, class, m).and_then(|c| c.compile()) {
            Ok(body) => body,
            Err(e) => {
                skipped.push((name.clone(), e));
                (Vec::new(), vec![W::Unreachable, W::End])
            }
        };
        exports.push((name, functions.len() as u32));
        functions.push((ty, locals, body));
    }
    // ensure(class): if !flag { flag = 1; <clinit>() }
    let none = plan.type_index(Vec::new(), Vec::new());
    for owner in &names {
        let (flag, clinit) = plan.inits[owner];
        let mut f = vec![
            W::GlobalGet(flag),
            W::I32Eqz,
            W::If(BlockType::Empty),
            W::I32Const(1),
            W::GlobalSet(flag),
        ];
        if let Some(c) = clinit {
            f.push(W::Call(c));
        }
        f.extend([W::End, W::End]);
        functions.push((none, Vec::new(), f));
    }
    let alloc_ty = plan.type_index(Vec::new(), vec![ANY]);
    for owner in &names {
        let mut f = Vec::new();
        match plan.alloc(&mut f, owner) {
            Ok(()) => {
                f.insert(0, W::Call(plan.ensure[owner]));
                f.push(W::End);
                exports.push((format!("new {owner}"), functions.len() as u32));
                functions.push((alloc_ty, Vec::new(), f));
            }
            Err(_) => functions.push((alloc_ty, Vec::new(), vec![W::Unreachable, W::End])),
        }
    }
    // Each static field's getter and setter (`get <class>.<field>`, `set
    // ...`), for other jars: their class is initialized first.
    let statics: Vec<((String, String), (u32, Kind))> =
        plan.statics.iter().map(|(k, v)| (k.clone(), *v)).collect();
    for ((owner, field), (g, kind)) in statics {
        let getter = plan.type_index(Vec::new(), vec![val(kind)]);
        exports.push((format!("get {owner}.{field}"), functions.len() as u32));
        functions.push((
            getter,
            Vec::new(),
            vec![W::Call(plan.ensure[&owner]), W::GlobalGet(g), W::End],
        ));
        let setter = plan.type_index(vec![val(kind)], Vec::new());
        exports.push((format!("set {owner}.{field}"), functions.len() as u32));
        functions.push((
            setter,
            Vec::new(),
            vec![
                W::Call(plan.ensure[&owner]),
                W::LocalGet(0),
                W::GlobalSet(g),
                W::End,
            ],
        ));
    }
    // The lambdas: closure and arguments in, the target method called.
    let sites = plan.sites.borrow().len();
    for n in 0..sites {
        let (ty, locals, body) = match lambda(&plan, n) {
            Ok(f) => f,
            Err(e) => {
                skipped.push((format!("lambda {n}"), e));
                (
                    lambda_type(&plan, n)?,
                    Vec::new(),
                    vec![W::Unreachable, W::End],
                )
            }
        };
        exports.push((format!("lambda {n}"), functions.len() as u32));
        functions.push((ty, locals, body));
    }
    let lambda_base = functions.len() as u32 - sites as u32;

    // The dispatchers of every method the jar declares and every lambda's
    // interface method are exported, for the game to call.
    let mut exported = plan.virtuals.clone();
    for (l, _) in plan.sites.borrow().iter() {
        exported.insert((l.method.clone(), l.descriptor.clone()));
    }
    for (name, desc) in &exported {
        plan.late(Late::Virtual(name.clone(), desc.clone()));
    }
    let late_base = functions.len() as u32;
    let mut done = 0;
    while done < plan.late.borrow().len() {
        let l = plan.late.borrow()[done].clone();
        let f = match &l {
            Late::Virtual(name, desc) => {
                let f = dispatcher(&plan, &names, lambda_base, name, desc)?;
                if exported.contains(&(name.clone(), desc.clone())) {
                    exports.push((format!("virtual {name} {desc}"), functions.len() as u32));
                }
                f
            }
            Late::InstanceOf(class) => instance_of(&plan, &names, class),
            Late::MultiArray(desc, dims) => multi_array(&plan, desc, *dims),
        };
        functions.push(f);
        done += 1;
    }

    let throwable_ty = plan.type_index(vec![ANY], Vec::new());
    // Now the imports are known: they come first.
    let imports = plan.imports.borrow();
    let shift = imports.len() as u32;
    let fix = |i: u32| {
        if i & IMPORT != 0 {
            i & !IMPORT
        } else if i & LATE != 0 {
            late_base + (i & !LATE) + shift
        } else {
            i + shift
        }
    };
    let mut types = TypeSection::new();
    // The struct types no other type extends: final, so a cast to one is a
    // single compare of the object's type (a cast to a type with subtypes
    // calls the runtime when the object is of a subtype). The jar's classes
    // are all known here, so a class nothing extends is a leaf.
    let extended: std::collections::HashSet<u32> = plan
        .types
        .borrow()
        .iter()
        .filter_map(|t| match t {
            TypeDef::Struct(supertype, _) if *supertype != u32::MAX => Some(*supertype),
            _ => None,
        })
        .collect();
    for (index, t) in plan.types.borrow().iter().enumerate() {
        match t {
            TypeDef::Func(params, results) => {
                types
                    .ty()
                    .function(params.iter().copied(), results.iter().copied());
            }
            TypeDef::Array(element) => {
                types.ty().array(element, true);
            }
            TypeDef::Struct(supertype, fields) => {
                let mut all = vec![
                    FieldType {
                        element_type: StorageType::Val(ValType::I32),
                        mutable: false,
                    },
                    FieldType {
                        element_type: StorageType::Val(ANY),
                        mutable: true,
                    },
                ];
                all.extend(fields.iter().map(|(v, mutable)| FieldType {
                    element_type: StorageType::Val(*v),
                    mutable: *mutable,
                }));
                types.ty().subtype(&SubType {
                    is_final: !extended.contains(&(index as u32)),
                    supertype_idx: (*supertype != u32::MAX).then_some(*supertype),
                    composite_type: CompositeType {
                        inner: CompositeInnerType::Struct(StructType {
                            fields: all.into_boxed_slice(),
                        }),
                        shared: false,
                        descriptor: None,
                        describes: None,
                    },
                });
            }
        }
    }
    let mut tags = wasm_encoder::TagSection::new();
    tags.tag(wasm_encoder::TagType {
        kind: wasm_encoder::TagKind::Exception,
        func_type_idx: throwable_ty,
    });
    let mut import_section = ImportSection::new();
    for (name, ty) in imports.iter() {
        import_section.import("host", name, EntityType::Function(*ty));
    }
    let mut function_section = FunctionSection::new();
    let mut code = CodeSection::new();
    for (ty, locals, body) in &functions {
        function_section.function(*ty);
        let mut f = Function::new_with_locals_types(locals.iter().copied());
        for i in body {
            match i {
                W::Call(c) => f.instruction(&W::Call(fix(*c))),
                i => f.instruction(i),
            };
        }
        code.function(&f);
    }
    let mut global_section = GlobalSection::new();
    for (kind, constant) in plan.globals.borrow().iter() {
        let init = constant.clone().unwrap_or_else(|| match kind {
            Kind::Int => ConstExpr::i32_const(0),
            Kind::Long => ConstExpr::i64_const(0),
            Kind::Float => ConstExpr::f32_const(0.0.into()),
            Kind::Double => ConstExpr::f64_const(0.0.into()),
            Kind::Ref => ConstExpr::ref_null(HeapType::ANY),
        });
        global_section.global(
            GlobalType {
                val_type: val(*kind),
                mutable: true,
                shared: false,
            },
            &init,
        );
    }
    let mut export_section = ExportSection::new();
    for (name, f) in &exports {
        export_section.export(name, ExportKind::Func, f + shift);
    }
    let mut module = Module::new();
    module.section(&types);
    module.section(&import_section);
    module.section(&function_section);
    module.section(&tags);
    module.section(&global_section);
    module.section(&export_section);
    module.section(&code);
    // Function names (the exported ones, and the imports): traps and
    // profiles name the Java method.
    let mut function_names = wasm_encoder::NameMap::new();
    let mut named: Vec<(u32, &str)> = imports
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (i as u32, n.as_str()))
        .chain(exports.iter().map(|(n, f)| (f + shift, n.as_str())))
        .collect();
    named.sort_by_key(|(i, _)| *i);
    named.dedup_by_key(|(i, _)| *i);
    for (i, n) in named {
        function_names.append(i, n);
    }
    let mut name_section = wasm_encoder::NameSection::new();
    name_section.functions(&function_names);
    module.section(&name_section);
    let info = names
        .iter()
        .map(|n| ClassInfo {
            name: n.clone(),
            superclass: plan.superclass(n).map(str::to_owned),
            interfaces: plan.interfaces(n),
            game_superclass: plan.game_superclass(n),
            fields: plan.structs[n]
                .1
                .iter()
                .map(|(_, f, _)| f.clone())
                .collect(),
        })
        .collect();
    Ok(Compiled {
        wasm: module.finish(),
        skipped,
        info,
        classes: names,
        lambdas: plan.sites.borrow().iter().map(|(l, _)| l.clone()).collect(),
        constants: plan
            .constants
            .borrow()
            .iter()
            .map(|(c, _)| c.clone())
            .collect(),
    })
}

/// The dispatcher of `name desc`: (object, arguments) → the result.
fn dispatcher(
    plan: &Plan,
    names: &[String],
    lambda_base: u32,
    name: &str,
    desc: &str,
) -> Result<(u32, Vec<ValType>, Vec<W<'static>>)> {
    let m = Method::parse(desc).ok_or("a bad descriptor")?;
    let mut params = vec![ANY];
    params.extend(m.param_kinds().into_iter().map(val));
    let results: Vec<ValType> = m.return_kind().map(val).into_iter().collect();
    let ty = plan.type_index(params.clone(), results);
    let id = params.len() as u32;
    // The implementations, with the ids of the objects that run each.
    let mut targets = plan
        .implementations
        .get(&(name.to_owned(), desc.to_owned()))
        .cloned()
        .unwrap_or_default();
    for (n, (l, _)) in plan.sites.borrow().iter().enumerate() {
        if l.method == name && l.descriptor == desc {
            targets
                .entry(lambda_base + n as u32)
                .or_default()
                .push((names.len() + 1 + n) as i32);
        }
    }
    let args = |out: &mut Vec<W<'static>>| {
        for i in 0..params.len() as u32 {
            out.push(W::LocalGet(i));
        }
    };
    let mut out = vec![
        W::Block(BlockType::Empty),
        W::LocalGet(0),
        W::RefTestNonNull(STRUCT_REF),
        W::I32Eqz,
        W::BrIf(0),
        W::LocalGet(0),
        W::RefCastNonNull(HeapType::Concrete(OBJECT)),
        W::StructGet {
            struct_type_index: OBJECT,
            field_index: 0,
        },
        W::LocalSet(id),
    ];
    for (f, ids) in &targets {
        for (j, i) in ids.iter().enumerate() {
            out.extend([W::LocalGet(id), W::I32Const(*i), W::I32Eq]);
            if j > 0 {
                out.push(W::I32Or);
            }
        }
        out.push(W::If(BlockType::Empty));
        args(&mut out);
        out.extend([W::Call(*f), W::Return, W::End]);
    }
    // A mod object whose class does not have it: its game superclass's.
    args(&mut out);
    out.push(W::Call(plan.host_method(
        "super",
        "java/lang/Object",
        name,
        desc,
    )?));
    out.push(W::Return);
    out.push(W::End);
    // An object of the host (or an array, or null): the host's method.
    args(&mut out);
    out.push(W::Call(plan.host_method(
        "invokevirtual",
        "java/lang/Object",
        name,
        desc,
    )?));
    out.push(W::End);
    Ok((ty, vec![ValType::I32], out))
}

/// `multianewarray`: (lengths) → an array of arrays of `desc`, as deep as
/// the lengths go (deeper levels stay null).
fn multi_array(plan: &Plan, desc: &str, dims: u32) -> (u32, Vec<ValType>, Vec<W<'static>>) {
    let ty = plan.type_index(vec![ValType::I32; dims as usize], vec![ANY]);
    let component = &desc[1..];
    if dims <= 1 {
        let out = vec![
            W::LocalGet(0),
            W::ArrayNewDefault(ARRAYS[array_kind(component)]),
            W::End,
        ];
        return (ty, Vec::new(), out);
    }
    let inner = plan.late(Late::MultiArray(component.to_owned(), dims - 1));
    let refs = ARRAYS[6];
    let (array, i) = (dims, dims + 1);
    let mut out = vec![
        W::LocalGet(0),
        W::ArrayNewDefault(refs),
        W::LocalSet(array),
        W::Block(BlockType::Empty),
        W::Loop(BlockType::Empty),
        W::LocalGet(i),
        W::LocalGet(0),
        W::I32GeS,
        W::BrIf(1),
        W::LocalGet(array),
        W::RefCastNonNull(HeapType::Concrete(refs)),
        W::LocalGet(i),
    ];
    for d in 1..dims {
        out.push(W::LocalGet(d));
    }
    out.extend([
        W::Call(inner),
        W::ArraySet(refs),
        W::LocalGet(i),
        W::I32Const(1),
        W::I32Add,
        W::LocalSet(i),
        W::Br(0),
        W::End,
        W::End,
        W::LocalGet(array),
        W::End,
    ]);
    (ty, vec![ANY, ValType::I32], out)
}

/// `instanceof` a class or interface of the jar: (object) → 0 or 1.
fn instance_of(
    plan: &Plan,
    names: &[String],
    target: &str,
) -> (u32, Vec<ValType>, Vec<W<'static>>) {
    let ty = plan.type_index(vec![ANY], vec![ValType::I32]);
    let mut ids: Vec<i32> = names
        .iter()
        .enumerate()
        .filter(|(_, c)| plan.is_subtype(c, target))
        .map(|(i, _)| i as i32 + 1)
        .collect();
    for (n, (l, _)) in plan.sites.borrow().iter().enumerate() {
        if plan.is_subtype(&l.interface, target) {
            ids.push((names.len() + 1 + n) as i32);
        }
    }
    let mut out = vec![
        W::LocalGet(0),
        W::RefTestNonNull(STRUCT_REF),
        W::I32Eqz,
        W::If(BlockType::Empty),
        W::I32Const(0),
        W::Return,
        W::End,
        W::LocalGet(0),
        W::RefCastNonNull(HeapType::Concrete(OBJECT)),
        W::StructGet {
            struct_type_index: OBJECT,
            field_index: 0,
        },
        W::LocalSet(1),
        W::I32Const(0),
    ];
    for i in ids {
        out.extend([W::LocalGet(1), W::I32Const(i), W::I32Eq, W::I32Or]);
    }
    out.push(W::End);
    (ty, vec![ValType::I32], out)
}

/// The wrapper class of a primitive descriptor, for boxing.
fn wrapper(desc: &str) -> Option<(&'static str, &'static str)> {
    Some(match desc {
        "I" => ("java/lang/Integer", "intValue"),
        "Z" => ("java/lang/Boolean", "booleanValue"),
        "B" => ("java/lang/Byte", "byteValue"),
        "C" => ("java/lang/Character", "charValue"),
        "S" => ("java/lang/Short", "shortValue"),
        "J" => ("java/lang/Long", "longValue"),
        "F" => ("java/lang/Float", "floatValue"),
        "D" => ("java/lang/Double", "doubleValue"),
        _ => return None,
    })
}

/// Converts the value on top of the WASM stack from type `from` to `to`,
/// as LambdaMetafactory does: boxing, unboxing and widening.
fn adapt(plan: &Plan, out: &mut Vec<W<'static>>, from: &str, to: &str) -> Result<()> {
    let (f, t) = (Kind::of(from), Kind::of(to));
    if f == t && (f == Some(Kind::Ref) || from == to || f == Some(Kind::Int)) {
        return Ok(());
    }
    match (f, t) {
        (Some(Kind::Ref), Some(_)) => {
            let (class, method) = wrapper(to).ok_or("a bad type")?;
            let desc = format!("(){to}");
            out.push(W::Call(plan.host_method(
                "invokevirtual",
                class,
                method,
                &desc,
            )?));
        }
        (Some(_), Some(Kind::Ref)) => {
            let (class, _) = wrapper(from).ok_or("a bad type")?;
            let desc = format!("({from})L{class};");
            out.push(W::Call(plan.host_method(
                "invokestatic",
                class,
                "valueOf",
                &desc,
            )?));
        }
        (Some(Kind::Int), Some(Kind::Long)) => out.push(W::I64ExtendI32S),
        (Some(Kind::Int), Some(Kind::Float)) => out.push(W::F32ConvertI32S),
        (Some(Kind::Int), Some(Kind::Double)) => out.push(W::F64ConvertI32S),
        (Some(Kind::Long), Some(Kind::Float)) => out.push(W::F32ConvertI64S),
        (Some(Kind::Long), Some(Kind::Double)) => out.push(W::F64ConvertI64S),
        (Some(Kind::Float), Some(Kind::Double)) => out.push(W::F64PromoteF32),
        _ => return Err(format!("converting {from} to {to} is not compiled yet")),
    }
    Ok(())
}

/// The function type of lambda site `n`: (closure, interface arguments) →
/// the interface method's result.
fn lambda_type(plan: &Plan, n: usize) -> Result<u32> {
    let sam = Method::parse(&plan.sites.borrow()[n].1.sam).ok_or("a bad descriptor")?;
    let mut params = vec![ANY];
    params.extend(sam.param_kinds().into_iter().map(val));
    let results: Vec<ValType> = sam.return_kind().map(val).into_iter().collect();
    Ok(plan.type_index(params, results))
}

/// The function behind lambda site `n`.
fn lambda(plan: &Plan, n: usize) -> Result<(u32, Vec<ValType>, Vec<W<'static>>)> {
    let ty = lambda_type(plan, n)?;
    let sites = plan.sites.borrow();
    let site = &sites[n].1;
    let sam = Method::parse(&site.sam).ok_or("a bad descriptor")?;
    let params = 1 + sam.params.len();

    let (kind, owner, name, desc) = &site.target;
    let target = Method::parse(desc).ok_or("a bad descriptor")?;
    let receiver = matches!(kind, 5 | 7 | 9);
    let mut wanted: Vec<String> = Vec::new();
    if receiver {
        wanted.push(format!("L{owner};"));
    }
    wanted.extend(target.params.iter().cloned());
    // The values: the captured ones, then the interface method's arguments.
    let mut values: Vec<(String, W<'static>, Option<u32>)> = Vec::new();
    for (j, c) in site.captures.iter().enumerate() {
        values.push((c.clone(), W::LocalGet(0), Some(j as u32 + 1)));
    }
    for (j, p) in sam.params.iter().enumerate() {
        values.push((p.clone(), W::LocalGet(j as u32 + 1), None));
    }
    if values.len() != wanted.len() {
        return Err(format!(
            "{} values for {} parameters",
            values.len(),
            wanted.len()
        ));
    }
    let mut out = Vec::new();
    let in_jar = plan.classes.contains_key(owner.as_str());
    // A constructor of the jar's class (`Foo::new`): allocate first.
    let object = params as u32;
    let mut locals = Vec::new();
    if *kind == 8 && in_jar {
        locals.push(ANY);
        out.push(W::Call(plan.ensure[owner.as_str()]));
        plan.alloc(&mut out, owner)?;
        out.push(W::LocalTee(object));
    }
    for ((from, get, field), to) in values.into_iter().zip(&wanted) {
        out.push(get);
        if let Some(f) = field {
            out.push(W::RefCastNullable(HeapType::Concrete(site.ty)));
            out.push(W::StructGet {
                struct_type_index: site.ty,
                field_index: f + 1,
            });
        }
        adapt(plan, &mut out, &from, to)?;
    }
    let returns = match kind {
        6 => {
            plan.call(&mut out, Call::Static, owner, name, desc, None)?;
            target.ret.clone()
        }
        5 | 9 | 7 => {
            let call = match kind {
                5 => Call::Virtual,
                9 => Call::Interface,
                _ => Call::Special,
            };
            plan.call(&mut out, call, owner, name, desc, None)?;
            target.ret.clone()
        }
        8 if in_jar => {
            plan.call(&mut out, Call::Special, owner, name, desc, None)?;
            out.push(W::LocalGet(object));
            format!("L{owner};")
        }
        8 => {
            out.push(W::Call(plan.host_method("new", owner, name, desc)?));
            format!("L{owner};")
        }
        _ => {
            return Err(format!(
                "method handles of kind {kind} are not compiled yet"
            ));
        }
    };
    if sam.ret == "V" {
        if returns != "V" {
            out.push(W::Drop);
        }
    } else {
        adapt(plan, &mut out, &returns, &sam.ret)?;
    }
    out.push(W::End);
    Ok((ty, locals, out))
}

/// A value on the stack, as the kinds pass sees it: its kind, and for an
/// object from `new` not yet constructed, that `new`'s offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    kind: Kind,
    new: Option<u32>,
    /// The method's own object (`aload_0` of an instance method that never
    /// stores to local 0): its class is known without a test.
    this: bool,
}

impl From<Kind> for Slot {
    fn from(kind: Kind) -> Slot {
        Slot {
            kind,
            new: None,
            this: false,
        }
    }
}

impl Slot {
    fn wide(self) -> bool {
        self.kind.wide()
    }
}

/// The stack before each instruction, by abstract interpretation.
fn stack_kinds(
    class: &ClassFile,
    insns: &[Instruction],
    at: &HashMap<u32, usize>,
    handlers: &[crate::classfile::Handler],
    this: bool,
) -> Result<Vec<Option<Vec<Slot>>>> {
    let mut before: Vec<Option<Vec<Slot>>> = vec![None; insns.len()];
    let mut work = vec![(0usize, Vec::new())];
    // A handler starts with the exception alone on the stack.
    for h in handlers {
        let i = *at
            .get(&u32::from(h.handler))
            .ok_or("a handler at no instruction")?;
        work.push((i, vec![Slot::from(Kind::Ref)]));
    }
    while let Some((i, stack)) = work.pop() {
        if i >= insns.len() {
            continue;
        }
        let mut stack = stack;
        if let Some(seen) = &before[i] {
            let same = |a: &Slot, b: &Slot| a.kind == b.kind && a.new == b.new;
            if seen.len() != stack.len() || seen.iter().zip(&stack).any(|(a, b)| !same(a, b)) {
                return Err(format!("stack kinds differ at {}", insns[i].at));
            }
            // A slot is the method's object only if it is on every path.
            for (s, a) in stack.iter_mut().zip(seen) {
                s.this &= a.this;
            }
            if *seen == stack {
                continue;
            }
        }
        before[i] = Some(stack.clone());
        let insn = &insns[i];
        let mut s = stack;
        effect(class, insn, &mut s)?;
        if this
            && insn.opcode == 0x2A
            && let Some(top) = s.last_mut()
        {
            top.this = true;
        }
        for t in successors(insn, i, at)? {
            work.push((t, s.clone()));
        }
    }
    Ok(before)
}

fn successors(insn: &Instruction, i: usize, at: &HashMap<u32, usize>) -> Result<Vec<usize>> {
    let target = |pc: u32| {
        at.get(&pc)
            .copied()
            .ok_or_else(|| format!("a jump into an instruction at {pc}"))
    };
    Ok(match (&insn.operand, insn.opcode) {
        (_, 0xAC..=0xB1 | 0xBF) => Vec::new(),
        (Operand::Target(t), 0xA7 | 0xC8) => vec![target(*t)?],
        (Operand::Target(t), _) => vec![target(*t)?, i + 1],
        (
            Operand::Table {
                default, targets, ..
            },
            _,
        ) => {
            let mut v = vec![target(*default)?];
            for t in targets {
                v.push(target(*t)?);
            }
            v
        }
        (Operand::Lookup { default, pairs }, _) => {
            let mut v = vec![target(*default)?];
            for (_, t) in pairs {
                v.push(target(*t)?);
            }
            v
        }
        _ => vec![i + 1],
    })
}

fn pop(s: &mut Vec<Slot>) -> Result<Slot> {
    s.pop().ok_or_else(|| "the stack underflows".to_owned())
}

fn push(s: &mut Vec<Slot>, v: impl Into<Slot>) {
    s.push(v.into());
}

/// The member an invoke instruction names, and its descriptor.
fn invoked(class: &ClassFile, insn: &Instruction) -> Result<(String, String, String)> {
    let (Operand::Constant(i) | Operand::Interface(i, _)) = insn.operand else {
        return Err("an invoke without a constant".into());
    };
    if insn.opcode == 0xBA {
        let Some(Constant::InvokeDynamic(_, nat)) = class.constants.get(usize::from(i)) else {
            return Err("a bad invokedynamic".into());
        };
        let (name, desc) = class.name_and_type(*nat).map_err(|e| e.to_string())?;
        return Ok((String::new(), name.to_owned(), desc.to_owned()));
    }
    let (o, n, d) = class.member_ref(i).map_err(|e| e.to_string())?;
    Ok((o.to_owned(), n.to_owned(), d.to_owned()))
}

/// What an instruction does to the stack's kinds.
fn effect(class: &ClassFile, insn: &Instruction, s: &mut Vec<Slot>) -> Result<()> {
    use Kind::*;
    let op = insn.opcode;
    let pops = |s: &mut Vec<Slot>, n: usize| -> Result<()> {
        for _ in 0..n {
            pop(s)?;
        }
        Ok(())
    };
    match op {
        0x00 => {}
        0x01 => push(s, Ref),
        0x02..=0x08 | 0x10 | 0x11 => push(s, Int),
        0x09 | 0x0A => push(s, Long),
        0x0B..=0x0D => push(s, Float),
        0x0E | 0x0F => push(s, Double),
        0x12..=0x14 => {
            let Operand::Constant(i) = insn.operand else {
                unreachable!()
            };
            push(
                s,
                match class.constants.get(usize::from(i)) {
                    Some(Constant::Integer(_)) => Int,
                    Some(Constant::Float(_)) => Float,
                    Some(Constant::Long(_)) => Long,
                    Some(Constant::Double(_)) => Double,
                    _ => Ref,
                },
            )
        }
        0x15 | 0x1A..=0x1D => push(s, Int),
        0x16 | 0x1E..=0x21 => push(s, Long),
        0x17 | 0x22..=0x25 => push(s, Float),
        0x18 | 0x26..=0x29 => push(s, Double),
        0x19 | 0x2A..=0x2D => push(s, Ref),
        0x2E..=0x35 => {
            pops(s, 2)?;
            push(
                s,
                match op {
                    0x2F => Long,
                    0x30 => Float,
                    0x31 => Double,
                    0x32 => Ref,
                    _ => Int,
                },
            )
        }
        0x36..=0x4E => pops(s, 1)?,
        0x4F..=0x56 => pops(s, 3)?,
        0x57 => pops(s, 1)?,
        0x58 => {
            if !pop(s)?.wide() {
                pop(s)?;
            }
        }
        0x59 => {
            let a = pop(s)?;
            s.extend([a, a]);
        }
        0x5A => {
            let a = pop(s)?;
            let b = pop(s)?;
            s.extend([a, b, a]);
        }
        0x5B => {
            let a = pop(s)?;
            let b = pop(s)?;
            if b.wide() {
                s.extend([a, b, a]);
            } else {
                let c = pop(s)?;
                s.extend([a, c, b, a]);
            }
        }
        0x5C => {
            let a = pop(s)?;
            if a.wide() {
                s.extend([a, a]);
            } else {
                let b = pop(s)?;
                s.extend([b, a, b, a]);
            }
        }
        0x5D => {
            let a = pop(s)?;
            if a.wide() {
                let b = pop(s)?;
                s.extend([a, b, a]);
            } else {
                let b = pop(s)?;
                let c = pop(s)?;
                s.extend([b, a, c, b, a]);
            }
        }
        0x5E => {
            let a = pop(s)?;
            if a.wide() {
                let b = pop(s)?;
                if b.wide() {
                    s.extend([a, b, a]);
                } else {
                    let c = pop(s)?;
                    s.extend([a, c, b, a]);
                }
            } else {
                let b = pop(s)?;
                let c = pop(s)?;
                if c.wide() {
                    s.extend([b, a, c, b, a]);
                } else {
                    let d = pop(s)?;
                    s.extend([b, a, d, c, b, a]);
                }
            }
        }
        0x5F => {
            let a = pop(s)?;
            let b = pop(s)?;
            s.extend([a, b]);
        }
        0x60..=0x73 | 0x7E..=0x83 => {
            let k = pop(s)?;
            pop(s)?;
            push(s, k);
        }
        0x74..=0x77 | 0x84 => {}
        0x78..=0x7D => {
            pop(s)?;
        }
        0x85..=0x93 => {
            pop(s)?;
            push(
                s,
                match op {
                    0x85 | 0x8C | 0x8F => Long,
                    0x86 | 0x89 | 0x90 => Float,
                    0x87 | 0x8A | 0x8D => Double,
                    _ => Int,
                },
            )
        }
        0x94..=0x98 => {
            pops(s, 2)?;
            push(s, Int)
        }
        0x99..=0x9E | 0xC6 | 0xC7 => pops(s, 1)?,
        0x9F..=0xA6 => pops(s, 2)?,
        0xA7 | 0xC8 => {}
        0xAA | 0xAB => pops(s, 1)?,
        0xAC..=0xB0 => pops(s, 1)?,
        0xB1 => {}
        0xB2..=0xB5 => {
            let Operand::Constant(i) = insn.operand else {
                unreachable!()
            };
            let (_, _, d) = class.member_ref(i).map_err(|e| e.to_string())?;
            let k = Kind::of(d).ok_or("a bad field descriptor")?;
            match op {
                0xB2 => push(s, k),
                0xB3 => pops(s, 1)?,
                0xB4 => {
                    pop(s)?;
                    push(s, k)
                }
                _ => pops(s, 2)?,
            }
        }
        0xB6..=0xBA => {
            let (_, name, d) = invoked(class, insn)?;
            let m = Method::parse(&d).ok_or("a bad method descriptor")?;
            pops(s, m.params.len())?;
            if matches!(op, 0xB6 | 0xB7 | 0xB9) {
                let receiver = pop(s)?;
                // A constructor makes every copy of the new object whole.
                if op == 0xB7 && name == "<init>" && receiver.new.is_some() {
                    for v in s.iter_mut() {
                        if v.new == receiver.new {
                            v.new = None;
                        }
                    }
                }
            }
            if let Some(k) = m.return_kind() {
                push(s, k);
            }
        }
        0xBC | 0xBD => {
            pop(s)?;
            push(s, Ref)
        }
        0xBE | 0xC1 => {
            pop(s)?;
            push(s, Int)
        }
        0xBF | 0xC2 | 0xC3 => pops(s, 1)?,
        0xC5 => {
            let Operand::Dimensions(_, dims) = insn.operand else {
                unreachable!()
            };
            pops(s, usize::from(dims))?;
            push(s, Ref)
        }
        0xBB => s.push(Slot {
            kind: Ref,
            new: Some(insn.at),
            this: false,
        }),
        0xC0 => {}
        _ => {
            return Err(format!("{} is not compiled yet", crate::bytecode::name(op)));
        }
    }
    Ok(())
}

/// The exception tag: its one value is the Throwable.
const THROWABLE: u32 = 0;

/// What a structured method's code is inside, innermost last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nest {
    /// A WASM block that join block `n` follows.
    Block(usize),
    /// The WASM loop of loop header `n`.
    Loop(usize),
    /// An arm of an `if`.
    If,
}

/// The facts the nesting needs about a control-flow graph.
struct Shape {
    /// Each block's position in reverse postorder.
    order: Vec<usize>,
    /// The blocks each block immediately dominates, in reverse postorder.
    children: Vec<Vec<usize>>,
    /// Blocks reached by more than one forward branch (or by a switch, or
    /// by both ways of a conditional branch): they follow a WASM block.
    join: Vec<bool>,
    /// Loop headers.
    header: Vec<bool>,
}

impl Shape {
    fn new(cfg: &crate::cfg::Cfg, insns: &[Instruction], at: &HashMap<u32, usize>) -> Shape {
        let n = cfg.blocks.len();
        let mut order = vec![usize::MAX; n];
        for (i, &b) in cfg.rpo.iter().enumerate() {
            order[b] = i;
        }
        let mut children = vec![Vec::new(); n];
        for &b in cfg.rpo.iter().skip(1) {
            let d = cfg.idom[b];
            if d != usize::MAX && d != b {
                children[d].push(b);
            }
        }
        let mut forward = vec![0usize; n];
        let mut join = vec![false; n];
        for &b in &cfg.rpo {
            let last = &insns[cfg.blocks[b].end - 1];
            let targets: Vec<usize> = crate::types::successors(insns, cfg.blocks[b].end - 1, at)
                .into_iter()
                .map(|i| cfg.block_of[i])
                .collect();
            let switch = matches!(last.opcode, 0xAA | 0xAB);
            let mut seen = Vec::new();
            for t in targets {
                if order[t] > order[b] {
                    if switch || seen.contains(&t) {
                        join[t] = true;
                    }
                    if !seen.contains(&t) {
                        forward[t] += 1;
                    }
                }
                seen.push(t);
            }
        }
        for b in 0..n {
            if forward[b] > 1 {
                join[b] = true;
            }
        }
        let mut header = vec![false; n];
        for (h, _) in &cfg.loops {
            header[*h] = true;
        }
        Shape {
            order,
            children,
            join,
            header,
        }
    }
}

struct MethodCompiler<'a> {
    plan: &'a Plan<'a>,
    class: &'a ClassFile,
    owner: String,
    method: &'a Member,
    insns: Vec<Instruction>,
    /// Local index by (JVM slot or stack depth, kind).
    locals: BTreeMap<(bool, u16, Kind), u32>,
    extra: Vec<ValType>,
    first_local: u32,
    out: Vec<W<'static>>,
    /// The method's own object as its class's struct: (local, struct type).
    this: Option<(u32, u32)>,
}

impl<'a> MethodCompiler<'a> {
    fn new(plan: &'a Plan<'a>, class: &'a ClassFile, method: &'a Member) -> Result<Self> {
        let code = method.code().ok_or("no code")?;
        let insns = decode(&code.code).map_err(|e| e.to_string())?;
        let sig = Method::parse(&method.descriptor).ok_or("a bad method descriptor")?;
        let mut locals = BTreeMap::new();
        let mut slot = 0u16;
        let mut index = 0u32;
        if method.access & STATIC == 0 {
            locals.insert((true, 0, Kind::Ref), 0);
            slot = 1;
            index = 1;
        }
        for k in sig.param_kinds() {
            locals.insert((true, slot, k), index);
            slot += if k.wide() { 2 } else { 1 };
            index += 1;
        }
        Ok(MethodCompiler {
            plan,
            class,
            owner: class.name().map_err(|e| e.to_string())?.to_owned(),
            method,
            insns,
            locals,
            extra: Vec::new(),
            first_local: index,
            out: Vec::new(),
            this: None,
        })
    }

    fn local(&mut self, variable: bool, n: u16, kind: Kind) -> u32 {
        if let Some(&i) = self.locals.get(&(variable, n, kind)) {
            return i;
        }
        let i = self.first_local + self.extra.len() as u32;
        self.extra.push(val(kind));
        self.locals.insert((variable, n, kind), i);
        i
    }
    fn var(&mut self, n: u16, kind: Kind) -> u32 {
        self.local(true, n, kind)
    }
    /// The local of stack depth `d`.
    fn st(&mut self, d: usize, kind: Kind) -> u32 {
        self.local(false, d as u16, kind)
    }
    fn emit(&mut self, i: W<'static>) {
        self.out.push(i);
    }
    fn get(&mut self, d: usize, kind: Kind) {
        let l = self.st(d, kind);
        self.emit(W::LocalGet(l));
    }
    fn set(&mut self, d: usize, kind: Kind) {
        let l = self.st(d, kind);
        self.emit(W::LocalSet(l));
    }
    /// The class whose initializer this is, if it is one.
    fn within(&self) -> Option<&str> {
        (self.method.name == "<clinit>").then_some(self.owner.as_str())
    }

    fn compile(mut self) -> Result<(Vec<ValType>, Vec<W<'static>>)> {
        let at: HashMap<u32, usize> = self
            .insns
            .iter()
            .enumerate()
            .map(|(i, insn)| (insn.at, i))
            .collect();
        let handlers = self
            .method
            .code()
            .map(|c| c.handlers.clone())
            .unwrap_or_default();
        // An instance method that never stores to local 0 has its object
        // there throughout: cast once, below, then used without tests.
        // (Not an interface's default method: its object is of whatever
        // class implements it.)
        let this_ty = (self.method.access & STATIC == 0
            && self.class.access & 0x0200 == 0
            && !self.insns.iter().any(|i| {
                i.opcode == 0x4B || (i.opcode == 0x3A && matches!(i.operand, Operand::Local(0)))
            }))
        .then(|| self.plan.structs.get(&self.owner).map(|(ty, _)| *ty))
        .flatten();
        let before = stack_kinds(self.class, &self.insns, &at, &handlers, this_ty.is_some())?;
        // Basic blocks: the first instruction, every target, and every
        // instruction after a jump or an end.
        let mut leaders = BTreeSet::from([0usize]);
        for (i, insn) in self.insns.iter().enumerate() {
            let succ = successors(insn, i, &at)?;
            let ends = matches!(
                insn.operand,
                Operand::Target(_) | Operand::Table { .. } | Operand::Lookup { .. }
            ) || matches!(insn.opcode, 0xAC..=0xB1 | 0xBF);
            if ends {
                leaders.extend(succ.iter().copied().filter(|&t| t < self.insns.len()));
                if i + 1 < self.insns.len() {
                    leaders.insert(i + 1);
                }
            }
        }
        // A try range's ends and its handler start blocks too.
        for h in &handlers {
            for pc in [h.start, h.end, h.handler] {
                if let Some(&i) = at.get(&u32::from(pc)) {
                    leaders.insert(i);
                }
            }
        }
        let blocks: Vec<usize> = leaders.into_iter().collect();
        let block_of: HashMap<usize, usize> =
            blocks.iter().enumerate().map(|(b, &i)| (i, b)).collect();
        let n = blocks.len();
        let label = self.first_local;
        self.extra.push(ValType::I32);
        self.locals.insert((false, u16::MAX, Kind::Int), label);
        if let Some(ty) = this_ty {
            let l = self.first_local + self.extra.len() as u32;
            self.extra.push(ValType::Ref(RefType {
                nullable: true,
                heap_type: HeapType::Concrete(ty),
            }));
            self.emit(W::LocalGet(0));
            self.emit(W::RefCastNullable(HeapType::Concrete(ty)));
            self.emit(W::LocalSet(l));
            self.this = Some((l, ty));
        }

        // A static method makes sure its class is initialized (the host may
        // call it first), as the JVM does on invokestatic.
        if self.method.access & STATIC != 0 && self.method.name != "<clinit>" {
            let e = self.plan.ensure[&self.owner];
            self.emit(W::Call(e));
        }
        // A method without exception handlers: nested blocks and loops from
        // its control-flow graph (javac's graphs are reducible), so each
        // branch is a direct one. With handlers: a block-number loop, below.
        // HORIZON_JAVA_FLAT=1: the block-number loop for every method (to
        // compare).
        if handlers.is_empty() && std::env::var_os("HORIZON_JAVA_FLAT").is_none() {
            let cfg = crate::cfg::Cfg::new(&self.insns, &handlers);
            if cfg.reducible {
                let shape = Shape::new(&cfg, &self.insns, &at);
                let mut ctx = Vec::new();
                self.tree(&cfg, &shape, &before, &at, 0, &mut ctx)?;
                self.emit(W::Unreachable);
                self.emit(W::End);
                return Ok((self.extra, self.out));
            }
        }
        self.emit(W::Loop(BlockType::Empty));
        for _ in 0..n {
            self.emit(W::Block(BlockType::Empty));
        }
        self.emit(W::LocalGet(label));
        let table: Vec<u32> = (0..n as u32).collect();
        self.out.push(W::BrTable(table.into(), 0));
        for (b, &start) in blocks.iter().enumerate() {
            self.emit(W::End);
            let end = blocks.get(b + 1).copied().unwrap_or(self.insns.len());
            // The loop is this deep from block b's code.
            let loop_depth = (n - 1 - b) as u32;
            if before[start].is_none() {
                // Unreachable code (dead after a return): leave it out.
                self.emit(W::Unreachable);
                continue;
            }
            // The handlers whose range has this block, in the table's order.
            let pc = self.insns[start].at;
            let covering: Vec<_> = handlers
                .iter()
                .filter(|h| u32::from(h.start) <= pc && pc < u32::from(h.end))
                .copied()
                .collect();
            if covering.is_empty() {
                for i in start..end {
                    let stack = before[i].clone().expect("reachable within a block");
                    let insn = self.insns[i].clone();
                    self.instruction(&insn, &stack, &block_of, &at, loop_depth)?;
                }
                continue;
            }
            // block $done { block $caught (anyref) { try_table (catch $caught)
            // { code } br $done } the handler that matches, else rethrow }
            self.emit(W::Block(BlockType::Empty));
            self.emit(W::Block(BlockType::Result(ANY)));
            self.out.push(W::TryTable(
                BlockType::Empty,
                vec![wasm_encoder::Catch::One {
                    tag: THROWABLE,
                    label: 0,
                }]
                .into(),
            ));
            for i in start..end {
                let stack = before[i].clone().expect("reachable within a block");
                let insn = self.insns[i].clone();
                self.instruction(&insn, &stack, &block_of, &at, loop_depth + 3)?;
            }
            self.emit(W::End);
            self.emit(W::Br(1));
            self.emit(W::End);
            let exception = self.st(2000, Kind::Ref);
            self.emit(W::LocalSet(exception));
            for h in covering {
                let target = *at
                    .get(&u32::from(h.handler))
                    .ok_or("a handler at no instruction")?;
                if h.catch_type == 0 {
                    self.emit(W::LocalGet(exception));
                    self.set(0, Kind::Ref);
                    self.jump(target, &block_of, loop_depth + 1)?;
                    break;
                }
                let class = self
                    .class
                    .class_name(h.catch_type)
                    .map_err(|e| e.to_string())?
                    .to_owned();
                self.emit(W::LocalGet(exception));
                self.instance_of(&class);
                self.emit(W::If(BlockType::Empty));
                self.emit(W::LocalGet(exception));
                self.set(0, Kind::Ref);
                self.jump(target, &block_of, loop_depth + 2)?;
                self.emit(W::End);
            }
            self.emit(W::LocalGet(exception));
            self.emit(W::Throw(THROWABLE));
            self.emit(W::End);
            // Falling through to the next block needs nothing: its code
            // follows the end of its block.
        }
        self.emit(W::End);
        self.emit(W::Unreachable);
        self.emit(W::End);
        Ok((self.extra, self.out))
    }

    /// Block `x` and the blocks it dominates that join paths, nested (Ramsey,
    /// "Beyond Relooper", 2022): each join block follows a WASM block that
    /// its branches leave; a loop header's code is in a WASM loop.
    fn tree(
        &mut self,
        cfg: &crate::cfg::Cfg,
        shape: &Shape,
        before: &[Option<Vec<Slot>>],
        at: &HashMap<u32, usize>,
        x: usize,
        ctx: &mut Vec<Nest>,
    ) -> Result<()> {
        let joins: Vec<usize> = shape.children[x]
            .iter()
            .copied()
            .filter(|&c| shape.join[c])
            .collect();
        if shape.header[x] {
            self.emit(W::Loop(BlockType::Empty));
            ctx.push(Nest::Loop(x));
            self.nest(cfg, shape, before, at, x, &joins, ctx)?;
            ctx.pop();
            self.emit(W::End);
        } else {
            self.nest(cfg, shape, before, at, x, &joins, ctx)?;
        }
        Ok(())
    }

    /// `x`'s code inside a WASM block for each join it dominates (the last in
    /// reverse postorder outermost), each join's code after its block.
    #[allow(clippy::too_many_arguments)]
    fn nest(
        &mut self,
        cfg: &crate::cfg::Cfg,
        shape: &Shape,
        before: &[Option<Vec<Slot>>],
        at: &HashMap<u32, usize>,
        x: usize,
        joins: &[usize],
        ctx: &mut Vec<Nest>,
    ) -> Result<()> {
        let Some((&y, rest)) = joins.split_last() else {
            return self.block_code(cfg, shape, before, at, x, ctx);
        };
        self.emit(W::Block(BlockType::Empty));
        ctx.push(Nest::Block(y));
        self.nest(cfg, shape, before, at, x, rest, ctx)?;
        ctx.pop();
        self.emit(W::End);
        self.tree(cfg, shape, before, at, y, ctx)
    }

    /// The instructions of block `x`, and its way out.
    fn block_code(
        &mut self,
        cfg: &crate::cfg::Cfg,
        shape: &Shape,
        before: &[Option<Vec<Slot>>],
        at: &HashMap<u32, usize>,
        x: usize,
        ctx: &mut Vec<Nest>,
    ) -> Result<()> {
        let (start, end) = (cfg.blocks[x].start, cfg.blocks[x].end);
        let none = HashMap::new();
        for i in start..end - 1 {
            let stack = before[i].clone().expect("reachable within a block");
            let insn = self.insns[i].clone();
            self.instruction(&insn, &stack, &none, at, 0)?;
        }
        let last = end - 1;
        let insn = self.insns[last].clone();
        let stack = before[last].clone().expect("reachable within a block");
        let d = stack.len();
        let block_at = |pc: u32| -> Result<usize> {
            at.get(&pc)
                .map(|&i| cfg.block_of[i])
                .ok_or_else(|| "a bad branch target".to_owned())
        };
        match (insn.opcode, &insn.operand) {
            (0x99..=0xA6 | 0xC6 | 0xC7, Operand::Target(t)) => {
                let (to, next) = (block_at(*t)?, cfg.block_of[end]);
                self.branch_condition(insn.opcode, d);
                self.emit(W::If(BlockType::Empty));
                ctx.push(Nest::If);
                self.branch(cfg, shape, before, at, x, to, ctx)?;
                self.emit(W::Else);
                self.branch(cfg, shape, before, at, x, next, ctx)?;
                ctx.pop();
                self.emit(W::End);
            }
            (0xA7 | 0xC8, Operand::Target(t)) => {
                let to = block_at(*t)?;
                self.branch(cfg, shape, before, at, x, to, ctx)?;
            }
            (0xAA | 0xAB, operand) => {
                let (default, pairs): (u32, Vec<(i32, u32)>) = match operand {
                    Operand::Table {
                        default,
                        low,
                        targets,
                    } => (
                        *default,
                        targets
                            .iter()
                            .enumerate()
                            .map(|(i, t)| (low + i as i32, *t))
                            .collect(),
                    ),
                    Operand::Lookup { default, pairs } => (*default, pairs.clone()),
                    _ => unreachable!(),
                };
                for (key, t) in pairs {
                    let to = block_at(t)?;
                    self.get(d - 1, Kind::Int);
                    self.emit(W::I32Const(key));
                    self.emit(W::I32Eq);
                    self.emit(W::If(BlockType::Empty));
                    ctx.push(Nest::If);
                    self.branch(cfg, shape, before, at, x, to, ctx)?;
                    ctx.pop();
                    self.emit(W::End);
                }
                let to = block_at(default)?;
                self.branch(cfg, shape, before, at, x, to, ctx)?;
            }
            (0xAC..=0xB1 | 0xBF, _) => self.instruction(&insn, &stack, &none, at, 0)?,
            _ => {
                // Falls through to the next block.
                self.instruction(&insn, &stack, &none, at, 0)?;
                let next = *cfg.block_of.get(end).ok_or("code falls off its end")?;
                self.branch(cfg, shape, before, at, x, next, ctx)?;
            }
        }
        Ok(())
    }

    /// From block `from` to block `to`: back to a loop's start, out of the
    /// block a join follows, or (the only way in) `to`'s code here.
    #[allow(clippy::too_many_arguments)]
    fn branch(
        &mut self,
        cfg: &crate::cfg::Cfg,
        shape: &Shape,
        before: &[Option<Vec<Slot>>],
        at: &HashMap<u32, usize>,
        from: usize,
        to: usize,
        ctx: &mut Vec<Nest>,
    ) -> Result<()> {
        let depth = |want: Nest| {
            ctx.iter()
                .rev()
                .position(|n| *n == want)
                .map(|i| i as u32)
                .ok_or_else(|| format!("no enclosing {want:?}"))
        };
        if shape.order[to] <= shape.order[from] {
            let d = depth(Nest::Loop(to))?;
            self.emit(W::Br(d));
        } else if shape.join[to] {
            let d = depth(Nest::Block(to))?;
            self.emit(W::Br(d));
        } else {
            self.tree(cfg, shape, before, at, to, ctx)?;
        }
        Ok(())
    }

    /// The condition of conditional branch `op` (stack depth `d`), as an
    /// i32 on the WASM stack: nonzero when it jumps.
    fn branch_condition(&mut self, op: u8, d: usize) {
        use Kind::*;
        match op {
            0x99..=0x9E => {
                self.get(d - 1, Int);
                self.emit(W::I32Const(0));
                self.emit(int_compare(op - 0x99));
            }
            0x9F..=0xA4 => {
                self.get(d - 2, Int);
                self.get(d - 1, Int);
                self.emit(int_compare(op - 0x9F));
            }
            0xA5 | 0xA6 => {
                self.get(d - 2, Ref);
                self.get(d - 1, Ref);
                let same = self
                    .plan
                    .import("same".into(), vec![ANY, ANY], vec![ValType::I32]);
                self.emit(W::Call(same));
                if op == 0xA6 {
                    self.emit(W::I32Eqz);
                }
            }
            _ => {
                self.get(d - 1, Ref);
                self.emit(W::RefIsNull);
                if op == 0xC7 {
                    self.emit(W::I32Eqz);
                }
            }
        }
    }

    /// Jumps to the block at instruction `target`.
    fn jump(&mut self, target: usize, block_of: &HashMap<usize, usize>, depth: u32) -> Result<()> {
        let b = *block_of.get(&target).ok_or("a jump to a non-leader")?;
        let label = self.first_local;
        self.emit(W::I32Const(b as i32));
        self.emit(W::LocalSet(label));
        self.emit(W::Br(depth));
        Ok(())
    }

    fn instruction(
        &mut self,
        insn: &Instruction,
        slots: &[Slot],
        block_of: &HashMap<usize, usize>,
        at: &HashMap<u32, usize>,
        loop_depth: u32,
    ) -> Result<()> {
        use Kind::*;
        let stack: Vec<Kind> = slots.iter().map(|s| s.kind).collect();
        let d = stack.len();
        let op = insn.opcode;
        let top = |k: usize| stack[d - 1 - k];
        let target_of = |pc: u32| at.get(&pc).copied().ok_or("a bad branch target".to_owned());
        match op {
            0x00 => {}
            0x01 => {
                self.emit(W::RefNull(HeapType::ANY));
                self.set(d, Ref);
            }
            0x02..=0x08 => {
                self.emit(W::I32Const(i32::from(op) - 3));
                self.set(d, Int);
            }
            0x09 | 0x0A => {
                self.emit(W::I64Const(i64::from(op) - 9));
                self.set(d, Long);
            }
            0x0B..=0x0D => {
                self.emit(W::F32Const((f32::from(op - 0x0B)).into()));
                self.set(d, Float);
            }
            0x0E | 0x0F => {
                self.emit(W::F64Const((f64::from(op - 0x0E)).into()));
                self.set(d, Double);
            }
            0x10 | 0x11 => {
                let Operand::Value(v) = insn.operand else {
                    unreachable!()
                };
                self.emit(W::I32Const(v));
                self.set(d, Int);
            }
            0x12..=0x14 => {
                let Operand::Constant(i) = insn.operand else {
                    unreachable!()
                };
                let (w, k) = match self.class.constants.get(usize::from(i)) {
                    Some(Constant::Integer(v)) => (W::I32Const(*v), Int),
                    Some(Constant::Float(v)) => (W::F32Const((*v).into()), Float),
                    Some(Constant::Long(v)) => (W::I64Const(*v), Long),
                    Some(Constant::Double(v)) => (W::F64Const((*v).into()), Double),
                    Some(Constant::String(s)) => {
                        let s = self.class.utf8(*s).map_err(|e| e.to_string())?;
                        self.host_constant(HostConstant::String(s.to_owned()));
                        self.set(d, Ref);
                        return Ok(());
                    }
                    Some(Constant::Class(_)) => {
                        let c = self.class.class_name(i).map_err(|e| e.to_string())?;
                        self.host_constant(HostConstant::Class(c.to_owned()));
                        self.set(d, Ref);
                        return Ok(());
                    }
                    _ => return Err("ldc of a method type or handle is not compiled yet".into()),
                };
                self.emit(w);
                self.set(d, k);
            }
            0x15..=0x2D => {
                let (n, k) = match (op, &insn.operand) {
                    (0x15..=0x19, Operand::Local(n)) => (*n, kind_of_load(op - 0x15)),
                    _ => {
                        let o = op - 0x1A;
                        (u16::from(o % 4), kind_of_load(o / 4))
                    }
                };
                let l = self.var(n, k);
                self.emit(W::LocalGet(l));
                self.set(d, k);
            }
            0x36..=0x4E => {
                let (n, k) = match (op, &insn.operand) {
                    (0x36..=0x3A, Operand::Local(n)) => (*n, kind_of_load(op - 0x36)),
                    _ => {
                        let o = op - 0x3B;
                        (u16::from(o % 4), kind_of_load(o / 4))
                    }
                };
                self.get(d - 1, k);
                let l = self.var(n, k);
                self.emit(W::LocalSet(l));
            }
            0x57 | 0x58 => {}
            0x59..=0x5F => {
                // The values the instruction moves: the top of the stack.
                let low = d - pick(op, &stack)?;
                let moved: Vec<(usize, Kind)> = (low..d).map(|i| (i, stack[i])).collect();
                let order = arrangement(op, &moved)?;
                for &(i, k) in &order {
                    self.get(i, k);
                }
                for (j, &(_, k)) in order.iter().enumerate().rev() {
                    self.set(low + j, k);
                }
            }
            0x60..=0x73 => {
                let k = top(0);
                self.get(d - 2, k);
                self.get(d - 1, k);
                match op {
                    0x6C => self.idiv(false),
                    0x70 => self.idiv(true),
                    0x6D => self.ldiv(false),
                    0x71 => self.ldiv(true),
                    0x72 | 0x73 => {
                        // a - b * trunc(a / b), as fmod for finite values.
                        let (ta, tb) = (self.st(d, k), self.st(d + 1, k));
                        self.emit(W::LocalSet(tb));
                        self.emit(W::LocalSet(ta));
                        self.emit(W::LocalGet(ta));
                        self.emit(W::LocalGet(ta));
                        self.emit(W::LocalGet(tb));
                        let float = op == 0x72;
                        self.emit(if float { W::F32Div } else { W::F64Div });
                        self.emit(if float { W::F32Trunc } else { W::F64Trunc });
                        self.emit(W::LocalGet(tb));
                        self.emit(if float { W::F32Mul } else { W::F64Mul });
                        self.emit(if float { W::F32Sub } else { W::F64Sub });
                    }
                    _ => self.emit(arith(op).ok_or("arithmetic")?),
                }
                self.set(d - 2, k);
            }
            0x74..=0x77 => {
                let k = top(0);
                match k {
                    Int => {
                        self.emit(W::I32Const(0));
                        self.get(d - 1, k);
                        self.emit(W::I32Sub);
                    }
                    Long => {
                        self.emit(W::I64Const(0));
                        self.get(d - 1, k);
                        self.emit(W::I64Sub);
                    }
                    Float => {
                        self.get(d - 1, k);
                        self.emit(W::F32Neg);
                    }
                    _ => {
                        self.get(d - 1, k);
                        self.emit(W::F64Neg);
                    }
                }
                self.set(d - 1, k);
            }
            0x78..=0x7D => {
                // Shifts: the count is an int; Java and WASM both mask it.
                let k = top(1);
                self.get(d - 2, k);
                self.get(d - 1, Int);
                if k == Long {
                    self.emit(W::I64ExtendI32U);
                }
                self.emit(match op {
                    0x78 => W::I32Shl,
                    0x79 => W::I64Shl,
                    0x7A => W::I32ShrS,
                    0x7B => W::I64ShrS,
                    0x7C => W::I32ShrU,
                    _ => W::I64ShrU,
                });
                self.set(d - 2, k);
            }
            0x7E..=0x83 => {
                let k = top(0);
                self.get(d - 2, k);
                self.get(d - 1, k);
                self.emit(match op {
                    0x7E => W::I32And,
                    0x7F => W::I64And,
                    0x80 => W::I32Or,
                    0x81 => W::I64Or,
                    0x82 => W::I32Xor,
                    _ => W::I64Xor,
                });
                self.set(d - 2, k);
            }
            0x84 => {
                let Operand::Increment(n, c) = insn.operand else {
                    unreachable!()
                };
                let l = self.var(n, Int);
                self.emit(W::LocalGet(l));
                self.emit(W::I32Const(c));
                self.emit(W::I32Add);
                self.emit(W::LocalSet(l));
            }
            0x85..=0x93 => {
                let from = top(0);
                self.get(d - 1, from);
                let (w, to) = conversion(op);
                for i in w {
                    self.emit(i);
                }
                self.set(d - 1, to);
            }
            0x94..=0x98 => {
                let k = top(0);
                self.compare(op, k, d);
                self.set(d - 2, Int);
            }
            0x99..=0xA6 | 0xC6 | 0xC7 => {
                let Operand::Target(t) = insn.operand else {
                    unreachable!()
                };
                self.branch_condition(op, d);
                self.emit(W::If(BlockType::Empty));
                let target = target_of(t)?;
                self.jump(target, block_of, loop_depth + 1)?;
                self.emit(W::End);
            }
            0xA7 | 0xC8 => {
                let Operand::Target(t) = insn.operand else {
                    unreachable!()
                };
                let target = target_of(t)?;
                self.jump(target, block_of, loop_depth)?;
            }
            0xAA | 0xAB => {
                let pairs: Vec<(i32, u32)> = match &insn.operand {
                    Operand::Table { low, targets, .. } => targets
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (low + i as i32, *t))
                        .collect(),
                    Operand::Lookup { pairs, .. } => pairs.clone(),
                    _ => unreachable!(),
                };
                let default = match &insn.operand {
                    Operand::Table { default, .. } | Operand::Lookup { default, .. } => *default,
                    _ => unreachable!(),
                };
                for (key, t) in pairs {
                    self.get(d - 1, Int);
                    self.emit(W::I32Const(key));
                    self.emit(W::I32Eq);
                    self.emit(W::If(BlockType::Empty));
                    let target = target_of(t)?;
                    self.jump(target, block_of, loop_depth + 1)?;
                    self.emit(W::End);
                }
                let target = target_of(default)?;
                self.jump(target, block_of, loop_depth)?;
            }
            0xAC..=0xB0 => {
                self.get(d - 1, top(0));
                self.emit(W::Return);
            }
            0xB1 => self.emit(W::Return),
            0xB2 | 0xB3 => {
                let Operand::Constant(i) = insn.operand else {
                    unreachable!()
                };
                let (owner, name, desc) = self.class.member_ref(i).map_err(|e| e.to_string())?;
                let k = Kind::of(desc).ok_or("a bad field descriptor")?;
                match self.plan.static_field(owner, name) {
                    Some((declaring, g, _)) => {
                        self.ensure(&declaring);
                        if op == 0xB2 {
                            self.emit(W::GlobalGet(g));
                            self.set(d, k);
                        } else {
                            self.get(d - 1, k);
                            self.emit(W::GlobalSet(g));
                        }
                    }
                    // The game's: one the class inherits (Block.FACING) is
                    // asked of its game superclass.
                    None if op == 0xB2 => {
                        let owner = self.plan.game_owner(owner);
                        let f = self.plan.host_field("getstatic", &owner, name, k, desc);
                        self.emit(W::Call(f));
                        self.set(d, k);
                    }
                    None => {
                        let owner = self.plan.game_owner(owner);
                        let f = self.plan.host_field("putstatic", &owner, name, k, desc);
                        self.get(d - 1, k);
                        self.emit(W::Call(f));
                    }
                }
            }
            0xB4 | 0xB5 => {
                let Operand::Constant(i) = insn.operand else {
                    unreachable!()
                };
                let (owner, name, desc) = self.class.member_ref(i).map_err(|e| e.to_string())?;
                let k = Kind::of(desc).ok_or("a bad field descriptor")?;
                let object = if op == 0xB4 { d - 1 } else { d - 2 };
                // The method's own object needs no test: its typed local.
                let this = self
                    .this
                    .filter(|_| slots[object].this && self.plan.is_subtype(&self.owner, owner));
                match this {
                    Some((l, _)) => self.emit(W::LocalGet(l)),
                    None => self.get(object, Ref),
                }
                match self.plan.field(owner, name) {
                    Some((ty, field)) => {
                        if this.is_none() {
                            self.emit(W::RefCastNullable(HeapType::Concrete(ty)));
                        }
                        if op == 0xB4 {
                            self.emit(W::StructGet {
                                struct_type_index: ty,
                                field_index: field,
                            });
                            self.set(d - 1, k);
                        } else {
                            self.get(d - 1, k);
                            self.emit(W::StructSet {
                                struct_type_index: ty,
                                field_index: field,
                            });
                        }
                    }
                    // The game's: of the peer, for a mod object.
                    None if op == 0xB4 => {
                        let owner = self.plan.game_owner(owner);
                        let f = self.plan.host_field("getfield", &owner, name, k, desc);
                        self.emit(W::Call(f));
                        self.set(d - 1, k);
                    }
                    None => {
                        let owner = self.plan.game_owner(owner);
                        let f = self.plan.host_field("putfield", &owner, name, k, desc);
                        self.get(d - 1, k);
                        self.emit(W::Call(f));
                    }
                }
            }
            0xB6..=0xB9 => {
                let (owner, name, desc) = invoked(self.class, insn)?;
                let m = Method::parse(&desc).ok_or("a bad method descriptor")?;
                let kinds = m.param_kinds();
                let base = d - kinds.len();
                let receiver = (op != 0xB8).then(|| base - 1);
                let within = self.within().map(str::to_owned);
                // An array's clone: a copy of the same type.
                if owner.starts_with('[') && name == "clone" {
                    let ty = ARRAYS[array_kind(&owner[1..])];
                    let r = base - 1;
                    let (src, len) = (self.st(r, Ref), self.st(r + 1, Int));
                    let dst = self.st(r + 2, Ref);
                    self.emit(W::LocalGet(src));
                    self.emit(W::RefCastNonNull(HeapType::Concrete(ty)));
                    self.emit(W::ArrayLen);
                    self.emit(W::LocalSet(len));
                    self.emit(W::LocalGet(len));
                    self.emit(W::ArrayNewDefault(ty));
                    self.emit(W::LocalSet(dst));
                    self.emit(W::LocalGet(dst));
                    self.emit(W::RefCastNonNull(HeapType::Concrete(ty)));
                    self.emit(W::I32Const(0));
                    self.emit(W::LocalGet(src));
                    self.emit(W::RefCastNonNull(HeapType::Concrete(ty)));
                    self.emit(W::I32Const(0));
                    self.emit(W::LocalGet(len));
                    self.emit(W::ArrayCopy {
                        array_type_index_dst: ty,
                        array_type_index_src: ty,
                    });
                    self.emit(W::LocalGet(dst));
                    self.set(r, Ref);
                    return Ok(());
                }
                // A game class's constructor on an object from `new`: the
                // host makes the object, which replaces every copy.
                if op == 0xB7
                    && name == "<init>"
                    && !self.plan.classes.contains_key(&owner)
                    && !base_class(&owner)
                    && slots[base - 1].new.is_none()
                {
                    // The superclass constructor of a mod object whose class
                    // extends a game class: the host makes its peer.
                    let r = base - 1;
                    self.get(r, Ref);
                    self.emit(W::RefCastNonNull(HeapType::Concrete(OBJECT)));
                    self.get(r, Ref);
                    for (j, k) in kinds.iter().enumerate() {
                        self.get(base + j, *k);
                    }
                    let f = self.plan.host_method("extend", &owner, &name, &desc)?;
                    self.emit(W::Call(f));
                    self.emit(W::StructSet {
                        struct_type_index: OBJECT,
                        field_index: 1,
                    });
                    return Ok(());
                }
                if op == 0xB7 && name == "<init>" && !self.plan.classes.contains_key(&owner) {
                    let r = base - 1;
                    if let Some(new) = slots[r].new {
                        for (j, k) in kinds.iter().enumerate() {
                            self.get(base + j, *k);
                        }
                        let f = self.plan.host_method("new", &owner, &name, &desc)?;
                        self.emit(W::Call(f));
                        self.set(r, Ref);
                        for j in 0..r {
                            if slots[j].new == Some(new) {
                                self.get(r, Ref);
                                self.set(j, Ref);
                            }
                        }
                        return Ok(());
                    }
                }
                if op == 0xB8 && self.intrinsic(&owner, &name, &desc, base) {
                    return Ok(());
                }
                if let Some(r) = receiver {
                    self.get(r, Ref);
                }
                for (j, k) in kinds.iter().enumerate() {
                    self.get(base + j, *k);
                }
                let call = match op {
                    0xB6 => Call::Virtual,
                    0xB7 => Call::Special,
                    0xB8 => Call::Static,
                    _ => Call::Interface,
                };
                let mut out = std::mem::take(&mut self.out);
                let r = self
                    .plan
                    .call(&mut out, call, &owner, &name, &desc, within.as_deref());
                self.out = out;
                r?;
                if let Some(k) = m.return_kind() {
                    self.set(receiver.unwrap_or(base), k);
                }
            }
            0xBA => self.invokedynamic(insn, d)?,
            0xBB => {
                let Operand::Constant(i) = insn.operand else {
                    unreachable!()
                };
                let class = self.class.class_name(i).map_err(|e| e.to_string())?;
                if self.plan.classes.contains_key(class) {
                    self.ensure(class);
                    let mut out = std::mem::take(&mut self.out);
                    let r = self.plan.alloc(&mut out, class);
                    self.out = out;
                    r?;
                } else {
                    // The host makes it in the constructor call.
                    self.emit(W::RefNull(HeapType::ANY));
                }
                self.set(d, Ref);
            }
            0xC0 => {}
            0xC1 => {
                let Operand::Constant(i) = insn.operand else {
                    unreachable!()
                };
                let class = self
                    .class
                    .class_name(i)
                    .map_err(|e| e.to_string())?
                    .to_owned();
                self.get(d - 1, Ref);
                self.instance_of(&class);
                self.set(d - 1, Int);
            }
            0xBF => {
                self.get(d - 1, Ref);
                self.emit(W::Throw(THROWABLE));
            }
            0xC2 | 0xC3 => {}
            0xC5 => {
                let Operand::Dimensions(i, dims) = insn.operand else {
                    unreachable!()
                };
                let class = self.class.class_name(i).map_err(|e| e.to_string())?;
                let f = self
                    .plan
                    .late(Late::MultiArray(class.to_owned(), u32::from(dims)));
                let base = d - usize::from(dims);
                for j in 0..usize::from(dims) {
                    self.get(base + j, Int);
                }
                self.emit(W::Call(f));
                self.set(base, Ref);
            }
            0xBC | 0xBD => {
                let ty = match &insn.operand {
                    Operand::ArrayType(t) => {
                        ARRAYS[match t {
                            4 | 8 => 0,
                            5 | 9 => 1,
                            10 => 2,
                            11 => 3,
                            6 => 4,
                            _ => 5,
                        }]
                    }
                    _ => ARRAYS[6],
                };
                self.get(d - 1, Int);
                self.emit(W::ArrayNewDefault(ty));
                self.set(d - 1, Ref);
            }
            0xBE => {
                self.get(d - 1, Ref);
                self.emit(W::RefCastNullable(ARRAY_REF));
                self.emit(W::ArrayLen);
                self.set(d - 1, Int);
            }
            0x2E..=0x35 => {
                let (kind, k, get) = match op {
                    0x2E => (2, Int, W::ArrayGet(ARRAYS[2])),
                    0x2F => (3, Long, W::ArrayGet(ARRAYS[3])),
                    0x30 => (4, Float, W::ArrayGet(ARRAYS[4])),
                    0x31 => (5, Double, W::ArrayGet(ARRAYS[5])),
                    0x32 => (6, Ref, W::ArrayGet(ARRAYS[6])),
                    0x33 => (0, Int, W::ArrayGetS(ARRAYS[0])),
                    0x34 => (1, Int, W::ArrayGetU(ARRAYS[1])),
                    _ => (1, Int, W::ArrayGetS(ARRAYS[1])),
                };
                self.get(d - 2, Ref);
                self.emit(W::RefCastNullable(HeapType::Concrete(ARRAYS[kind])));
                self.get(d - 1, Int);
                self.emit(get);
                self.set(d - 2, k);
            }
            0x4F..=0x56 => {
                let (kind, k) = match op {
                    0x4F => (2, Int),
                    0x50 => (3, Long),
                    0x51 => (4, Float),
                    0x52 => (5, Double),
                    0x53 => (6, Ref),
                    0x54 => (0, Int),
                    _ => (1, Int),
                };
                self.get(d - 3, Ref);
                self.emit(W::RefCastNullable(HeapType::Concrete(ARRAYS[kind])));
                self.get(d - 2, Int);
                self.get(d - 1, k);
                self.emit(W::ArraySet(ARRAYS[kind]));
            }
            _ => {
                return Err(format!("{} is not compiled yet", crate::bytecode::name(op)));
            }
        }
        Ok(())
    }

    /// A pattern `switch` (SwitchBootstraps.typeSwitch or enumSwitch): with
    /// the object and the label to restart from on the stack, the index of
    /// the first label from there that matches; -1 for null, the number of
    /// labels for none.
    fn type_switch(&mut self, bootstrap: &str, labels: &[u16], d: usize) -> Result<()> {
        let (object, restart, out) = (
            self.st(d - 2, Kind::Ref),
            self.st(d - 1, Kind::Int),
            self.st(2003, Kind::Int),
        );
        let equals = self.plan.host_method(
            "invokevirtual",
            "java/lang/Object",
            "equals",
            "(Ljava/lang/Object;)Z",
        )?;
        self.emit(W::Block(BlockType::Empty));
        self.emit(W::LocalGet(object));
        self.emit(W::RefIsNull);
        self.emit(W::If(BlockType::Empty));
        self.emit(W::I32Const(-1));
        self.emit(W::LocalSet(out));
        self.emit(W::Br(1));
        self.emit(W::End);
        for (i, &label) in labels.iter().enumerate() {
            self.emit(W::LocalGet(restart));
            self.emit(W::I32Const(i as i32));
            self.emit(W::I32LeS);
            self.emit(W::If(BlockType::Empty));
            match self.class.constants.get(usize::from(label)) {
                Some(Constant::Class(_)) => {
                    let class = self
                        .class
                        .class_name(label)
                        .map_err(|e| e.to_string())?
                        .to_owned();
                    self.emit(W::LocalGet(object));
                    self.instance_of(&class);
                }
                Some(Constant::String(s)) => {
                    let s = self.class.utf8(*s).map_err(|e| e.to_string())?.to_owned();
                    self.emit(W::LocalGet(object));
                    if bootstrap == "enumSwitch" {
                        // An enum constant's label is its name.
                        let name = self.plan.host_method(
                            "invokevirtual",
                            "java/lang/Enum",
                            "name",
                            "()Ljava/lang/String;",
                        )?;
                        self.emit(W::Call(name));
                    }
                    self.host_constant(HostConstant::String(s));
                    self.emit(W::Call(equals));
                }
                Some(Constant::Integer(v)) => {
                    let boxed = self.plan.host_method(
                        "invokestatic",
                        "java/lang/Integer",
                        "valueOf",
                        "(I)Ljava/lang/Integer;",
                    )?;
                    self.emit(W::LocalGet(object));
                    self.emit(W::I32Const(*v));
                    self.emit(W::Call(boxed));
                    self.emit(W::Call(equals));
                }
                _ => return Err(format!("a {bootstrap} label of an unknown kind")),
            }
            self.emit(W::If(BlockType::Empty));
            self.emit(W::I32Const(i as i32));
            self.emit(W::LocalSet(out));
            self.emit(W::Br(2));
            self.emit(W::End);
            self.emit(W::End);
        }
        self.emit(W::I32Const(labels.len() as i32));
        self.emit(W::LocalSet(out));
        self.emit(W::End);
        self.emit(W::LocalGet(out));
        self.set(d - 2, Kind::Int);
        Ok(())
    }

    /// A JDK static method whose Java semantics are exactly a few WASM
    /// instructions, inline (its arguments in slots `base..`, its result to
    /// `base`): no call to the host. Only exact ones: `Math.sin` and the
    /// like stay with the host. Whether it was one.
    fn intrinsic(&mut self, owner: &str, name: &str, desc: &str, base: usize) -> bool {
        use Kind::{Double as D, Float as F, Int as I, Long as J};
        let math = matches!(owner, "java/lang/Math" | "java/lang/StrictMath");
        // One instruction on the arguments as they are.
        let unary: Option<(Kind, W<'static>, Kind)> = match (owner, name, desc) {
            (_, "floor", "(D)D") if math => Some((D, W::F64Floor, D)),
            (_, "ceil", "(D)D") if math => Some((D, W::F64Ceil, D)),
            (_, "sqrt", "(D)D") if math => Some((D, W::F64Sqrt, D)),
            (_, "rint", "(D)D") if math => Some((D, W::F64Nearest, D)),
            (_, "abs", "(D)D") if math => Some((D, W::F64Abs, D)),
            (_, "abs", "(F)F") if math => Some((F, W::F32Abs, F)),
            ("java/lang/Double", "doubleToRawLongBits", "(D)J") => {
                Some((D, W::I64ReinterpretF64, J))
            }
            ("java/lang/Double", "longBitsToDouble", "(J)D") => Some((J, W::F64ReinterpretI64, D)),
            ("java/lang/Float", "floatToRawIntBits", "(F)I") => Some((F, W::I32ReinterpretF32, I)),
            ("java/lang/Float", "intBitsToFloat", "(I)F") => Some((I, W::F32ReinterpretI32, F)),
            _ => None,
        };
        if let Some((k, op, out)) = unary {
            self.get(base, k);
            self.emit(op);
            self.set(base, out);
            return true;
        }
        let binary: Option<(Kind, W<'static>)> = match (name, desc) {
            // WASM's min and max are Java's: NaN wins, -0.0 below 0.0.
            ("max", "(DD)D") if math => Some((D, W::F64Max)),
            ("min", "(DD)D") if math => Some((D, W::F64Min)),
            ("max", "(FF)F") if math => Some((F, W::F32Max)),
            ("min", "(FF)F") if math => Some((F, W::F32Min)),
            _ => None,
        };
        if let Some((k, op)) = binary {
            self.get(base, k);
            self.get(base + 1, k);
            self.emit(op);
            self.set(base, k);
            return true;
        }
        // a, b, select(a, b, a <cmp> b).
        let pick: Option<(Kind, W<'static>)> = match (name, desc) {
            ("max", "(II)I") if math => Some((I, W::I32GtS)),
            ("min", "(II)I") if math => Some((I, W::I32LtS)),
            ("max", "(JJ)J") if math => Some((J, W::I64GtS)),
            ("min", "(JJ)J") if math => Some((J, W::I64LtS)),
            _ => None,
        };
        if let Some((k, cmp)) = pick {
            self.get(base, k);
            self.get(base + 1, k);
            self.get(base, k);
            self.get(base + 1, k);
            self.emit(cmp);
            self.emit(W::Select);
            self.set(base, k);
            return true;
        }
        match (owner, name, desc) {
            // x, -x, select(x, -x, x >= 0): abs(MIN_VALUE) stays MIN_VALUE.
            (_, "abs", "(I)I") if math => {
                self.get(base, I);
                self.emit(W::I32Const(0));
                self.get(base, I);
                self.emit(W::I32Sub);
                self.get(base, I);
                self.emit(W::I32Const(0));
                self.emit(W::I32GeS);
                self.emit(W::Select);
                self.set(base, I);
            }
            (_, "abs", "(J)J") if math => {
                self.get(base, J);
                self.emit(W::I64Const(0));
                self.get(base, J);
                self.emit(W::I64Sub);
                self.get(base, J);
                self.emit(W::I64Const(0));
                self.emit(W::I64GeS);
                self.emit(W::Select);
                self.set(base, J);
            }
            ("java/lang/Double", "isNaN", "(D)Z") => {
                self.get(base, D);
                self.get(base, D);
                self.emit(W::F64Ne);
                self.set(base, I);
            }
            ("java/lang/Float", "isNaN", "(F)Z") => {
                self.get(base, F);
                self.get(base, F);
                self.emit(W::F32Ne);
                self.set(base, I);
            }
            // Raw bits, but every NaN as the canonical one.
            ("java/lang/Double", "doubleToLongBits", "(D)J") => {
                self.get(base, D);
                self.emit(W::I64ReinterpretF64);
                self.emit(W::I64Const(0x7ff8_0000_0000_0000));
                self.get(base, D);
                self.get(base, D);
                self.emit(W::F64Eq);
                self.emit(W::Select);
                self.set(base, J);
            }
            ("java/lang/Float", "floatToIntBits", "(F)I") => {
                self.get(base, F);
                self.emit(W::I32ReinterpretF32);
                self.emit(W::I32Const(0x7fc0_0000));
                self.get(base, F);
                self.get(base, F);
                self.emit(W::F32Eq);
                self.emit(W::Select);
                self.set(base, I);
            }
            _ => return false,
        }
        true
    }

    /// `instanceof class` of the reference on top of the WASM stack: 0 or 1.
    fn instance_of(&mut self, class: &str) {
        if let Some(component) = class.strip_prefix('[') {
            let ty = ARRAYS[array_kind(component)];
            let v = self.st(2001, Kind::Ref);
            self.emit(W::LocalTee(v));
            self.emit(W::RefTestNonNull(ARRAY_REF));
            self.emit(W::If(BlockType::Result(ValType::I32)));
            self.emit(W::LocalGet(v));
            self.emit(W::RefTestNonNull(HeapType::Concrete(ty)));
            self.emit(W::Else);
            self.emit(W::I32Const(0));
            self.emit(W::End);
        } else if self.plan.classes.contains_key(class) {
            let f = self.plan.late(Late::InstanceOf(class.to_owned()));
            self.emit(W::Call(f));
        } else {
            let f = self
                .plan
                .import(format!("instanceof {class}"), vec![ANY], vec![ValType::I32]);
            self.emit(W::Call(f));
        }
    }

    /// Pushes a host constant to the stack local `d` (the caller sets it).
    fn host_constant(&mut self, c: HostConstant) {
        let (index, g) = self.plan.constant(c);
        let f = self
            .plan
            .import("constant".into(), vec![ValType::I32], vec![ANY]);
        self.emit(W::GlobalGet(g));
        self.emit(W::RefIsNull);
        self.emit(W::If(BlockType::Empty));
        self.emit(W::I32Const(index as i32));
        self.emit(W::Call(f));
        self.emit(W::GlobalSet(g));
        self.emit(W::End);
        self.emit(W::GlobalGet(g));
    }

    fn invokedynamic(&mut self, insn: &Instruction, d: usize) -> Result<()> {
        let e = |e: crate::Error| e.to_string();
        let Operand::Constant(i) = insn.operand else {
            unreachable!()
        };
        let Some(Constant::InvokeDynamic(bsm, nat)) = self.class.constants.get(usize::from(i))
        else {
            return Err("a bad invokedynamic".into());
        };
        let (method, desc) = self.class.name_and_type(*nat).map_err(e)?;
        let bootstrap = self
            .class
            .bootstrap_methods()
            .get(usize::from(*bsm))
            .ok_or("a bad bootstrap method")?;
        let handle = |index: u16| match self.class.constants.get(usize::from(index)) {
            Some(Constant::MethodHandle(kind, r)) => {
                let (o, n, d) = self.class.member_ref(*r).map_err(e)?;
                Ok((*kind, o.to_owned(), n.to_owned(), d.to_owned()))
            }
            _ => Err("a bad method handle".to_owned()),
        };
        let (_, bo, bn, _) = handle(bootstrap.method_handle)?;
        let site_desc = Method::parse(desc).ok_or("a bad descriptor")?;
        let base = d - site_desc.params.len();
        let string = |index: u16| -> Result<String> {
            match self.class.constants.get(usize::from(index)) {
                Some(Constant::String(s)) => Ok(self.class.utf8(*s).map_err(e)?.to_owned()),
                Some(Constant::Class(_)) => Ok(self.class.class_name(index).map_err(e)?.to_owned()),
                _ => Err("a bad bootstrap argument".to_owned()),
            }
        };
        // String concatenation and a record's methods: the host's.
        let host = match (bo.as_str(), bn.as_str()) {
            ("java/lang/invoke/StringConcatFactory", "makeConcatWithConstants") => {
                let mut args = bootstrap.arguments.iter();
                let recipe = string(*args.next().ok_or("a bad concatenation")?)?;
                // The recipe's constants (\u{2}) go in it.
                let mut text = String::new();
                for c in recipe.chars() {
                    match c {
                        '\u{2}' => text.push_str(&string(*args.next().ok_or("a bad recipe")?)?),
                        c => text.push(c),
                    }
                }
                Some(format!("concat {desc} {text}"))
            }
            ("java/lang/invoke/StringConcatFactory", "makeConcat") => Some(format!(
                "concat {desc} {}",
                "\u{1}".repeat(site_desc.params.len())
            )),
            ("java/lang/runtime/ObjectMethods", "bootstrap") => {
                let class = string(bootstrap.arguments[0])?;
                Some(format!("record {class}.{method} {desc}"))
            }
            _ => None,
        };
        if let Some(import) = host {
            let params = site_desc.param_kinds();
            let results: Vec<ValType> = site_desc.return_kind().map(val).into_iter().collect();
            let f = self
                .plan
                .import(import, params.iter().map(|k| val(*k)).collect(), results);
            for (j, k) in params.iter().enumerate() {
                self.get(base + j, *k);
            }
            self.emit(W::Call(f));
            if let Some(k) = site_desc.return_kind() {
                self.set(base, k);
            }
            return Ok(());
        }
        if bo == "java/lang/runtime/SwitchBootstraps" {
            return self.type_switch(&bn, &bootstrap.arguments, d);
        }
        // altMetafactory adds flags (serializable, markers, bridges) after
        // the same three arguments: none of them changes the call.
        if bo != "java/lang/invoke/LambdaMetafactory"
            || (bn != "metafactory" && bn != "altMetafactory")
        {
            return Err(format!(
                "invokedynamic through {bo}.{bn} is not compiled yet"
            ));
        }
        let [sam, target, _, ..] = bootstrap.arguments[..] else {
            return Err("a bad lambda".into());
        };
        let Some(Constant::MethodType(sam)) = self.class.constants.get(usize::from(sam)) else {
            return Err("a bad lambda".into());
        };
        let sam = self.class.utf8(*sam).map_err(e)?.to_owned();
        let target = handle(target)?;
        let interface = site_desc
            .ret
            .strip_prefix('L')
            .and_then(|r| r.strip_suffix(';'))
            .ok_or("a lambda of no interface")?
            .to_owned();
        let captures = site_desc.param_kinds();
        let ty = self
            .plan
            .struct_type(OBJECT, captures.iter().map(|k| (val(*k), false)).collect());
        let n = {
            let mut sites = self.plan.sites.borrow_mut();
            sites.push((
                Lambda {
                    interface,
                    method: method.to_owned(),
                    descriptor: sam.clone(),
                    target: format!("{}.{} {}", target.1, target.2, target.3),
                },
                Site {
                    sam,
                    captures: site_desc.params.clone(),
                    target,
                    ty,
                },
            ));
            sites.len() - 1
        };
        let id = self.plan.classes.len() + 1 + n;
        self.emit(W::I32Const(id as i32));
        self.emit(W::RefNull(HeapType::ANY));
        for (j, k) in captures.iter().enumerate() {
            self.get(base + j, *k);
        }
        self.emit(W::StructNew(ty));
        self.set(base, Kind::Ref);
        Ok(())
    }

    fn ensure(&mut self, owner: &str) {
        if self.within() == Some(owner) {
            return;
        }
        if let Some(&e) = self.plan.ensure.get(owner) {
            self.emit(W::Call(e));
        }
    }

    /// int division and remainder with Java's results: MIN / -1 is MIN and
    /// MIN % -1 is 0 (WASM traps on both); a zero divisor traps.
    fn idiv(&mut self, rem: bool) {
        let b = self.st(1000, Kind::Int);
        let a = self.st(1001, Kind::Int);
        self.emit(W::LocalSet(b));
        self.emit(W::LocalSet(a));
        self.emit(W::LocalGet(b));
        self.emit(W::I32Const(-1));
        self.emit(W::I32Eq);
        self.emit(W::If(BlockType::Result(ValType::I32)));
        if rem {
            self.emit(W::I32Const(0));
        } else {
            self.emit(W::I32Const(0));
            self.emit(W::LocalGet(a));
            self.emit(W::I32Sub);
        }
        self.emit(W::Else);
        self.emit(W::LocalGet(a));
        self.emit(W::LocalGet(b));
        self.emit(if rem { W::I32RemS } else { W::I32DivS });
        self.emit(W::End);
    }
    fn ldiv(&mut self, rem: bool) {
        let b = self.st(1000, Kind::Long);
        let a = self.st(1001, Kind::Long);
        self.emit(W::LocalSet(b));
        self.emit(W::LocalSet(a));
        self.emit(W::LocalGet(b));
        self.emit(W::I64Const(-1));
        self.emit(W::I64Eq);
        self.emit(W::If(BlockType::Result(ValType::I64)));
        if rem {
            self.emit(W::I64Const(0));
        } else {
            self.emit(W::I64Const(0));
            self.emit(W::LocalGet(a));
            self.emit(W::I64Sub);
        }
        self.emit(W::Else);
        self.emit(W::LocalGet(a));
        self.emit(W::LocalGet(b));
        self.emit(if rem { W::I64RemS } else { W::I64DivS });
        self.emit(W::End);
    }

    /// lcmp, fcmpl, fcmpg, dcmpl, dcmpg: -1, 0 or 1; with a NaN, -1 for
    /// the `l` forms and 1 for the `g` forms.
    fn compare(&mut self, op: u8, k: Kind, d: usize) {
        let (gt, lt) = match k {
            Kind::Long => (W::I64GtS, W::I64LtS),
            Kind::Float => (W::F32Gt, W::F32Lt),
            _ => (W::F64Gt, W::F64Lt),
        };
        // (a > b) - (a < b)
        self.get(d - 2, k);
        self.get(d - 1, k);
        self.emit(gt);
        self.get(d - 2, k);
        self.get(d - 1, k);
        self.emit(lt);
        self.emit(W::I32Sub);
        if matches!(op, 0x95..=0x98) {
            // A NaN (a != a or b != b) makes neither > nor < hold: the `l`
            // forms give -1 and the `g` forms 1.
            let ne = match k {
                Kind::Float => W::F32Ne,
                _ => W::F64Ne,
            };
            let ordered = self.st(1003, Kind::Int);
            self.emit(W::LocalSet(ordered));
            let nan = if matches!(op, 0x95 | 0x97) { -1 } else { 1 };
            self.emit(W::I32Const(nan));
            self.emit(W::LocalGet(ordered));
            self.get(d - 2, k);
            self.get(d - 2, k);
            self.emit(ne.clone());
            self.get(d - 1, k);
            self.get(d - 1, k);
            self.emit(ne);
            self.emit(W::I32Or);
            self.emit(W::Select);
        }
    }
}

fn kind_of_load(n: u8) -> Kind {
    [Kind::Int, Kind::Long, Kind::Float, Kind::Double, Kind::Ref][usize::from(n)]
}

fn int_compare(n: u8) -> W<'static> {
    match n {
        0 => W::I32Eq,
        1 => W::I32Ne,
        2 => W::I32LtS,
        3 => W::I32GeS,
        4 => W::I32GtS,
        _ => W::I32LeS,
    }
}

fn arith(op: u8) -> Option<W<'static>> {
    Some(match op {
        0x60 => W::I32Add,
        0x61 => W::I64Add,
        0x62 => W::F32Add,
        0x63 => W::F64Add,
        0x64 => W::I32Sub,
        0x65 => W::I64Sub,
        0x66 => W::F32Sub,
        0x67 => W::F64Sub,
        0x68 => W::I32Mul,
        0x69 => W::I64Mul,
        0x6A => W::F32Mul,
        0x6B => W::F64Mul,
        0x6E => W::F32Div,
        0x6F => W::F64Div,
        _ => return None,
    })
}

/// The WASM for a conversion, and the kind it makes.
fn conversion(op: u8) -> (Vec<W<'static>>, Kind) {
    use Kind::*;
    match op {
        0x85 => (vec![W::I64ExtendI32S], Long),
        0x86 => (vec![W::F32ConvertI32S], Float),
        0x87 => (vec![W::F64ConvertI32S], Double),
        0x88 => (vec![W::I32WrapI64], Int),
        0x89 => (vec![W::F32ConvertI64S], Float),
        0x8A => (vec![W::F64ConvertI64S], Double),
        // Java's float-to-int conversions saturate and send NaN to 0.
        0x8B => (vec![W::I32TruncSatF32S], Int),
        0x8C => (vec![W::I64TruncSatF32S], Long),
        0x8D => (vec![W::F64PromoteF32], Double),
        0x8E => (vec![W::I32TruncSatF64S], Int),
        0x8F => (vec![W::I64TruncSatF64S], Long),
        0x90 => (vec![W::F32DemoteF64], Float),
        0x91 => (vec![W::I32Extend8S], Int),
        0x92 => (vec![W::I32Const(0xFFFF), W::I32And], Int),
        _ => (vec![W::I32Extend16S], Int),
    }
}

/// How many stack values a dup, pop or swap form looks at.
fn pick(op: u8, stack: &[Kind]) -> Result<usize> {
    let d = stack.len();
    let w = |k: usize| stack.get(d.wrapping_sub(1 + k)).map(|s| s.wide());
    Ok(match op {
        0x59 => 1,
        0x5A => 2,
        0x5B => {
            if w(1) == Some(true) {
                2
            } else {
                3
            }
        }
        0x5C => {
            if w(0) == Some(true) {
                1
            } else {
                2
            }
        }
        0x5D => {
            if w(0) == Some(true) {
                2
            } else {
                3
            }
        }
        0x5E => match (w(0), w(1), w(2)) {
            (Some(true), Some(true), _) => 2,
            (Some(true), _, _) => 3,
            (_, _, Some(true)) => 3,
            _ => 4,
        },
        0x5F => 2,
        _ => return Err("not a stack instruction".into()),
    })
}

/// The values of a dup or swap form after it, as indices into `moved`
/// (the top values before it, lowest first).
fn arrangement(op: u8, moved: &[(usize, Kind)]) -> Result<Vec<(usize, Kind)>> {
    let n = moved.len();
    let v = |i: usize| moved[i];
    Ok(match op {
        // dup: a -> a a
        0x59 => vec![v(n - 1), v(n - 1)],
        // dup_x1: b a -> a b a
        0x5A => vec![v(n - 1), v(n - 2), v(n - 1)],
        // dup_x2: c b a -> a c b a (or b(wide) a -> a b a)
        0x5B => {
            let mut out = vec![v(n - 1)];
            out.extend_from_slice(moved);
            out
        }
        // dup2: b a -> b a b a (or a(wide) -> a a)
        0x5C => {
            let mut out = moved.to_vec();
            out.extend_from_slice(moved);
            out
        }
        // dup2_x1: c b a -> b a c b a (or b a(wide) -> a b a)
        0x5D => {
            if n == 2 {
                vec![v(1), v(0), v(1)]
            } else {
                vec![v(1), v(2), v(0), v(1), v(2)]
            }
        }
        // dup2_x2: the forms by width.
        0x5E => match n {
            2 => vec![v(1), v(0), v(1)],
            3 if moved[2].1.wide() => vec![v(2), v(0), v(1), v(2)],
            3 => vec![v(1), v(2), v(0), v(1), v(2)],
            _ => vec![v(2), v(3), v(0), v(1), v(2), v(3)],
        },
        // swap: b a -> a b
        0x5F => vec![v(1), v(0)],
        _ => return Err("not a stack instruction".into()),
    })
}
