//! The compiler (docs/java-compat/README.md section 6): the classes of one
//! jar become one WASM module.
//!
//! The JVM's operand stack and local variables both become WASM locals: one
//! for each stack depth or variable slot, per kind. Every push and pop is a
//! local set or get (Cranelift keeps them in registers), so values never have
//! to cross a block boundary on WASM's own stack. A method's basic blocks are
//! cases of one dispatch loop: any jump the JVM allows compiles.
//!
//! This first version compiles the arithmetic, the local variables, every
//! branch and switch, static fields and static calls inside the jar, and
//! each class's static initializer, run once before the class's first use.
use crate::{
    ClassFile,
    bytecode::{Instruction, Operand, decode},
    descriptor::{Kind, Method},
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection,
    GlobalSection, GlobalType, Instruction as W, Module, TypeSection, ValType,
};

pub type Result<T> = std::result::Result<T, String>;

fn val(kind: Kind) -> ValType {
    match kind {
        Kind::Int => ValType::I32,
        Kind::Long => ValType::I64,
        Kind::Float => ValType::F32,
        Kind::Double => ValType::F64,
        Kind::Ref => ValType::Ref(wasm_encoder::RefType::ANYREF),
    }
}

/// The export name of a method: `owner.name(descriptor)`.
pub fn export_name(owner: &str, name: &str, descriptor: &str) -> String {
    format!("{owner}.{name}{descriptor}")
}

struct Plan {
    /// (class, method, descriptor) → function index.
    functions: BTreeMap<(String, String, String), u32>,
    /// (class, field) → (global index, kind).
    statics: BTreeMap<(String, String), (u32, Kind)>,
    /// class → (init flag global, its initializer function).
    inits: BTreeMap<String, (u32, Option<u32>)>,
    /// class → the function that runs its initializer once.
    ensure: BTreeMap<String, u32>,
    types: Vec<(Vec<ValType>, Vec<ValType>)>,
}

impl Plan {
    fn type_index(&mut self, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        if let Some(i) = self
            .types
            .iter()
            .position(|t| t.0 == params && t.1 == results)
        {
            return i as u32;
        }
        self.types.push((params, results));
        (self.types.len() - 1) as u32
    }
}

/// A compiled jar.
pub struct Compiled {
    pub wasm: Vec<u8>,
    /// The methods not compiled, with the reason: each one traps when
    /// called (docs/java-compat/plan.md: stub what is missing, and say so).
    pub skipped: Vec<(String, String)>,
}

