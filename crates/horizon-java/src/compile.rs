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
//! field 0 is its class id, then the instance fields, the superclass's first.
//! A lambda is a struct too: its site's id, then the captured values. Every
//! other object belongs to the host (the game and the JDK, given by the
//! compatibility layer) and is an `externref` converted to `anyref`.
//!
//! Whatever is outside the jar is an import of module `host`, named
//! `<operation> <owner>.<member> <descriptor>`: `invokestatic`,
//! `invokevirtual`, `invokeinterface`, `new` (a constructor: the arguments
//! in, the object out), `getstatic`, `putstatic`, `getfield` and
//! `putfield`. Two more: `constant` (an `ldc` string or class, by index into
//! [`Compiled::constants`]) and `same` (`==` on references).
//!
//! Each method is exported as `<class>.<name><descriptor>`, each lambda as
//! `lambda <n>` and each class's allocator as `new <class>`.
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
/// The struct type all objects share: just the class id.
const OBJECT: u32 = 0;

fn val(kind: Kind) -> ValType {
    match kind {
        Kind::Int => ValType::I32,
        Kind::Long => ValType::I64,
        Kind::Float => ValType::F32,
        Kind::Double => ValType::F64,
        Kind::Ref => ANY,
    }
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
    /// A struct type: its supertype, and its fields after the class id.
    Struct(u32, Vec<(ValType, bool)>),
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
        if matches!(op, "invokevirtual" | "invokeinterface") {
            params.push(ANY);
        }
        params.extend(m.param_kinds().into_iter().map(val));
        let results = if op == "new" {
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

    /// Whether a class of the jar below `declaring` overrides the method.
    fn overridden(&self, declaring: &str, name: &str, desc: &str) -> bool {
        self.functions.keys().any(|(c, n, d)| {
            n == name && d == desc && c != declaring && {
                let mut s = self.superclass(c);
                let mut below = false;
                while let Some(x) = s {
                    if x == declaring {
                        below = true;
                        break;
                    }
                    s = self.superclass(x);
                }
                below
            }
        })
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
            Call::Static if in_jar => {
                let (declaring, f) = self
                    .resolve(owner, name, desc)
                    .ok_or_else(|| format!("{owner}.{name}{desc} has no code"))?;
                if within != Some(declaring.as_str()) {
                    out.push(W::Call(self.ensure[&declaring]));
                }
                out.push(W::Call(f));
            }
            Call::Static => out.push(W::Call(self.host_method(
                "invokestatic",
                owner,
                name,
                desc,
            )?)),
            Call::Virtual | Call::Interface | Call::Special if in_jar => {
                match self.resolve(owner, name, desc) {
                    Some((declaring, f)) => {
                        if call != Call::Special && self.overridden(&declaring, name, desc) {
                            return Err(format!(
                                "{owner}.{name}{desc} is overridden: virtual calls between \
                                 classes of the jar are not compiled yet"
                            ));
                        }
                        out.push(W::Call(f));
                    }
                    // Inherited from the host (java/lang/Object's methods).
                    None if call != Call::Special => {
                        out.push(W::Call(self.host_method(
                            "invokevirtual",
                            owner,
                            name,
                            desc,
                        )?));
                    }
                    None => return Err(format!("{owner}.{name}{desc} has no code")),
                }
            }
            Call::Special if owner == "java/lang/Object" && name == "<init>" => {
                // The superclass constructor of a class that extends Object.
                out.push(W::Drop);
            }
            Call::Special => {
                return Err(format!(
                    "{owner}.{name}{desc}: classes that extend game classes are not compiled yet"
                ));
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
        if let Some(s) = self.superclass(class)
            && s != "java/lang/Object"
            && !self.classes.contains_key(s)
        {
            return Err(format!(
                "{class} extends the game class {s}: not compiled yet"
            ));
        }
        out.push(W::I32Const(self.ids[class] as i32));
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
                return Some((*ty, i as u32 + 1));
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
        types: RefCell::new(vec![TypeDef::Struct(u32::MAX, Vec::new())]),
        imports: RefCell::new(Vec::new()),
        globals: RefCell::new(Vec::new()),
        constants: RefCell::new(Vec::new()),
        sites: RefCell::new(Vec::new()),
    };
    let mut names = Vec::new();
    for class in classes {
        let name = class.name().map_err(|e| e.to_string())?.to_owned();
        plan.ids.insert(name.clone(), names.len() as u32 + 1);
        plan.classes.insert(name.clone(), class);
        names.push(name);
    }
    // Functions: every method with code, then for each class its `ensure`
    // and its allocator, then the lambdas.
    let mut bodies = Vec::new();
    for class in classes {
        let owner = class.name().map_err(|e| e.to_string())?.to_owned();
        for m in &class.methods {
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
    // The lambdas: closure and arguments in, the target method called.
    let sites = plan.sites.borrow().len();
    for n in 0..sites {
        let (ty, locals, body) = lambda(&plan, n).map_err(|e| format!("lambda {n}: {e}"))?;
        exports.push((format!("lambda {n}"), functions.len() as u32));
        functions.push((ty, locals, body));
    }

    // Now the imports are known: they come first.
    let imports = plan.imports.borrow();
    let shift = imports.len() as u32;
    let fix = |i: u32| {
        if i & IMPORT != 0 {
            i & !IMPORT
        } else {
            i + shift
        }
    };
    let mut types = TypeSection::new();
    for t in plan.types.borrow().iter() {
        match t {
            TypeDef::Func(params, results) => {
                types
                    .ty()
                    .function(params.iter().copied(), results.iter().copied());
            }
            TypeDef::Struct(supertype, fields) => {
                let mut all = vec![FieldType {
                    element_type: StorageType::Val(ValType::I32),
                    mutable: false,
                }];
                all.extend(fields.iter().map(|(v, mutable)| FieldType {
                    element_type: StorageType::Val(*v),
                    mutable: *mutable,
                }));
                types.ty().subtype(&SubType {
                    is_final: false,
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
    module.section(&global_section);
    module.section(&export_section);
    module.section(&code);
    Ok(Compiled {
        wasm: module.finish(),
        skipped,
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
/// as LambdaMetafactory does: boxing and unboxing.
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
        _ => return Err(format!("converting {from} to {to} is not compiled yet")),
    }
    Ok(())
}

/// The function behind lambda site `n`: (closure, interface arguments) →
/// the interface method's result.
fn lambda(plan: &Plan, n: usize) -> Result<(u32, Vec<ValType>, Vec<W<'static>>)> {
    let sites = plan.sites.borrow();
    let site = &sites[n].1;
    let sam = Method::parse(&site.sam).ok_or("a bad descriptor")?;
    let mut params = vec![ANY];
    params.extend(sam.param_kinds().into_iter().map(val));
    let results: Vec<ValType> = sam.return_kind().map(val).into_iter().collect();
    let ty = plan.type_index(params.clone(), results);

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
    let object = params.len() as u32;
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
                field_index: f,
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
}

impl From<Kind> for Slot {
    fn from(kind: Kind) -> Slot {
        Slot { kind, new: None }
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
) -> Result<Vec<Option<Vec<Slot>>>> {
    let mut before: Vec<Option<Vec<Slot>>> = vec![None; insns.len()];
    let mut work = vec![(0usize, Vec::new())];
    while let Some((i, stack)) = work.pop() {
        if i >= insns.len() {
            continue;
        }
        if let Some(seen) = &before[i] {
            if *seen != stack {
                return Err(format!("stack kinds differ at {}", insns[i].at));
            }
            continue;
        }
        before[i] = Some(stack.clone());
        let insn = &insns[i];
        let mut s = stack;
        effect(class, insn, &mut s)?;
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
        0xBB => s.push(Slot {
            kind: Ref,
            new: Some(insn.at),
        }),
        0xC0 => {}
        _ => {
            return Err(format!("{} is not compiled yet", crate::bytecode::name(op)));
        }
    }
    Ok(())
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
}

impl<'a> MethodCompiler<'a> {
    fn new(plan: &'a Plan<'a>, class: &'a ClassFile, method: &'a Member) -> Result<Self> {
        let code = method.code().ok_or("no code")?;
        if !code.handlers.is_empty() {
            return Err("exception handlers are not compiled yet".into());
        }
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
        let before = stack_kinds(self.class, &self.insns, &at)?;
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
        let blocks: Vec<usize> = leaders.into_iter().collect();
        let block_of: HashMap<usize, usize> =
            blocks.iter().enumerate().map(|(b, &i)| (i, b)).collect();
        let n = blocks.len();
        let label = self.first_local;
        self.extra.push(ValType::I32);
        self.locals.insert((false, u16::MAX, Kind::Int), label);

        // A static method makes sure its class is initialized (the host may
        // call it first), as the JVM does on invokestatic.
        if self.method.access & STATIC != 0 && self.method.name != "<clinit>" {
            let e = self.plan.ensure[&self.owner];
            self.emit(W::Call(e));
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
            for i in start..end {
                let stack = before[i].clone().expect("reachable within a block");
                let insn = self.insns[i].clone();
                self.instruction(&insn, &stack, &block_of, &at, loop_depth)?;
            }
            // Falling through to the next block needs nothing: its code
            // follows the end of its block.
        }
        self.emit(W::End);
        self.emit(W::Unreachable);
        self.emit(W::End);
        Ok((self.extra, self.out))
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
                        let same =
                            self.plan
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
                match self.plan.statics.get(&(owner.to_owned(), name.to_owned())) {
                    Some(&(g, _)) => {
                        self.ensure(owner);
                        if op == 0xB2 {
                            self.emit(W::GlobalGet(g));
                            self.set(d, k);
                        } else {
                            self.get(d - 1, k);
                            self.emit(W::GlobalSet(g));
                        }
                    }
                    None if self.plan.classes.contains_key(owner) => {
                        return Err(format!("the static field {owner}.{name} is inherited"));
                    }
                    None if op == 0xB2 => {
                        let f = self.plan.host_field("getstatic", owner, name, k, desc);
                        self.emit(W::Call(f));
                        self.set(d, k);
                    }
                    None => {
                        let f = self.plan.host_field("putstatic", owner, name, k, desc);
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
                self.get(object, Ref);
                match self.plan.field(owner, name) {
                    Some((ty, field)) => {
                        self.emit(W::RefCastNullable(HeapType::Concrete(ty)));
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
                    None if self.plan.classes.contains_key(owner) => {
                        return Err(format!("the field {owner}.{name} is the game's"));
                    }
                    None if op == 0xB4 => {
                        let f = self.plan.host_field("getfield", owner, name, k, desc);
                        self.emit(W::Call(f));
                        self.set(d - 1, k);
                    }
                    None => {
                        let f = self.plan.host_field("putfield", owner, name, k, desc);
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
                // A game class's constructor on an object from `new`: the
                // host makes the object, which replaces every copy.
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
            _ => {
                return Err(format!("{} is not compiled yet", crate::bytecode::name(op)));
            }
        }
        Ok(())
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
        if bo != "java/lang/invoke/LambdaMetafactory" || bn != "metafactory" {
            return Err(format!(
                "invokedynamic through {bo}.{bn} is not compiled yet"
            ));
        }
        let [sam, target, _] = bootstrap.arguments[..] else {
            return Err("a bad lambda".into());
        };
        let Some(Constant::MethodType(sam)) = self.class.constants.get(usize::from(sam)) else {
            return Err("a bad lambda".into());
        };
        let sam = self.class.utf8(*sam).map_err(e)?.to_owned();
        let target = handle(target)?;
        let site_desc = Method::parse(desc).ok_or("a bad descriptor")?;
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
        let base = d - captures.len();
        self.emit(W::I32Const(id as i32));
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
