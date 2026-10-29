//! The type of every local and stack slot before each instruction of a
//! method, as the JVM verifier finds them (JVMS 4.10.2, type inference): the
//! typed front end of the compiler (docs/java-compat/compiler.md, step 1).
//!
//! At a merge point two types join by the class hierarchy; two classes that
//! are not related, or an interface, join to `java/lang/Object`, as the
//! verifier does. This works for class files of every version. A class file
//! from Java 7 on also has a `StackMapTable`: [`check`] tests the inferred
//! types against it.
use crate::{
    bytecode::{Instruction, Operand},
    classfile::{Attribute, ClassFile, Constant, Handler, Member},
    descriptor::Method,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

const OBJECT: &str = "java/lang/Object";
const STATIC: u16 = 0x0008;

/// A verification type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    /// Unusable: no value, or the second slot of a `long` or `double` local.
    Top,
    Int,
    Float,
    Long,
    Double,
    Null,
    /// A reference: a class's internal name (`java/lang/String`) or an array
    /// descriptor (`[I`, `[Ljava/lang/String;`).
    Ref(Arc<str>),
    /// An object from `new` at this position, not yet constructed.
    Uninit(u32),
    /// `this` in a constructor before the superclass constructor runs.
    UninitThis,
    /// One object, followed for escape analysis ([`Options::sites`]): the
    /// object of the `new` at this position (or of [`THIS_SITE`]), and its
    /// class. It joins with anything else to a plain reference.
    Site(u32, Arc<str>),
}

/// The site of a method's own object, with [`Options::this_site`].
pub const THIS_SITE: u32 = u32::MAX;

/// What [`infer_with`] follows besides the verifier's types.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Constructed objects keep their `new` site ([`Ty::Site`]).
    pub sites: bool,
    /// The method's own object is [`Ty::Site`]`(`[`THIS_SITE`]`, class)`.
    pub this_site: bool,
}

impl Ty {
    pub fn wide(&self) -> bool {
        matches!(self, Ty::Long | Ty::Double)
    }
    pub fn class(name: &str) -> Ty {
        Ty::Ref(name.into())
    }
    /// A followed object as the plain reference it is.
    pub fn plain(&self) -> Ty {
        match self {
            Ty::Site(_, c) => Ty::Ref(c.clone()),
            t => t.clone(),
        }
    }
    /// The type of a field descriptor: `I` and the other small ints are
    /// `Int`; a class or array is its reference.
    pub fn of_descriptor(d: &str) -> Option<Ty> {
        Some(match d.as_bytes().first()? {
            b'Z' | b'B' | b'C' | b'S' | b'I' => Ty::Int,
            b'J' => Ty::Long,
            b'F' => Ty::Float,
            b'D' => Ty::Double,
            b'L' => Ty::class(d.strip_prefix('L')?.strip_suffix(';')?),
            b'[' => Ty::Ref(d.into()),
            _ => return None,
        })
    }
    fn is_reference(&self) -> bool {
        matches!(
            self,
            Ty::Null | Ty::Ref(_) | Ty::Uninit(_) | Ty::UninitThis | Ty::Site(..)
        )
    }
}

/// The element type of an array type (`[I` → int, `[[I` → `[I`).
fn component(array: &str) -> Option<Ty> {
    Ty::of_descriptor(array.strip_prefix('[')?)
}

/// The array type of elements of type `element`.
fn array_of(element: &Ty) -> Ty {
    match element {
        Ty::Ref(n) if n.starts_with('[') => Ty::Ref(format!("[{n}").into()),
        Ty::Ref(n) => Ty::Ref(format!("[L{n};").into()),
        Ty::Int => Ty::class("[I"),
        Ty::Long => Ty::class("[J"),
        Ty::Float => Ty::class("[F"),
        Ty::Double => Ty::class("[D"),
        _ => Ty::class("[Ljava/lang/Object;"),
    }
}

/// What the inference knows about classes: their superclass and whether
/// they are interfaces. A class it does not know joins to `Object`.
pub trait Hierarchy {
    fn superclass(&self, class: &str) -> Option<&str>;
    fn is_interface(&self, class: &str) -> bool;
    /// Whether it has the class at all.
    fn knows(&self, class: &str) -> bool;
}

/// The hierarchy of a set of class files.
#[derive(Default)]
pub struct Classes {
    supers: HashMap<String, (Option<String>, bool)>,
}

