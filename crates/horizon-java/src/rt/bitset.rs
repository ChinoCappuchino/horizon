//! `java.util.BitSet` for the native backend's runtime library, written
//! against the JDK's public API: [vtable][words: a long[]], the words
//! growing as Java's do (to at least twice their number), so `size()`,
//! `length()`, `hashCode()`, `equals()` and `toString()` are Java's.
use super::{A_DATA, A_LEN, Builtin, Method, alloc, new_string, set_word, throw, vt, word};

const BITSET: &str = "java/util/BitSet";
const WORDS: usize = 8;

fn words_of(b: u64) -> u64 {
    // SAFETY: b is a BitSet.
    unsafe { word(b, WORDS) }
}
fn count(a: u64) -> usize {
    // SAFETY: a is a long[].
    unsafe { word(a, A_LEN) as usize }
}
fn slice<'a>(a: u64) -> &'a mut [u64] {
    // SAFETY: a is a long[]: its length, then its words.
    unsafe { std::slice::from_raw_parts_mut((a as usize + A_DATA) as *mut u64, count(a)) }
}
fn long_array(n: usize) -> u64 {
    let a = alloc(A_DATA + 8 * n, 8);
    // SAFETY: a fresh array.
    unsafe { set_word(a, A_LEN, n as u64) };
    a
}
fn words<'a>(b: u64) -> &'a mut [u64] {
    slice(words_of(b))
}
/// Room for word `index`, growing to at least twice the words (Java's).
fn ensure(b: u64, index: usize) {
    let old = words_of(b);
    let n = count(old);
    if index < n {
        return;
    }
    let grown = long_array((2 * n).max(index + 1));
    slice(grown)[..n].copy_from_slice(slice(old));
    // SAFETY: b is a BitSet.
    unsafe { set_word(b, WORDS, grown) };
}
fn negative(what: &str, v: i32) -> bool {
    if v < 0 {
        throw(
            "java/lang/IndexOutOfBoundsException",
            Some(&format!("{what} < 0: {v}")),
        );
    }
    v < 0
}
fn range(from: i32, to: i32) -> bool {
    if negative("fromIndex", from) || negative("toIndex", to) {
        return false;
    }
    if from > to {
        throw(
            "java/lang/IndexOutOfBoundsException",
            Some(&format!("fromIndex: {from} > toIndex: {to}")),
        );
        return false;
    }
    true
}

fn init_with(b: u64, bits: i32) {
    if bits < 0 {
        throw(
            "java/lang/NegativeArraySizeException",
            Some(&format!("nbits < 0: {bits}")),
        );
        return;
    }
    let n = (bits as usize).div_ceil(64);
    // SAFETY: b is a fresh BitSet.
    unsafe { set_word(b, WORDS, long_array(n)) };
}
extern "C" fn init(b: u64) {
    init_with(b, 64);
}
extern "C" fn init_bits(b: u64, bits: i32) {
    init_with(b, bits);
}

extern "C" fn get(b: u64, i: i32) -> i32 {
    if negative("bitIndex", i) {
        return 0;
    }
    let w = words(b);
    let k = i as usize / 64;
    (k < w.len() && w[k] & (1 << (i % 64)) != 0).into()
}
fn set_bit(b: u64, i: usize, on: bool) {
    let k = i / 64;
    if on {
        ensure(b, k);
        words(b)[k] |= 1 << (i % 64);
    } else if let Some(w) = words(b).get_mut(k) {
        *w &= !(1 << (i % 64));
    }
}
extern "C" fn set(b: u64, i: i32) {
    if !negative("bitIndex", i) {
        set_bit(b, i as usize, true);
    }
}
extern "C" fn set_to(b: u64, i: i32, on: i32) {
    if !negative("bitIndex", i) {
        set_bit(b, i as usize, on != 0);
    }
}
extern "C" fn clear_bit(b: u64, i: i32) {
    if !negative("bitIndex", i) {
        set_bit(b, i as usize, false);
    }
}
extern "C" fn flip(b: u64, i: i32) {
    if !negative("bitIndex", i) {
        let on = get(b, i) == 0;
        set_bit(b, i as usize, on);
    }
}
/// Bits [from, to): set, cleared or flipped, word by word.
fn each_range(b: u64, from: i32, to: i32, op: fn(&mut u64, u64)) {
    if !range(from, to) || from == to {
        return;
    }
    let (from, to) = (from as usize, to as usize);
    ensure(b, (to - 1) / 64);
    let w = words(b);
    for k in from / 64..=(to - 1) / 64 {
        let lo = if k == from / 64 { from % 64 } else { 0 };
        let hi = if k == (to - 1) / 64 {
            (to - 1) % 64
        } else {
            63
        };
        let mask = (u64::MAX >> (63 - hi)) & (u64::MAX << lo);
        op(&mut w[k], mask);
    }
}
extern "C" fn set_range(b: u64, from: i32, to: i32) {
    each_range(b, from, to, |w, m| *w |= m);
}
extern "C" fn set_range_to(b: u64, from: i32, to: i32, on: i32) {
    if on != 0 {
        set_range(b, from, to);
    } else {
        clear_range(b, from, to);
    }
}
extern "C" fn clear_range(b: u64, from: i32, to: i32) {
    each_range(b, from, to, |w, m| *w &= !m);
}
extern "C" fn flip_range(b: u64, from: i32, to: i32) {
    each_range(b, from, to, |w, m| *w ^= m);
}
extern "C" fn clear_all(b: u64) {
    words(b).fill(0);
}

