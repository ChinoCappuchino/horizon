//! The native backend's runtime library (docs/java-compat/compiler.md 4.4):
//! the JDK classes almost every mod uses, written in Rust against the
//! public API documentation, with the behavior code can see kept as Java's
//! (string hashes, number formatting, the cached boxes' identity,
//! exceptions). The compiler treats them as classes of the closed world:
//! they have layouts, vtables and ancestors, and compiled code calls their
//! methods directly; they call compiled code back through vtables.
//!
//! Object layouts (8-byte header: the vtable; then fields, 8 bytes each):
//! - `String`: length, hash (bit 32 set when computed), UTF-16 units;
//! - `StringBuilder`: its `char[]`, its count;
//! - boxes: the value;
//! - `Throwable`: message, cause;
//! - arrays (the compiler's): kind, length, elements.
use crate::heap::Context;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
};

mod bitset;
mod collections;
mod executors;
mod logging;
mod strings;
mod system;
pub use system::static_getter;
pub mod threads;
pub use collections::array_clone;

/// A built-in class.
pub struct Builtin {
    pub name: &'static str,
    pub super_name: Option<&'static str>,
    pub interfaces: &'static [&'static str],
    /// The instance size, header included; a subclass's fields follow.
    pub size: i32,
    /// No subclass can override its methods: calls go straight to them.
    pub is_final: bool,
    /// An interface: its methods are defaults for the classes that
    /// implement it.
    pub is_interface: bool,
    pub methods: Vec<Method>,
}

/// A built-in method: its address, in the platform's C convention, with
/// the receiver first for an instance method.
pub struct Method {
    pub name: &'static str,
    pub desc: &'static str,
    pub is_static: bool,
    pub f: usize,
}

/// What the runtime needs from the compiled program.
pub struct Rt {
    /// The vtables of the built-in classes, by name.
    pub vtables: HashMap<&'static str, u64>,
    /// Each class id's name (for `Object.toString`).
    pub names: Vec<String>,
    /// The vtable selectors of `toString`, `hashCode` and `equals`.
    pub to_string: Option<usize>,
    pub hash_code: Option<usize>,
    pub equals: Option<usize>,
    /// The cached boxes: `Integer`, `Short`, `Byte`, `Long` from -128 to
    /// 127, `Character` from 0 to 127, and `Boolean`'s two.
    pub cache: HashMap<&'static str, Vec<u64>>,
    /// Every vtable selector, by `name descriptor` (the runtime calls the
    /// program's methods through them).
    pub selectors: HashMap<String, usize>,
    /// The dispatch table (compiled code points into it): `[method,
    /// selector + 1]` at each class id plus the selector's offset, and the
    /// offsets (`i64::MIN`: no class responds to the selector).
    pub dispatch: Box<[u64]>,
    pub offsets: Vec<i64>,
    /// Each class id's object size in bytes (0: not known here).
    pub sizes: Vec<u32>,
    /// Each class's id by name (`java/lang/String`), each class id's
    /// superclass (0: none), and whether it is an interface.
    pub ids: HashMap<String, u32>,
    pub supers: Vec<u32>,
    pub interfaces: Vec<bool>,
    /// Interfaces outside the program by name (those its classes implement).
    pub outside_interfaces: HashSet<String>,
    /// Each class id's `Class` object.
    pub classes: Vec<u64>,
    /// Each class id's ancestors by name (itself, superclasses, interfaces).
    pub ancestors: Vec<HashSet<String>>,
    /// Each enum class's `$VALUES` static (its address), by class id.
    pub enum_values: HashMap<u32, u64>,
    /// Objects the runtime makes once (`Optional.empty()`...), by name.
    pub once: std::sync::Mutex<HashMap<&'static str, u64>>,
}

thread_local! {
    static RT: Cell<*const Rt> = const { Cell::new(std::ptr::null()) };
    /// This thread's context (`heap::Context`): its buffer and exception.
    static CTX: Cell<*mut Context> = const { Cell::new(std::ptr::null_mut()) };
}

/// Makes `ctx` this thread's context for the runtime library (compiled code
/// gets it as an argument). It must stay alive while the thread runs code.
pub fn install_context(ctx: &Context) {
    CTX.with(|c| c.set(ctx as *const Context as *mut Context));
}

/// This thread's context, as compiled code takes it.
pub(crate) fn ctx() -> *mut Context {
    CTX.with(Cell::get)
}

/// Makes `rt` the runtime of this thread's compiled code. It must stay
/// alive (and in place) while that code runs.
pub fn install(rt: &Rt) {
    RT.with(|r| r.set(rt as *const Rt));
}

fn rt() -> &'static Rt {
    // SAFETY: install() is called before compiled code runs, and the
    // Native that owns the Rt outlives it.
    RT.with(|r| unsafe { &*r.get() })
}

const S_LEN: usize = 8;
const S_HASH: usize = 16;
const S_DATA: usize = 24;
const A_LEN: usize = 8;
const A_DATA: usize = 16;

unsafe fn word(obj: u64, at: usize) -> u64 {
    // SAFETY: the caller has an object with a word at `at`.
    unsafe { *((obj as usize + at) as *const u64) }
}
unsafe fn set_word(obj: u64, at: usize, v: u64) {
    // SAFETY: as above.
    unsafe { *((obj as usize + at) as *mut u64) = v }
}

/// Allocation in this thread's buffer, zeroed, with its header; a new
/// buffer from the heap when it is out.
pub fn alloc(size: usize, vtable: u64) -> u64 {
    let size = ((size + 7) & !7) as u64;
    let ctx = ctx();
    // SAFETY: this thread's context, installed before its code runs.
    let c = unsafe { &mut *ctx };
    let at = if c.bump + size <= c.limit && c.bump != 0 {
        let at = c.bump;
        c.bump += size;
        at
    } else {
        crate::heap::refill(ctx, size)
    };
    assert!(at != 0, "the heap is full");
    // SAFETY: a fresh object of `size` bytes.
    unsafe { *(at as *mut u64) = vtable };
    at
}

fn vt(name: &str) -> u64 {
    rt().vtables.get(name).copied().unwrap_or(0)
}

/// Throws a new built-in exception of class `name` with `message`.
pub fn throw(name: &str, message: Option<&str>) {
    let e = alloc(24, vt(name));
    if let Some(m) = message {
        let s = new_string(&m.encode_utf16().collect::<Vec<_>>());
        // SAFETY: a Throwable has its message at 8.
        unsafe { set_word(e, 8, s) };
    }
    // SAFETY: this thread's context.
    unsafe { (*ctx()).exception = e };
}

/// Throws a new exception whose class has vtable `vtable` (for the
/// compiled code's implicit exceptions: null, bounds, division, cast).
pub extern "C" fn throw_new(vtable: u64) {
    let e = alloc(24, vtable);
    // SAFETY: this thread's context.
    unsafe { (*ctx()).exception = e };
}

/// `multianewarray`: nested arrays for the first `dims` of `counts` (the
/// outer levels hold references; the innermost one's elements are
/// `inner_size` bytes). Below a count of 0 nothing is made, as in the JVM.
pub extern "C" fn multi_array(
    inner_size: i64,
    dims: i32,
    c0: i32,
    c1: i32,
    c2: i32,
    c3: i32,
) -> u64 {
    let counts = [c0, c1, c2, c3];
    let counts = &counts[..dims.clamp(1, 4) as usize];
    if let Some(&n) = counts.iter().find(|&&n| n < 0) {
        throw("java/lang/NegativeArraySizeException", Some(&n.to_string()));
        return 0;
    }
    fn level(counts: &[i32], inner_size: u64) -> u64 {
        let n = counts[0] as u64;
        let size = if counts.len() == 1 { inner_size } else { 8 };
        let a = alloc((A_DATA as u64 + size * n) as usize, size);
        // SAFETY: a fresh array of n elements.
        unsafe { set_word(a, A_LEN, n) };
        if counts.len() > 1 {
            for i in 0..n {
                let inner = level(&counts[1..], inner_size);
                // SAFETY: as above.
                unsafe { set_word(a, A_DATA + 8 * i as usize, inner) };
            }
        }
        a
    }
    level(counts, inner_size as u64)
}

/// Why each stop compiled (`native::MethodGen::stop`), by its number.
static STOP_REASONS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Numbers a stop's reason for `stopped`.
pub fn stop_reason(why: String) -> u32 {
    let mut reasons = STOP_REASONS.lock().unwrap();
    reasons.push(why);
    (reasons.len() - 1) as u32
}

/// Every stop compiled so far: where and why.
pub fn stop_reasons() -> Vec<String> {
    STOP_REASONS.lock().unwrap().clone()
}

/// A virtual call reached a class without the method: says which method
/// and which class before it traps.
pub extern "C" fn stopped_on(id: i32, receiver: u64) {
    let reasons = STOP_REASONS.lock().unwrap();
    let why = reasons.get(id as usize).map_or("?", String::as_str);
    let class = if receiver == 0 {
        "null".to_owned()
    } else {
        // SAFETY: an object's vtable starts with its class id.
        let id = unsafe { word(word(receiver, 0), 0) } as usize;
        rt().names
            .get(id)
            .cloned()
            .unwrap_or_else(|| format!("class {id}"))
    };
    eprintln!("native code stopped in {why} (on a {class})");
}

/// Compiled code reached a stop: says which before it traps.
pub extern "C" fn stopped(id: i32) {
    let reasons = STOP_REASONS.lock().unwrap();
    let why = reasons.get(id as usize).map_or("?", String::as_str);
    eprintln!("native code stopped in {why}");
}

fn pending() -> bool {
    // SAFETY: as above.
    unsafe { (*ctx()).exception != 0 }
}

// ---- Strings ----------------------------------------------------------