impl Classes {
    pub fn new<'a>(classes: impl IntoIterator<Item = &'a ClassFile>) -> Classes {
        let mut supers = HashMap::new();
        for c in classes {
            if let Ok(name) = c.name() {
                let sup = c.super_name().ok().flatten().map(str::to_owned);
                supers.insert(name.to_owned(), (sup, c.access & 0x0200 != 0));
            }
        }
        Classes { supers }
    }
}

impl Hierarchy for Classes {
    fn superclass(&self, class: &str) -> Option<&str> {
        self.supers.get(class)?.0.as_deref()
    }
    fn is_interface(&self, class: &str) -> bool {
        self.supers.get(class).is_some_and(|s| s.1)
    }
    fn knows(&self, class: &str) -> bool {
        self.supers.contains_key(class)
    }
}

/// Whether `class` is `target` or extends it, as far as `h` knows: `None`
/// when a class on the way is not known.
pub fn extends(h: &dyn Hierarchy, class: &str, target: &str) -> Option<bool> {
    let mut c = class;
    for _ in 0..256 {
        if c == target {
            return Some(true);
        }
        if c == OBJECT {
            return Some(false);
        }
        c = h.superclass(c)?;
    }
    None
}

/// The superclasses of `class`, itself first, as far as `h` knows.
fn chain<'a>(h: &'a dyn Hierarchy, class: &'a str) -> Vec<&'a str> {
    let mut out = vec![class];
    let mut c = class;
    while let Some(s) = h.superclass(c) {
        if out.len() > 256 {
            break;
        }
        out.push(s);
        c = s;
    }
    out
}

/// The least common superclass of two reference types.
fn join_refs(h: &dyn Hierarchy, a: &str, b: &str) -> Arc<str> {
    if a == b {
        return a.into();
    }
    match (a.strip_prefix('['), b.strip_prefix('[')) {
        (Some(x), Some(y)) => {
            // Arrays of references join to the array of their elements'
            // join; any other pair of arrays to Object.
            match (Ty::of_descriptor(x), Ty::of_descriptor(y)) {
                (Some(Ty::Ref(p)), Some(Ty::Ref(q))) => {
                    match array_of(&Ty::Ref(join_refs(h, &p, &q))) {
                        Ty::Ref(n) => n,
                        _ => OBJECT.into(),
                    }
                }
                _ => OBJECT.into(),
            }
        }
        (None, None) if h.is_interface(a) || h.is_interface(b) => OBJECT.into(),
        (None, None) => {
            let ca = chain(h, a);
            chain(h, b)
                .into_iter()
                .find(|c| ca.contains(c))
                .unwrap_or(OBJECT)
                .into()
        }
        _ => OBJECT.into(),
    }
}

/// Two types at a merge point; `None` when they cannot share a stack slot.
fn join(h: &dyn Hierarchy, a: &Ty, b: &Ty) -> Option<Ty> {
    if a != b && (matches!(a, Ty::Site(..)) || matches!(b, Ty::Site(..))) {
        return join(h, &a.plain(), &b.plain());
    }
    Some(match (a, b) {
        _ if a == b => a.clone(),
        (Ty::Null, Ty::Ref(_)) => b.clone(),
        (Ty::Ref(_), Ty::Null) => a.clone(),
        (Ty::Ref(x), Ty::Ref(y)) => Ty::Ref(join_refs(h, x, y)),
        _ => return None,
    })
}

/// The locals (a `long` or `double` takes two, the second `Top`) and the
/// stack (one entry per value) before an instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub locals: Vec<Ty>,
    pub stack: Vec<Ty>,
}