extern "C" fn next_set_bit(b: u64, from: i32) -> i32 {
    if negative("fromIndex", from) {
        return -1;
    }
    let w = words(b);
    let mut k = from as usize / 64;
    if k >= w.len() {
        return -1;
    }
    let mut x = w[k] & (u64::MAX << (from % 64));
    loop {
        if x != 0 {
            return (k * 64 + x.trailing_zeros() as usize) as i32;
        }
        k += 1;
        if k == w.len() {
            return -1;
        }
        x = w[k];
    }
}
extern "C" fn next_clear_bit(b: u64, from: i32) -> i32 {
    if negative("fromIndex", from) {
        return -1;
    }
    let w = words(b);
    let mut k = from as usize / 64;
    if k >= w.len() {
        return from;
    }
    let mut x = !w[k] & (u64::MAX << (from % 64));
    loop {
        if x != 0 {
            return (k * 64 + x.trailing_zeros() as usize) as i32;
        }
        k += 1;
        if k == w.len() {
            return (k * 64) as i32;
        }
        x = !w[k];
    }
}
extern "C" fn previous_set_bit(b: u64, from: i32) -> i32 {
    if from < 0 {
        if from != -1 {
            negative("fromIndex", from);
        }
        return -1;
    }
    let w = words(b);
    let mut k = from as usize / 64;
    if k >= w.len() {
        return length(b) - 1;
    }
    let mut x = w[k] & (u64::MAX >> (63 - from % 64));
    loop {
        if x != 0 {
            return (k * 64 + 63 - x.leading_zeros() as usize) as i32;
        }
        if k == 0 {
            return -1;
        }
        k -= 1;
        x = w[k];
    }
}
extern "C" fn length(b: u64) -> i32 {
    let w = words(b);
    w.iter()
        .rposition(|&x| x != 0)
        .map_or(0, |k| (k * 64 + 64 - w[k].leading_zeros() as usize) as i32)
}
extern "C" fn size(b: u64) -> i32 {
    (words(b).len() * 64) as i32
}
extern "C" fn is_empty(b: u64) -> i32 {
    words(b).iter().all(|&x| x == 0).into()
}
extern "C" fn cardinality(b: u64) -> i32 {
    words(b).iter().map(|x| x.count_ones() as i32).sum()
}

/// `b op= other`, word by word (the missing words of the shorter as 0).
fn combine(b: u64, other: u64, op: fn(u64, u64) -> u64, grow: bool) {
    if other == 0 {
        throw("java/lang/NullPointerException", None);
        return;
    }
    let theirs: Vec<u64> = words(other).to_vec();
    if grow && !theirs.is_empty() {
        ensure(b, theirs.len() - 1);
    }
    for (k, w) in words(b).iter_mut().enumerate() {
        *w = op(*w, theirs.get(k).copied().unwrap_or(0));
    }
}
extern "C" fn and(b: u64, o: u64) {
    combine(b, o, |a, c| a & c, false);
}
extern "C" fn or(b: u64, o: u64) {
    combine(b, o, |a, c| a | c, true);
}
extern "C" fn xor(b: u64, o: u64) {
    combine(b, o, |a, c| a ^ c, true);
}
extern "C" fn and_not(b: u64, o: u64) {
    combine(b, o, |a, c| a & !c, false);
}
extern "C" fn intersects(b: u64, o: u64) -> i32 {
    words(b)
        .iter()
        .zip(words(o).iter())
        .any(|(a, c)| a & c != 0)
        .into()
}