/// A `String[]` of `items` (for a `main` method's arguments).
pub fn string_array(items: &[&str]) -> u64 {
    let a = collections::object_array(items.len());
    for (i, item) in items.iter().enumerate() {
        // SAFETY: element i of a reference array.
        unsafe { set_word(a, A_DATA + 8 * i, str_of(item)) };
    }
    a
}
/// The exception the calling thread is throwing, as `Class: message`
/// (after a call into the code returned), and clears it.
pub fn take_exception() -> Option<String> {
    // SAFETY: this thread's context.
    let e = unsafe { std::mem::replace(&mut (*ctx()).exception, 0) };
    (e != 0).then(|| String::from_utf16_lossy(units(to_string(e))))
}
pub fn new_string(units: &[u16]) -> u64 {
    let s = alloc(S_DATA + 2 * units.len(), vt("java/lang/String"));
    // SAFETY: the String was just made with room for the units.
    unsafe {
        set_word(s, S_LEN, units.len() as u64);
        std::ptr::copy_nonoverlapping(
            units.as_ptr(),
            (s as usize + S_DATA) as *mut u16,
            units.len(),
        );
    }
    s
}

/// A literal: made outside the arena (it lives as long as the program).
pub fn literal(units: &[u16], vtable: u64) -> Box<[u64]> {
    let mut o = vec![0u64; 3 + units.len().div_ceil(4)];
    o[0] = vtable;
    o[1] = units.len() as u64;
    for (k, u) in units.iter().enumerate() {
        o[3 + k / 4] |= u64::from(*u) << (16 * (k % 4));
    }
    o.into_boxed_slice()
}

fn units<'a>(s: u64) -> &'a [u16] {
    // SAFETY: s is a String: its length, then its units.
    unsafe {
        let n = word(s, S_LEN) as usize;
        std::slice::from_raw_parts((s as usize + S_DATA) as *const u16, n)
    }
}

fn is_string(o: u64) -> bool {
    // SAFETY: o is an object (non-null).
    o != 0 && unsafe { word(o, 0) } == vt("java/lang/String")
}

fn str_of(text: &str) -> u64 {
    new_string(&text.encode_utf16().collect::<Vec<_>>())
}

/// `String.valueOf(o)`: `"null"`, the string itself, or `o.toString()`.
pub fn to_string(o: u64) -> u64 {
    if o == 0 {
        return str_of("null");
    }
    if is_string(o) {
        return o;
    }
    match call_virtual(o, rt().to_string) {
        Some(f) => {
            // SAFETY: the selector's slot holds a (receiver) -> String method.
            let f: extern "C" fn(*mut Context, u64) -> u64 = unsafe { std::mem::transmute(f) };
            f(ctx(), o)
        }
        None => object_to_string(o),
    }
}

/// The method in `o`'s vtable for `selector`.
fn call_virtual(o: u64, selector: Option<usize>) -> Option<usize> {
    let sel = selector?;
    let r = rt();
    let offset = *r.offsets.get(sel)?;
    if offset == i64::MIN {
        return None;
    }
    // SAFETY: an object's vtable starts with its class id.
    let id = unsafe { word(word(o, 0), 0) } as i64;
    // The entry is the class's method only when it is tagged with `sel`
    // (another selector's, or empty, where the class does not respond).
    let at = usize::try_from(id + offset).ok()?;
    let entry = r.dispatch.get(2 * at..2 * at + 2)?;
    (entry[1] == sel as u64 + 1 && entry[0] != 0).then_some(entry[0] as usize)
}

/// The method of `o`'s class for `name desc`, if it has one.
fn method(o: u64, name_desc: &str) -> Option<usize> {
    let sel = rt().selectors.get(name_desc).copied();
    call_virtual(o, sel)
}

/// Whether object `o` is an instance of class or interface `name`.
fn instance_of(o: u64, name: &str) -> bool {
    if o == 0 {
        return false;
    }
    // SAFETY: an object's vtable starts with its class id.
    let id = unsafe { word(word(o, 0), 0) } as usize;
    rt().ancestors.get(id).is_some_and(|a| a.contains(name))
}

fn java_hash(u: &[u16]) -> i32 {
    u.iter()
        .fold(0i32, |h, &c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
}

extern "C" fn string_length(s: u64) -> i32 {
    units(s).len() as i32
}
extern "C" fn string_char_at(s: u64, i: i32) -> i32 {
    match units(s).get(i as usize) {
        Some(&c) if i >= 0 => i32::from(c),
        _ => {
            throw(
                "java/lang/StringIndexOutOfBoundsException",
                Some(&format!("index {i}, length {}", units(s).len())),
            );
            0
        }
    }
}
extern "C" fn string_is_empty(s: u64) -> i32 {
    units(s).is_empty() as i32
}
extern "C" fn string_equals(s: u64, o: u64) -> i32 {
    (s == o || (is_string(o) && units(s) == units(o))) as i32
}
extern "C" fn string_hash(s: u64) -> i32 {
    // SAFETY: a String's hash word.
    unsafe {
        let h = word(s, S_HASH);
        if h & (1 << 32) != 0 {
            return h as u32 as i32;
        }
        let v = java_hash(units(s));
        set_word(s, S_HASH, u64::from(v as u32) | (1 << 32));
        v
    }
}
extern "C" fn string_to_string(s: u64) -> u64 {
    s
}
extern "C" fn string_compare(s: u64, o: u64) -> i32 {
    if o == 0 {
        throw("java/lang/NullPointerException", None);
        return 0;
    }
    let (a, b) = (units(s), units(o));
    for (x, y) in a.iter().zip(b) {
        if x != y {
            return i32::from(*x) - i32::from(*y);
        }
    }
    a.len() as i32 - b.len() as i32
}
/// A CharSequence argument as units (a String, else its toString()).
fn chars_of<'a>(o: u64) -> Option<&'a [u16]> {
    if o == 0 {
        throw("java/lang/NullPointerException", None);
        return None;
    }
    let s = to_string(o);
    (!pending()).then(|| units(s))
}
fn find(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    (from..=hay.len().saturating_sub(needle.len())).find(|&i| hay[i..].starts_with(needle))
}
extern "C" fn string_contains(s: u64, o: u64) -> i32 {
    chars_of(o).is_some_and(|n| find(units(s), n, 0).is_some()) as i32
}
extern "C" fn string_starts_with(s: u64, o: u64) -> i32 {
    chars_of(o).is_some_and(|n| units(s).starts_with(n)) as i32
}
extern "C" fn string_ends_with(s: u64, o: u64) -> i32 {
    chars_of(o).is_some_and(|n| units(s).ends_with(n)) as i32
}
extern "C" fn string_index_of_char(s: u64, c: i32) -> i32 {
    // A supplementary code point is two units.
    let u = units(s);
    match char::from_u32(c as u32) {
        Some(ch) if c > 0xFFFF => {
            let mut buf = [0u16; 2];
            let pair = ch.encode_utf16(&mut buf);
            find(u, pair, 0).map_or(-1, |i| i as i32)
        }
        _ => u
            .iter()
            .position(|&x| i32::from(x) == c)
            .map_or(-1, |i| i as i32),
    }
}
extern "C" fn string_index_of(s: u64, o: u64) -> i32 {
    chars_of(o).map_or(-1, |n| find(units(s), n, 0).map_or(-1, |i| i as i32))
}
fn substring(s: u64, b: i32, e: i32) -> u64 {
    let u = units(s);
    if b < 0 || e > u.len() as i32 || b > e {
        throw(
            "java/lang/StringIndexOutOfBoundsException",
            Some(&format!("begin {b}, end {e}, length {}", u.len())),
        );
        return 0;
    }
    if b == 0 && e == u.len() as i32 {
        return s;
    }
    new_string(&u[b as usize..e as usize])
}
extern "C" fn string_substring(s: u64, b: i32) -> u64 {
    substring(s, b, units(s).len() as i32)
}
extern "C" fn string_substring2(s: u64, b: i32, e: i32) -> u64 {
    substring(s, b, e)
}
extern "C" fn string_trim(s: u64) -> u64 {
    let u = units(s);
    let start = u.iter().position(|&c| c > 0x20).unwrap_or(u.len());
    let end = u.iter().rposition(|&c| c > 0x20).map_or(start, |i| i + 1);
    substring(s, start as i32, end as i32)
}
extern "C" fn string_equals_ignore_case(s: u64, o: u64) -> i32 {
    if o == 0 || !is_string(o) {
        return 0;
    }
    let (a, b) = (units(s), units(o));
    // Java compares unit by unit: upper case, then lower case.
    let fold = |c: u16| -> u32 {
        let ch = char::from_u32(u32::from(c)).unwrap_or('\u{0}');
        let up = ch.to_uppercase().next().unwrap_or(ch);
        let low = up.to_lowercase().next().unwrap_or(up);
        u32::from(low)
    };
    (a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y || fold(*x) == fold(*y))) as i32
}
extern "C" fn string_concat(s: u64, o: u64) -> u64 {
    if o == 0 {
        throw("java/lang/NullPointerException", None);
        return 0;
    }
    let mut v = units(s).to_vec();
    v.extend_from_slice(units(o));
    new_string(&v)
}
extern "C" fn string_replace_char(s: u64, a: i32, b: i32) -> u64 {
    let v: Vec<u16> = units(s)
        .iter()
        .map(|&c| if i32::from(c) == a { b as u16 } else { c })
        .collect();
    new_string(&v)
}
extern "C" fn string_replace(s: u64, a: u64, b: u64) -> u64 {
    let (Some(from), Some(to)) = (chars_of(a), chars_of(b)) else {
        return 0;
    };
    let u = units(s);
    let mut out = Vec::new();
    let mut i = 0;
    if from.is_empty() {
        // Java puts the replacement between every unit and at both ends.
        for &c in u {
            out.extend_from_slice(to);
            out.push(c);
        }
        out.extend_from_slice(to);
        return new_string(&out);
    }
    while let Some(j) = find(u, from, i) {
        out.extend_from_slice(&u[i..j]);
        out.extend_from_slice(to);
        i = j + from.len();
    }
    out.extend_from_slice(&u[i..]);
    new_string(&out)
}
extern "C" fn string_value_of_object(o: u64) -> u64 {
    to_string(o)
}
extern "C" fn string_value_of_int(i: i32) -> u64 {
    str_of(&i.to_string())
}
extern "C" fn string_value_of_long(i: i64) -> u64 {
    str_of(&i.to_string())
}
extern "C" fn string_value_of_float(f: f32) -> u64 {
    str_of(&java_float(f))
}
extern "C" fn string_value_of_double(d: f64) -> u64 {
    str_of(&java_double(d))
}
extern "C" fn string_value_of_bool(b: i32) -> u64 {
    str_of(if b != 0 { "true" } else { "false" })
}
extern "C" fn string_value_of_char(c: i32) -> u64 {
    new_string(&[c as u16])
}