impl Frame {
    fn local(&self, n: u16) -> Ty {
        self.locals.get(usize::from(n)).cloned().unwrap_or(Ty::Top)
    }
    fn store(&mut self, n: u16, t: Ty) {
        let n = usize::from(n);
        let need = n + if t.wide() { 2 } else { 1 };
        if self.locals.len() < need {
            self.locals.resize(need, Ty::Top);
        }
        // Writing the second half of a long or double kills it.
        if n > 0 && self.locals[n - 1].wide() {
            self.locals[n - 1] = Ty::Top;
        }
        let wide = t.wide();
        self.locals[n] = t;
        if wide {
            self.locals[n + 1] = Ty::Top;
        }
    }
    fn pop(&mut self) -> Result<Ty, String> {
        self.stack
            .pop()
            .ok_or_else(|| "pop of an empty stack".into())
    }
    fn pops(&mut self, n: usize) -> Result<(), String> {
        for _ in 0..n {
            self.pop()?;
        }
        Ok(())
    }
    fn push(&mut self, t: Ty) {
        self.stack.push(t);
    }
    /// Joins `other` into this frame; whether it changed.
    fn merge(&mut self, h: &dyn Hierarchy, other: &Frame) -> Result<bool, String> {
        if self.stack.len() != other.stack.len() {
            return Err("stack heights differ at a merge point".into());
        }
        let mut changed = false;
        for (a, b) in self.stack.iter_mut().zip(&other.stack) {
            let j = join(h, a, b).ok_or("stack types differ at a merge point")?;
            changed |= *a != j;
            *a = j;
        }
        let n = self.locals.len().max(other.locals.len());
        self.locals.resize(n, Ty::Top);
        for i in 0..n {
            let b = other.locals.get(i).cloned().unwrap_or(Ty::Top);
            let j = join(h, &self.locals[i], &b).unwrap_or(Ty::Top);
            changed |= self.locals[i] != j;
            self.locals[i] = j;
        }
        // A long whose second half is gone is gone.
        for i in 0..n {
            if self.locals[i].wide() && self.locals.get(i + 1) != Some(&Ty::Top) {
                self.locals[i] = Ty::Top;
                changed = true;
            }
        }
        Ok(changed)
    }
}

/// The frame on entry to `method` of `class`.
pub fn entry(class: &ClassFile, method: &Member) -> Result<Frame, String> {
    entry_with(class, method, &Options::default())
}

fn entry_with(class: &ClassFile, method: &Member, opts: &Options) -> Result<Frame, String> {
    let owner = class.name().map_err(|e| e.to_string())?;
    let sig = Method::parse(&method.descriptor).ok_or("a bad method descriptor")?;
    let mut f = Frame {
        locals: Vec::new(),
        stack: Vec::new(),
    };
    let mut n = 0u16;
    if method.access & STATIC == 0 {
        let this = if method.name == "<init>" && owner != OBJECT {
            Ty::UninitThis
        } else if opts.this_site {
            Ty::Site(THIS_SITE, owner.into())
        } else {
            Ty::class(owner)
        };
        f.store(0, this);
        n = 1;
    }
    for p in &sig.params {
        let t = Ty::of_descriptor(p).ok_or("a bad parameter descriptor")?;
        let w = t.wide();
        f.store(n, t);
        n += if w { 2 } else { 1 };
    }
    Ok(f)
}

/// The type of a loadable constant.
fn constant_type(class: &ClassFile, index: u16) -> Result<Ty, String> {
    Ok(match class.constants.get(usize::from(index)) {
        Some(Constant::Integer(_)) => Ty::Int,
        Some(Constant::Float(_)) => Ty::Float,
        Some(Constant::Long(_)) => Ty::Long,
        Some(Constant::Double(_)) => Ty::Double,
        Some(Constant::String(_)) => Ty::class("java/lang/String"),
        Some(Constant::Class(_)) => Ty::class("java/lang/Class"),
        Some(Constant::MethodType(_)) => Ty::class("java/lang/invoke/MethodType"),
        Some(Constant::MethodHandle(..)) => Ty::class("java/lang/invoke/MethodHandle"),
        Some(Constant::Dynamic(_, nat)) => {
            let (_, d) = class.name_and_type(*nat).map_err(|e| e.to_string())?;
            Ty::of_descriptor(d).ok_or("a bad dynamic constant type")?
        }
        _ => return Err(format!("ldc of constant {index}")),
    })
}

/// The local an instruction loads or stores: its operand, or the `_n` in its
/// opcode.
fn local_of(insn: &Instruction, first: u8) -> u16 {
    match insn.operand {
        Operand::Local(n) => n,
        _ => u16::from((insn.opcode - first) % 4),
    }
}