extern "C" fn clone(b: u64) -> u64 {
    let c = alloc(16, vt(BITSET));
    let w = words(b);
    let a = long_array(w.len());
    slice(a).copy_from_slice(w);
    // SAFETY: c is a fresh BitSet.
    unsafe { set_word(c, WORDS, a) };
    c
}
/// The words up to the last nonzero one (Java compares and hashes these).
fn used(b: u64) -> &'static [u64] {
    let w = words(b);
    let n = w.iter().rposition(|&x| x != 0).map_or(0, |k| k + 1);
    &w[..n]
}
extern "C" fn equals(b: u64, o: u64) -> i32 {
    // SAFETY: o is an object (or null).
    (o != 0 && unsafe { word(o, 0) } == vt(BITSET) && used(b) == used(o)).into()
}
extern "C" fn hash_code(b: u64) -> i32 {
    let mut h: i64 = 1234;
    for (k, &w) in used(b).iter().enumerate().rev() {
        h ^= (w as i64).wrapping_mul(k as i64 + 1);
    }
    ((h >> 32) ^ h) as i32
}
extern "C" fn to_string(b: u64) -> u64 {
    let mut out = String::from("{");
    let mut i = next_set_bit(b, 0);
    while i >= 0 {
        if out.len() > 1 {
            out.push_str(", ");
        }
        out.push_str(&i.to_string());
        i = next_set_bit(b, i + 1);
    }
    out.push('}');
    new_string(&out.encode_utf16().collect::<Vec<_>>())
}
extern "C" fn to_long_array(b: u64) -> u64 {
    let w = used(b);
    let a = long_array(w.len());
    slice(a).copy_from_slice(w);
    a
}
extern "C" fn value_of(a: u64) -> u64 {
    let c = alloc(16, vt(BITSET));
    let src = slice(a);
    let n = src.iter().rposition(|&x| x != 0).map_or(0, |k| k + 1);
    let w = long_array(n);
    slice(w).copy_from_slice(&src[..n]);
    // SAFETY: c is a fresh BitSet.
    unsafe { set_word(c, WORDS, w) };
    c
}

pub fn builtins() -> Vec<Builtin> {
    let m = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: false,
        f,
    };
    macro_rules! f {
        ($f:expr) => {
            $f as *const () as usize
        };
    }
    vec![Builtin {
        name: BITSET,
        super_name: Some("java/lang/Object"),
        interfaces: &[],
        size: 16,
        is_final: true,
        is_interface: false,
        methods: vec![
            m("<init>", "()V", f!(init)),
            m("<init>", "(I)V", f!(init_bits)),
            m("get", "(I)Z", f!(get)),
            m("set", "(I)V", f!(set)),
            m("set", "(IZ)V", f!(set_to)),
            m("set", "(II)V", f!(set_range)),
            m("set", "(IIZ)V", f!(set_range_to)),
            m("clear", "(I)V", f!(clear_bit)),
            m("clear", "(II)V", f!(clear_range)),
            m("clear", "()V", f!(clear_all)),
            m("flip", "(I)V", f!(flip)),
            m("flip", "(II)V", f!(flip_range)),
            m("nextSetBit", "(I)I", f!(next_set_bit)),
            m("nextClearBit", "(I)I", f!(next_clear_bit)),
            m("previousSetBit", "(I)I", f!(previous_set_bit)),
            m("length", "()I", f!(length)),
            m("size", "()I", f!(size)),
            m("isEmpty", "()Z", f!(is_empty)),
            m("cardinality", "()I", f!(cardinality)),
            m("and", "(Ljava/util/BitSet;)V", f!(and)),
            m("or", "(Ljava/util/BitSet;)V", f!(or)),
            m("xor", "(Ljava/util/BitSet;)V", f!(xor)),
            m("andNot", "(Ljava/util/BitSet;)V", f!(and_not)),
            m("intersects", "(Ljava/util/BitSet;)Z", f!(intersects)),
            m("clone", "()Ljava/lang/Object;", f!(clone)),
            m("equals", "(Ljava/lang/Object;)Z", f!(equals)),
            m("hashCode", "()I", f!(hash_code)),
            m("toString", "()Ljava/lang/String;", f!(to_string)),
            m("toLongArray", "()[J", f!(to_long_array)),
            Method {
                name: "valueOf",
                desc: "([J)Ljava/util/BitSet;",
                is_static: true,
                f: f!(value_of),
            },
        ],
    }]
}