/// Java's `Double.toString`: the shortest digits that give back `d`, in
/// plain notation from 10^-3 to 10^7, else as `d.dddE±n`.
pub fn java_double(d: f64) -> String {
    java_decimal(
        d.is_nan(),
        d.is_infinite(),
        d.is_sign_negative(),
        d == 0.0,
        &format!("{d:e}"),
    )
}

/// Java's `Float.toString` (the shortest digits of the float).
pub fn java_float(f: f32) -> String {
    java_decimal(
        f.is_nan(),
        f.is_infinite(),
        f.is_sign_negative(),
        f == 0.0,
        &format!("{f:e}"),
    )
}

fn java_decimal(nan: bool, inf: bool, neg: bool, zero: bool, sci: &str) -> String {
    if nan {
        return "NaN".into();
    }
    if inf {
        return if neg { "-Infinity" } else { "Infinity" }.into();
    }
    if zero {
        return if neg { "-0.0" } else { "0.0" }.into();
    }
    // `sci` is Rust's shortest form: [-]d[.ddd]e[-]n.
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if neg { "-" } else { "" };
    if (-3..7).contains(&exp) {
        let point = exp + 1;
        let (int, frac) = if point <= 0 {
            (
                "0".to_owned(),
                format!("{}{digits}", "0".repeat((-point) as usize)),
            )
        } else if point as usize >= digits.len() {
            (
                format!("{digits}{}", "0".repeat(point as usize - digits.len())),
                String::new(),
            )
        } else {
            (
                digits[..point as usize].to_owned(),
                digits[point as usize..].to_owned(),
            )
        };
        let frac = if frac.is_empty() {
            "0".to_owned()
        } else {
            frac
        };
        format!("{sign}{int}.{frac}")
    } else {
        let frac = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{sign}{}.{frac}E{exp}", &digits[..1])
    }
}

// ---- StringBuilder ------------------------------------------------------

const SB_VALUE: usize = 8;
const SB_COUNT: usize = 16;

fn char_array(n: usize) -> u64 {
    let a = alloc(A_DATA + 2 * n, 0);
    // SAFETY: the array was just made.
    unsafe { set_word(a, A_LEN, n as u64) };
    a
}

extern "C" fn sb_init(sb: u64) {
    // SAFETY: sb is a StringBuilder.
    unsafe { set_word(sb, SB_VALUE, char_array(16)) };
}
extern "C" fn sb_init_cap(sb: u64, n: i32) {
    if n < 0 {
        throw("java/lang/NegativeArraySizeException", None);
        return;
    }
    // SAFETY: as above.
    unsafe { set_word(sb, SB_VALUE, char_array(n as usize)) };
}
extern "C" fn sb_init_str(sb: u64, s: u64) {
    if s == 0 {
        throw("java/lang/NullPointerException", None);
        return;
    }
    sb_init_cap(sb, units(s).len() as i32 + 16);
    sb_push(sb, units(s));
}
fn sb_units<'a>(sb: u64) -> &'a [u16] {
    // SAFETY: a StringBuilder: its array and count.
    unsafe {
        let a = word(sb, SB_VALUE);
        let n = word(sb, SB_COUNT) as usize;
        std::slice::from_raw_parts((a as usize + A_DATA) as *const u16, n)
    }
}
fn sb_push(sb: u64, more: &[u16]) {
    // SAFETY: as above; the array grows as Java's does (2n + 2).
    unsafe {
        let mut a = word(sb, SB_VALUE);
        let n = word(sb, SB_COUNT) as usize;
        let cap = word(a, A_LEN) as usize;
        if n + more.len() > cap {
            let new_cap = (2 * cap + 2).max(n + more.len());
            let b = char_array(new_cap);
            std::ptr::copy_nonoverlapping(
                (a as usize + A_DATA) as *const u16,
                (b as usize + A_DATA) as *mut u16,
                n,
            );
            set_word(sb, SB_VALUE, b);
            a = b;
        }
        std::ptr::copy_nonoverlapping(
            more.as_ptr(),
            (a as usize + A_DATA + 2 * n) as *mut u16,
            more.len(),
        );
        set_word(sb, SB_COUNT, (n + more.len()) as u64);
    }
}
fn sb_push_str(sb: u64, s: &str) {
    sb_push(sb, &s.encode_utf16().collect::<Vec<_>>());
}
pub extern "C" fn sb_append_string(sb: u64, s: u64) -> u64 {
    if s == 0 {
        sb_push_str(sb, "null");
    } else {
        sb_push(sb, units(s));
    }
    sb
}
pub extern "C" fn sb_append_object(sb: u64, o: u64) -> u64 {
    let s = to_string(o);
    if !pending() {
        sb_append_string(sb, s);
    }
    sb
}
pub extern "C" fn sb_append_int(sb: u64, i: i32) -> u64 {
    sb_push_str(sb, &i.to_string());
    sb
}
pub extern "C" fn sb_append_long(sb: u64, i: i64) -> u64 {
    sb_push_str(sb, &i.to_string());
    sb
}
pub extern "C" fn sb_append_float(sb: u64, f: f32) -> u64 {
    sb_push_str(sb, &java_float(f));
    sb
}
pub extern "C" fn sb_append_double(sb: u64, d: f64) -> u64 {
    sb_push_str(sb, &java_double(d));
    sb
}
pub extern "C" fn sb_append_char(sb: u64, c: i32) -> u64 {
    sb_push(sb, &[c as u16]);
    sb
}
pub extern "C" fn sb_append_bool(sb: u64, b: i32) -> u64 {
    sb_push_str(sb, if b != 0 { "true" } else { "false" });
    sb
}
pub extern "C" fn sb_to_string(sb: u64) -> u64 {
    new_string(sb_units(sb))
}
extern "C" fn sb_length(sb: u64) -> i32 {
    sb_units(sb).len() as i32
}
extern "C" fn sb_char_at(sb: u64, i: i32) -> i32 {
    match sb_units(sb).get(i as usize) {
        Some(&c) if i >= 0 => i32::from(c),
        _ => {
            throw("java/lang/StringIndexOutOfBoundsException", None);
            0
        }
    }
}
extern "C" fn sb_set_length(sb: u64, n: i32) {
    if n < 0 {
        throw("java/lang/StringIndexOutOfBoundsException", None);
        return;
    }
    let cur = sb_units(sb).len();
    if n as usize > cur {
        sb_push(sb, &vec![0u16; n as usize - cur]);
    } else {
        // SAFETY: shrinking the count.
        unsafe { set_word(sb, SB_COUNT, n as u64) };
    }
}
/// A new StringBuilder (for string concatenation).
pub extern "C" fn sb_new() -> u64 {
    let sb = alloc(24, vt("java/lang/StringBuilder"));
    sb_init(sb);
    sb
}

// ---- Object ---------------------------------------------------------------

extern "C" fn object_init(_o: u64) {}
extern "C" fn object_equals(o: u64, other: u64) -> i32 {
    (o == other) as i32
}
/// The identity hash: from the address (objects do not move yet).
/// `Object.clone`: a copy of the object's fields (an array: of its
/// elements); `CloneNotSupportedException` for a class that is not
/// `Cloneable`.
extern "C" fn object_clone(o: u64) -> u64 {
    // SAFETY: an object's header is its class record; an array's is its
    // element size (1 to 8).
    let header = unsafe { word(o, 0) };
    if header <= 8 {
        return collections::array_clone(o);
    }
    // SAFETY: a class record starts with the class id.
    let id = unsafe { word(header, 0) } as usize;
    let r = rt();
    let size = r.sizes.get(id).copied().unwrap_or(0) as usize;
    let cloneable = r
        .ancestors
        .get(id)
        .is_some_and(|a| a.contains("java/lang/Cloneable"));
    if size == 0 || !cloneable {
        throw("java/lang/CloneNotSupportedException", None);
        return 0;
    }
    let c = alloc(size, header);
    // SAFETY: two objects of `size` bytes; the header is already set.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (o as usize + 8) as *const u8,
            (c as usize + 8) as *mut u8,
            size - 8,
        );
    }
    c
}
pub extern "C" fn identity_hash(o: u64) -> i32 {
    ((o >> 3) ^ (o >> 35)) as i32 & 0x7FFF_FFFF
}
extern "C" fn object_to_string_c(o: u64) -> u64 {
    object_to_string(o)
}
fn object_to_string(o: u64) -> u64 {
    // SAFETY: the vtable's first word is the class id.
    let id = unsafe { word(word(o, 0), 0) } as usize;
    let name = rt()
        .names
        .get(id)
        .map_or("?", |s| s.as_str())
        .replace('/', ".");
    let hash = match call_virtual(o, rt().hash_code) {
        Some(f) => {
            // SAFETY: the hashCode selector's slot: (receiver) -> int.
            let f: extern "C" fn(*mut Context, u64) -> i32 = unsafe { std::mem::transmute(f) };
            f(ctx(), o)
        }
        None => identity_hash(o),
    };
    str_of(&format!("{name}@{:x}", hash as u32))
}
/// `o.equals(other)`, through o's class.
pub fn equals(o: u64, other: u64) -> bool {
    if o == other {
        return true;
    }
    match call_virtual(o, rt().equals) {
        Some(f) => {
            // SAFETY: the equals selector's slot: (receiver, Object) -> boolean.
            let f: extern "C" fn(*mut Context, u64, u64) -> i32 = unsafe { std::mem::transmute(f) };
            f(ctx(), o, other) != 0
        }
        None => false,
    }
}
/// `o.hashCode()`, through o's class.
pub fn hash_code(o: u64) -> i32 {
    match call_virtual(o, rt().hash_code) {
        Some(f) => {
            // SAFETY: as above.
            let f: extern "C" fn(*mut Context, u64) -> i32 = unsafe { std::mem::transmute(f) };
            f(ctx(), o)
        }
        None => identity_hash(o),
    }
}