/// `insn`'s effect on `f`.
fn effect(
    class: &ClassFile,
    news: &HashMap<u32, String>,
    opts: &Options,
    insn: &Instruction,
    f: &mut Frame,
) -> Result<(), String> {
    use Ty::*;
    let op = insn.opcode;
    let me = |e: crate::classfile::Error| e.to_string();
    match op {
        0x00 => {}
        0x01 => f.push(Null),
        0x02..=0x08 | 0x10 | 0x11 => f.push(Int),
        0x09 | 0x0A => f.push(Long),
        0x0B..=0x0D => f.push(Float),
        0x0E | 0x0F => f.push(Double),
        0x12..=0x14 => {
            let Operand::Constant(i) = insn.operand else {
                return Err("ldc without a constant".into());
            };
            f.push(constant_type(class, i)?);
        }
        0x15 | 0x1A..=0x1D => f.push(Int),
        0x16 | 0x1E..=0x21 => f.push(Long),
        0x17 | 0x22..=0x25 => f.push(Float),
        0x18 | 0x26..=0x29 => f.push(Double),
        0x19 => f.push(f.local(local_of(insn, 0))),
        0x2A..=0x2D => f.push(f.local(local_of(insn, 0x2A))),
        0x2E..=0x35 => {
            f.pop()?;
            let array = f.pop()?;
            f.push(match op {
                0x2F => Long,
                0x30 => Float,
                0x31 => Double,
                0x32 => match &array {
                    Ref(a) => component(a).unwrap_or(Null),
                    _ => Null,
                },
                _ => Int,
            });
        }
        0x36..=0x3A => {
            let t = f.pop()?;
            f.store(local_of(insn, 0), t);
        }
        0x3B..=0x4E => {
            let t = f.pop()?;
            f.store(local_of(insn, 0x3B), t);
        }
        0x4F..=0x56 => f.pops(3)?,
        0x57 => {
            f.pop()?;
        }
        0x58 => {
            if !f.pop()?.wide() {
                f.pop()?;
            }
        }
        0x59..=0x5F => stack_op(op, f)?,
        0x60..=0x73 => {
            f.pop()?;
            let t = f.pop()?;
            f.push(t);
        }
        0x74..=0x77 => {}
        0x78..=0x7D => {
            // Shifts: an int count.
            f.pop()?;
        }
        0x7E..=0x83 => {
            f.pop()?;
        }
        0x84 => {}
        0x85..=0x93 => {
            f.pop()?;
            f.push(match op {
                0x85 | 0x8C | 0x8F => Long,
                0x86 | 0x89 | 0x90 => Float,
                0x87 | 0x8A | 0x8D => Double,
                _ => Int,
            });
        }
        0x94..=0x98 => {
            f.pops(2)?;
            f.push(Int);
        }
        0x99..=0x9E | 0xC6 | 0xC7 => {
            f.pop()?;
        }
        0x9F..=0xA6 => f.pops(2)?,
        0xA7 | 0xC8 => {}
        0xA8 | 0xA9 | 0xC9 => return Err("jsr/ret (subroutines)".into()),
        0xAA | 0xAB => {
            f.pop()?;
        }
        0xAC..=0xB0 => {
            f.pop()?;
        }
        0xB1 => {}
        0xB2..=0xB5 => {
            let Operand::Constant(i) = insn.operand else {
                return Err("a field instruction without a constant".into());
            };
            let (_, _, d) = class.member_ref(i).map_err(me)?;
            let t = Ty::of_descriptor(d).ok_or("a bad field descriptor")?;
            match op {
                0xB2 => f.push(t),
                0xB3 => {
                    f.pop()?;
                }
                0xB4 => {
                    f.pop()?;
                    f.push(t);
                }
                _ => f.pops(2)?,
            }
        }
        0xB6..=0xBA => {
            let (_, name, desc) = match insn.operand {
                Operand::Constant(i) | Operand::Interface(i, _) if op != 0xBA => {
                    class.member_ref(i).map_err(me)?
                }
                Operand::Constant(i) => {
                    let Some(Constant::InvokeDynamic(_, nat)) = class.constants.get(usize::from(i))
                    else {
                        return Err("invokedynamic without its constant".into());
                    };
                    let (n, d) = class.name_and_type(*nat).map_err(me)?;
                    ("", n, d)
                }
                _ => return Err("an invoke without a constant".into()),
            };
            let m = Method::parse(desc).ok_or("a bad method descriptor")?;
            f.pops(m.params.len())?;
            if op != 0xB8 && op != 0xBA {
                let receiver = f.pop()?;
                if name == "<init>" {
                    // The object is constructed: every copy of it now has its
                    // class.
                    let made = match &receiver {
                        Uninit(at) => {
                            let Some(new) = news.get(at) else {
                                return Err("a constructor on an unknown new".into());
                            };
                            if opts.sites {
                                Site(*at, new.as_str().into())
                            } else {
                                Ty::class(new)
                            }
                        }
                        UninitThis if opts.this_site => {
                            Site(THIS_SITE, class.name().map_err(me)?.into())
                        }
                        UninitThis => Ty::class(class.name().map_err(me)?),
                        _ => receiver.clone(),
                    };
                    for t in f.locals.iter_mut().chain(f.stack.iter_mut()) {
                        if *t == receiver {
                            *t = made.clone();
                        }
                    }
                }
            }
            if m.ret != "V" {
                f.push(Ty::of_descriptor(&m.ret).ok_or("a bad return descriptor")?);
            }
        }
        0xBB => f.push(Uninit(insn.at)),
        0xBC => {
            f.pop()?;
            let Operand::ArrayType(t) = insn.operand else {
                return Err("newarray without a type".into());
            };
            let d = match t {
                4 => "[Z",
                5 => "[C",
                6 => "[F",
                7 => "[D",
                8 => "[B",
                9 => "[S",
                10 => "[I",
                11 => "[J",
                _ => return Err("newarray of a bad type".into()),
            };
            f.push(Ty::class(d));
        }
        0xBD => {
            f.pop()?;
            let Operand::Constant(i) = insn.operand else {
                return Err("anewarray without a class".into());
            };
            let c = class.class_name(i).map_err(me)?;
            let element = if c.starts_with('[') {
                Ty::Ref(c.into())
            } else {
                Ty::class(c)
            };
            f.push(array_of(&element));
        }
        0xBE => {
            f.pop()?;
            f.push(Int);
        }
        0xBF => {
            f.pop()?;
        }
        0xC0 => {
            let t = f.pop()?;
            let Operand::Constant(i) = insn.operand else {
                return Err("checkcast without a class".into());
            };
            let c = class.class_name(i).map_err(me)?;
            f.push(match t {
                Null => Null,
                // A cast does not change which object it is.
                Site(..) => t,
                _ => Ty::class(c),
            });
        }
        0xC1 => {
            f.pop()?;
            f.push(Int);
        }
        0xC2 | 0xC3 => {
            f.pop()?;
        }
        0xC5 => {
            let Operand::Dimensions(i, dims) = insn.operand else {
                return Err("multianewarray without dimensions".into());
            };
            f.pops(usize::from(dims))?;
            f.push(Ty::class(class.class_name(i).map_err(me)?));
        }
        _ => return Err(format!("opcode {op:#x}")),
    }
    Ok(())
}