/// Compiles the classes of one jar into a WASM module.
pub fn compile(classes: &[ClassFile]) -> Result<Compiled> {
    let mut skipped = Vec::new();
    let mut plan = Plan {
        functions: BTreeMap::new(),
        statics: BTreeMap::new(),
        inits: BTreeMap::new(),
        ensure: BTreeMap::new(),
        types: Vec::new(),
    };
    // Functions: every method with code, then one `ensure` per class.
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
    let mut globals: Vec<(Kind, Option<ConstExpr>)> = Vec::new();
    for class in classes {
        let owner = class.name().map_err(|e| e.to_string())?.to_owned();
        for f in &class.fields {
            if f.access & 0x0008 == 0 {
                continue;
            }
            let kind = Kind::of(&f.descriptor).ok_or("a field with a bad descriptor")?;
            plan.statics.insert(
                (owner.clone(), f.name.clone()),
                (globals.len() as u32, kind),
            );
            // A constant (`static final int X = 3`) has its value here, not
            // in the initializer.
            let constant = f.attributes.iter().find_map(|a| match a {
                crate::classfile::Attribute::ConstantValue(i) => {
                    match class.constants.get(usize::from(*i)) {
                        Some(crate::Constant::Integer(v)) => Some(ConstExpr::i32_const(*v)),
                        Some(crate::Constant::Float(v)) => Some(ConstExpr::f32_const((*v).into())),
                        Some(crate::Constant::Long(v)) => Some(ConstExpr::i64_const(*v)),
                        Some(crate::Constant::Double(v)) => Some(ConstExpr::f64_const((*v).into())),
                        _ => None,
                    }
                }
                _ => None,
            });
            globals.push((kind, constant));
        }
        let clinit = plan
            .functions
            .get(&(owner.clone(), "<clinit>".to_owned(), "()V".to_owned()))
            .copied();
        plan.inits
            .insert(owner.clone(), (globals.len() as u32, clinit));
        globals.push((Kind::Int, None));
    }
    let ensure_base = bodies.len() as u32;
    for (i, owner) in plan
        .inits
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .enumerate()
    {
        plan.ensure.insert(owner, ensure_base + i as u32);
    }

    let mut functions = FunctionSection::new();
    let mut code = CodeSection::new();
    let mut exports = ExportSection::new();
    for (class, m) in &bodies {
        let owner = class.name().map_err(|e| e.to_string())?;
        let sig = Method::parse(&m.descriptor).ok_or("a bad method descriptor")?;
        let mut params: Vec<ValType> = Vec::new();
        if m.access & 0x0008 == 0 {
            params.push(val(Kind::Ref));
        }
        params.extend(sig.param_kinds().into_iter().map(val));
        let results: Vec<ValType> = sig.return_kind().map(val).into_iter().collect();
        let ty = plan.type_index(params, results);
        functions.function(ty);
        let body = match MethodCompiler::new(&plan, class, m).and_then(|c| c.compile()) {
            Ok(body) => body,
            Err(e) => {
                skipped.push((export_name(owner, &m.name, &m.descriptor), e));
                let mut f = Function::new([]);
                f.instruction(&W::Unreachable);
                f.instruction(&W::End);
                f
            }
        };
        code.function(&body);
        exports.export(
            &export_name(owner, &m.name, &m.descriptor),
            ExportKind::Func,
            plan.functions[&(owner.to_owned(), m.name.clone(), m.descriptor.clone())],
        );
    }
    // ensure(class): if !flag { flag = 1; <clinit>() }
    let ensure_ty = plan.type_index(Vec::new(), Vec::new());
    for (flag, clinit) in plan.inits.values() {
        functions.function(ensure_ty);
        let mut f = Function::new([]);
        f.instruction(&W::GlobalGet(*flag));
        f.instruction(&W::I32Eqz);
        f.instruction(&W::If(BlockType::Empty));
        f.instruction(&W::I32Const(1));
        f.instruction(&W::GlobalSet(*flag));
        if let Some(c) = clinit {
            f.instruction(&W::Call(*c));
        }
        f.instruction(&W::End);
        f.instruction(&W::End);
        code.function(&f);
    }

    let mut types = TypeSection::new();
    for (params, results) in &plan.types {
        types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
    }
    let mut global_section = GlobalSection::new();
    for (kind, constant) in &globals {
        let init = constant.clone().unwrap_or_else(|| match kind {
            Kind::Int => ConstExpr::i32_const(0),
            Kind::Long => ConstExpr::i64_const(0),
            Kind::Float => ConstExpr::f32_const(0.0.into()),
            Kind::Double => ConstExpr::f64_const(0.0.into()),
            Kind::Ref => ConstExpr::ref_null(wasm_encoder::HeapType::ANY),
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
    let mut module = Module::new();
    module.section(&types);
    module.section(&functions);
    module.section(&global_section);
    module.section(&exports);
    module.section(&code);
    Ok(Compiled {
        wasm: module.finish(),
        skipped,
    })
}

/// The stack kinds before each instruction, by abstract interpretation.
fn stack_kinds(
    class: &ClassFile,
    insns: &[Instruction],
    at: &HashMap<u32, usize>,
) -> Result<Vec<Option<Vec<Kind>>>> {
    let mut before: Vec<Option<Vec<Kind>>> = vec![None; insns.len()];
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

fn pop(s: &mut Vec<Kind>) -> Result<Kind> {
    s.pop().ok_or_else(|| "the stack underflows".to_owned())
}

/// What an instruction does to the stack's kinds.
fn effect(class: &ClassFile, insn: &Instruction, s: &mut Vec<Kind>) -> Result<()> {
    use Kind::*;
    let op = insn.opcode;
    let pops = |s: &mut Vec<Kind>, n: usize| -> Result<()> {
        for _ in 0..n {
            pop(s)?;
        }
        Ok(())
    };
    match op {
        0x00 => {}
        0x01 => s.push(Ref),
        0x02..=0x08 | 0x10 | 0x11 => s.push(Int),
        0x09 | 0x0A => s.push(Long),
        0x0B..=0x0D => s.push(Float),
        0x0E | 0x0F => s.push(Double),
        0x12..=0x14 => {
            let Operand::Constant(i) = insn.operand else {
                unreachable!()
            };
            s.push(match class.constants.get(usize::from(i)) {
                Some(crate::Constant::Integer(_)) => Int,
                Some(crate::Constant::Float(_)) => Float,
                Some(crate::Constant::Long(_)) => Long,
                Some(crate::Constant::Double(_)) => Double,
                _ => Ref,
            })
        }
        0x15 | 0x1A..=0x1D => s.push(Int),
        0x16 | 0x1E..=0x21 => s.push(Long),
        0x17 | 0x22..=0x25 => s.push(Float),
        0x18 | 0x26..=0x29 => s.push(Double),
        0x19 | 0x2A..=0x2D => s.push(Ref),
        0x2E..=0x35 => {
            pops(s, 2)?;
            s.push(match op {
                0x2F => Long,
                0x30 => Float,
                0x31 => Double,
                0x32 => Ref,
                _ => Int,
            })
        }
        0x36..=0x4E => pops(s, 1)?,
        0x4F..=0x56 => pops(s, 3)?,
        0x57 => pops(s, 1)?,
        0x58 => {
            if pop(s)?.wide() {
            } else {
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
        0x60..=0x73 => {
            let k = pop(s)?;
            pop(s)?;
            s.push(k);
        }
        0x74..=0x77 => {}
        0x78..=0x7D => {
            pop(s)?;
        }
        0x7E..=0x83 => {
            let k = pop(s)?;
            pop(s)?;
            s.push(k);
        }
        0x84 => {}
        0x85..=0x93 => {
            pop(s)?;
            s.push(match op {
                0x85 | 0x8C | 0x8F => Long,
                0x86 | 0x89 | 0x90 => Float,
                0x87 | 0x8A | 0x8D => Double,
                _ => Int,
            })
        }
        0x94..=0x98 => {
            pops(s, 2)?;
            s.push(Int)
        }
        0x99..=0x9E | 0xC6 | 0xC7 => pops(s, 1)?,
        0x9F..=0xA6 => pops(s, 2)?,
        0xA7 | 0xC8 => {}
        0xAA | 0xAB => pops(s, 1)?,
        0xAC..=0xB0 => pops(s, 1)?,
        0xB1 => {}
        0xB2 | 0xB3 => {
            let Operand::Constant(i) = insn.operand else {
                unreachable!()
            };
            let (_, _, d) = class.member_ref(i).map_err(|e| e.to_string())?;
            let k = Kind::of(d).ok_or("a bad field descriptor")?;
            if op == 0xB2 {
                s.push(k)
            } else {
                pop(s)?;
            }
        }
        0xB8 => {
            let Operand::Constant(i) = insn.operand else {
                unreachable!()
            };
            let (_, _, d) = class.member_ref(i).map_err(|e| e.to_string())?;
            let m = Method::parse(d).ok_or("a bad method descriptor")?;
            pops(s, m.params.len())?;
            if let Some(k) = m.return_kind() {
                s.push(k);
            }
        }
        _ => {
            return Err(format!("{} is not compiled yet", crate::bytecode::name(op)));
        }
    }
    Ok(())
}

struct MethodCompiler<'a> {
    plan: &'a Plan,
    class: &'a ClassFile,
    owner: String,
    method: &'a crate::classfile::Member,
    insns: Vec<Instruction>,
    /// Local index by (JVM slot or stack depth, kind).
    locals: BTreeMap<(bool, u16, Kind), u32>,
    extra: Vec<ValType>,
    first_local: u32,
    out: Vec<W<'static>>,
}

impl<'a> MethodCompiler<'a> {
    fn new(
        plan: &'a Plan,
        class: &'a ClassFile,
        method: &'a crate::classfile::Member,
    ) -> Result<Self> {
        let code = method.code().ok_or("no code")?;
        let insns = decode(&code.code).map_err(|e| e.to_string())?;
        let sig = Method::parse(&method.descriptor).ok_or("a bad method descriptor")?;
        let mut locals = BTreeMap::new();
        let mut slot = 0u16;
        let mut index = 0u32;
        if method.access & 0x0008 == 0 {
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

    fn compile(mut self) -> Result<Function> {
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
            let ends = !matches!(
                insn.operand,
                Operand::None
                    | Operand::Value(_)
                    | Operand::Local(_)
                    | Operand::Increment(..)
                    | Operand::Constant(_)
                    | Operand::Interface(..)
                    | Operand::Dimensions(..)
                    | Operand::ArrayType(_)
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
        if self.method.access & 0x0008 != 0 && self.method.name != "<clinit>" {
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
                let last = i + 1 == end;
                self.instruction(&insn, &stack, &block_of, &at, loop_depth, last)?;
            }
            // Falling through to the next block needs nothing: its code
            // follows the end of its block.
        }
        self.emit(W::End);
        self.emit(W::Unreachable);
        self.emit(W::End);
        let mut f = Function::new_with_locals_types(self.extra.iter().copied());
        for i in &self.out {
            f.instruction(i);
        }
        Ok(f)
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
        stack: &[Kind],
        block_of: &HashMap<usize, usize>,
        at: &HashMap<u32, usize>,
        loop_depth: u32,
        _last: bool,
    ) -> Result<()> {
        use Kind::*;
        let d = stack.len();
        let op = insn.opcode;
        let top = |k: usize| stack[d - 1 - k];
        let target_of = |pc: u32| at.get(&pc).copied().ok_or("a bad branch target".to_owned());
        match op {
            0x00 => {}
            0x01 => {
                self.emit(W::RefNull(wasm_encoder::HeapType::ANY));
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
                    Some(crate::Constant::Integer(v)) => (W::I32Const(*v), Int),
                    Some(crate::Constant::Float(v)) => (W::F32Const((*v).into()), Float),
                    Some(crate::Constant::Long(v)) => (W::I64Const(*v), Long),
                    Some(crate::Constant::Double(v)) => (W::F64Const((*v).into()), Double),
                    _ => return Err("ldc of a string or class is not compiled yet".into()),
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
            0x57 => {}
            0x58 => {}
            0x59..=0x5F => {
                // Rearrange the stack locals as the kinds pass computed.
                // The values the instruction moves: the top of the stack.
                let low = d - pick(op, stack)?;
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
                        return Err("reference comparisons are not compiled yet".into());
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
                let Some(&(g, _)) = self.plan.statics.get(&(owner.to_owned(), name.to_owned()))
                else {
                    return Err(format!(
                        "the static field {owner}.{name} is outside the jar"
                    ));
                };
                self.ensure(owner);
                if op == 0xB2 {
                    self.emit(W::GlobalGet(g));
                    self.set(d, k);
                } else {
                    self.get(d - 1, k);
                    self.emit(W::GlobalSet(g));
                }
            }
            0xB8 => {
                let Operand::Constant(i) = insn.operand else {
                    unreachable!()
                };
                let (owner, name, desc) = self.class.member_ref(i).map_err(|e| e.to_string())?;
                let key = (owner.to_owned(), name.to_owned(), desc.to_owned());
                let Some(&f) = self.plan.functions.get(&key) else {
                    return Err(format!(
                        "the method {owner}.{name}{desc} is outside the jar"
                    ));
                };
                let m = Method::parse(desc).ok_or("a bad method descriptor")?;
                let kinds = m.param_kinds();
                let base = d - kinds.len();
                for (j, k) in kinds.iter().enumerate() {
                    self.get(base + j, *k);
                }
                self.ensure(owner);
                self.emit(W::Call(f));
                if let Some(k) = m.return_kind() {
                    self.set(base, k);
                }
            }
            _ => {
                return Err(format!("{} is not compiled yet", crate::bytecode::name(op)));
            }
        }
        Ok(())
    }

    fn ensure(&mut self, owner: &str) {
        if owner == self.owner && self.method.name == "<clinit>" {
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