// ---- Boxes ------------------------------------------------------------------

fn boxed(class: &'static str, value: u64) -> u64 {
    let b = alloc(16, vt(class));
    // SAFETY: a box has its value at 8.
    unsafe { set_word(b, 8, value) };
    b
}
fn cached(class: &'static str, index: i64) -> Option<u64> {
    let c = rt().cache.get(class)?;
    usize::try_from(index).ok().and_then(|i| c.get(i)).copied()
}
fn value(b: u64) -> u64 {
    // SAFETY: a box's value.
    unsafe { word(b, 8) }
}
pub extern "C" fn integer_value_of(i: i32) -> u64 {
    cached("java/lang/Integer", i64::from(i) + 128)
        .unwrap_or_else(|| boxed("java/lang/Integer", i as u32 as u64))
}
pub extern "C" fn long_value_of(i: i64) -> u64 {
    if (-128..=127).contains(&i)
        && let Some(b) = cached("java/lang/Long", i + 128)
    {
        return b;
    }
    boxed("java/lang/Long", i as u64)
}
pub extern "C" fn short_value_of(i: i32) -> u64 {
    let i = i as i16;
    cached("java/lang/Short", i64::from(i) + 128)
        .unwrap_or_else(|| boxed("java/lang/Short", i as i32 as u32 as u64))
}
pub extern "C" fn byte_value_of(i: i32) -> u64 {
    cached("java/lang/Byte", i64::from(i as i8) + 128).expect("all bytes are cached")
}
pub extern "C" fn char_value_of(c: i32) -> u64 {
    let c = c as u16;
    cached("java/lang/Character", i64::from(c))
        .unwrap_or_else(|| boxed("java/lang/Character", u64::from(c)))
}
pub extern "C" fn bool_value_of(b: i32) -> u64 {
    cached("java/lang/Boolean", i64::from(b != 0)).expect("both booleans are cached")
}
pub extern "C" fn float_value_of(f: f32) -> u64 {
    boxed("java/lang/Float", u64::from(f.to_bits()))
}
pub extern "C" fn double_value_of(d: f64) -> u64 {
    boxed("java/lang/Double", d.to_bits())
}
pub extern "C" fn box_int(b: u64) -> i32 {
    value(b) as u32 as i32
}
pub extern "C" fn box_long(b: u64) -> i64 {
    value(b) as i64
}
pub extern "C" fn box_float(b: u64) -> f32 {
    f32::from_bits(value(b) as u32)
}
pub extern "C" fn box_double(b: u64) -> f64 {
    f64::from_bits(value(b))
}
// Number conversions of each box (`intValue` and the others).
extern "C" fn int_as_long(b: u64) -> i64 {
    i64::from(box_int(b))
}
extern "C" fn int_as_float(b: u64) -> f32 {
    box_int(b) as f32
}
extern "C" fn int_as_double(b: u64) -> f64 {
    f64::from(box_int(b))
}
extern "C" fn long_as_int(b: u64) -> i32 {
    box_long(b) as i32
}
extern "C" fn long_as_float(b: u64) -> f32 {
    box_long(b) as f32
}
extern "C" fn long_as_double(b: u64) -> f64 {
    box_long(b) as f64
}
extern "C" fn float_as_int(b: u64) -> i32 {
    // Java's conversion: NaN is 0, out of range saturates.
    box_float(b) as i32
}
extern "C" fn float_as_long(b: u64) -> i64 {
    box_float(b) as i64
}
extern "C" fn float_as_double(b: u64) -> f64 {
    f64::from(box_float(b))
}
extern "C" fn double_as_int(b: u64) -> i32 {
    box_double(b) as i32
}
extern "C" fn double_as_long(b: u64) -> i64 {
    box_double(b) as i64
}
extern "C" fn double_as_float(b: u64) -> f32 {
    box_double(b) as f32
}
extern "C" fn small_as_int(b: u64) -> i32 {
    box_int(b)
}
extern "C" fn box_equals(b: u64, o: u64) -> i32 {
    // SAFETY: both objects' headers.
    (o != 0 && unsafe { word(b, 0) == word(o, 0) } && value(b) == value(o)) as i32
}
extern "C" fn float_equals(b: u64, o: u64) -> i32 {
    // Float.equals compares floatToIntBits: every NaN is equal.
    // SAFETY: as above.
    if o == 0 || unsafe { word(b, 0) != word(o, 0) } {
        return 0;
    }
    let bits = |x: u64| {
        let f = box_float(x);
        if f.is_nan() { 0x7fc0_0000 } else { f.to_bits() }
    };
    (bits(b) == bits(o)) as i32
}
extern "C" fn double_equals(b: u64, o: u64) -> i32 {
    // SAFETY: as above.
    if o == 0 || unsafe { word(b, 0) != word(o, 0) } {
        return 0;
    }
    let bits = |x: u64| {
        let d = box_double(x);
        if d.is_nan() {
            0x7ff8_0000_0000_0000
        } else {
            d.to_bits()
        }
    };
    (bits(b) == bits(o)) as i32
}
extern "C" fn int_hash(b: u64) -> i32 {
    box_int(b)
}
extern "C" fn long_hash(b: u64) -> i32 {
    let v = value(b);
    (v ^ (v >> 32)) as i32
}
extern "C" fn float_hash(b: u64) -> i32 {
    let f = box_float(b);
    (if f.is_nan() { 0x7fc0_0000 } else { f.to_bits() }) as i32
}
extern "C" fn double_hash(b: u64) -> i32 {
    let d = box_double(b);
    let v = if d.is_nan() {
        0x7ff8_0000_0000_0000
    } else {
        d.to_bits()
    };
    (v ^ (v >> 32)) as i32
}
extern "C" fn bool_hash(b: u64) -> i32 {
    if value(b) != 0 { 1231 } else { 1237 }
}
extern "C" fn int_to_string(b: u64) -> u64 {
    str_of(&box_int(b).to_string())
}
extern "C" fn long_to_string(b: u64) -> u64 {
    str_of(&box_long(b).to_string())
}
extern "C" fn float_to_string(b: u64) -> u64 {
    str_of(&java_float(box_float(b)))
}
extern "C" fn double_to_string(b: u64) -> u64 {
    str_of(&java_double(box_double(b)))
}
extern "C" fn bool_to_string(b: u64) -> u64 {
    str_of(if value(b) != 0 { "true" } else { "false" })
}
extern "C" fn char_to_string(b: u64) -> u64 {
    new_string(&[value(b) as u16])
}
extern "C" fn int_compare_to(b: u64, o: u64) -> i32 {
    if o == 0 {
        throw("java/lang/NullPointerException", None);
        return 0;
    }
    box_int(b).cmp(&box_int(o)) as i32
}
extern "C" fn long_compare_to(b: u64, o: u64) -> i32 {
    if o == 0 {
        throw("java/lang/NullPointerException", None);
        return 0;
    }
    box_long(b).cmp(&box_long(o)) as i32
}
extern "C" fn int_compare(a: i32, b: i32) -> i32 {
    a.cmp(&b) as i32
}
extern "C" fn long_compare(a: i64, b: i64) -> i32 {
    a.cmp(&b) as i32
}
extern "C" fn int_sum(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}
extern "C" fn int_max(a: i32, b: i32) -> i32 {
    a.max(b)
}
extern "C" fn int_min(a: i32, b: i32) -> i32 {
    a.min(b)
}
extern "C" fn int_static_to_string(i: i32) -> u64 {
    str_of(&i.to_string())
}
extern "C" fn int_hash_static(i: i32) -> i32 {
    i
}
extern "C" fn parse_int(s: u64) -> i32 {
    parse_int_radix(s, 10)
}
extern "C" fn parse_int_radix(s: u64, radix: i32) -> i32 {
    if s == 0 {
        throw(
            "java/lang/NumberFormatException",
            Some("Cannot parse null string: null"),
        );
        return 0;
    }
    let text = String::from_utf16_lossy(units(s));
    // Java: an optional sign, then digits; no spaces, no "+" alone.
    let ok = !text.is_empty() && text != "+" && text != "-";
    match ok
        .then(|| i32::from_str_radix(&text, radix as u32).ok())
        .flatten()
    {
        Some(v) => v,
        None => {
            throw(
                "java/lang/NumberFormatException",
                Some(&format!("For input string: \"{text}\"")),
            );
            0
        }
    }
}
extern "C" fn parse_double(s: u64) -> f64 {
    if s == 0 {
        throw("java/lang/NullPointerException", None);
        return 0.0;
    }
    let text = String::from_utf16_lossy(units(s));
    let t = text.trim_matches(|c: char| c <= ' ');
    let t = t.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(t);
    match t.parse::<f64>() {
        Ok(v) => v,
        Err(_) => {
            throw(
                "java/lang/NumberFormatException",
                Some(&format!("For input string: \"{text}\"")),
            );
            0.0
        }
    }
}

// ---- Float and Double ----------------------------------------------------------
// (From the java.lang.Float and java.lang.Double API documentation.)