/// The `dup`, `dup_x1`, `dup_x2`, `dup2`, `dup2_x1`, `dup2_x2` and `swap`
/// forms, by value category.
fn stack_op(op: u8, f: &mut Frame) -> Result<(), String> {
    let a = f.pop()?;
    match op {
        0x59 => {
            f.push(a.clone());
            f.push(a);
        }
        0x5A => {
            let b = f.pop()?;
            f.push(a.clone());
            f.push(b);
            f.push(a);
        }
        0x5B => {
            let b = f.pop()?;
            if b.wide() {
                f.push(a.clone());
                f.push(b);
                f.push(a);
            } else {
                let c = f.pop()?;
                f.push(a.clone());
                f.push(c);
                f.push(b);
                f.push(a);
            }
        }
        0x5C => {
            if a.wide() {
                f.push(a.clone());
                f.push(a);
            } else {
                let b = f.pop()?;
                f.push(b.clone());
                f.push(a.clone());
                f.push(b);
                f.push(a);
            }
        }
        0x5D => {
            if a.wide() {
                let b = f.pop()?;
                f.push(a.clone());
                f.push(b);
                f.push(a);
            } else {
                let b = f.pop()?;
                let c = f.pop()?;
                f.push(b.clone());
                f.push(a.clone());
                f.push(c);
                f.push(b);
                f.push(a);
            }
        }
        0x5E => {
            // Four forms: the top one or two values over one or two.
            let top: Vec<Ty> = if a.wide() {
                vec![a]
            } else {
                let b = f.pop()?;
                vec![b, a]
            };
            let c = f.pop()?;
            let under: Vec<Ty> = if c.wide() {
                vec![c]
            } else {
                let d = f.pop()?;
                vec![d, c]
            };
            for t in top.iter().chain(&under).chain(&top) {
                f.push(t.clone());
            }
        }
        0x5F => {
            let b = f.pop()?;
            f.push(a);
            f.push(b);
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// The frame after one instruction, for passes that follow values across
/// edges (escape analysis).
pub struct Stepper {
    news: HashMap<u32, String>,
}

impl Stepper {
    pub fn new(class: &ClassFile, insns: &[Instruction]) -> Stepper {
        let news = insns
            .iter()
            .filter(|x| x.opcode == 0xBB)
            .filter_map(|x| match x.operand {
                Operand::Constant(i) => class.class_name(i).ok().map(|c| (x.at, c.to_owned())),
                _ => None,
            })
            .collect();
        Stepper { news }
    }
    pub fn after(
        &self,
        class: &ClassFile,
        opts: &Options,
        insn: &Instruction,
        before: &Frame,
    ) -> Result<Frame, String> {
        let mut f = before.clone();
        effect(class, &self.news, opts, insn, &mut f)?;
        Ok(f)
    }
}

/// Where control goes after `insns[i]`: the next instruction (if it falls
/// through) and its targets, as instruction indices.
pub fn successors(insns: &[Instruction], i: usize, at: &HashMap<u32, usize>) -> Vec<usize> {
    let insn = &insns[i];
    let target = |t: u32| at.get(&t).copied();
    let mut out = Vec::new();
    match &insn.operand {
        Operand::Target(t) => {
            out.extend(target(*t));
            if !matches!(insn.opcode, 0xA7 | 0xC8) {
                out.push(i + 1);
            }
        }
        Operand::Table {
            default, targets, ..
        } => {
            out.extend(target(*default));
            out.extend(targets.iter().filter_map(|t| target(*t)));
        }
        Operand::Lookup { default, pairs } => {
            out.extend(target(*default));
            out.extend(pairs.iter().filter_map(|(_, t)| target(*t)));
        }
        _ if matches!(insn.opcode, 0xAC..=0xB1 | 0xBF) => {}
        _ => out.push(i + 1),
    }
    out.retain(|&t| t < insns.len());
    out
}

/// The frame before each instruction of `method` (`None`: unreachable).
pub fn infer(
    h: &dyn Hierarchy,
    class: &ClassFile,
    method: &Member,
    insns: &[Instruction],
) -> Result<Vec<Option<Frame>>, String> {
    infer_with(h, class, method, insns, &Options::default())
}

/// [`infer`], following objects as `opts` asks.
pub fn infer_with(
    h: &dyn Hierarchy,
    class: &ClassFile,
    method: &Member,
    insns: &[Instruction],
    opts: &Options,
) -> Result<Vec<Option<Frame>>, String> {
    let code = method.code().ok_or("no code")?;
    let at: HashMap<u32, usize> = insns.iter().enumerate().map(|(i, x)| (x.at, i)).collect();
    // The `new` instructions, for the constructor calls on their objects.
    let news: HashMap<u32, String> = insns
        .iter()
        .filter(|x| x.opcode == 0xBB)
        .filter_map(|x| match x.operand {
            Operand::Constant(i) => class.class_name(i).ok().map(|c| (x.at, c.to_owned())),
            _ => None,
        })
        .collect();
    let mut before: Vec<Option<Frame>> = vec![None; insns.len()];
    let mut work: VecDeque<usize> = VecDeque::new();
    before[0] = Some(entry_with(class, method, opts)?);
    work.push_back(0);
    let handlers: Vec<(usize, usize, usize, Ty)> = code
        .handlers
        .iter()
        .map(|hd: &Handler| {
            let catch = if hd.catch_type == 0 {
                Ty::class("java/lang/Throwable")
            } else {
                Ty::class(
                    class
                        .class_name(hd.catch_type)
                        .unwrap_or("java/lang/Throwable"),
                )
            };
            let pos = |pc: u16| at.get(&u32::from(pc)).copied().unwrap_or(insns.len());
            (pos(hd.start), pos(hd.end), pos(hd.handler), catch)
        })
        .collect();
    let flow = |before: &mut Vec<Option<Frame>>,
                work: &mut VecDeque<usize>,
                t: usize,
                f: &Frame|
     -> Result<(), String> {
        match &mut before[t] {
            Some(seen) => {
                if seen.merge(h, f)? {
                    work.push_back(t);
                }
            }
            slot => {
                *slot = Some(f.clone());
                work.push_back(t);
            }
        }
        Ok(())
    };
    let mut steps = 0usize;
    while let Some(i) = work.pop_front() {
        steps += 1;
        if steps > insns.len() * 64 + 10_000 {
            return Err("types do not settle".into());
        }
        let f0 = before[i].clone().expect("a frame on the work list");
        // A handler sees the locals of every instruction it covers.
        for (s, e, hd, catch) in &handlers {
            if (*s..*e).contains(&i) && *hd < insns.len() {
                let hf = Frame {
                    locals: f0.locals.clone(),
                    stack: vec![catch.clone()],
                };
                flow(&mut before, &mut work, *hd, &hf)?;
            }
        }
        let mut f = f0;
        effect(class, &news, opts, &insns[i], &mut f)
            .map_err(|e| format!("at {} ({}): {e}", insns[i].at, insns[i].opcode))?;
        // The locals after a store matter to the handlers too.
        if matches!(insns[i].opcode, 0x36..=0x4E) {
            for (s, e, hd, catch) in &handlers {
                if (*s..*e).contains(&i) && *hd < insns.len() {
                    let hf = Frame {
                        locals: f.locals.clone(),
                        stack: vec![catch.clone()],
                    };
                    flow(&mut before, &mut work, *hd, &hf)?;
                }
            }
        }
        for t in successors(insns, i, &at) {
            flow(&mut before, &mut work, t, &f)?;
        }
    }
    Ok(before)
}

/// A frame of a `StackMapTable`: its position and its declared types (a
/// `long` or `double` local is one entry).
#[derive(Clone, Debug)]
pub struct MapFrame {
    pub at: u32,
    pub locals: Vec<Ty>,
    pub stack: Vec<Ty>,
}

/// The `StackMapTable` of `method`, if its class file has one.
pub fn stack_map(class: &ClassFile, method: &Member) -> Result<Option<Vec<MapFrame>>, String> {
    let code = method.code().ok_or("no code")?;
    let Some(bytes) = code.attributes.iter().find_map(|a| match a {
        Attribute::Other(n, b) if n == "StackMapTable" => Some(b),
        _ => None,
    }) else {
        return Ok(None);
    };
    let mut r = bytes.as_slice();
    let mut u8_ = |r: &mut &[u8]| -> Result<u8, String> {
        let (&b, rest) = r.split_first().ok_or("a short StackMapTable")?;
        *r = rest;
        Ok(b)
    };
    let u16_ = |r: &mut &[u8], u8_: &mut dyn FnMut(&mut &[u8]) -> Result<u8, String>| {
        Ok::<u16, String>(u16::from(u8_(r)?) << 8 | u16::from(u8_(r)?))
    };
    let vt = |r: &mut &[u8], u8_: &mut dyn FnMut(&mut &[u8]) -> Result<u8, String>| {
        Ok::<Ty, String>(match u8_(r)? {
            0 => Ty::Top,
            1 => Ty::Int,
            2 => Ty::Float,
            3 => Ty::Double,
            4 => Ty::Long,
            5 => Ty::Null,
            6 => Ty::UninitThis,
            7 => {
                let i = u16_(r, u8_)?;
                let c = class.class_name(i).map_err(|e| e.to_string())?;
                Ty::Ref(c.into())
            }
            8 => Ty::Uninit(u32::from(u16_(r, u8_)?)),
            t => return Err(format!("verification type {t}")),
        })
    };
    // The implicit first frame: the entry, as entries.
    let start = entry(class, method)?;
    let mut locals: Vec<Ty> = Vec::new();
    let mut i = 0;
    while i < start.locals.len() {
        let t = start.locals[i].clone();
        i += if t.wide() { 2 } else { 1 };
        locals.push(t);
    }
    let count = u16_(&mut r, &mut u8_)?;
    let mut out = Vec::new();
    let mut at: i64 = -1;
    for _ in 0..count {
        let kind = u8_(&mut r)?;
        let (delta, stack) = match kind {
            0..=63 => (u16::from(kind), Vec::new()),
            64..=127 => (u16::from(kind - 64), vec![vt(&mut r, &mut u8_)?]),
            247 => {
                let d = u16_(&mut r, &mut u8_)?;
                (d, vec![vt(&mut r, &mut u8_)?])
            }
            248..=250 => {
                let d = u16_(&mut r, &mut u8_)?;
                let k = usize::from(251 - kind);
                locals.truncate(locals.len().saturating_sub(k));
                (d, Vec::new())
            }
            251 => (u16_(&mut r, &mut u8_)?, Vec::new()),
            252..=254 => {
                let d = u16_(&mut r, &mut u8_)?;
                for _ in 0..(kind - 251) {
                    locals.push(vt(&mut r, &mut u8_)?);
                }
                (d, Vec::new())
            }
            255 => {
                let d = u16_(&mut r, &mut u8_)?;
                let nl = u16_(&mut r, &mut u8_)?;
                locals = (0..nl)
                    .map(|_| vt(&mut r, &mut u8_))
                    .collect::<Result<_, _>>()?;
                let ns = u16_(&mut r, &mut u8_)?;
                let stack = (0..ns)
                    .map(|_| vt(&mut r, &mut u8_))
                    .collect::<Result<_, _>>()?;
                (d, stack)
            }
            _ => return Err(format!("frame type {kind}")),
        };
        at += i64::from(delta) + 1;
        out.push(MapFrame {
            at: at as u32,
            locals: locals.clone(),
            stack,
        });
    }
    Ok(Some(out))
}

/// Whether a value of inferred type `got` may be where the class file
/// declares `want` (an interface takes any reference, as in the verifier).
/// `None` when the hierarchy does not know enough to say.
pub fn assignable(h: &dyn Hierarchy, got: &Ty, want: &Ty) -> Option<bool> {
    let (got, want) = (&got.plain(), &want.plain());
    Some(match (got, want) {
        (_, Ty::Top) => true,
        _ if got == want => true,
        (Ty::Null, w) => w.is_reference() && !matches!(w, Ty::Uninit(_) | Ty::UninitThis),
        (Ty::Ref(g), Ty::Ref(w)) => {
            if &**w == OBJECT || h.is_interface(w) {
                true
            } else if !w.starts_with('[') && !h.knows(w) {
                return None;
            } else if let (Some(gc), Some(wc)) = (g.strip_prefix('['), w.strip_prefix('[')) {
                match (Ty::of_descriptor(gc), Ty::of_descriptor(wc)) {
                    (Some(a @ Ty::Ref(_)), Some(b @ Ty::Ref(_))) => assignable(h, &a, &b)?,
                    (a, b) => a == b,
                }
            } else if g.starts_with('[') {
                matches!(&**w, "java/lang/Cloneable" | "java/io/Serializable")
            } else if w.starts_with('[') {
                false
            } else {
                extends(h, g, w)?
            }
        }
        _ => false,
    })
}

/// How the inferred frames of a method compare with its `StackMapTable`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Agreement {
    /// Slots where the inferred type fits the declared one.
    pub agree: usize,
    /// Slots where it does not.
    pub disagree: usize,
    /// Slots the hierarchy cannot decide (a class it does not know).
    pub unknown: usize,
}

/// Tests the inferred frames against the class file's own (Java 7 on).
/// The first disagreement, if any, is in the `Err`.
pub fn check(
    h: &dyn Hierarchy,
    insns: &[Instruction],
    frames: &[Option<Frame>],
    map: &[MapFrame],
) -> (Agreement, Option<String>) {
    let mut a = Agreement::default();
    let mut first = None;
    let at: HashMap<u32, usize> = insns.iter().enumerate().map(|(i, x)| (x.at, i)).collect();
    for mf in map {
        let Some(Some(f)) = at.get(&mf.at).map(|&i| &frames[i]) else {
            // A frame the inference never reached: dead code javac kept.
            continue;
        };
        let mut slot = 0usize;
        let mut pairs: Vec<(Ty, &Ty)> = Vec::new();
        for want in &mf.locals {
            pairs.push((f.locals.get(slot).cloned().unwrap_or(Ty::Top), want));
            slot += if want.wide() { 2 } else { 1 };
        }
        if f.stack.len() != mf.stack.len() {
            a.disagree += 1;
            first.get_or_insert_with(|| format!("stack height at {}", mf.at));
            continue;
        }
        pairs.extend(f.stack.iter().cloned().zip(&mf.stack));
        for (got, want) in pairs {
            match assignable(h, &got, want) {
                Some(true) => a.agree += 1,
                Some(false) => {
                    a.disagree += 1;
                    first.get_or_insert_with(|| {
                        format!("at {}: inferred {got:?}, declared {want:?}", mf.at)
                    });
                }
                None => a.unknown += 1,
            }
        }
    }
    (a, first)
}