/// `floatToIntBits`: the bits, every NaN as the canonical one.
fn float_bits(f: f32) -> i32 {
    if f.is_nan() {
        0x7fc0_0000
    } else {
        f.to_bits() as i32
    }
}
/// `doubleToLongBits`: the bits, every NaN as the canonical one.
fn double_bits(d: f64) -> i64 {
    if d.is_nan() {
        0x7ff8_0000_0000_0000
    } else {
        d.to_bits() as i64
    }
}
/// `compare`: -0.0 below 0.0, NaN above all and equal to itself.
fn double_order(a: f64, b: f64) -> i32 {
    if a < b {
        -1
    } else if a > b {
        1
    } else {
        double_bits(a).cmp(&double_bits(b)) as i32
    }
}
/// `Math.max`: NaN if either is, 0.0 above -0.0.
fn java_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { b } else { a }
    } else if a >= b {
        a
    } else {
        b
    }
}
/// `Math.min`: NaN if either is, -0.0 below 0.0.
fn java_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { a } else { b }
    } else if a <= b {
        a
    } else {
        b
    }
}
extern "C" fn float_to_int_bits(f: f32) -> i32 {
    float_bits(f)
}
extern "C" fn double_to_long_bits(d: f64) -> i64 {
    double_bits(d)
}
extern "C" fn float_compare(a: f32, b: f32) -> i32 {
    // (Widening is exact, and keeps the order and NaN.)
    double_order(f64::from(a), f64::from(b))
}
extern "C" fn double_compare(a: f64, b: f64) -> i32 {
    double_order(a, b)
}
extern "C" fn float_is_infinite(f: f32) -> i32 {
    i32::from(f.is_infinite())
}
extern "C" fn float_is_finite(f: f32) -> i32 {
    i32::from(f.is_finite())
}
extern "C" fn double_is_infinite(d: f64) -> i32 {
    i32::from(d.is_infinite())
}
extern "C" fn double_is_finite(d: f64) -> i32 {
    i32::from(d.is_finite())
}
extern "C" fn float_sum(a: f32, b: f32) -> f32 {
    a + b
}
extern "C" fn double_sum(a: f64, b: f64) -> f64 {
    a + b
}
extern "C" fn float_max(a: f32, b: f32) -> f32 {
    java_max(f64::from(a), f64::from(b)) as f32
}
extern "C" fn float_min(a: f32, b: f32) -> f32 {
    java_min(f64::from(a), f64::from(b)) as f32
}
extern "C" fn double_max(a: f64, b: f64) -> f64 {
    java_max(a, b)
}
extern "C" fn double_min(a: f64, b: f64) -> f64 {
    java_min(a, b)
}
extern "C" fn float_hash_static(f: f32) -> i32 {
    float_bits(f)
}
extern "C" fn double_hash_static(d: f64) -> i32 {
    let v = double_bits(d) as u64;
    (v ^ (v >> 32)) as i32
}
extern "C" fn float_static_to_string(f: f32) -> u64 {
    str_of(&java_float(f))
}
extern "C" fn double_static_to_string(d: f64) -> u64 {
    str_of(&java_double(d))
}
/// The text of a `parseFloat`/`parseDouble` argument (the JDK trims
/// spaces and control characters and takes a type suffix), or a thrown
/// exception.
fn float_text(s: u64) -> Option<(String, String)> {
    if s == 0 {
        throw("java/lang/NullPointerException", None);
        return None;
    }
    let text = String::from_utf16_lossy(units(s));
    let t = text.trim_matches(|c: char| c <= ' ');
    let t = t.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(t).to_owned();
    Some((text, t))
}
fn bad_number(text: &str) {
    throw(
        "java/lang/NumberFormatException",
        Some(&format!("For input string: \"{text}\"")),
    );
}
extern "C" fn parse_float(s: u64) -> f32 {
    let Some((text, t)) = float_text(s) else {
        return 0.0;
    };
    // (Straight to float: no double rounding.)
    t.parse::<f32>().unwrap_or_else(|_| {
        bad_number(&text);
        0.0
    })
}
extern "C" fn float_value_of_string(s: u64) -> u64 {
    let f = parse_float(s);
    if pending() { 0 } else { float_value_of(f) }
}
extern "C" fn double_value_of_string(s: u64) -> u64 {
    let d = parse_double(s);
    if pending() { 0 } else { double_value_of(d) }
}
extern "C" fn float_compare_to(b: u64, o: u64) -> i32 {
    if o == 0 {
        throw("java/lang/NullPointerException", None);
        return 0;
    }
    float_compare(box_float(b), box_float(o))
}
extern "C" fn double_compare_to(b: u64, o: u64) -> i32 {
    if o == 0 {
        throw("java/lang/NullPointerException", None);
        return 0;
    }
    double_order(box_double(b), box_double(o))
}
extern "C" fn float_box_is_nan(b: u64) -> i32 {
    i32::from(box_float(b).is_nan())
}
extern "C" fn float_box_is_infinite(b: u64) -> i32 {
    i32::from(box_float(b).is_infinite())
}
extern "C" fn double_box_is_nan(b: u64) -> i32 {
    i32::from(box_double(b).is_nan())
}
extern "C" fn double_box_is_infinite(b: u64) -> i32 {
    i32::from(box_double(b).is_infinite())
}

// ---- Throwable ----------------------------------------------------------------

extern "C" fn throwable_init(_t: u64) {}
extern "C" fn throwable_init_message(t: u64, m: u64) {
    // SAFETY: a Throwable: message at 8.
    unsafe { set_word(t, 8, m) };
}
extern "C" fn throwable_init_message_cause(t: u64, m: u64, c: u64) {
    // SAFETY: message at 8, cause at 16.
    unsafe {
        set_word(t, 8, m);
        set_word(t, 16, c);
    }
}
extern "C" fn throwable_init_cause(t: u64, c: u64) {
    // Java: the message is the cause's toString().
    let m = if c == 0 { 0 } else { to_string(c) };
    throwable_init_message_cause(t, m, c);
}
/// `AssertionError(Object)`: the message is `String.valueOf(detail)`; a
/// `Throwable` detail is the cause too.
extern "C" fn assertion_init_object(t: u64, d: u64) {
    let m = if d == 0 { str_of("null") } else { to_string(d) };
    let c = if instance_of(d, "java/lang/Throwable") {
        d
    } else {
        0
    };
    throwable_init_message_cause(t, m, c);
}
extern "C" fn assertion_init_bool(t: u64, d: i32) {
    throwable_init_message(t, str_of(if d != 0 { "true" } else { "false" }));
}
extern "C" fn assertion_init_char(t: u64, d: i32) {
    throwable_init_message(t, new_string(&[d as u16]));
}
extern "C" fn assertion_init_int(t: u64, d: i32) {
    throwable_init_message(t, str_of(&d.to_string()));
}
extern "C" fn assertion_init_long(t: u64, d: i64) {
    throwable_init_message(t, str_of(&d.to_string()));
}
extern "C" fn assertion_init_float(t: u64, d: f32) {
    throwable_init_message(t, str_of(&java_float(d)));
}
extern "C" fn assertion_init_double(t: u64, d: f64) {
    throwable_init_message(t, str_of(&java_double(d)));
}
/// A JDK exception class's own constructors (the others are Throwable's).
pub fn exception_methods(class: &str) -> Vec<Method> {
    match class {
        "java/lang/AssertionError" => vec![
            m(
                "<init>",
                "(Ljava/lang/Object;)V",
                assertion_init_object as *const () as usize,
            ),
            m("<init>", "(Z)V", assertion_init_bool as *const () as usize),
            m("<init>", "(C)V", assertion_init_char as *const () as usize),
            m("<init>", "(I)V", assertion_init_int as *const () as usize),
            m("<init>", "(J)V", assertion_init_long as *const () as usize),
            m("<init>", "(F)V", assertion_init_float as *const () as usize),
            m(
                "<init>",
                "(D)V",
                assertion_init_double as *const () as usize,
            ),
        ],
        _ => Vec::new(),
    }
}
extern "C" fn throwable_message(t: u64) -> u64 {
    // SAFETY: as above.
    unsafe { word(t, 8) }
}
extern "C" fn throwable_cause(t: u64) -> u64 {
    // SAFETY: as above.
    unsafe { word(t, 16) }
}
extern "C" fn throwable_to_string(t: u64) -> u64 {
    // SAFETY: as above.
    let id = unsafe { word(word(t, 0), 0) } as usize;
    let name = rt()
        .names
        .get(id)
        .map_or("?", |s| s.as_str())
        .replace('/', ".");
    let m = throwable_message(t);
    if m == 0 {
        str_of(&name)
    } else {
        let mut v: Vec<u16> = format!("{name}: ").encode_utf16().collect();
        v.extend_from_slice(units(m));
        new_string(&v)
    }
}
extern "C" fn throwable_add_suppressed(_t: u64, _s: u64) {}

// ---- Objects --------------------------------------------------------------------

extern "C" fn objects_require_non_null(o: u64) -> u64 {
    if o == 0 {
        throw("java/lang/NullPointerException", None);
    }
    o
}
extern "C" fn objects_require_non_null_message(o: u64, m: u64) -> u64 {
    if o == 0 {
        let text = if m == 0 {
            None
        } else {
            Some(String::from_utf16_lossy(units(m)))
        };
        throw("java/lang/NullPointerException", text.as_deref());
    }
    o
}
extern "C" fn objects_to_string(o: u64) -> u64 {
    if o == 0 { str_of("null") } else { to_string(o) }
}
extern "C" fn objects_to_string_or(o: u64, otherwise: u64) -> u64 {
    if o == 0 { otherwise } else { to_string(o) }
}
extern "C" fn objects_is_null(o: u64) -> i32 {
    i32::from(o == 0)
}
extern "C" fn objects_non_null(o: u64) -> i32 {
    i32::from(o != 0)
}
extern "C" fn objects_require_non_null_else(o: u64, otherwise: u64) -> u64 {
    if o != 0 {
        o
    } else if otherwise != 0 {
        otherwise
    } else {
        throw("java/lang/NullPointerException", Some("defaultObj"));
        0
    }
}
extern "C" fn small_compare(a: i32, b: i32) -> i32 {
    // Byte.compare and Short.compare: the difference.
    a - b
}
extern "C" fn int_to_hex(i: i32) -> u64 {
    str_of(&format!("{:x}", i as u32))
}
extern "C" fn int_to_binary(i: i32) -> u64 {
    str_of(&format!("{:b}", i as u32))
}
extern "C" fn int_to_octal(i: i32) -> u64 {
    str_of(&format!("{:o}", i as u32))
}
extern "C" fn long_to_hex(i: i64) -> u64 {
    str_of(&format!("{:x}", i as u64))
}
extern "C" fn long_to_binary(i: i64) -> u64 {
    str_of(&format!("{:b}", i as u64))
}
extern "C" fn long_static_to_string(i: i64) -> u64 {
    str_of(&i.to_string())
}
extern "C" fn long_hash_static(i: i64) -> i32 {
    (i ^ ((i as u64) >> 32) as i64) as i32
}
extern "C" fn long_sum(a: i64, b: i64) -> i64 {
    a.wrapping_add(b)
}
extern "C" fn long_max(a: i64, b: i64) -> i64 {
    a.max(b)
}
extern "C" fn long_min(a: i64, b: i64) -> i64 {
    a.min(b)
}
extern "C" fn parse_long_radix(s: u64, radix: i32) -> i64 {
    if s == 0 {
        throw(
            "java/lang/NumberFormatException",
            Some("Cannot parse null string: null"),
        );
        return 0;
    }
    let text = String::from_utf16_lossy(units(s));
    let ok = !text.is_empty() && text != "+" && text != "-";
    match ok
        .then(|| i64::from_str_radix(&text, radix as u32).ok())
        .flatten()
    {
        Some(v) => v,
        None => {
            throw(
                "java/lang/NumberFormatException",
                Some(&format!("For input string: \"{text}\"")),
            );
            0
        }
    }
}
extern "C" fn parse_long(s: u64) -> i64 {
    parse_long_radix(s, 10)
}
pub extern "C" fn objects_equals(a: u64, b: u64) -> i32 {
    (a == b || (a != 0 && equals(a, b))) as i32
}
pub extern "C" fn objects_hash_code(o: u64) -> i32 {
    if o == 0 { 0 } else { hash_code(o) }
}

fn m(name: &'static str, desc: &'static str, f: usize) -> Method {
    Method {
        name,
        desc,
        is_static: false,
        f,
    }
}
fn s(name: &'static str, desc: &'static str, f: usize) -> Method {
    Method {
        name,
        desc,
        is_static: true,
        f,
    }
}

/// The built-in classes.
pub fn builtins() -> Vec<Builtin> {
    let object = "java/lang/Object";
    let mut out = vec![
        Builtin {
            name: object,
            super_name: None,
            interfaces: &[],
            size: 8,
            is_final: false,
            is_interface: false,
            methods: vec![
                m("<init>", "()V", object_init as *const () as usize),
                m(
                    "equals",
                    "(Ljava/lang/Object;)Z",
                    object_equals as *const () as usize,
                ),
                m("hashCode", "()I", identity_hash as *const () as usize),
                m(
                    "clone",
                    "()Ljava/lang/Object;",
                    object_clone as *const () as usize,
                ),
                m(
                    "toString",
                    "()Ljava/lang/String;",
                    object_to_string_c as *const () as usize,
                ),
                m(
                    "getClass",
                    "()Ljava/lang/Class;",
                    collections::object_get_class as *const () as usize,
                ),
            ],
        },
        Builtin {
            name: "java/lang/String",
            super_name: Some(object),
            interfaces: &["java/lang/CharSequence", "java/lang/Comparable"],
            size: S_DATA as i32,
            is_final: true,
            is_interface: false,
            methods: vec![
                m("length", "()I", string_length as *const () as usize),
                m("charAt", "(I)C", string_char_at as *const () as usize),
                m("isEmpty", "()Z", string_is_empty as *const () as usize),
                m(
                    "equals",
                    "(Ljava/lang/Object;)Z",
                    string_equals as *const () as usize,
                ),
                m("hashCode", "()I", string_hash as *const () as usize),
                m(
                    "toString",
                    "()Ljava/lang/String;",
                    string_to_string as *const () as usize,
                ),
                m(
                    "compareTo",
                    "(Ljava/lang/String;)I",
                    string_compare as *const () as usize,
                ),
                m(
                    "compareTo",
                    "(Ljava/lang/Object;)I",
                    string_compare as *const () as usize,
                ),
                m(
                    "contains",
                    "(Ljava/lang/CharSequence;)Z",
                    string_contains as *const () as usize,
                ),
                m(
                    "startsWith",
                    "(Ljava/lang/String;)Z",
                    string_starts_with as *const () as usize,
                ),
                m(
                    "endsWith",
                    "(Ljava/lang/String;)Z",
                    string_ends_with as *const () as usize,
                ),
                m(
                    "indexOf",
                    "(I)I",
                    string_index_of_char as *const () as usize,
                ),
                m(
                    "indexOf",
                    "(Ljava/lang/String;)I",
                    string_index_of as *const () as usize,
                ),
                m(
                    "substring",
                    "(I)Ljava/lang/String;",
                    string_substring as *const () as usize,
                ),
                m(
                    "substring",
                    "(II)Ljava/lang/String;",
                    string_substring2 as *const () as usize,
                ),
                m(
                    "trim",
                    "()Ljava/lang/String;",
                    string_trim as *const () as usize,
                ),
                m(
                    "equalsIgnoreCase",
                    "(Ljava/lang/String;)Z",
                    string_equals_ignore_case as *const () as usize,
                ),
                m(
                    "concat",
                    "(Ljava/lang/String;)Ljava/lang/String;",
                    string_concat as *const () as usize,
                ),
                m(
                    "replace",
                    "(CC)Ljava/lang/String;",
                    string_replace_char as *const () as usize,
                ),
                m(
                    "replace",
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Ljava/lang/String;",
                    string_replace as *const () as usize,
                ),
                s(
                    "valueOf",
                    "(Ljava/lang/Object;)Ljava/lang/String;",
                    string_value_of_object as *const () as usize,
                ),
                s(
                    "valueOf",
                    "(I)Ljava/lang/String;",
                    string_value_of_int as *const () as usize,
                ),
                s(
                    "valueOf",
                    "(J)Ljava/lang/String;",
                    string_value_of_long as *const () as usize,
                ),
                s(
                    "valueOf",
                    "(F)Ljava/lang/String;",
                    string_value_of_float as *const () as usize,
                ),
                s(
                    "valueOf",
                    "(D)Ljava/lang/String;",
                    string_value_of_double as *const () as usize,
                ),
                s(
                    "valueOf",
                    "(Z)Ljava/lang/String;",
                    string_value_of_bool as *const () as usize,
                ),
                s(
                    "valueOf",
                    "(C)Ljava/lang/String;",
                    string_value_of_char as *const () as usize,
                ),
            ],
        },
        Builtin {
            name: "java/lang/StringBuilder",
            super_name: Some(object),
            interfaces: &["java/lang/CharSequence"],
            size: 24,
            is_final: true,
            is_interface: false,
            methods: vec![
                m("<init>", "()V", sb_init as *const () as usize),
                m("<init>", "(I)V", sb_init_cap as *const () as usize),
                m(
                    "<init>",
                    "(Ljava/lang/String;)V",
                    sb_init_str as *const () as usize,
                ),
                m(
                    "append",
                    "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
                    sb_append_string as *const () as usize,
                ),
                m(
                    "append",
                    "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
                    sb_append_object as *const () as usize,
                ),
                m(
                    "append",
                    "(Ljava/lang/CharSequence;)Ljava/lang/StringBuilder;",
                    sb_append_object as *const () as usize,
                ),
                m(
                    "append",
                    "(I)Ljava/lang/StringBuilder;",
                    sb_append_int as *const () as usize,
                ),
                m(
                    "append",
                    "(J)Ljava/lang/StringBuilder;",
                    sb_append_long as *const () as usize,
                ),
                m(
                    "append",
                    "(F)Ljava/lang/StringBuilder;",
                    sb_append_float as *const () as usize,
                ),
                m(
                    "append",
                    "(D)Ljava/lang/StringBuilder;",
                    sb_append_double as *const () as usize,
                ),
                m(
                    "append",
                    "(C)Ljava/lang/StringBuilder;",
                    sb_append_char as *const () as usize,
                ),
                m(
                    "append",
                    "(Z)Ljava/lang/StringBuilder;",
                    sb_append_bool as *const () as usize,
                ),
                m(
                    "toString",
                    "()Ljava/lang/String;",
                    sb_to_string as *const () as usize,
                ),
                m("length", "()I", sb_length as *const () as usize),
                m("charAt", "(I)C", sb_char_at as *const () as usize),
                m("setLength", "(I)V", sb_set_length as *const () as usize),
            ],
        },
        Builtin {
            name: "java/lang/Number",
            super_name: Some(object),
            interfaces: &[],
            size: 8,
            is_final: false,
            is_interface: false,
            methods: vec![],
        },
        Builtin {
            name: "java/lang/Record",
            super_name: Some(object),
            interfaces: &[],
            size: 8,
            is_final: false,
            is_interface: false,
            methods: vec![m("<init>", "()V", object_init as *const () as usize)],
        },
        Builtin {
            name: "java/lang/Throwable",
            super_name: Some(object),
            interfaces: &[],
            size: 24,
            is_final: false,
            is_interface: false,
            methods: vec![
                m("<init>", "()V", throwable_init as *const () as usize),
                m(
                    "<init>",
                    "(Ljava/lang/String;)V",
                    throwable_init_message as *const () as usize,
                ),
                m(
                    "<init>",
                    "(Ljava/lang/String;Ljava/lang/Throwable;)V",
                    throwable_init_message_cause as *const () as usize,
                ),
                m(
                    "<init>",
                    "(Ljava/lang/Throwable;)V",
                    throwable_init_cause as *const () as usize,
                ),
                m(
                    "getMessage",
                    "()Ljava/lang/String;",
                    throwable_message as *const () as usize,
                ),
                m(
                    "getLocalizedMessage",
                    "()Ljava/lang/String;",
                    throwable_message as *const () as usize,
                ),
                m(
                    "getCause",
                    "()Ljava/lang/Throwable;",
                    throwable_cause as *const () as usize,
                ),
                m(
                    "toString",
                    "()Ljava/lang/String;",
                    throwable_to_string as *const () as usize,
                ),
                m(
                    "addSuppressed",
                    "(Ljava/lang/Throwable;)V",
                    throwable_add_suppressed as *const () as usize,
                ),
            ],
        },
        Builtin {
            name: "java/util/Objects",
            super_name: Some(object),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                s(
                    "requireNonNull",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    objects_require_non_null as *const () as usize,
                ),
                s(
                    "requireNonNull",
                    "(Ljava/lang/Object;Ljava/lang/String;)Ljava/lang/Object;",
                    objects_require_non_null_message as *const () as usize,
                ),
                s(
                    "equals",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                    objects_equals as *const () as usize,
                ),
                s(
                    "hashCode",
                    "(Ljava/lang/Object;)I",
                    objects_hash_code as *const () as usize,
                ),
                s(
                    "hash",
                    "([Ljava/lang/Object;)I",
                    collections::arrays_hash_objects as *const () as usize,
                ),
                s(
                    "toString",
                    "(Ljava/lang/Object;)Ljava/lang/String;",
                    objects_to_string as *const () as usize,
                ),
                s(
                    "toString",
                    "(Ljava/lang/Object;Ljava/lang/String;)Ljava/lang/String;",
                    objects_to_string_or as *const () as usize,
                ),
                s(
                    "isNull",
                    "(Ljava/lang/Object;)Z",
                    objects_is_null as *const () as usize,
                ),
                s(
                    "nonNull",
                    "(Ljava/lang/Object;)Z",
                    objects_non_null as *const () as usize,
                ),
                s(
                    "requireNonNullElse",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    objects_require_non_null_else as *const () as usize,
                ),
            ],
        },
    ];
    // The boxes.
    let number = |int: usize, long: usize, float: usize, double: usize| {
        vec![
            m("intValue", "()I", int),
            m("longValue", "()J", long),
            m("floatValue", "()F", float),
            m("doubleValue", "()D", double),
        ]
    };
    let mut integer = number(
        small_as_int as *const () as usize,
        int_as_long as *const () as usize,
        int_as_float as *const () as usize,
        int_as_double as *const () as usize,
    );
    integer.extend([
        s(
            "valueOf",
            "(I)Ljava/lang/Integer;",
            integer_value_of as *const () as usize,
        ),
        m(
            "equals",
            "(Ljava/lang/Object;)Z",
            box_equals as *const () as usize,
        ),
        m("hashCode", "()I", int_hash as *const () as usize),
        m(
            "toString",
            "()Ljava/lang/String;",
            int_to_string as *const () as usize,
        ),
        m(
            "compareTo",
            "(Ljava/lang/Integer;)I",
            int_compare_to as *const () as usize,
        ),
        m(
            "compareTo",
            "(Ljava/lang/Object;)I",
            int_compare_to as *const () as usize,
        ),
        s("compare", "(II)I", int_compare as *const () as usize),
        s("sum", "(II)I", int_sum as *const () as usize),
        s("max", "(II)I", int_max as *const () as usize),
        s("min", "(II)I", int_min as *const () as usize),
        s(
            "toString",
            "(I)Ljava/lang/String;",
            int_static_to_string as *const () as usize,
        ),
        s("hashCode", "(I)I", int_hash_static as *const () as usize),
        s(
            "parseInt",
            "(Ljava/lang/String;)I",
            parse_int as *const () as usize,
        ),
        s(
            "parseInt",
            "(Ljava/lang/String;I)I",
            parse_int_radix as *const () as usize,
        ),
    ]);
    let mut long = number(
        long_as_int as *const () as usize,
        box_long as *const () as usize,
        long_as_float as *const () as usize,
        long_as_double as *const () as usize,
    );
    long.extend([
        s(
            "valueOf",
            "(J)Ljava/lang/Long;",
            long_value_of as *const () as usize,
        ),
        m(
            "equals",
            "(Ljava/lang/Object;)Z",
            box_equals as *const () as usize,
        ),
        m("hashCode", "()I", long_hash as *const () as usize),
        m(
            "toString",
            "()Ljava/lang/String;",
            long_to_string as *const () as usize,
        ),
        m(
            "compareTo",
            "(Ljava/lang/Long;)I",
            long_compare_to as *const () as usize,
        ),
        m(
            "compareTo",
            "(Ljava/lang/Object;)I",
            long_compare_to as *const () as usize,
        ),
        s("compare", "(JJ)I", long_compare as *const () as usize),
    ]);
    let mut float = number(
        float_as_int as *const () as usize,
        float_as_long as *const () as usize,
        box_float as *const () as usize,
        float_as_double as *const () as usize,
    );
    float.extend([
        s(
            "valueOf",
            "(F)Ljava/lang/Float;",
            float_value_of as *const () as usize,
        ),
        m(
            "equals",
            "(Ljava/lang/Object;)Z",
            float_equals as *const () as usize,
        ),
        m("hashCode", "()I", float_hash as *const () as usize),
        m(
            "toString",
            "()Ljava/lang/String;",
            float_to_string as *const () as usize,
        ),
        m(
            "compareTo",
            "(Ljava/lang/Float;)I",
            float_compare_to as *const () as usize,
        ),
        m(
            "compareTo",
            "(Ljava/lang/Object;)I",
            float_compare_to as *const () as usize,
        ),
        m("isNaN", "()Z", float_box_is_nan as *const () as usize),
        m(
            "isInfinite",
            "()Z",
            float_box_is_infinite as *const () as usize,
        ),
        s(
            "floatToIntBits",
            "(F)I",
            float_to_int_bits as *const () as usize,
        ),
        s("compare", "(FF)I", float_compare as *const () as usize),
        s(
            "isInfinite",
            "(F)Z",
            float_is_infinite as *const () as usize,
        ),
        s("isFinite", "(F)Z", float_is_finite as *const () as usize),
        s("sum", "(FF)F", float_sum as *const () as usize),
        s("max", "(FF)F", float_max as *const () as usize),
        s("min", "(FF)F", float_min as *const () as usize),
        s("hashCode", "(F)I", float_hash_static as *const () as usize),
        s(
            "toString",
            "(F)Ljava/lang/String;",
            float_static_to_string as *const () as usize,
        ),
        s(
            "parseFloat",
            "(Ljava/lang/String;)F",
            parse_float as *const () as usize,
        ),
        s(
            "valueOf",
            "(Ljava/lang/String;)Ljava/lang/Float;",
            float_value_of_string as *const () as usize,
        ),
    ]);
    let mut double = number(
        double_as_int as *const () as usize,
        double_as_long as *const () as usize,
        double_as_float as *const () as usize,
        box_double as *const () as usize,
    );
    double.extend([
        s(
            "valueOf",
            "(D)Ljava/lang/Double;",
            double_value_of as *const () as usize,
        ),
        m(
            "equals",
            "(Ljava/lang/Object;)Z",
            double_equals as *const () as usize,
        ),
        m("hashCode", "()I", double_hash as *const () as usize),
        m(
            "toString",
            "()Ljava/lang/String;",
            double_to_string as *const () as usize,
        ),
        s(
            "parseDouble",
            "(Ljava/lang/String;)D",
            parse_double as *const () as usize,
        ),
        m(
            "compareTo",
            "(Ljava/lang/Double;)I",
            double_compare_to as *const () as usize,
        ),
        m(
            "compareTo",
            "(Ljava/lang/Object;)I",
            double_compare_to as *const () as usize,
        ),
        m("isNaN", "()Z", double_box_is_nan as *const () as usize),
        m(
            "isInfinite",
            "()Z",
            double_box_is_infinite as *const () as usize,
        ),
        s(
            "doubleToLongBits",
            "(D)J",
            double_to_long_bits as *const () as usize,
        ),
        s("compare", "(DD)I", double_compare as *const () as usize),
        s(
            "isInfinite",
            "(D)Z",
            double_is_infinite as *const () as usize,
        ),
        s("isFinite", "(D)Z", double_is_finite as *const () as usize),
        s("sum", "(DD)D", double_sum as *const () as usize),
        s("max", "(DD)D", double_max as *const () as usize),
        s("min", "(DD)D", double_min as *const () as usize),
        s("hashCode", "(D)I", double_hash_static as *const () as usize),
        s(
            "toString",
            "(D)Ljava/lang/String;",
            double_static_to_string as *const () as usize,
        ),
        s(
            "valueOf",
            "(Ljava/lang/String;)Ljava/lang/Double;",
            double_value_of_string as *const () as usize,
        ),
    ]);
    let small = |of: usize, as_int: &'static str| {
        let mut v = number(
            small_as_int as *const () as usize,
            int_as_long as *const () as usize,
            int_as_float as *const () as usize,
            int_as_double as *const () as usize,
        );
        v.extend([
            s("valueOf", as_int, of),
            m(
                "equals",
                "(Ljava/lang/Object;)Z",
                box_equals as *const () as usize,
            ),
            m("hashCode", "()I", int_hash as *const () as usize),
            m(
                "toString",
                "()Ljava/lang/String;",
                int_to_string as *const () as usize,
            ),
        ]);
        v
    };
    for (name, size, methods, is_number) in [
        ("java/lang/Integer", 16, integer, true),
        ("java/lang/Long", 16, long, true),
        ("java/lang/Float", 16, float, true),
        ("java/lang/Double", 16, double, true),
        (
            "java/lang/Short",
            16,
            small(short_value_of as *const () as usize, "(S)Ljava/lang/Short;"),
            true,
        ),
        (
            "java/lang/Byte",
            16,
            small(byte_value_of as *const () as usize, "(B)Ljava/lang/Byte;"),
            true,
        ),
        (
            "java/lang/Character",
            16,
            vec![
                s(
                    "valueOf",
                    "(C)Ljava/lang/Character;",
                    char_value_of as *const () as usize,
                ),
                m("charValue", "()C", small_as_int as *const () as usize),
                m(
                    "equals",
                    "(Ljava/lang/Object;)Z",
                    box_equals as *const () as usize,
                ),
                m("hashCode", "()I", int_hash as *const () as usize),
                m(
                    "toString",
                    "()Ljava/lang/String;",
                    char_to_string as *const () as usize,
                ),
            ],
            false,
        ),
        (
            "java/lang/Boolean",
            16,
            vec![
                s(
                    "valueOf",
                    "(Z)Ljava/lang/Boolean;",
                    bool_value_of as *const () as usize,
                ),
                m("booleanValue", "()Z", small_as_int as *const () as usize),
                m(
                    "equals",
                    "(Ljava/lang/Object;)Z",
                    box_equals as *const () as usize,
                ),
                m("hashCode", "()I", bool_hash as *const () as usize),
                m(
                    "toString",
                    "()Ljava/lang/String;",
                    bool_to_string as *const () as usize,
                ),
            ],
            false,
        ),
    ] {
        out.push(Builtin {
            name,
            super_name: Some(if is_number {
                "java/lang/Number"
            } else {
                object
            }),
            interfaces: &["java/lang/Comparable"],
            size,
            is_final: true,
            is_interface: false,
            methods,
        });
    }
    if let Some(object) = out.iter_mut().find(|b| b.name == "java/lang/Object") {
        object.methods.extend(threads::object_methods());
    }
    // StringBuffer: StringBuilder's methods (its locking is not needed by
    // code that uses one buffer on one thread, which is how mods use it).
    if let Some(sb) = out.iter().find(|b| b.name == "java/lang/StringBuilder") {
        let as_buffer = |d: &str| -> &'static str {
            Box::leak(
                d.replace("java/lang/StringBuilder", "java/lang/StringBuffer")
                    .into_boxed_str(),
            )
        };
        let methods = sb
            .methods
            .iter()
            .map(|m| Method {
                name: m.name,
                desc: as_buffer(m.desc),
                is_static: m.is_static,
                f: m.f,
            })
            .collect();
        let buffer = Builtin {
            name: "java/lang/StringBuffer",
            super_name: sb.super_name,
            interfaces: sb.interfaces,
            size: sb.size,
            is_final: true,
            is_interface: false,
            methods,
        };
        out.push(buffer);
    }
    // The boxes' other statics.
    let s = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: true,
        f,
    };
    for (class, extra) in [
        (
            "java/lang/Byte",
            vec![s("compare", "(BB)I", small_compare as *const () as usize)],
        ),
        (
            "java/lang/Short",
            vec![s("compare", "(SS)I", small_compare as *const () as usize)],
        ),
        (
            "java/lang/Integer",
            vec![
                s(
                    "toHexString",
                    "(I)Ljava/lang/String;",
                    int_to_hex as *const () as usize,
                ),
                s(
                    "toBinaryString",
                    "(I)Ljava/lang/String;",
                    int_to_binary as *const () as usize,
                ),
                s(
                    "toOctalString",
                    "(I)Ljava/lang/String;",
                    int_to_octal as *const () as usize,
                ),
            ],
        ),
        (
            "java/lang/Long",
            vec![
                s(
                    "toHexString",
                    "(J)Ljava/lang/String;",
                    long_to_hex as *const () as usize,
                ),
                s(
                    "toBinaryString",
                    "(J)Ljava/lang/String;",
                    long_to_binary as *const () as usize,
                ),
                s(
                    "toString",
                    "(J)Ljava/lang/String;",
                    long_static_to_string as *const () as usize,
                ),
                s("hashCode", "(J)I", long_hash_static as *const () as usize),
                s("sum", "(JJ)J", long_sum as *const () as usize),
                s("max", "(JJ)J", long_max as *const () as usize),
                s("min", "(JJ)J", long_min as *const () as usize),
                s(
                    "parseLong",
                    "(Ljava/lang/String;)J",
                    parse_long as *const () as usize,
                ),
                s(
                    "parseLong",
                    "(Ljava/lang/String;I)J",
                    parse_long_radix as *const () as usize,
                ),
            ],
        ),
    ] {
        if let Some(b) = out.iter_mut().find(|b| b.name == class) {
            // (Only what it does not have yet.)
            for m in extra {
                if !b
                    .methods
                    .iter()
                    .any(|x| x.name == m.name && x.desc == m.desc)
                {
                    b.methods.push(m);
                }
            }
        }
    }
    out.extend(collections::builtins());
    out.extend(collections::concurrent_builtins());
    out.extend(collections::stream_builtins());
    out.extend(logging::builtins());
    out.extend(threads::builtins());
    out.extend(system::builtins());
    out.extend(executors::builtins());
    out.extend(bitset::builtins());
    if let Some(system) = out.iter_mut().find(|b| b.name == "java/lang/System") {
        system.methods.extend(system::system_methods());
    }
    if let Some(string) = out.iter_mut().find(|b| b.name == "java/lang/String") {
        string.methods.extend(strings::string_methods());
    }
    if let Some(c) = out.iter_mut().find(|b| b.name == "java/lang/Character") {
        c.methods.extend(system::character_methods());
    }
    if let Some(math) = out.iter_mut().find(|b| b.name == "java/lang/Math") {
        math.methods.extend(system::math_methods());
    }
    out
}

/// The boxes a value of a primitive descriptor goes into, and back:
/// (box class, valueOf, unbox).
pub fn boxing(prim: &str) -> Option<(&'static str, usize, usize)> {
    Some(match prim {
        "I" => (
            "java/lang/Integer",
            integer_value_of as *const () as usize,
            box_int as *const () as usize,
        ),
        "J" => (
            "java/lang/Long",
            long_value_of as *const () as usize,
            box_long as *const () as usize,
        ),
        "F" => (
            "java/lang/Float",
            float_value_of as *const () as usize,
            box_float as *const () as usize,
        ),
        "D" => (
            "java/lang/Double",
            double_value_of as *const () as usize,
            box_double as *const () as usize,
        ),
        "Z" => (
            "java/lang/Boolean",
            bool_value_of as *const () as usize,
            box_int as *const () as usize,
        ),
        "C" => (
            "java/lang/Character",
            char_value_of as *const () as usize,
            box_int as *const () as usize,
        ),
        "S" => (
            "java/lang/Short",
            short_value_of as *const () as usize,
            box_int as *const () as usize,
        ),
        "B" => (
            "java/lang/Byte",
            byte_value_of as *const () as usize,
            box_int as *const () as usize,
        ),
        _ => return None,
    })
}

/// Makes the cached boxes, in memory that lives as long as the program.
pub fn make_caches(
    vtables: &HashMap<&'static str, u64>,
    keep: &mut Vec<Box<[u64]>>,
) -> HashMap<&'static str, Vec<u64>> {
    let mut out = HashMap::new();
    let mut make = |class: &'static str, values: Vec<u64>| {
        let v = values
            .into_iter()
            .map(|x| {
                let b: Box<[u64]> = vec![vtables[class], x].into_boxed_slice();
                let at = b.as_ptr() as u64;
                keep.push(b);
                at
            })
            .collect();
        out.insert(class, v);
    };
    let small: Vec<i64> = (-128..=127).collect();
    make(
        "java/lang/Integer",
        small.iter().map(|&i| i as i32 as u32 as u64).collect(),
    );
    make(
        "java/lang/Short",
        small.iter().map(|&i| i as i32 as u32 as u64).collect(),
    );
    make(
        "java/lang/Byte",
        small.iter().map(|&i| i as i32 as u32 as u64).collect(),
    );
    make("java/lang/Long", small.iter().map(|&i| i as u64).collect());
    make("java/lang/Character", (0..=127).collect());
    make("java/lang/Boolean", vec![0, 1]);
    out
}

/// Runtime helpers the compiler calls directly (string concatenation).
pub mod concat {
    pub use super::{
        sb_append_bool as append_bool, sb_append_char as append_char,
        sb_append_double as append_double, sb_append_float as append_float,
        sb_append_int as append_int, sb_append_long as append_long,
        sb_append_object as append_object, sb_append_string as append_string, sb_new as new,
        sb_to_string as finish,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_print_as_java_prints_them() {
        for (d, java) in [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (100.0, "100.0"),
            (1234567.0, "1234567.0"),
            (12345678.0, "1.2345678E7"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (1e21, "1.0E21"),
            (-2.5, "-2.5"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
            (-0.0, "-0.0"),
            (1.0 / 3.0, "0.3333333333333333"),
        ] {
            assert_eq!(java_double(d), java, "{d}");
        }
        assert_eq!(java_float(0.1), "0.1");
        assert_eq!(java_float(1.0e10), "1.0E10");
        assert_eq!(java_float(3.4028235e38), "3.4028235E38");
    }

    #[test]
    fn string_hashes_are_java_hashes() {
        let u: Vec<u16> = "hello".encode_utf16().collect();
        assert_eq!(java_hash(&u), 99162322);
        assert_eq!(java_hash(&[]), 0);
    }
}
