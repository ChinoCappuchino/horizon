//! The collections, arrays, `Optional`, `Enum`, `Class` and `Math` methods
//! mods use most (docs/java-compat/compiler.md 4.4), with the behaviour code
//! can see kept as the JDK documents it: `HashMap`'s hash spreading, bucket
//! order and order-keeping resize (iteration order), `ArrayList`'s growth,
//! the exceptions and their messages, `ConcurrentModificationException`,
//! `toString` forms. Not yet: tree bins (a bucket of 8 or more keys with
//! colliding hashes in a table of 64 or more keeps its list order here).
use super::{
    A_DATA, A_LEN, Builtin, Method, alloc, ctx, equals, hash_code, instance_of, method, new_string,
    pending, rt, set_word, str_of, throw, to_string, units, vt, word,
};
use crate::heap::Context;

mod functions;
mod streams;

// ---- Helpers ---------------------------------------------------------------

/// An object array: element size 8 in its header.
/// A new reference array of `n` elements (null).
pub(super) fn object_array(n: usize) -> u64 {
    obj_array(n)
}
fn obj_array(n: usize) -> u64 {
    let a = alloc(A_DATA + 8 * n, 8);
    // SAFETY: just made, with room for n elements.
    unsafe { set_word(a, A_LEN, n as u64) };
    a
}
fn len(a: u64) -> usize {
    if a == 0 {
        return 0;
    }
    // SAFETY: an array's length.
    unsafe { word(a, A_LEN) as usize }
}
fn get(a: u64, i: usize) -> u64 {
    // SAFETY: the caller checked i < len(a).
    unsafe { word(a, A_DATA + 8 * i) }
}
fn put(a: u64, i: usize, v: u64) {
    // SAFETY: as above.
    unsafe { set_word(a, A_DATA + 8 * i, v) }
}
fn field(o: u64, at: usize) -> u64 {
    // SAFETY: the caller has an object with a word at `at`.
    unsafe { word(o, at) }
}
fn set(o: u64, at: usize, v: u64) {
    // SAFETY: as above.
    unsafe { set_word(o, at, v) }
}
fn new_of(class: &str, size: usize) -> u64 {
    alloc(size, vt(class))
}
fn objects_equals(a: u64, b: u64) -> bool {
    a == b || (a != 0 && equals(a, b))
}
fn objects_hash(o: u64) -> i32 {
    if o == 0 { 0 } else { hash_code(o) }
}
fn npe() {
    throw("java/lang/NullPointerException", None);
}
fn index_oob(i: i64, n: usize) {
    throw(
        "java/lang/IndexOutOfBoundsException",
        Some(&format!("Index {i} out of bounds for length {n}")),
    );
}
fn unsupported() {
    throw("java/lang/UnsupportedOperationException", None);
}
fn missing(o: u64, what: &str) {
    if o == 0 {
        npe();
    } else {
        throw(
            "java/lang/UnsupportedOperationException",
            Some(&format!("{what} is not compiled for this object")),
        );
    }
}

// Calls into the program (lambdas, its collections' methods), by name.
fn call0(o: u64, nd: &str) -> u64 {
    match method(o, nd) {
        Some(f) => {
            // SAFETY: the selector's slot holds a method of this shape.
            let f: extern "C" fn(*mut Context, u64) -> u64 = unsafe { std::mem::transmute(f) };
            f(ctx(), o)
        }
        None => {
            missing(o, nd);
            0
        }
    }
}
fn call0_i(o: u64, nd: &str) -> i32 {
    match method(o, nd) {
        Some(f) => {
            // SAFETY: as above.
            let f: extern "C" fn(*mut Context, u64) -> i32 = unsafe { std::mem::transmute(f) };
            f(ctx(), o)
        }
        None => {
            missing(o, nd);
            0
        }
    }
}
fn call1(o: u64, nd: &str, a: u64) -> u64 {
    match method(o, nd) {
        Some(f) => {
            // SAFETY: as above.
            let f: extern "C" fn(*mut Context, u64, u64) -> u64 = unsafe { std::mem::transmute(f) };
            f(ctx(), o, a)
        }
        None => {
            missing(o, nd);
            0
        }
    }
}
fn call1_i(o: u64, nd: &str, a: u64) -> i32 {
    match method(o, nd) {
        Some(f) => {
            // SAFETY: as above.
            let f: extern "C" fn(*mut Context, u64, u64) -> i32 = unsafe { std::mem::transmute(f) };
            f(ctx(), o, a)
        }
        None => {
            missing(o, nd);
            0
        }
    }
}
/// A method of `o` taking one reference and returning a double.
fn call1_d(o: u64, nd: &str, a: u64) -> f64 {
    match method(o, nd) {
        Some(f) => {
            // SAFETY: the selector's slot holds a method of this shape.
            let f: extern "C" fn(*mut Context, u64, u64) -> f64 = unsafe { std::mem::transmute(f) };
            f(ctx(), o, a)
        }
        None => {
            missing(o, nd);
            0.0
        }
    }
}
fn call2(o: u64, nd: &str, a: u64, b: u64) -> u64 {
    match method(o, nd) {
        Some(f) => {
            // SAFETY: as above.
            let f: extern "C" fn(*mut Context, u64, u64, u64) -> u64 =
                unsafe { std::mem::transmute(f) };
            f(ctx(), o, a, b)
        }
        None => {
            missing(o, nd);
            0
        }
    }
}
fn call2_i(o: u64, nd: &str, a: u64, b: u64) -> i32 {
    match method(o, nd) {
        Some(f) => {
            // SAFETY: as above.
            let f: extern "C" fn(*mut Context, u64, u64, u64) -> i32 =
                unsafe { std::mem::transmute(f) };
            f(ctx(), o, a, b)
        }
        None => {
            missing(o, nd);
            0
        }
    }
}
const ACCEPT: &str = "accept (Ljava/lang/Object;)V";
const ACCEPT2: &str = "accept (Ljava/lang/Object;Ljava/lang/Object;)V";
const APPLY: &str = "apply (Ljava/lang/Object;)Ljava/lang/Object;";
const APPLY2: &str = "apply (Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;";
const TEST: &str = "test (Ljava/lang/Object;)Z";
const SUPPLY: &str = "get ()Ljava/lang/Object;";
const ITERATOR: &str = "iterator ()Ljava/util/Iterator;";
const HAS_NEXT: &str = "hasNext ()Z";
const NEXT: &str = "next ()Ljava/lang/Object;";
const SIZE: &str = "size ()I";
const COMPARE_TO: &str = "compareTo (Ljava/lang/Object;)I";
const COMPARE: &str = "compare (Ljava/lang/Object;Ljava/lang/Object;)I";

/// The elements of any Iterable, in its iteration order (`String.join`).
pub(super) fn iterate(it: u64) -> Option<Vec<u64>> {
    elements(it)
}

/// The elements of any Collection, in its iteration order.
fn elements(c: u64) -> Option<Vec<u64>> {
    if c == 0 {
        npe();
        return None;
    }
    if is(c, "java/util/ArrayList") {
        let n = field(c, AL_SIZE) as usize;
        let a = field(c, AL_DATA);
        return Some((0..n).map(|i| get(a, i)).collect());
    }
    let it = call0(c, ITERATOR);
    let mut out = Vec::new();
    while !pending() && call0_i(it, HAS_NEXT) != 0 {
        let e = call0(it, NEXT);
        if pending() {
            return None;
        }
        out.push(e);
    }
    (!pending()).then_some(out)
}
fn is(o: u64, class: &str) -> bool {
    // SAFETY: an object's vtable.
    o != 0 && unsafe { word(o, 0) } == vt(class)
}

/// AbstractCollection.toString: [a, b], the collection itself as
/// "(this Collection)".
fn collection_string(c: u64, items: &[u64]) -> u64 {
    let mut out: Vec<u16> = vec![u16::from(b'[')];
    for (k, &e) in items.iter().enumerate() {
        if k > 0 {
            out.extend(", ".encode_utf16());
        }
        if e == c {
            out.extend("(this Collection)".encode_utf16());
        } else {
            let s = to_string(e);
            if pending() {
                return 0;
            }
            out.extend_from_slice(units(s));
        }
    }
    out.push(u16::from(b']'));
    new_string(&out)
}
/// AbstractList.hashCode.
fn list_hash(items: &[u64]) -> i32 {
    items.iter().fold(1i32, |h, &e| {
        h.wrapping_mul(31).wrapping_add(objects_hash(e))
    })
}
/// AbstractList.equals against any List.
fn list_equals(me: u64, items: &[u64], o: u64) -> i32 {
    if o == me {
        return 1;
    }
    if !instance_of(o, "java/util/List") {
        return 0;
    }
    let Some(other) = elements(o) else { return 0 };
    (items.len() == other.len()
        && items
            .iter()
            .zip(&other)
            .all(|(a, b)| objects_equals(*a, *b))) as i32
}

// ---- ArrayList ---------------------------------------------------------------

const AL_DATA: usize = 8;
const AL_SIZE: usize = 16;
const AL_MOD: usize = 24;
const ARRAY_LIST: &str = "java/util/ArrayList";

fn al_size(l: u64) -> usize {
    field(l, AL_SIZE) as usize
}
fn al_items(l: u64) -> Vec<u64> {
    let a = field(l, AL_DATA);
    (0..al_size(l)).map(|i| get(a, i)).collect()
}
fn al_modified(l: u64) {
    set(l, AL_MOD, field(l, AL_MOD) + 1);
}
/// Room for `min` elements: 10 at first, then 1.5 times (Java's growth).
fn al_grow(l: u64, min: usize) {
    let a = field(l, AL_DATA);
    let cap = len(a);
    if min <= cap {
        return;
    }
    let new_cap = if a == 0 {
        min.max(10)
    } else {
        (cap + (cap >> 1)).max(min)
    };
    let b = obj_array(new_cap);
    for i in 0..al_size(l) {
        put(b, i, get(a, i));
    }
    set(l, AL_DATA, b);
}
extern "C" fn al_init(_l: u64) {}

/// A new, empty `ArrayList` (for the runtime's own results).
pub(super) fn new_array_list() -> u64 {
    let l = alloc(32, vt(ARRAY_LIST));
    al_init(l);
    l
}
extern "C" fn al_init_cap(l: u64, cap: i32) {
    if cap < 0 {
        throw(
            "java/lang/IllegalArgumentException",
            Some(&format!("Illegal Capacity: {cap}")),
        );
        return;
    }
    if cap > 0 {
        set(l, AL_DATA, obj_array(cap as usize));
    }
}
extern "C" fn al_init_from(l: u64, c: u64) {
    if let Some(items) = elements(c)
        && !items.is_empty()
    {
        let a = obj_array(items.len());
        for (i, e) in items.iter().enumerate() {
            put(a, i, *e);
        }
        set(l, AL_DATA, a);
        set(l, AL_SIZE, items.len() as u64);
    }
}
pub extern "C" fn al_add(l: u64, e: u64) -> i32 {
    al_modified(l);
    let n = al_size(l);
    al_grow(l, n + 1);
    put(field(l, AL_DATA), n, e);
    set(l, AL_SIZE, (n + 1) as u64);
    1
}
extern "C" fn al_add_at(l: u64, i: i32, e: u64) {
    let n = al_size(l);
    if i < 0 || i as usize > n {
        throw(
            "java/lang/IndexOutOfBoundsException",
            Some(&format!("Index: {i}, Size: {n}")),
        );
        return;
    }
    al_modified(l);
    al_grow(l, n + 1);
    let a = field(l, AL_DATA);
    for k in (i as usize..n).rev() {
        put(a, k + 1, get(a, k));
    }
    put(a, i as usize, e);
    set(l, AL_SIZE, (n + 1) as u64);
}
extern "C" fn al_get(l: u64, i: i32) -> u64 {
    let n = al_size(l);
    if i < 0 || i as usize >= n {
        index_oob(i64::from(i), n);
        return 0;
    }
    get(field(l, AL_DATA), i as usize)
}
extern "C" fn al_set(l: u64, i: i32, e: u64) -> u64 {
    let n = al_size(l);
    if i < 0 || i as usize >= n {
        index_oob(i64::from(i), n);
        return 0;
    }
    let a = field(l, AL_DATA);
    let old = get(a, i as usize);
    put(a, i as usize, e);
    old
}
fn al_remove_index(l: u64, i: usize) -> u64 {
    al_modified(l);
    let a = field(l, AL_DATA);
    let n = al_size(l);
    let old = get(a, i);
    for k in i..n - 1 {
        put(a, k, get(a, k + 1));
    }
    put(a, n - 1, 0);
    set(l, AL_SIZE, (n - 1) as u64);
    old
}
extern "C" fn al_remove_at(l: u64, i: i32) -> u64 {
    let n = al_size(l);
    if i < 0 || i as usize >= n {
        index_oob(i64::from(i), n);
        return 0;
    }
    al_remove_index(l, i as usize)
}
fn index_in(items: &[u64], o: u64) -> Option<usize> {
    items
        .iter()
        .position(|&e| if o == 0 { e == 0 } else { equals(o, e) })
}
extern "C" fn al_remove_object(l: u64, o: u64) -> i32 {
    match index_in(&al_items(l), o) {
        Some(i) if !pending() => {
            al_remove_index(l, i);
            1
        }
        _ => 0,
    }
}
extern "C" fn al_clear(l: u64) {
    al_modified(l);
    let a = field(l, AL_DATA);
    for i in 0..al_size(l) {
        put(a, i, 0);
    }
    set(l, AL_SIZE, 0);
}
extern "C" fn al_size_c(l: u64) -> i32 {
    al_size(l) as i32
}
extern "C" fn al_is_empty(l: u64) -> i32 {
    (al_size(l) == 0) as i32
}
extern "C" fn al_contains(l: u64, o: u64) -> i32 {
    index_in(&al_items(l), o).is_some() as i32
}
extern "C" fn al_index_of(l: u64, o: u64) -> i32 {
    index_in(&al_items(l), o).map_or(-1, |i| i as i32)
}
extern "C" fn al_add_all(l: u64, c: u64) -> i32 {
    let Some(items) = elements(c) else { return 0 };
    al_modified(l);
    let n = al_size(l);
    al_grow(l, n + items.len());
    let a = field(l, AL_DATA);
    for (k, e) in items.iter().enumerate() {
        put(a, n + k, *e);
    }
    set(l, AL_SIZE, (n + items.len()) as u64);
    (!items.is_empty()) as i32
}
extern "C" fn al_for_each(l: u64, f: u64) {
    if f == 0 {
        npe();
        return;
    }
    let expected = field(l, AL_MOD);
    let n = al_size(l);
    let mut i = 0;
    while i < n && field(l, AL_MOD) == expected && !pending() {
        call1(f, ACCEPT, get(field(l, AL_DATA), i));
        i += 1;
    }
    if !pending() && field(l, AL_MOD) != expected {
        throw("java/util/ConcurrentModificationException", None);
    }
}
extern "C" fn al_remove_if(l: u64, p: u64) -> i32 {
    if p == 0 {
        npe();
        return 0;
    }
    let items = al_items(l);
    let mut keep = Vec::new();
    for &e in &items {
        let t = call1_i(p, TEST, e);
        if pending() {
            return 0;
        }
        if t == 0 {
            keep.push(e);
        }
    }
    if keep.len() == items.len() {
        return 0;
    }
    al_modified(l);
    let a = field(l, AL_DATA);
    for (i, e) in keep.iter().enumerate() {
        put(a, i, *e);
    }
    for i in keep.len()..items.len() {
        put(a, i, 0);
    }
    set(l, AL_SIZE, keep.len() as u64);
    1
}
extern "C" fn al_to_string(l: u64) -> u64 {
    collection_string(l, &al_items(l))
}
extern "C" fn al_equals(l: u64, o: u64) -> i32 {
    list_equals(l, &al_items(l), o)
}
extern "C" fn al_hash(l: u64) -> i32 {
    list_hash(&al_items(l))
}
extern "C" fn al_to_array(l: u64) -> u64 {
    let items = al_items(l);
    let a = obj_array(items.len());
    for (i, e) in items.iter().enumerate() {
        put(a, i, *e);
    }
    a
}
/// A stable sort by a comparator (or natural order): Java's sorts of
/// objects are stable, so any stable sort gives the same order.
fn sort_items(items: &mut [u64], cmp: u64) {
    let mut failed = false;
    items.sort_by(|&a, &b| {
        if failed || pending() {
            failed = true;
            return std::cmp::Ordering::Equal;
        }
        let r = if cmp == 0 {
            if a == 0 {
                npe();
                0
            } else {
                call1_i(a, COMPARE_TO, b)
            }
        } else {
            call2_i(cmp, COMPARE, a, b)
        };
        r.cmp(&0)
    });
}
extern "C" fn al_sort(l: u64, cmp: u64) {
    let mut items = al_items(l);
    sort_items(&mut items, cmp);
    if pending() {
        return;
    }
    let a = field(l, AL_DATA);
    for (i, e) in items.iter().enumerate() {
        put(a, i, *e);
    }
    al_modified(l);
}

// ArrayList's iterator: the list, the cursor, the last returned (-1: none),
// the modification count it expects.
const IT_LIST: usize = 8;
const IT_CURSOR: usize = 16;
const IT_LAST: usize = 24;
const IT_EXPECT: usize = 32;
extern "C" fn al_iterator(l: u64) -> u64 {
    let it = new_of("java/util/ArrayList$Itr", 40);
    set(it, IT_LIST, l);
    set(it, IT_LAST, u64::MAX);
    set(it, IT_EXPECT, field(l, AL_MOD));
    it
}
extern "C" fn itr_has_next(it: u64) -> i32 {
    (field(it, IT_CURSOR) as usize != al_size(field(it, IT_LIST))) as i32
}
extern "C" fn itr_next(it: u64) -> u64 {
    let l = field(it, IT_LIST);
    if field(l, AL_MOD) != field(it, IT_EXPECT) {
        throw("java/util/ConcurrentModificationException", None);
        return 0;
    }
    let i = field(it, IT_CURSOR) as usize;
    if i >= al_size(l) {
        throw("java/util/NoSuchElementException", None);
        return 0;
    }
    set(it, IT_CURSOR, (i + 1) as u64);
    set(it, IT_LAST, i as u64);
    get(field(l, AL_DATA), i)
}
extern "C" fn itr_remove(it: u64) {
    let last = field(it, IT_LAST);
    if last == u64::MAX {
        throw("java/lang/IllegalStateException", None);
        return;
    }
    let l = field(it, IT_LIST);
    if field(l, AL_MOD) != field(it, IT_EXPECT) {
        throw("java/util/ConcurrentModificationException", None);
        return;
    }
    al_remove_index(l, last as usize);
    set(it, IT_CURSOR, last);
    set(it, IT_LAST, u64::MAX);
    set(it, IT_EXPECT, field(l, AL_MOD));
}

// ---- Fixed lists (List.of, Arrays.asList, the empty list) ------------------
//
// [vtable][array][writable: Arrays.asList's set writes through].

const FL_ARRAY: usize = 8;
const LIST_N: &str = "java/util/ImmutableCollections$ListN";
const AS_LIST: &str = "java/util/Arrays$ArrayList";

fn fixed_items(l: u64) -> Vec<u64> {
    let a = field(l, FL_ARRAY);
    (0..len(a)).map(|i| get(a, i)).collect()
}
fn fixed_list(class: &str, items: &[u64]) -> u64 {
    let a = obj_array(items.len());
    for (i, e) in items.iter().enumerate() {
        put(a, i, *e);
    }
    let l = new_of(class, 16);
    set(l, FL_ARRAY, a);
    l
}
/// List.of(...): no nulls.
fn list_of(items: &[u64]) -> u64 {
    if items.contains(&0) {
        npe();
        return 0;
    }
    fixed_list(LIST_N, items)
}
// ---- Collections' algorithms --------------------------------------------------------

/// The greatest (`sign` 1) or least (-1) element by `cmp` (natural order
/// when 0): the first of equals wins, as Java's does.
fn extreme(c: u64, cmp: u64, sign: i32) -> u64 {
    let Some(items) = elements(c) else { return 0 };
    let Some((&first, rest)) = items.split_first() else {
        throw("java/util/NoSuchElementException", None);
        return 0;
    };
    let mut best = first;
    for &e in rest {
        let r = if cmp == 0 {
            call1_i(e, COMPARE_TO, best)
        } else {
            call2_i(cmp, COMPARE, e, best)
        };
        if pending() {
            return 0;
        }
        if (sign > 0 && r > 0) || (sign < 0 && r < 0) {
            best = e;
        }
    }
    best
}
extern "C" fn collections_max(c: u64) -> u64 {
    extreme(c, 0, 1)
}
extern "C" fn collections_max_by(c: u64, cmp: u64) -> u64 {
    extreme(c, cmp, 1)
}
extern "C" fn collections_min(c: u64) -> u64 {
    extreme(c, 0, -1)
}
extern "C" fn collections_min_by(c: u64, cmp: u64) -> u64 {
    extreme(c, cmp, -1)
}
/// `Collections.sort(list[, cmp])`: the list's own `sort`.
extern "C" fn collections_sort(l: u64) {
    call1(l, "sort (Ljava/util/Comparator;)V", 0);
}
extern "C" fn collections_sort_by(l: u64, cmp: u64) {
    call1(l, "sort (Ljava/util/Comparator;)V", cmp);
}
extern "C" fn collections_reverse(l: u64) {
    let Some(items) = elements(l) else { return };
    for (i, &e) in items.iter().rev().enumerate() {
        call2(
            l,
            "set (ILjava/lang/Object;)Ljava/lang/Object;",
            i as u64,
            e,
        );
        if pending() {
            return;
        }
    }
}
extern "C" fn collections_frequency(c: u64, o: u64) -> i32 {
    elements(c).map_or(0, |items| {
        items.iter().filter(|&&e| objects_equals(e, o)).count() as i32
    })
}
extern "C" fn collections_singleton_list(e: u64) -> u64 {
    let l = new_array_list();
    al_add(l, e);
    l
}
extern "C" fn collections_singleton(e: u64) -> u64 {
    let set = new_of("java/util/HashSet", 16);
    hs_init(set);
    hs_add(set, e);
    set
}
extern "C" fn collections_singleton_map(k: u64, v: u64) -> u64 {
    let m = new_hash_map();
    hm_put_c(m, k, v);
    m
}
extern "C" fn collections_n_copies(n: i32, e: u64) -> u64 {
    if n < 0 {
        throw(
            "java/lang/IllegalArgumentException",
            Some(&format!("List length = {n}")),
        );
        return 0;
    }
    let l = new_array_list();
    for _ in 0..n {
        al_add(l, e);
    }
    l
}
/// The `unmodifiable*` views: the collection itself (code that doesn't
/// try to change them sees no difference).
extern "C" fn collections_same(c: u64) -> u64 {
    if c == 0 {
        npe();
    }
    c
}

extern "C" fn list_of0() -> u64 {
    once("emptyList", || fixed_list(LIST_N, &[]))
}
extern "C" fn list_of1(a: u64) -> u64 {
    list_of(&[a])
}
extern "C" fn list_of2(a: u64, b: u64) -> u64 {
    list_of(&[a, b])
}
extern "C" fn list_of3(a: u64, b: u64, c: u64) -> u64 {
    list_of(&[a, b, c])
}
extern "C" fn list_of4(a: u64, b: u64, c: u64, d: u64) -> u64 {
    list_of(&[a, b, c, d])
}
extern "C" fn list_of5(a: u64, b: u64, c: u64, d: u64, e: u64) -> u64 {
    list_of(&[a, b, c, d, e])
}
extern "C" fn list_of6(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> u64 {
    list_of(&[a, b, c, d, e, f])
}
extern "C" fn list_of_array(arr: u64) -> u64 {
    if arr == 0 {
        npe();
        return 0;
    }
    list_of(&(0..len(arr)).map(|i| get(arr, i)).collect::<Vec<_>>())
}
extern "C" fn list_copy_of(c: u64) -> u64 {
    match elements(c) {
        Some(items) => list_of(&items),
        None => 0,
    }
}
extern "C" fn arrays_as_list(arr: u64) -> u64 {
    if arr == 0 {
        npe();
        return 0;
    }
    let l = new_of(AS_LIST, 16);
    // The same array: writes go through to it.
    set(l, FL_ARRAY, arr);
    l
}
extern "C" fn fl_get(l: u64, i: i32) -> u64 {
    let a = field(l, FL_ARRAY);
    if i < 0 || i as usize >= len(a) {
        index_oob(i64::from(i), len(a));
        return 0;
    }
    get(a, i as usize)
}
extern "C" fn as_list_set(l: u64, i: i32, e: u64) -> u64 {
    let a = field(l, FL_ARRAY);
    if i < 0 || i as usize >= len(a) {
        index_oob(i64::from(i), len(a));
        return 0;
    }
    let old = get(a, i as usize);
    put(a, i as usize, e);
    old
}
extern "C" fn fl_size(l: u64) -> i32 {
    len(field(l, FL_ARRAY)) as i32
}
extern "C" fn fl_is_empty(l: u64) -> i32 {
    (len(field(l, FL_ARRAY)) == 0) as i32
}
extern "C" fn fl_contains(l: u64, o: u64) -> i32 {
    index_in(&fixed_items(l), o).is_some() as i32
}
extern "C" fn fl_index_of(l: u64, o: u64) -> i32 {
    index_in(&fixed_items(l), o).map_or(-1, |i| i as i32)
}
extern "C" fn fl_to_string(l: u64) -> u64 {
    collection_string(l, &fixed_items(l))
}
extern "C" fn fl_equals(l: u64, o: u64) -> i32 {
    list_equals(l, &fixed_items(l), o)
}
extern "C" fn fl_hash(l: u64) -> i32 {
    list_hash(&fixed_items(l))
}
extern "C" fn fl_for_each(l: u64, f: u64) {
    if f == 0 {
        npe();
        return;
    }
    for e in fixed_items(l) {
        call1(f, ACCEPT, e);
        if pending() {
            return;
        }
    }
}
extern "C" fn fl_iterator(l: u64) -> u64 {
    let it = new_of("java/util/ImmutableCollections$ListItr", 24);
    set(it, IT_LIST, field(l, FL_ARRAY));
    it
}
extern "C" fn fl_to_array(l: u64) -> u64 {
    let items = fixed_items(l);
    let a = obj_array(items.len());
    for (i, e) in items.iter().enumerate() {
        put(a, i, *e);
    }
    a
}
extern "C" fn arr_it_has_next(it: u64) -> i32 {
    ((field(it, IT_CURSOR) as usize) < len(field(it, IT_LIST))) as i32
}
extern "C" fn arr_it_next(it: u64) -> u64 {
    let a = field(it, IT_LIST);
    let i = field(it, IT_CURSOR) as usize;
    if i >= len(a) {
        throw("java/util/NoSuchElementException", None);
        return 0;
    }
    set(it, IT_CURSOR, (i + 1) as u64);
    get(a, i)
}
extern "C" fn uoe_bool(_l: u64, _o: u64) -> i32 {
    unsupported();
    0
}
extern "C" fn uoe_bool_2(_l: u64, _i: i32, _o: u64) {
    unsupported();
}
extern "C" fn uoe_set(_l: u64, _i: i32, _o: u64) -> u64 {
    unsupported();
    0
}
extern "C" fn uoe_remove_at(_l: u64, _i: i32) -> u64 {
    unsupported();
    0
}
extern "C" fn uoe_void(_l: u64) {
    unsupported();
}
extern "C" fn uoe_put(_m: u64, _k: u64, _v: u64) -> u64 {
    unsupported();
    0
}
extern "C" fn uoe_remove(_m: u64, _k: u64) -> u64 {
    unsupported();
    0
}

/// The objects `once` is making, by which thread, and their condition.
static MAKING: std::sync::LazyLock<(
    std::sync::Mutex<std::collections::HashMap<&'static str, std::thread::ThreadId>>,
    std::sync::Condvar,
)> = std::sync::LazyLock::new(Default::default);

/// An object the runtime makes once: one thread makes it while the others
/// wait for it, and every caller gets the same object. No lock is held
/// while it is made, so `make` may make other such objects.
pub(super) fn once(name: &'static str, make: impl FnOnce() -> u64) -> u64 {
    let (making, done) = &*MAKING;
    let me = std::thread::current().id();
    {
        let mut busy = making.lock().unwrap();
        loop {
            if let Some(&o) = rt().once.lock().unwrap().get(name) {
                return o;
            }
            match busy.get(name) {
                None => {
                    busy.insert(name, me);
                    break;
                }
                Some(&t) if t == me => panic!("{name} is made from its own making"),
                Some(_) => busy = done.wait(busy).unwrap(),
            }
        }
    }
    let o = make();
    rt().once.lock().unwrap().insert(name, o);
    making.lock().unwrap().remove(name);
    done.notify_all();
    o
}

// ---- HashMap ------------------------------------------------------------------
//
// [vtable][table: Node[]][size][modCount][threshold]; a Node is
// [vtable][hash][key][value][next].

const HM_TABLE: usize = 8;
const HM_SIZE: usize = 16;
const HM_MOD: usize = 24;
const HM_THRESHOLD: usize = 32;
const N_HASH: usize = 8;
const N_KEY: usize = 16;
const N_VALUE: usize = 24;
const N_NEXT: usize = 32;
const HASH_MAP: &str = "java/util/HashMap";
const NODE: &str = "java/util/HashMap$Node";
const MAX_CAPACITY: usize = 1 << 30;

/// Java's `HashMap.hash`: the high bits spread into the low.
fn spread(key: u64) -> i32 {
    if key == 0 {
        return 0;
    }
    let h = hash_code(key);
    h ^ ((h as u32) >> 16) as i32
}
fn node_hash(n: u64) -> i32 {
    field(n, N_HASH) as u32 as i32
}
fn new_node(hash: i32, key: u64, value: u64, next: u64) -> u64 {
    let n = new_of(NODE, 40);
    set(n, N_HASH, u64::from(hash as u32));
    set(n, N_KEY, key);
    set(n, N_VALUE, value);
    set(n, N_NEXT, next);
    n
}
/// The smallest power of two at least `cap`.
fn table_size_for(cap: usize) -> usize {
    cap.max(1).next_power_of_two().min(MAX_CAPACITY)
}
fn hm_threshold(m: u64) -> usize {
    field(m, HM_THRESHOLD) as usize
}
/// Java's resize: double, keeping each bucket's order in its low and high
/// halves.
fn hm_resize(m: u64) -> u64 {
    let old = field(m, HM_TABLE);
    let old_cap = len(old);
    let old_thr = hm_threshold(m);
    let (new_cap, mut new_thr);
    if old_cap > 0 {
        if old_cap >= MAX_CAPACITY {
            set(m, HM_THRESHOLD, i32::MAX as u64);
            return old;
        }
        new_cap = old_cap << 1;
        new_thr = if new_cap < MAX_CAPACITY && old_cap >= 16 {
            old_thr << 1
        } else {
            0
        };
    } else if old_thr > 0 {
        new_cap = old_thr;
        new_thr = 0;
    } else {
        new_cap = 16;
        new_thr = 12;
    }
    if new_thr == 0 {
        let ft = new_cap as f32 * 0.75;
        new_thr = if new_cap < MAX_CAPACITY && ft < MAX_CAPACITY as f32 {
            ft as usize
        } else {
            i32::MAX as usize
        };
    }
    set(m, HM_THRESHOLD, new_thr as u64);
    let tab = obj_array(new_cap);
    set(m, HM_TABLE, tab);
    for j in 0..old_cap {
        let mut e = get(old, j);
        if e == 0 {
            continue;
        }
        if field(e, N_NEXT) == 0 {
            put(tab, node_hash(e) as u32 as usize & (new_cap - 1), e);
            continue;
        }
        let (mut lo_head, mut lo_tail, mut hi_head, mut hi_tail) = (0u64, 0u64, 0u64, 0u64);
        while e != 0 {
            let next = field(e, N_NEXT);
            if node_hash(e) as u32 as usize & old_cap == 0 {
                if lo_tail == 0 {
                    lo_head = e;
                } else {
                    set(lo_tail, N_NEXT, e);
                }
                lo_tail = e;
            } else {
                if hi_tail == 0 {
                    hi_head = e;
                } else {
                    set(hi_tail, N_NEXT, e);
                }
                hi_tail = e;
            }
            e = next;
        }
        if lo_tail != 0 {
            set(lo_tail, N_NEXT, 0);
            put(tab, j, lo_head);
        }
        if hi_tail != 0 {
            set(hi_tail, N_NEXT, 0);
            put(tab, j + old_cap, hi_head);
        }
    }
    tab
}
fn same_key(n: u64, hash: i32, key: u64) -> bool {
    if node_hash(n) != hash {
        return false;
    }
    let k = field(n, N_KEY);
    k == key || (key != 0 && equals(key, k))
}
fn hm_node(m: u64, key: u64) -> u64 {
    let tab = field(m, HM_TABLE);
    let n = len(tab);
    if n == 0 {
        return 0;
    }
    let hash = spread(key);
    if pending() {
        return 0;
    }
    let mut e = get(tab, hash as u32 as usize & (n - 1));
    while e != 0 {
        if same_key(e, hash, key) {
            return e;
        }
        if pending() {
            return 0;
        }
        e = field(e, N_NEXT);
    }
    0
}
/// A bucket that reached 8 nodes: Java resizes a table smaller than 64
/// (it would make a tree bin in a larger one; its list order stays here).
fn hm_bin_full(m: u64) {
    if len(field(m, HM_TABLE)) < 64 {
        hm_resize(m);
    }
}
fn hm_put(m: u64, key: u64, value: u64, only_if_absent: bool) -> u64 {
    let hash = spread(key);
    if pending() {
        return 0;
    }
    let mut tab = field(m, HM_TABLE);
    if len(tab) == 0 {
        tab = hm_resize(m);
    }
    let n = len(tab);
    let i = hash as u32 as usize & (n - 1);
    let mut p = get(tab, i);
    if p == 0 {
        put(tab, i, new_node(hash, key, value, 0));
    } else {
        let mut found = 0u64;
        if same_key(p, hash, key) {
            found = p;
        } else {
            let mut count = 0;
            loop {
                let e = field(p, N_NEXT);
                if e == 0 {
                    set(p, N_NEXT, new_node(hash, key, value, 0));
                    if count >= 7 {
                        hm_bin_full(m);
                    }
                    break;
                }
                if same_key(e, hash, key) {
                    found = e;
                    break;
                }
                if pending() {
                    return 0;
                }
                p = e;
                count += 1;
            }
        }
        if found != 0 {
            let old = field(found, N_VALUE);
            if !only_if_absent || old == 0 {
                set(found, N_VALUE, value);
            }
            return old;
        }
    }
    set(m, HM_MOD, field(m, HM_MOD) + 1);
    let size = field(m, HM_SIZE) + 1;
    set(m, HM_SIZE, size);
    if size as usize > hm_threshold(m) {
        hm_resize(m);
    }
    0
}
fn hm_remove_node(m: u64, key: u64) -> u64 {
    let tab = field(m, HM_TABLE);
    let n = len(tab);
    if n == 0 {
        return 0;
    }
    let hash = spread(key);
    let i = hash as u32 as usize & (n - 1);
    let mut prev = 0u64;
    let mut e = get(tab, i);
    while e != 0 {
        if same_key(e, hash, key) {
            let next = field(e, N_NEXT);
            if prev == 0 {
                put(tab, i, next);
            } else {
                set(prev, N_NEXT, next);
            }
            set(m, HM_MOD, field(m, HM_MOD) + 1);
            set(m, HM_SIZE, field(m, HM_SIZE) - 1);
            return e;
        }
        prev = e;
        e = field(e, N_NEXT);
    }
    0
}
/// Every node, in iteration order (bucket by bucket).
fn hm_nodes(m: u64) -> Vec<u64> {
    let tab = field(m, HM_TABLE);
    let mut out = Vec::new();
    for j in 0..len(tab) {
        let mut e = get(tab, j);
        while e != 0 {
            out.push(e);
            e = field(e, N_NEXT);
        }
    }
    out
}
/// Before a compute: Java resizes first when the map is over its threshold
/// or has no table.
fn hm_prepare(m: u64) -> u64 {
    let tab = field(m, HM_TABLE);
    if field(m, HM_SIZE) as usize > hm_threshold(m) || len(tab) == 0 {
        hm_resize(m)
    } else {
        tab
    }
}
/// Puts a new node at the head of its bucket (as Java's computes and merge
/// do), counting it.
fn hm_insert_head(m: u64, tab: u64, hash: i32, key: u64, value: u64, bin: usize) {
    let i = hash as u32 as usize & (len(tab) - 1);
    put(tab, i, new_node(hash, key, value, get(tab, i)));
    if bin >= 7 {
        hm_bin_full(m);
    }
    set(m, HM_MOD, field(m, HM_MOD) + 1);
    set(m, HM_SIZE, field(m, HM_SIZE) + 1);
}
/// The node of `key` in `tab` and its bucket's length before it.
fn hm_find(tab: u64, hash: i32, key: u64) -> (u64, usize) {
    let mut e = get(tab, hash as u32 as usize & (len(tab) - 1));
    let mut bin = 0;
    while e != 0 {
        if same_key(e, hash, key) {
            return (e, bin);
        }
        bin += 1;
        e = field(e, N_NEXT);
    }
    (0, bin)
}

extern "C" fn hm_init(_m: u64) {}
extern "C" fn hm_init_cap(m: u64, cap: i32) {
    if cap < 0 {
        throw(
            "java/lang/IllegalArgumentException",
            Some(&format!("Illegal initial capacity: {cap}")),
        );
        return;
    }
    set(m, HM_THRESHOLD, table_size_for(cap as usize) as u64);
}
extern "C" fn hm_get(m: u64, k: u64) -> u64 {
    let e = hm_node(m, k);
    if e == 0 { 0 } else { field(e, N_VALUE) }
}
extern "C" fn hm_get_or_default(m: u64, k: u64, d: u64) -> u64 {
    let e = hm_node(m, k);
    if e == 0 { d } else { field(e, N_VALUE) }
}
extern "C" fn hm_contains_key(m: u64, k: u64) -> i32 {
    (hm_node(m, k) != 0) as i32
}
extern "C" fn hm_contains_value(m: u64, v: u64) -> i32 {
    hm_nodes(m)
        .iter()
        .any(|&e| objects_equals(field(e, N_VALUE), v)) as i32
}
pub extern "C" fn hm_put_c(m: u64, k: u64, v: u64) -> u64 {
    hm_put(m, k, v, false)
}
extern "C" fn hm_put_if_absent(m: u64, k: u64, v: u64) -> u64 {
    hm_put(m, k, v, true)
}
extern "C" fn hm_remove(m: u64, k: u64) -> u64 {
    let e = hm_remove_node(m, k);
    if e == 0 { 0 } else { field(e, N_VALUE) }
}
extern "C" fn hm_size(m: u64) -> i32 {
    field(m, HM_SIZE) as i32
}
extern "C" fn hm_is_empty(m: u64) -> i32 {
    (field(m, HM_SIZE) == 0) as i32
}
extern "C" fn hm_clear(m: u64) {
    set(m, HM_MOD, field(m, HM_MOD) + 1);
    let tab = field(m, HM_TABLE);
    if field(m, HM_SIZE) > 0 {
        set(m, HM_SIZE, 0);
        for j in 0..len(tab) {
            put(tab, j, 0);
        }
    }
}
extern "C" fn hm_compute_if_absent(m: u64, k: u64, f: u64) -> u64 {
    if f == 0 {
        npe();
        return 0;
    }
    let hash = spread(k);
    let tab = hm_prepare(m);
    let (old, bin) = hm_find(tab, hash, k);
    if old != 0 && field(old, N_VALUE) != 0 {
        return field(old, N_VALUE);
    }
    let mc = field(m, HM_MOD);
    let v = call1(f, APPLY, k);
    if pending() {
        return 0;
    }
    if field(m, HM_MOD) != mc {
        throw("java/util/ConcurrentModificationException", None);
        return 0;
    }
    if v == 0 {
        return 0;
    }
    if old != 0 {
        set(old, N_VALUE, v);
        return v;
    }
    hm_insert_head(m, tab, hash, k, v, bin);
    v
}
extern "C" fn hm_compute(m: u64, k: u64, f: u64) -> u64 {
    if f == 0 {
        npe();
        return 0;
    }
    let hash = spread(k);
    let tab = hm_prepare(m);
    let (old, bin) = hm_find(tab, hash, k);
    let old_value = if old == 0 { 0 } else { field(old, N_VALUE) };
    let mc = field(m, HM_MOD);
    let v = call2(f, APPLY2, k, old_value);
    if pending() {
        return 0;
    }
    if field(m, HM_MOD) != mc {
        throw("java/util/ConcurrentModificationException", None);
        return 0;
    }
    if old != 0 {
        if v != 0 {
            set(old, N_VALUE, v);
        } else {
            hm_remove_node(m, k);
        }
    } else if v != 0 {
        hm_insert_head(m, tab, hash, k, v, bin);
    }
    v
}
extern "C" fn hm_merge(m: u64, k: u64, value: u64, f: u64) -> u64 {
    if value == 0 || f == 0 {
        npe();
        return 0;
    }
    let hash = spread(k);
    let tab = hm_prepare(m);
    let (old, bin) = hm_find(tab, hash, k);
    if old != 0 {
        let ov = field(old, N_VALUE);
        let v = if ov != 0 {
            let mc = field(m, HM_MOD);
            let v = call2(f, APPLY2, ov, value);
            if pending() {
                return 0;
            }
            if field(m, HM_MOD) != mc {
                throw("java/util/ConcurrentModificationException", None);
                return 0;
            }
            v
        } else {
            value
        };
        if v != 0 {
            set(old, N_VALUE, v);
        } else {
            hm_remove_node(m, k);
        }
        return v;
    }
    hm_insert_head(m, tab, hash, k, value, bin);
    value
}
extern "C" fn hm_for_each(m: u64, f: u64) {
    if f == 0 {
        npe();
        return;
    }
    let mc = field(m, HM_MOD);
    for e in hm_nodes(m) {
        call2(f, ACCEPT2, field(e, N_KEY), field(e, N_VALUE));
        if pending() {
            return;
        }
    }
    if field(m, HM_MOD) != mc {
        throw("java/util/ConcurrentModificationException", None);
    }
}
fn map_string(m: u64, entries: &[(u64, u64)]) -> u64 {
    let mut out: Vec<u16> = vec![u16::from(b'{')];
    for (k, &(key, value)) in entries.iter().enumerate() {
        if k > 0 {
            out.extend(", ".encode_utf16());
        }
        for (x, sep) in [(key, "="), (value, "")] {
            if x == m {
                out.extend("(this Map)".encode_utf16());
            } else {
                let s = to_string(x);
                if pending() {
                    return 0;
                }
                out.extend_from_slice(units(s));
            }
            out.extend(sep.encode_utf16());
        }
    }
    out.push(u16::from(b'}'));
    new_string(&out)
}
fn hm_entries(m: u64) -> Vec<(u64, u64)> {
    hm_nodes(m)
        .iter()
        .map(|&e| (field(e, N_KEY), field(e, N_VALUE)))
        .collect()
}
extern "C" fn hm_to_string(m: u64) -> u64 {
    map_string(m, &hm_entries(m))
}
fn map_hash(entries: &[(u64, u64)]) -> i32 {
    entries.iter().fold(0i32, |h, &(k, v)| {
        h.wrapping_add(objects_hash(k) ^ objects_hash(v))
    })
}
extern "C" fn hm_hash(m: u64) -> i32 {
    map_hash(&hm_entries(m))
}
/// AbstractMap.equals against any Map.
fn map_equals(me: u64, entries: &[(u64, u64)], o: u64) -> i32 {
    if o == me {
        return 1;
    }
    if !instance_of(o, "java/util/Map") {
        return 0;
    }
    if call0_i(o, SIZE) as usize != entries.len() {
        return 0;
    }
    for &(k, v) in entries {
        let other = call1(o, "get (Ljava/lang/Object;)Ljava/lang/Object;", k);
        if pending() {
            return 0;
        }
        let same = if v == 0 {
            other == 0 && call1_i(o, "containsKey (Ljava/lang/Object;)Z", k) != 0
        } else {
            equals(v, other)
        };
        if !same {
            return 0;
        }
    }
    1
}
extern "C" fn hm_equals(m: u64, o: u64) -> i32 {
    map_equals(m, &hm_entries(m), o)
}
extern "C" fn hm_key_set(m: u64) -> u64 {
    view("java/util/HashMap$KeySet", m)
}
extern "C" fn hm_values(m: u64) -> u64 {
    view("java/util/HashMap$Values", m)
}
extern "C" fn hm_entry_set(m: u64) -> u64 {
    view("java/util/HashMap$EntrySet", m)
}

// ---- ConcurrentHashMap ---------------------------------------------------------
//
// HashMap's layout and code, each operation under the map's monitor
// (reentrant: a function a compute calls may use the map). Null keys and
// values are refused, as the JDK documents. (Its iterators see the map as
// it changes, as HashMap's do: the JDK's are weakly consistent instead.)

fn no_nulls(k: u64, v: u64) -> bool {
    if k == 0 || v == 0 {
        npe();
        false
    } else {
        true
    }
}
macro_rules! locked {
    ($name:ident, $inner:ident, ($($a:ident: $t:ty),*) $(-> $r:ty)?) => {
        extern "C" fn $name(m: u64 $(, $a: $t)*) $(-> $r)? {
            super::threads::monitor_enter(m);
            let r = $inner(m $(, $a)*);
            super::threads::monitor_exit(m);
            r
        }
    };
}
locked!(chm_get, hm_get, (k: u64) -> u64);
locked!(chm_get_or_default, hm_get_or_default, (k: u64, d: u64) -> u64);
locked!(chm_contains_key, hm_contains_key, (k: u64) -> i32);
locked!(chm_contains_value, hm_contains_value, (v: u64) -> i32);
locked!(chm_remove, hm_remove, (k: u64) -> u64);
locked!(chm_size, hm_size, () -> i32);
locked!(chm_is_empty, hm_is_empty, () -> i32);
locked!(chm_clear, hm_clear, ());
locked!(chm_compute_if_absent, hm_compute_if_absent, (k: u64, f: u64) -> u64);
locked!(chm_compute, hm_compute, (k: u64, f: u64) -> u64);
locked!(chm_merge_inner, hm_merge, (k: u64, v: u64, f: u64) -> u64);
locked!(chm_for_each, hm_for_each, (f: u64));
locked!(chm_to_string, hm_to_string, () -> u64);
locked!(chm_hash, hm_hash, () -> i32);
locked!(chm_equals, hm_equals, (o: u64) -> i32);
locked!(chm_put_inner, hm_put_c, (k: u64, v: u64) -> u64);
locked!(chm_put_if_absent_inner, hm_put_if_absent, (k: u64, v: u64) -> u64);
extern "C" fn chm_put(m: u64, k: u64, v: u64) -> u64 {
    if no_nulls(k, v) {
        chm_put_inner(m, k, v)
    } else {
        0
    }
}
extern "C" fn chm_put_if_absent(m: u64, k: u64, v: u64) -> u64 {
    if no_nulls(k, v) {
        chm_put_if_absent_inner(m, k, v)
    } else {
        0
    }
}
extern "C" fn chm_merge(m: u64, k: u64, v: u64, f: u64) -> u64 {
    if no_nulls(k, v) {
        chm_merge_inner(m, k, v, f)
    } else {
        0
    }
}
extern "C" fn chm_init_cap_load(m: u64, cap: i32, _load: f32) {
    hm_init_cap(m, cap);
}
extern "C" fn chm_init_cap_load_level(m: u64, cap: i32, _load: f32, _level: i32) {
    hm_init_cap(m, cap);
}
extern "C" fn chm_mapping_count(m: u64) -> i64 {
    i64::from(chm_size(m))
}
extern "C" fn chm_new_key_set() -> u64 {
    // (A HashSet: the JDK's is backed by a ConcurrentHashMap.)
    let set = new_of("java/util/HashSet", 16);
    hs_init(set);
    set
}

/// `java.util.concurrent.ConcurrentHashMap`.
pub(super) fn concurrent_builtins() -> Vec<Builtin> {
    macro_rules! f {
        ($f:expr) => {
            $f as *const () as usize
        };
    }
    let m = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: false,
        f,
    };
    let s = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: true,
        f,
    };
    const O2: &str = "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;";
    vec![class(
        "java/util/concurrent/ConcurrentHashMap",
        "java/lang/Object",
        &["java/util/concurrent/ConcurrentMap", "java/util/Map"],
        40,
        vec![
            m("<init>", "()V", f!(hm_init)),
            m("<init>", "(I)V", f!(hm_init_cap)),
            m("<init>", "(IF)V", f!(chm_init_cap_load)),
            m("<init>", "(IFI)V", f!(chm_init_cap_load_level)),
            m("get", "(Ljava/lang/Object;)Ljava/lang/Object;", f!(chm_get)),
            m("getOrDefault", O2, f!(chm_get_or_default)),
            m("containsKey", "(Ljava/lang/Object;)Z", f!(chm_contains_key)),
            m(
                "containsValue",
                "(Ljava/lang/Object;)Z",
                f!(chm_contains_value),
            ),
            m("put", O2, f!(chm_put)),
            m("putIfAbsent", O2, f!(chm_put_if_absent)),
            m(
                "remove",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
                f!(chm_remove),
            ),
            m("size", "()I", f!(chm_size)),
            m("mappingCount", "()J", f!(chm_mapping_count)),
            m("isEmpty", "()Z", f!(chm_is_empty)),
            m("clear", "()V", f!(chm_clear)),
            m(
                "computeIfAbsent",
                "(Ljava/lang/Object;Ljava/util/function/Function;)Ljava/lang/Object;",
                f!(chm_compute_if_absent),
            ),
            m(
                "compute",
                "(Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
                f!(chm_compute),
            ),
            m(
                "merge",
                "(Ljava/lang/Object;Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
                f!(chm_merge),
            ),
            m(
                "forEach",
                "(Ljava/util/function/BiConsumer;)V",
                f!(chm_for_each),
            ),
            m("keySet", "()Ljava/util/Set;", f!(hm_key_set)),
            m("values", "()Ljava/util/Collection;", f!(hm_values)),
            m("entrySet", "()Ljava/util/Set;", f!(hm_entry_set)),
            m("toString", "()Ljava/lang/String;", f!(chm_to_string)),
            m("equals", "(Ljava/lang/Object;)Z", f!(chm_equals)),
            m("hashCode", "()I", f!(chm_hash)),
            s("newKeySet", "()Ljava/util/Set;", f!(chm_new_key_set)),
        ],
    )]
}
fn view(class: &str, m: u64) -> u64 {
    let v = new_of(class, 16);
    set(v, 8, m);
    v
}
// The views: [vtable][map].
fn view_map(v: u64) -> u64 {
    field(v, 8)
}
extern "C" fn view_size(v: u64) -> i32 {
    hm_size(view_map(v))
}
extern "C" fn view_is_empty(v: u64) -> i32 {
    hm_is_empty(view_map(v))
}
extern "C" fn view_clear(v: u64) {
    hm_clear(view_map(v))
}
extern "C" fn keys_contains(v: u64, k: u64) -> i32 {
    hm_contains_key(view_map(v), k)
}
extern "C" fn keys_remove(v: u64, k: u64) -> i32 {
    (hm_remove_node(view_map(v), k) != 0) as i32
}
extern "C" fn values_contains(v: u64, x: u64) -> i32 {
    hm_contains_value(view_map(v), x)
}
extern "C" fn entries_contains(v: u64, e: u64) -> i32 {
    if !instance_of(e, "java/util/Map$Entry") {
        return 0;
    }
    let k = call0(e, "getKey ()Ljava/lang/Object;");
    let n = hm_node(view_map(v), k);
    (n != 0 && node_equals(n, e) != 0) as i32
}
fn keys_of(v: u64) -> Vec<u64> {
    hm_nodes(view_map(v))
        .iter()
        .map(|&e| field(e, N_KEY))
        .collect()
}
fn values_of(v: u64) -> Vec<u64> {
    hm_nodes(view_map(v))
        .iter()
        .map(|&e| field(e, N_VALUE))
        .collect()
}
extern "C" fn keys_to_string(v: u64) -> u64 {
    collection_string(v, &keys_of(v))
}
extern "C" fn values_to_string(v: u64) -> u64 {
    collection_string(v, &values_of(v))
}
extern "C" fn entries_to_string(v: u64) -> u64 {
    collection_string(v, &hm_nodes(view_map(v)))
}
fn for_each_of(items: Vec<u64>, f: u64) {
    if f == 0 {
        npe();
        return;
    }
    for e in items {
        call1(f, ACCEPT, e);
        if pending() {
            return;
        }
    }
}
extern "C" fn keys_for_each(v: u64, f: u64) {
    for_each_of(keys_of(v), f)
}
extern "C" fn values_for_each(v: u64, f: u64) {
    for_each_of(values_of(v), f)
}
extern "C" fn entries_for_each(v: u64, f: u64) {
    for_each_of(hm_nodes(view_map(v)), f)
}
fn set_hash(items: &[u64]) -> i32 {
    items
        .iter()
        .fold(0i32, |h, &e| h.wrapping_add(objects_hash(e)))
}
/// AbstractSet.equals against any Set.
fn set_equals(me: u64, items: &[u64], o: u64) -> i32 {
    if o == me {
        return 1;
    }
    if !instance_of(o, "java/util/Set") {
        return 0;
    }
    if call0_i(o, SIZE) as usize != items.len() {
        return 0;
    }
    items
        .iter()
        .all(|&e| call1_i(o, "contains (Ljava/lang/Object;)Z", e) != 0 && !pending()) as i32
}
extern "C" fn keys_hash(v: u64) -> i32 {
    set_hash(&keys_of(v))
}
extern "C" fn keys_equals(v: u64, o: u64) -> i32 {
    set_equals(v, &keys_of(v), o)
}

// The iterators over a HashMap: [vtable][map][next node][current node]
// [expected modCount][next bucket].
const HI_MAP: usize = 8;
const HI_NEXT: usize = 16;
const HI_CURRENT: usize = 24;
const HI_EXPECT: usize = 32;
const HI_INDEX: usize = 40;
fn hash_iterator(class: &str, m: u64) -> u64 {
    let it = new_of(class, 48);
    set(it, HI_MAP, m);
    set(it, HI_EXPECT, field(m, HM_MOD));
    if field(m, HM_SIZE) > 0 {
        hi_advance(it, field(m, HM_TABLE), 0);
    }
    it
}
/// The next node from bucket `from` on.
fn hi_advance(it: u64, tab: u64, from: usize) {
    let mut i = from;
    while i < len(tab) {
        let e = get(tab, i);
        i += 1;
        if e != 0 {
            set(it, HI_NEXT, e);
            set(it, HI_INDEX, i as u64);
            return;
        }
    }
    set(it, HI_NEXT, 0);
    set(it, HI_INDEX, i as u64);
}
extern "C" fn hi_has_next(it: u64) -> i32 {
    (field(it, HI_NEXT) != 0) as i32
}
fn hi_next_node(it: u64) -> u64 {
    let m = field(it, HI_MAP);
    if field(m, HM_MOD) != field(it, HI_EXPECT) {
        throw("java/util/ConcurrentModificationException", None);
        return 0;
    }
    let e = field(it, HI_NEXT);
    if e == 0 {
        throw("java/util/NoSuchElementException", None);
        return 0;
    }
    set(it, HI_CURRENT, e);
    let next = field(e, N_NEXT);
    if next != 0 {
        set(it, HI_NEXT, next);
    } else {
        hi_advance(it, field(m, HM_TABLE), field(it, HI_INDEX) as usize);
    }
    e
}
extern "C" fn key_it_next(it: u64) -> u64 {
    let e = hi_next_node(it);
    if e == 0 { 0 } else { field(e, N_KEY) }
}
extern "C" fn value_it_next(it: u64) -> u64 {
    let e = hi_next_node(it);
    if e == 0 { 0 } else { field(e, N_VALUE) }
}
extern "C" fn entry_it_next(it: u64) -> u64 {
    hi_next_node(it)
}
extern "C" fn hi_remove(it: u64) {
    let e = field(it, HI_CURRENT);
    if e == 0 {
        throw("java/lang/IllegalStateException", None);
        return;
    }
    let m = field(it, HI_MAP);
    if field(m, HM_MOD) != field(it, HI_EXPECT) {
        throw("java/util/ConcurrentModificationException", None);
        return;
    }
    set(it, HI_CURRENT, 0);
    hm_remove_node(m, field(e, N_KEY));
    set(it, HI_EXPECT, field(m, HM_MOD));
}
extern "C" fn keys_iterator(v: u64) -> u64 {
    hash_iterator("java/util/HashMap$KeyIterator", view_map(v))
}
extern "C" fn values_iterator(v: u64) -> u64 {
    hash_iterator("java/util/HashMap$ValueIterator", view_map(v))
}
extern "C" fn entries_iterator(v: u64) -> u64 {
    hash_iterator("java/util/HashMap$EntryIterator", view_map(v))
}

// The Map.Entry of a node.
extern "C" fn node_key(n: u64) -> u64 {
    field(n, N_KEY)
}
extern "C" fn node_value(n: u64) -> u64 {
    field(n, N_VALUE)
}
extern "C" fn node_set_value(n: u64, v: u64) -> u64 {
    let old = field(n, N_VALUE);
    set(n, N_VALUE, v);
    old
}
extern "C" fn node_hash_code(n: u64) -> i32 {
    objects_hash(field(n, N_KEY)) ^ objects_hash(field(n, N_VALUE))
}
extern "C" fn node_equals(n: u64, o: u64) -> i32 {
    if o == n {
        return 1;
    }
    if !instance_of(o, "java/util/Map$Entry") {
        return 0;
    }
    let k = call0(o, "getKey ()Ljava/lang/Object;");
    let v = call0(o, "getValue ()Ljava/lang/Object;");
    (!pending() && objects_equals(field(n, N_KEY), k) && objects_equals(field(n, N_VALUE), v))
        as i32
}
extern "C" fn node_to_string(n: u64) -> u64 {
    let k = to_string(field(n, N_KEY));
    let v = to_string(field(n, N_VALUE));
    if pending() {
        return 0;
    }
    let mut out = units(k).to_vec();
    out.push(u16::from(b'='));
    out.extend_from_slice(units(v));
    new_string(&out)
}

// ---- HashSet: [vtable][its HashMap] -------------------------------------

const HS_MAP: usize = 8;
fn present() -> u64 {
    once("PRESENT", || alloc(8, vt("java/lang/Object")))
}
fn new_hash_map() -> u64 {
    new_of(HASH_MAP, 40)
}
extern "C" fn hs_init(s: u64) {
    set(s, HS_MAP, new_hash_map());
}
extern "C" fn hs_init_cap(s: u64, cap: i32) {
    let m = new_hash_map();
    hm_init_cap(m, cap);
    set(s, HS_MAP, m);
}
extern "C" fn hs_init_from(s: u64, c: u64) {
    let Some(items) = elements(c) else { return };
    // Java: room for the collection at the default load factor.
    let cap = ((items.len() as f32 / 0.75) as usize + 1).max(16);
    let m = new_hash_map();
    hm_init_cap(m, cap as i32);
    set(s, HS_MAP, m);
    for e in items {
        hm_put(m, e, present(), false);
        if pending() {
            return;
        }
    }
}
extern "C" fn hs_add(s: u64, e: u64) -> i32 {
    (hm_put(field(s, HS_MAP), e, present(), false) == 0 && !pending()) as i32
}
extern "C" fn hs_contains(s: u64, e: u64) -> i32 {
    hm_contains_key(field(s, HS_MAP), e)
}
extern "C" fn hs_remove(s: u64, e: u64) -> i32 {
    (hm_remove_node(field(s, HS_MAP), e) != 0) as i32
}
extern "C" fn hs_size(s: u64) -> i32 {
    hm_size(field(s, HS_MAP))
}
extern "C" fn hs_is_empty(s: u64) -> i32 {
    hm_is_empty(field(s, HS_MAP))
}
extern "C" fn hs_clear(s: u64) {
    hm_clear(field(s, HS_MAP))
}
extern "C" fn hs_iterator(s: u64) -> u64 {
    hash_iterator("java/util/HashMap$KeyIterator", field(s, HS_MAP))
}
fn hs_items(s: u64) -> Vec<u64> {
    hm_nodes(field(s, HS_MAP))
        .iter()
        .map(|&e| field(e, N_KEY))
        .collect()
}
extern "C" fn hs_to_string(s: u64) -> u64 {
    collection_string(s, &hs_items(s))
}
extern "C" fn hs_hash(s: u64) -> i32 {
    set_hash(&hs_items(s))
}
extern "C" fn hs_equals(s: u64, o: u64) -> i32 {
    set_equals(s, &hs_items(s), o)
}
extern "C" fn hs_for_each(s: u64, f: u64) {
    for_each_of(hs_items(s), f)
}
extern "C" fn hs_add_all(s: u64, c: u64) -> i32 {
    let Some(items) = elements(c) else { return 0 };
    let mut changed = 0;
    for e in items {
        changed |= hs_add(s, e);
        if pending() {
            return 0;
        }
    }
    changed
}

// ---- Immutable sets and maps (Set.of, Map.of, the empty ones) -------------
//
// The JDK's iteration order of these is different on every run (a random
// salt): no program can depend on it. Here: the order given.

const SET_N: &str = "java/util/ImmutableCollections$SetN";
const MAP_N: &str = "java/util/ImmutableCollections$MapN";
const KV: &str = "java/util/KeyValueHolder";
fn set_of(items: &[u64]) -> u64 {
    if items.contains(&0) {
        npe();
        return 0;
    }
    for (i, &a) in items.iter().enumerate() {
        if items[..i].iter().any(|&b| equals(a, b)) {
            let s = to_string(a);
            throw(
                "java/lang/IllegalArgumentException",
                Some(&format!(
                    "duplicate element: {}",
                    String::from_utf16_lossy(units(s))
                )),
            );
            return 0;
        }
    }
    fixed_list(SET_N, items)
}
extern "C" fn set_of0() -> u64 {
    once("emptySet", || fixed_list(SET_N, &[]))
}
extern "C" fn set_of1(a: u64) -> u64 {
    set_of(&[a])
}
extern "C" fn set_of2(a: u64, b: u64) -> u64 {
    set_of(&[a, b])
}
extern "C" fn set_of3(a: u64, b: u64, c: u64) -> u64 {
    set_of(&[a, b, c])
}
extern "C" fn set_of_array(arr: u64) -> u64 {
    if arr == 0 {
        npe();
        return 0;
    }
    set_of(&(0..len(arr)).map(|i| get(arr, i)).collect::<Vec<_>>())
}
extern "C" fn setn_hash(s: u64) -> i32 {
    set_hash(&fixed_items(s))
}
extern "C" fn setn_equals(s: u64, o: u64) -> i32 {
    set_equals(s, &fixed_items(s), o)
}
// MapN: [vtable][keys][values].
fn map_of(pairs: &[(u64, u64)]) -> u64 {
    for (i, &(k, v)) in pairs.iter().enumerate() {
        if k == 0 || v == 0 {
            npe();
            return 0;
        }
        if pairs[..i].iter().any(|&(b, _)| equals(k, b)) {
            let s = to_string(k);
            throw(
                "java/lang/IllegalArgumentException",
                Some(&format!(
                    "duplicate key: {}",
                    String::from_utf16_lossy(units(s))
                )),
            );
            return 0;
        }
    }
    let keys: Vec<u64> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<u64> = pairs.iter().map(|p| p.1).collect();
    let m = new_of(MAP_N, 24);
    set(m, 8, fixed_list(LIST_N, &keys));
    set(m, 16, fixed_list(LIST_N, &values));
    m
}
fn mapn_entries(m: u64) -> Vec<(u64, u64)> {
    fixed_items(field(m, 8))
        .into_iter()
        .zip(fixed_items(field(m, 16)))
        .collect()
}
extern "C" fn map_of0() -> u64 {
    once("emptyMap", || map_of(&[]))
}
extern "C" fn map_of1(k: u64, v: u64) -> u64 {
    map_of(&[(k, v)])
}
extern "C" fn map_of2(k: u64, v: u64, k2: u64, v2: u64) -> u64 {
    map_of(&[(k, v), (k2, v2)])
}
extern "C" fn map_of3(k: u64, v: u64, k2: u64, v2: u64, k3: u64, v3: u64) -> u64 {
    map_of(&[(k, v), (k2, v2), (k3, v3)])
}
extern "C" fn map_of4(k: u64, v: u64, k2: u64, v2: u64, k3: u64, v3: u64, k4: u64, v4: u64) -> u64 {
    map_of(&[(k, v), (k2, v2), (k3, v3), (k4, v4)])
}
#[allow(clippy::too_many_arguments)]
extern "C" fn map_of5(
    k: u64,
    v: u64,
    k2: u64,
    v2: u64,
    k3: u64,
    v3: u64,
    k4: u64,
    v4: u64,
    k5: u64,
    v5: u64,
) -> u64 {
    map_of(&[(k, v), (k2, v2), (k3, v3), (k4, v4), (k5, v5)])
}
fn mapn_find(m: u64, k: u64) -> Option<u64> {
    mapn_entries(m)
        .into_iter()
        .find(|&(key, _)| k != 0 && equals(k, key))
        .map(|p| p.1)
}
extern "C" fn mapn_get(m: u64, k: u64) -> u64 {
    mapn_find(m, k).unwrap_or(0)
}
extern "C" fn mapn_get_or_default(m: u64, k: u64, d: u64) -> u64 {
    mapn_find(m, k).unwrap_or(d)
}
extern "C" fn mapn_contains_key(m: u64, k: u64) -> i32 {
    mapn_find(m, k).is_some() as i32
}
extern "C" fn mapn_size(m: u64) -> i32 {
    len(field(field(m, 8), FL_ARRAY)) as i32
}
extern "C" fn mapn_is_empty(m: u64) -> i32 {
    (mapn_size(m) == 0) as i32
}
extern "C" fn mapn_key_set(m: u64) -> u64 {
    fixed_list(SET_N, &fixed_items(field(m, 8)))
}
extern "C" fn mapn_values(m: u64) -> u64 {
    field(m, 16)
}
extern "C" fn mapn_entry_set(m: u64) -> u64 {
    let entries: Vec<u64> = mapn_entries(m)
        .into_iter()
        .map(|(k, v)| {
            let e = new_of(KV, 24);
            set(e, 8, k);
            set(e, 16, v);
            e
        })
        .collect();
    fixed_list(SET_N, &entries)
}
extern "C" fn mapn_for_each(m: u64, f: u64) {
    if f == 0 {
        npe();
        return;
    }
    for (k, v) in mapn_entries(m) {
        call2(f, ACCEPT2, k, v);
        if pending() {
            return;
        }
    }
}
extern "C" fn mapn_to_string(m: u64) -> u64 {
    map_string(m, &mapn_entries(m))
}
extern "C" fn mapn_hash(m: u64) -> i32 {
    map_hash(&mapn_entries(m))
}
extern "C" fn mapn_equals(m: u64, o: u64) -> i32 {
    map_equals(m, &mapn_entries(m), o)
}
// KeyValueHolder: [vtable][key][value].
extern "C" fn kv_key(e: u64) -> u64 {
    field(e, 8)
}
extern "C" fn kv_value(e: u64) -> u64 {
    field(e, 16)
}

// ---- Optional: [vtable][value] ---------------------------------------------

const OPTIONAL: &str = "java/util/Optional";
fn optional(v: u64) -> u64 {
    if v == 0 {
        return optional_empty();
    }
    let o = new_of(OPTIONAL, 16);
    set(o, 8, v);
    o
}
extern "C" fn optional_empty() -> u64 {
    once("Optional.empty", || new_of(OPTIONAL, 16))
}
extern "C" fn optional_of(v: u64) -> u64 {
    if v == 0 {
        npe();
        return 0;
    }
    optional(v)
}
extern "C" fn optional_of_nullable(v: u64) -> u64 {
    optional(v)
}
fn opt(o: u64) -> u64 {
    field(o, 8)
}
extern "C" fn optional_is_present(o: u64) -> i32 {
    (opt(o) != 0) as i32
}
extern "C" fn optional_is_empty(o: u64) -> i32 {
    (opt(o) == 0) as i32
}
extern "C" fn optional_get(o: u64) -> u64 {
    let v = opt(o);
    if v == 0 {
        throw("java/util/NoSuchElementException", Some("No value present"));
    }
    v
}
extern "C" fn optional_or_else(o: u64, other: u64) -> u64 {
    let v = opt(o);
    if v == 0 { other } else { v }
}
extern "C" fn optional_or_else_get(o: u64, s: u64) -> u64 {
    let v = opt(o);
    if v != 0 {
        return v;
    }
    call0(s, SUPPLY)
}
extern "C" fn optional_map(o: u64, f: u64) -> u64 {
    if f == 0 {
        npe();
        return 0;
    }
    let v = opt(o);
    if v == 0 {
        return optional_empty();
    }
    let r = call1(f, APPLY, v);
    if pending() { 0 } else { optional(r) }
}
extern "C" fn optional_flat_map(o: u64, f: u64) -> u64 {
    if f == 0 {
        npe();
        return 0;
    }
    let v = opt(o);
    if v == 0 {
        return optional_empty();
    }
    let r = call1(f, APPLY, v);
    if !pending() && r == 0 {
        npe();
    }
    r
}
extern "C" fn optional_filter(o: u64, p: u64) -> u64 {
    if p == 0 {
        npe();
        return 0;
    }
    let v = opt(o);
    if v == 0 {
        return o;
    }
    let t = call1_i(p, TEST, v);
    if pending() {
        0
    } else if t != 0 {
        o
    } else {
        optional_empty()
    }
}
/// `orElseThrow(Supplier)`: the value, or the supplier's exception thrown.
extern "C" fn optional_or_else_throw(o: u64, supplier: u64) -> u64 {
    let v = field(o, 8);
    if v != 0 {
        return v;
    }
    if supplier == 0 {
        npe();
        return 0;
    }
    let e = call0(supplier, SUPPLY);
    if pending() {
        return 0;
    }
    if e == 0 {
        npe();
        return 0;
    }
    // SAFETY: this thread's context: the exception it now throws.
    unsafe { (*ctx()).exception = e };
    0
}
extern "C" fn optional_if_present_or_else(o: u64, c: u64, r: u64) {
    let v = field(o, 8);
    if v != 0 {
        call1(c, ACCEPT, v);
    } else {
        call0(r, "run ()V");
    }
}
extern "C" fn optional_or(o: u64, supplier: u64) -> u64 {
    if field(o, 8) != 0 {
        return o;
    }
    call0(supplier, SUPPLY)
}
extern "C" fn optional_stream(o: u64) -> u64 {
    let v = field(o, 8);
    let a = obj_array(usize::from(v != 0));
    if v != 0 {
        put(a, 0, v);
    }
    streams::of_array(a)
}

extern "C" fn optional_if_present(o: u64, c: u64) {
    let v = opt(o);
    if v != 0 {
        call1(c, ACCEPT, v);
    }
}
extern "C" fn optional_equals(o: u64, other: u64) -> i32 {
    (o == other || (is(other, OPTIONAL) && objects_equals(opt(o), opt(other)))) as i32
}
extern "C" fn optional_hash(o: u64) -> i32 {
    objects_hash(opt(o))
}
extern "C" fn optional_to_string(o: u64) -> u64 {
    let v = opt(o);
    if v == 0 {
        return str_of("Optional.empty");
    }
    let s = to_string(v);
    if pending() {
        return 0;
    }
    let mut out: Vec<u16> = "Optional[".encode_utf16().collect();
    out.extend_from_slice(units(s));
    out.push(u16::from(b']'));
    new_string(&out)
}

// ---- Arrays, System.arraycopy, array clone ---------------------------------

/// An array's element size (its header).
fn elem_size(a: u64) -> usize {
    // SAFETY: an array's header.
    (unsafe { word(a, 0) } as usize).clamp(1, 8)
}
pub extern "C" fn array_clone(a: u64) -> u64 {
    let n = len(a);
    let size = elem_size(a);
    // SAFETY: a copy of the array's whole memory.
    unsafe {
        let b = alloc(A_DATA + size * n, word(a, 0));
        set_word(b, A_LEN, n as u64);
        std::ptr::copy_nonoverlapping(
            (a as usize + A_DATA) as *const u8,
            (b as usize + A_DATA) as *mut u8,
            size * n,
        );
        b
    }
}
extern "C" fn system_arraycopy(src: u64, sp: i32, dst: u64, dp: i32, n: i32) {
    if src == 0 || dst == 0 {
        npe();
        return;
    }
    let (sl, dl) = (len(src) as i64, len(dst) as i64);
    let (sp, dp, n) = (i64::from(sp), i64::from(dp), i64::from(n));
    if sp < 0 || dp < 0 || n < 0 || sp + n > sl || dp + n > dl {
        throw(
            "java/lang/ArrayIndexOutOfBoundsException",
            Some(&format!(
                "arraycopy: last source index {} out of bounds for length {sl}",
                sp + n
            )),
        );
        return;
    }
    let size = elem_size(src);
    // SAFETY: both ranges are in their arrays; they may overlap.
    unsafe {
        std::ptr::copy(
            (src as usize + A_DATA + size * sp as usize) as *const u8,
            (dst as usize + A_DATA + size * dp as usize) as *mut u8,
            size * n as usize,
        );
    }
}
fn array_string<T>(a: u64, at: impl Fn(usize) -> T, show: impl Fn(T) -> String) -> u64 {
    if a == 0 {
        return str_of("null");
    }
    let parts: Vec<String> = (0..len(a)).map(|i| show(at(i))).collect();
    str_of(&format!("[{}]", parts.join(", ")))
}
fn prim<T: Copy>(a: u64, i: usize) -> T {
    // SAFETY: element i of a primitive array of T.
    unsafe { *((a as usize + A_DATA + std::mem::size_of::<T>() * i) as *const T) }
}
extern "C" fn arrays_to_string_int(a: u64) -> u64 {
    array_string(a, |i| prim::<i32>(a, i), |v| v.to_string())
}
extern "C" fn arrays_to_string_long(a: u64) -> u64 {
    array_string(a, |i| prim::<i64>(a, i), |v| v.to_string())
}
extern "C" fn arrays_to_string_double(a: u64) -> u64 {
    array_string(a, |i| prim::<f64>(a, i), super::java_double)
}
extern "C" fn arrays_to_string_float(a: u64) -> u64 {
    array_string(a, |i| prim::<f32>(a, i), super::java_float)
}
extern "C" fn arrays_to_string_byte(a: u64) -> u64 {
    array_string(a, |i| prim::<i8>(a, i), |v| v.to_string())
}
extern "C" fn arrays_to_string_bool(a: u64) -> u64 {
    array_string(a, |i| prim::<u8>(a, i), |v| (v != 0).to_string())
}
extern "C" fn arrays_to_string_objects(a: u64) -> u64 {
    if a == 0 {
        return str_of("null");
    }
    let items: Vec<u64> = (0..len(a)).map(|i| get(a, i)).collect();
    // Arrays.toString does not mark itself as a collection does.
    let mut out: Vec<u16> = vec![u16::from(b'[')];
    for (k, &e) in items.iter().enumerate() {
        if k > 0 {
            out.extend(", ".encode_utf16());
        }
        let s = to_string(e);
        if pending() {
            return 0;
        }
        out.extend_from_slice(units(s));
    }
    out.push(u16::from(b']'));
    new_string(&out)
}
pub(super) extern "C" fn arrays_hash_objects(a: u64) -> i32 {
    if a == 0 {
        return 0;
    }
    list_hash(&(0..len(a)).map(|i| get(a, i)).collect::<Vec<_>>())
}
extern "C" fn arrays_hash_int(a: u64) -> i32 {
    if a == 0 {
        return 0;
    }
    (0..len(a)).fold(1i32, |h, i| {
        h.wrapping_mul(31).wrapping_add(prim::<i32>(a, i))
    })
}
extern "C" fn arrays_equals_int(a: u64, b: u64) -> i32 {
    if a == b {
        return 1;
    }
    if a == 0 || b == 0 || len(a) != len(b) {
        return 0;
    }
    (0..len(a)).all(|i| prim::<i32>(a, i) == prim::<i32>(b, i)) as i32
}
extern "C" fn arrays_equals_objects(a: u64, b: u64) -> i32 {
    if a == b {
        return 1;
    }
    if a == 0 || b == 0 || len(a) != len(b) {
        return 0;
    }
    (0..len(a)).all(|i| objects_equals(get(a, i), get(b, i))) as i32
}
extern "C" fn arrays_fill_int(a: u64, v: i32) {
    if a == 0 {
        npe();
        return;
    }
    for i in 0..len(a) {
        // SAFETY: element i of an int array.
        unsafe { *((a as usize + A_DATA + 4 * i) as *mut i32) = v };
    }
}
extern "C" fn arrays_fill_objects(a: u64, v: u64) {
    if a == 0 {
        npe();
        return;
    }
    for i in 0..len(a) {
        put(a, i, v);
    }
}
extern "C" fn arrays_sort_int(a: u64) {
    if a == 0 {
        npe();
        return;
    }
    // SAFETY: the int array's elements.
    let s = unsafe { std::slice::from_raw_parts_mut((a as usize + A_DATA) as *mut i32, len(a)) };
    s.sort_unstable();
}
extern "C" fn arrays_sort_objects(a: u64) {
    arrays_sort_by(a, 0)
}
extern "C" fn arrays_sort_by(a: u64, cmp: u64) {
    if a == 0 {
        npe();
        return;
    }
    let mut items: Vec<u64> = (0..len(a)).map(|i| get(a, i)).collect();
    sort_items(&mut items, cmp);
    if !pending() {
        for (i, e) in items.iter().enumerate() {
            put(a, i, *e);
        }
    }
}
extern "C" fn arrays_copy_of_objects(a: u64, n: i32) -> u64 {
    if n < 0 {
        throw("java/lang/NegativeArraySizeException", Some(&n.to_string()));
        return 0;
    }
    let b = obj_array(n as usize);
    for i in 0..len(a).min(n as usize) {
        put(b, i, get(a, i));
    }
    b
}
// ---- Arrays: every element type (an array knows its element size) ----

/// Whether `from..to` is a range of `a` (else the JDK's exception).
fn range_ok(a: u64, from: i32, to: i32) -> bool {
    if from > to {
        throw(
            "java/lang/IllegalArgumentException",
            Some(&format!("fromIndex({from}) > toIndex({to})")),
        );
        false
    } else if from < 0 || to as usize > len(a) {
        throw(
            "java/lang/ArrayIndexOutOfBoundsException",
            Some(&format!(
                "Array index out of range: {}",
                if from < 0 { from } else { to }
            )),
        );
        false
    } else {
        true
    }
}
/// Each element of `a` from `from` to `to` set to the low bytes of `bits`.
fn fill_bits(a: u64, from: usize, to: usize, bits: u64) {
    let size = elem_size(a);
    let bytes = bits.to_le_bytes();
    for i in from..to {
        // SAFETY: element i of the array, `size` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (a as usize + A_DATA + size * i) as *mut u8,
                size,
            );
        }
    }
}
/// A new array of `a`'s element type and `n` elements, those from `from`
/// copied from `a` (the rest zero).
fn copy_range(a: u64, from: usize, n: usize) -> u64 {
    let size = elem_size(a);
    // SAFETY: an array's header is its element size.
    let b = alloc(A_DATA + size * n, unsafe { word(a, 0) });
    let have = len(a).saturating_sub(from).min(n);
    // SAFETY: both arrays' elements.
    unsafe {
        set_word(b, A_LEN, n as u64);
        std::ptr::copy_nonoverlapping(
            (a as usize + A_DATA + size * from) as *const u8,
            (b as usize + A_DATA) as *mut u8,
            size * have,
        );
    }
    b
}
extern "C" fn arrays_copy_of(a: u64, n: i32) -> u64 {
    if a == 0 {
        npe();
        return 0;
    }
    if n < 0 {
        throw("java/lang/NegativeArraySizeException", Some(&n.to_string()));
        return 0;
    }
    copy_range(a, 0, n as usize)
}
extern "C" fn arrays_copy_of_range(a: u64, from: i32, to: i32) -> u64 {
    if a == 0 {
        npe();
        return 0;
    }
    if from > to {
        throw(
            "java/lang/IllegalArgumentException",
            Some(&format!("{from} > {to}")),
        );
        return 0;
    }
    if from < 0 || from as usize > len(a) {
        throw(
            "java/lang/ArrayIndexOutOfBoundsException",
            Some(&format!("Array index out of range: {from}")),
        );
        return 0;
    }
    copy_range(a, from as usize, (to - from) as usize)
}
/// `Arrays.equals` of two primitive arrays: the same elements, bit for bit.
/// (Java compares float and double elements by `floatToIntBits`, so two
/// NaNs of other bits are equal there; not here.)
extern "C" fn arrays_equals_prim(a: u64, b: u64) -> i32 {
    if a == b {
        return 1;
    }
    if a == 0 || b == 0 || len(a) != len(b) {
        return 0;
    }
    let n = elem_size(a) * len(a);
    // SAFETY: both arrays' elements, `n` bytes each.
    unsafe {
        let x = std::slice::from_raw_parts((a as usize + A_DATA) as *const u8, n);
        let y = std::slice::from_raw_parts((b as usize + A_DATA) as *const u8, n);
        i32::from(x == y)
    }
}
macro_rules! array_kind {
    ($fill:ident, $fill_range:ident, $hash:ident, $t:ty, $bits:expr, $elem_hash:expr) => {
        extern "C" fn $fill(a: u64, v: $t) {
            if a == 0 {
                npe();
                return;
            }
            fill_bits(a, 0, len(a), ($bits)(v));
        }
        extern "C" fn $fill_range(a: u64, from: i32, to: i32, v: $t) {
            if a == 0 {
                npe();
                return;
            }
            if range_ok(a, from, to) {
                fill_bits(a, from as usize, to as usize, ($bits)(v));
            }
        }
        extern "C" fn $hash(a: u64) -> i32 {
            if a == 0 {
                return 0;
            }
            (0..len(a)).fold(1i32, |h, i| {
                h.wrapping_mul(31)
                    .wrapping_add(($elem_hash)(prim::<$t>(a, i)))
            })
        }
    };
}
// (The narrow kinds come in as ints; elements are read at their own width.)
array_kind!(
    arrays_fill_long,
    arrays_fill_long_range,
    arrays_hash_long,
    i64,
    |v: i64| v as u64,
    |v: i64| (v ^ ((v as u64) >> 32) as i64) as i32
);
array_kind!(
    arrays_fill_double,
    arrays_fill_double_range,
    arrays_hash_double,
    f64,
    |v: f64| v.to_bits(),
    |v: f64| {
        let b = if v.is_nan() {
            0x7ff8_0000_0000_0000u64
        } else {
            v.to_bits()
        };
        (b ^ (b >> 32)) as i32
    }
);
array_kind!(
    arrays_fill_float,
    arrays_fill_float_range,
    arrays_hash_float,
    f32,
    |v: f32| u64::from(v.to_bits()),
    |v: f32| if v.is_nan() {
        0x7fc0_0000
    } else {
        v.to_bits() as i32
    }
);
extern "C" fn arrays_fill_narrow(a: u64, v: i32) {
    if a == 0 {
        npe();
        return;
    }
    fill_bits(a, 0, len(a), v as u32 as u64);
}
extern "C" fn arrays_fill_narrow_range(a: u64, from: i32, to: i32, v: i32) {
    if a == 0 {
        npe();
        return;
    }
    if range_ok(a, from, to) {
        fill_bits(a, from as usize, to as usize, v as u32 as u64);
    }
}
extern "C" fn arrays_fill_objects_range(a: u64, from: i32, to: i32, v: u64) {
    if a == 0 {
        npe();
        return;
    }
    if range_ok(a, from, to) {
        for i in from as usize..to as usize {
            put(a, i, v);
        }
    }
}
fn hash_narrow(a: u64, elem: impl Fn(u64, usize) -> i32) -> i32 {
    if a == 0 {
        return 0;
    }
    (0..len(a)).fold(1i32, |h, i| h.wrapping_mul(31).wrapping_add(elem(a, i)))
}
extern "C" fn arrays_hash_byte(a: u64) -> i32 {
    hash_narrow(a, |a, i| i32::from(prim::<i8>(a, i)))
}
extern "C" fn arrays_hash_short(a: u64) -> i32 {
    hash_narrow(a, |a, i| i32::from(prim::<i16>(a, i)))
}
extern "C" fn arrays_hash_char(a: u64) -> i32 {
    hash_narrow(a, |a, i| i32::from(prim::<u16>(a, i)))
}
extern "C" fn arrays_hash_bool(a: u64) -> i32 {
    hash_narrow(a, |a, i| if prim::<u8>(a, i) != 0 { 1231 } else { 1237 })
}
extern "C" fn arrays_copy_of_int(a: u64, n: i32) -> u64 {
    if n < 0 {
        throw("java/lang/NegativeArraySizeException", Some(&n.to_string()));
        return 0;
    }
    let b = alloc(A_DATA + 4 * n as usize, 4);
    // SAFETY: both arrays' int elements.
    unsafe {
        set_word(b, A_LEN, n as u64);
        std::ptr::copy_nonoverlapping(
            (a as usize + A_DATA) as *const u8,
            (b as usize + A_DATA) as *mut u8,
            4 * len(a).min(n as usize),
        );
    }
    b
}

// ---- Math ------------------------------------------------------------------

extern "C" fn math_round(a: f64) -> i64 {
    // Java: the closest long, ties up; NaN 0; beyond the range, the bound.
    if a.is_nan() {
        return 0;
    }
    let f = a.floor();
    let r = if a - f >= 0.5 { f + 1.0 } else { f };
    r as i64
}
extern "C" fn math_round_f(a: f32) -> i32 {
    if a.is_nan() {
        return 0;
    }
    let f = a.floor();
    let r = if a - f >= 0.5 { f + 1.0 } else { f };
    r as i32
}
extern "C" fn math_floor_div(a: i32, b: i32) -> i32 {
    if b == 0 {
        throw("java/lang/ArithmeticException", Some("/ by zero"));
        return 0;
    }
    a.wrapping_div(b) - i32::from((a % b != 0) && ((a ^ b) < 0))
}
extern "C" fn math_floor_div_l(a: i64, b: i64) -> i64 {
    if b == 0 {
        throw("java/lang/ArithmeticException", Some("/ by zero"));
        return 0;
    }
    a.wrapping_div(b) - i64::from((a % b != 0) && ((a ^ b) < 0))
}
extern "C" fn math_floor_mod(a: i32, b: i32) -> i32 {
    if b == 0 {
        throw("java/lang/ArithmeticException", Some("/ by zero"));
        return 0;
    }
    let m = a.wrapping_rem(b);
    if m != 0 && ((m ^ b) < 0) { m + b } else { m }
}
extern "C" fn math_floor_mod_l(a: i64, b: i64) -> i64 {
    if b == 0 {
        throw("java/lang/ArithmeticException", Some("/ by zero"));
        return 0;
    }
    let m = a.wrapping_rem(b);
    if m != 0 && ((m ^ b) < 0) { m + b } else { m }
}
extern "C" fn math_signum(a: f64) -> f64 {
    if a == 0.0 || a.is_nan() {
        a
    } else {
        a.signum()
    }
}
extern "C" fn math_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}
extern "C" fn math_atan2(a: f64, b: f64) -> f64 {
    a.atan2(b)
}
extern "C" fn math_clamp(v: i64, lo: i32, hi: i32) -> i32 {
    if lo > hi {
        throw(
            "java/lang/IllegalArgumentException",
            Some(&format!("{lo} > {hi}")),
        );
        return 0;
    }
    v.clamp(i64::from(lo), i64::from(hi)) as i32
}

// ---- Class, Enum, Object.getClass -------------------------------------------
//
// A Class: [vtable][class id][name (a String, with dots)].

fn class_name<'a>(c: u64) -> &'a [u16] {
    units(field(c, 16))
}
extern "C" fn class_get_name(c: u64) -> u64 {
    field(c, 16)
}
extern "C" fn class_get_simple_name(c: u64) -> u64 {
    let n = class_name(c);
    let start = n
        .iter()
        .rposition(|&u| u == u16::from(b'.') || u == u16::from(b'$'))
        .map_or(0, |i| i + 1);
    new_string(&n[start..])
}
extern "C" fn class_to_string(c: u64) -> u64 {
    let id = field(c, 8) as usize;
    let interface = rt()
        .ancestors
        .get(id)
        .is_some_and(|a| a.len() == 1 && false);
    let mut out: Vec<u16> = if interface { "interface " } else { "class " }
        .encode_utf16()
        .collect();
    out.extend_from_slice(class_name(c));
    new_string(&out)
}
extern "C" fn class_assertions(_c: u64) -> i32 {
    0
}
/// A class's name as the type tables have it (`java/lang/String`, `[I`).
fn class_slash_name(c: u64) -> String {
    String::from_utf16_lossy(class_name(c)).replace('.', "/")
}
const PRIMITIVES: [&str; 9] = [
    "boolean", "byte", "char", "short", "int", "long", "float", "double", "void",
];
/// Whether a value of class `b` can be assigned to class `a` (names as
/// the type tables have them).
fn assignable(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if PRIMITIVES.contains(&a) || PRIMITIVES.contains(&b) {
        return false;
    }
    if let Some(cb) = b.strip_prefix('[') {
        if matches!(
            a,
            "java/lang/Object" | "java/lang/Cloneable" | "java/io/Serializable"
        ) {
            return true;
        }
        let Some(ca) = a.strip_prefix('[') else {
            return false;
        };
        return match (ca.as_bytes()[0], cb.as_bytes()[0]) {
            (b'L', b'L') => assignable(&ca[1..ca.len() - 1], &cb[1..cb.len() - 1]),
            (b'[', b'[') => assignable(ca, cb),
            _ => ca == cb,
        };
    }
    if a.starts_with('[') {
        return false;
    }
    if a == "java/lang/Object" {
        return true;
    }
    let r = rt();
    r.ids
        .get(b)
        .and_then(|&id| r.ancestors.get(id as usize))
        .is_some_and(|anc| anc.contains(a))
}
/// The Class object of a class by name (`java/lang/String`, `[I`, `int`):
/// the program's own, or one made here (the same each time).
fn class_named(name: &str) -> u64 {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    let r = rt();
    if let Some(&id) = r.ids.get(name)
        && let Some(&c) = r.classes.get(id as usize)
        && c != 0
    {
        return c;
    }
    static MADE: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    let mut made = MADE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    *made.entry(name.to_owned()).or_insert_with(|| {
        let c = alloc(24, vt("java/lang/Class"));
        set(c, 16, str_of(&name.replace('/', ".")));
        c
    })
}
extern "C" fn class_is_assignable_from(c: u64, other: u64) -> i32 {
    if other == 0 {
        npe();
        return 0;
    }
    i32::from(c == other || assignable(&class_slash_name(c), &class_slash_name(other)))
}
extern "C" fn class_is_array(c: u64) -> i32 {
    i32::from(class_name(c).first() == Some(&u16::from(b'[')))
}
extern "C" fn class_is_primitive(c: u64) -> i32 {
    i32::from(PRIMITIVES.contains(&class_slash_name(c).as_str()))
}
extern "C" fn class_get_component_type(c: u64) -> u64 {
    let name = class_slash_name(c);
    let Some(comp) = name.strip_prefix('[') else {
        return 0;
    };
    let of = match comp.as_bytes()[0] {
        b'L' => comp[1..comp.len() - 1].to_owned(),
        b'[' => comp.to_owned(),
        b'Z' => "boolean".into(),
        b'B' => "byte".into(),
        b'C' => "char".into(),
        b'S' => "short".into(),
        b'I' => "int".into(),
        b'J' => "long".into(),
        b'F' => "float".into(),
        b'D' => "double".into(),
        _ => return 0,
    };
    class_named(&of)
}
fn class_id(c: u64) -> usize {
    field(c, 8) as usize
}
extern "C" fn class_get_superclass(c: u64) -> u64 {
    let r = rt();
    let id = class_id(c);
    if id == 0 {
        // An array's superclass is Object; other classes outside the
        // program are not known here.
        return if class_is_array(c) != 0 {
            class_named("java/lang/Object")
        } else {
            0
        };
    }
    if r.interfaces.get(id) == Some(&true) {
        return 0;
    }
    match r.supers.get(id) {
        Some(&s) if s != 0 => r.classes.get(s as usize).copied().unwrap_or(0),
        _ => 0,
    }
}
extern "C" fn class_is_interface(c: u64) -> i32 {
    let r = rt();
    let id = class_id(c);
    i32::from(if id == 0 {
        r.outside_interfaces.contains(&class_slash_name(c))
    } else {
        r.interfaces.get(id) == Some(&true)
    })
}
extern "C" fn class_is_enum(c: u64) -> i32 {
    let r = rt();
    let id = class_id(c);
    let parent = r.supers.get(id).copied().unwrap_or(0) as usize;
    i32::from(id != 0 && r.names.get(parent).is_some_and(|n| n == "java/lang/Enum"))
}
extern "C" fn class_for_name(name: u64) -> u64 {
    if name == 0 {
        npe();
        return 0;
    }
    let text = String::from_utf16_lossy(units(name));
    let slash = text.replace('.', "/");
    let r = rt();
    if slash.starts_with('[') || r.ids.contains_key(&slash) {
        return class_named(&slash);
    }
    throw("java/lang/ClassNotFoundException", Some(&text));
    0
}
extern "C" fn class_for_name_loader(name: u64, _init: i32, _loader: u64) -> u64 {
    class_for_name(name)
}
extern "C" fn class_is_instance(c: u64, o: u64) -> i32 {
    let name = String::from_utf16_lossy(class_name(c)).replace('.', "/");
    instance_of(o, &name) as i32
}
/// `o.getClass()`.
pub extern "C" fn object_get_class(o: u64) -> u64 {
    // SAFETY: an object's header; an array's is a small element size.
    let header = unsafe { word(o, 0) };
    if header < 4096 {
        return once("array class", || {
            let c = alloc(24, vt("java/lang/Class"));
            set(c, 16, str_of("[Ljava.lang.Object;"));
            c
        });
    }
    // SAFETY: a vtable starts with its class id.
    let id = unsafe { word(header, 0) } as usize;
    rt().classes.get(id).copied().unwrap_or(0)
}
// Enum: [vtable][name][ordinal].
extern "C" fn enum_init(e: u64, name: u64, ordinal: i32) {
    set(e, 8, name);
    set(e, 16, ordinal as u32 as u64);
}
extern "C" fn enum_name(e: u64) -> u64 {
    field(e, 8)
}
extern "C" fn enum_ordinal(e: u64) -> i32 {
    field(e, 16) as u32 as i32
}
extern "C" fn enum_compare(e: u64, o: u64) -> i32 {
    if o == 0 {
        npe();
        return 0;
    }
    enum_ordinal(e) - enum_ordinal(o)
}
extern "C" fn enum_declaring_class(e: u64) -> u64 {
    object_get_class(e)
}
extern "C" fn enum_value_of(class: u64, name: u64) -> u64 {
    if name == 0 {
        throw("java/lang/NullPointerException", Some("Name is null"));
        return 0;
    }
    let id = field(class, 8) as u32;
    if let Some(&at) = rt().enum_values.get(&id) {
        // SAFETY: the address of the enum's $VALUES static.
        let values = unsafe { *(at as *const u64) };
        for i in 0..len(values) {
            let e = get(values, i);
            if units(field(e, 8)) == units(name) {
                return e;
            }
        }
    }
    let class_text = String::from_utf16_lossy(class_name(class)).replace('$', ".");
    throw(
        "java/lang/IllegalArgumentException",
        Some(&format!(
            "No enum constant {class_text}.{}",
            String::from_utf16_lossy(units(name))
        )),
    );
    0
}

// ---- Interfaces' default methods (for the program's own classes) -----------

extern "C" fn iterable_for_each(c: u64, f: u64) {
    if let Some(items) = elements(c) {
        for_each_of(items, f);
    }
}
extern "C" fn iterator_for_each_remaining(it: u64, f: u64) {
    if f == 0 {
        npe();
        return;
    }
    while !pending() && call0_i(it, HAS_NEXT) != 0 {
        let e = call0(it, NEXT);
        if pending() {
            return;
        }
        call1(f, ACCEPT, e);
    }
}
extern "C" fn iterator_remove(_it: u64) {
    throw("java/lang/UnsupportedOperationException", Some("remove"));
}
extern "C" fn map_get_or_default(m: u64, k: u64, d: u64) -> u64 {
    let v = call1(m, "get (Ljava/lang/Object;)Ljava/lang/Object;", k);
    if pending() {
        return 0;
    }
    if v != 0 || call1_i(m, "containsKey (Ljava/lang/Object;)Z", k) != 0 {
        v
    } else {
        d
    }
}
extern "C" fn collection_is_empty(c: u64) -> i32 {
    (call0_i(c, SIZE) == 0) as i32
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
fn class(
    name: &'static str,
    super_name: &'static str,
    interfaces: &'static [&'static str],
    size: i32,
    methods: Vec<Method>,
) -> Builtin {
    Builtin {
        name,
        super_name: Some(super_name),
        interfaces,
        size,
        is_final: false,
        is_interface: false,
        methods,
    }
}
fn interface(
    name: &'static str,
    interfaces: &'static [&'static str],
    methods: Vec<Method>,
) -> Builtin {
    Builtin {
        name,
        super_name: Some("java/lang/Object"),
        interfaces,
        size: 8,
        is_final: false,
        is_interface: true,
        methods,
    }
}

macro_rules! f {
    ($e:expr) => {
        $e as *const () as usize
    };
}

/// The collections' and utilities' built-in classes.
// ---- AbstractCollection, AbstractList, AbstractSet, AbstractMap ---------------------
//
// The JDK's skeletons for the program's own collections (fastutil's
// extend them): their methods work through the subclass's size(),
// iterator(), get() and add().

extern "C" fn abstract_init(_c: u64) {}
extern "C" fn abstract_is_empty(c: u64) -> i32 {
    (call0_i(c, SIZE) == 0).into()
}
extern "C" fn abstract_contains(c: u64, o: u64) -> i32 {
    elements(c)
        .is_some_and(|items| items.iter().any(|&e| objects_equals(e, o)))
        .into()
}
extern "C" fn abstract_contains_all(c: u64, other: u64) -> i32 {
    let (Some(mine), Some(theirs)) = (elements(c), elements(other)) else {
        return 0;
    };
    theirs
        .iter()
        .all(|&t| mine.iter().any(|&e| objects_equals(e, t)))
        .into()
}
extern "C" fn abstract_add_all(c: u64, other: u64) -> i32 {
    let Some(items) = elements(other) else {
        return 0;
    };
    let mut changed = false;
    for e in items {
        changed |= call1_i(c, "add (Ljava/lang/Object;)Z", e) != 0;
        if pending() {
            return 0;
        }
    }
    changed.into()
}
extern "C" fn abstract_to_array(c: u64) -> u64 {
    let Some(items) = elements(c) else { return 0 };
    let a = obj_array(items.len());
    for (i, e) in items.iter().enumerate() {
        put(a, i, *e);
    }
    a
}
extern "C" fn abstract_to_string(c: u64) -> u64 {
    elements(c).map_or(0, |items| collection_string(c, &items))
}
extern "C" fn abstract_add_unsupported(_c: u64, _e: u64) -> i32 {
    unsupported();
    0
}
/// `AbstractList.add(e)`: `add(size(), e)`.
extern "C" fn abstract_list_add(l: u64, e: u64) -> i32 {
    let n = call0_i(l, SIZE);
    if pending() {
        return 0;
    }
    call2(l, "add (ILjava/lang/Object;)V", n as u64, e);
    (!pending()).into()
}
extern "C" fn abstract_list_add_at(_l: u64, _i: i32, _e: u64) {
    unsupported();
}
extern "C" fn abstract_list_index_of(l: u64, o: u64) -> i32 {
    elements(l).map_or(-1, |items| {
        items
            .iter()
            .position(|&e| objects_equals(e, o))
            .map_or(-1, |i| i as i32)
    })
}
extern "C" fn abstract_list_equals(l: u64, o: u64) -> i32 {
    elements(l).map_or(0, |items| list_equals(l, &items, o))
}
extern "C" fn abstract_list_hash(l: u64) -> i32 {
    elements(l).map_or(0, |items| list_hash(&items))
}
/// `AbstractList.iterator()`: by index, through the list's get and size.
/// [vtable][list][next index].
const INDEX_ITERATOR: &str = "horizon/rt/IndexIterator";
extern "C" fn abstract_list_iterator(l: u64) -> u64 {
    let it = new_of(INDEX_ITERATOR, 24);
    set(it, 8, l);
    it
}
extern "C" fn index_has_next(it: u64) -> i32 {
    ((field(it, 16) as i32) < call0_i(field(it, 8), SIZE)).into()
}
extern "C" fn index_next(it: u64) -> u64 {
    let (l, i) = (field(it, 8), field(it, 16) as i32);
    if i >= call0_i(l, SIZE) {
        throw("java/util/NoSuchElementException", None);
        return 0;
    }
    set(it, 16, (i + 1) as u64);
    call1(l, "get (I)Ljava/lang/Object;", i as u64)
}

/// `AbstractSet.hashCode`: the sum of the elements' hashes.
extern "C" fn abstract_set_hash(s: u64) -> i32 {
    elements(s).map_or(0, |items| {
        items
            .iter()
            .fold(0i32, |h, &e| h.wrapping_add(objects_hash(e)))
    })
}

fn abstract_builtins() -> Vec<Builtin> {
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
    let collection = || {
        vec![
            m("<init>", "()V", f!(abstract_init)),
            m("isEmpty", "()Z", f!(abstract_is_empty)),
            m("contains", "(Ljava/lang/Object;)Z", f!(abstract_contains)),
            m(
                "containsAll",
                "(Ljava/util/Collection;)Z",
                f!(abstract_contains_all),
            ),
            m("addAll", "(Ljava/util/Collection;)Z", f!(abstract_add_all)),
            m("toArray", "()[Ljava/lang/Object;", f!(abstract_to_array)),
            m("toString", "()Ljava/lang/String;", f!(abstract_to_string)),
            m("add", "(Ljava/lang/Object;)Z", f!(abstract_add_unsupported)),
        ]
    };
    let mut list = collection();
    list.retain(|x| x.name != "add");
    list.extend([
        m("add", "(Ljava/lang/Object;)Z", f!(abstract_list_add)),
        m("add", "(ILjava/lang/Object;)V", f!(abstract_list_add_at)),
        m(
            "indexOf",
            "(Ljava/lang/Object;)I",
            f!(abstract_list_index_of),
        ),
        m("equals", "(Ljava/lang/Object;)Z", f!(abstract_list_equals)),
        m("hashCode", "()I", f!(abstract_list_hash)),
        m(
            "iterator",
            "()Ljava/util/Iterator;",
            f!(abstract_list_iterator),
        ),
    ]);
    let mut set = collection();
    set.push(m("hashCode", "()I", f!(abstract_set_hash)));
    const O: &str = "java/lang/Object";
    vec![
        class(
            "java/util/AbstractCollection",
            O,
            &["java/util/Collection", "java/lang/Iterable"],
            8,
            collection(),
        ),
        class(
            "java/util/AbstractList",
            "java/util/AbstractCollection",
            &[
                "java/util/List",
                "java/util/Collection",
                "java/lang/Iterable",
            ],
            8,
            list,
        ),
        class(
            "java/util/AbstractSet",
            "java/util/AbstractCollection",
            &[
                "java/util/Set",
                "java/util/Collection",
                "java/lang/Iterable",
            ],
            8,
            set,
        ),
        class(
            INDEX_ITERATOR,
            O,
            &["java/util/Iterator"],
            24,
            vec![
                m("hasNext", "()Z", f!(index_has_next)),
                m("next", "()Ljava/lang/Object;", f!(index_next)),
            ],
        ),
        class(
            "java/util/AbstractMap",
            O,
            &["java/util/Map"],
            8,
            vec![m("<init>", "()V", f!(abstract_init))],
        ),
    ]
}

/// Streams and Collectors, and the functional interfaces' methods
/// (collections/streams.rs, collections/functions.rs).
pub fn stream_builtins() -> Vec<Builtin> {
    let mut out = streams::builtins();
    out.extend(functions::builtins());
    out.extend(abstract_builtins());
    out
}

pub fn builtins() -> Vec<Builtin> {
    const O: &str = "java/lang/Object";
    const LIST: &[&str] = &[
        "java/util/List",
        "java/util/Collection",
        "java/lang/Iterable",
        "java/util/SequencedCollection",
    ];
    const SET: &[&str] = &[
        "java/util/Set",
        "java/util/Collection",
        "java/lang/Iterable",
    ];
    const MAP: &[&str] = &["java/util/Map"];
    const ITER: &[&str] = &["java/util/Iterator"];
    let list_reads = |get: usize,
                      size: usize,
                      empty: usize,
                      contains: usize,
                      index: usize,
                      iter: usize,
                      string: usize,
                      eq: usize,
                      hash: usize,
                      each: usize,
                      array: usize| {
        vec![
            m("get", "(I)Ljava/lang/Object;", get),
            m("size", "()I", size),
            m("isEmpty", "()Z", empty),
            m("contains", "(Ljava/lang/Object;)Z", contains),
            m("indexOf", "(Ljava/lang/Object;)I", index),
            m("iterator", "()Ljava/util/Iterator;", iter),
            m("toString", "()Ljava/lang/String;", string),
            m("equals", "(Ljava/lang/Object;)Z", eq),
            m("hashCode", "()I", hash),
            m("forEach", "(Ljava/util/function/Consumer;)V", each),
            m("toArray", "()[Ljava/lang/Object;", array),
        ]
    };
    let immutable = || {
        vec![
            m("add", "(Ljava/lang/Object;)Z", f!(uoe_bool)),
            m("add", "(ILjava/lang/Object;)V", f!(uoe_bool_2)),
            m("remove", "(Ljava/lang/Object;)Z", f!(uoe_bool)),
            m("remove", "(I)Ljava/lang/Object;", f!(uoe_remove_at)),
            m("addAll", "(Ljava/util/Collection;)Z", f!(uoe_bool)),
            m("clear", "()V", f!(uoe_void)),
        ]
    };
    let mut list_n = list_reads(
        f!(fl_get),
        f!(fl_size),
        f!(fl_is_empty),
        f!(fl_contains),
        f!(fl_index_of),
        f!(fl_iterator),
        f!(fl_to_string),
        f!(fl_equals),
        f!(fl_hash),
        f!(fl_for_each),
        f!(fl_to_array),
    );
    list_n.extend(immutable());
    list_n.push(m(
        "set",
        "(ILjava/lang/Object;)Ljava/lang/Object;",
        f!(uoe_set),
    ));
    let mut as_list = list_reads(
        f!(fl_get),
        f!(fl_size),
        f!(fl_is_empty),
        f!(fl_contains),
        f!(fl_index_of),
        f!(fl_iterator),
        f!(fl_to_string),
        f!(fl_equals),
        f!(fl_hash),
        f!(fl_for_each),
        f!(fl_to_array),
    );
    as_list.extend(immutable());
    as_list.push(m(
        "set",
        "(ILjava/lang/Object;)Ljava/lang/Object;",
        f!(as_list_set),
    ));
    let mut set_n = vec![
        m("size", "()I", f!(fl_size)),
        m("isEmpty", "()Z", f!(fl_is_empty)),
        m("contains", "(Ljava/lang/Object;)Z", f!(fl_contains)),
        m("iterator", "()Ljava/util/Iterator;", f!(fl_iterator)),
        m("toString", "()Ljava/lang/String;", f!(fl_to_string)),
        m("equals", "(Ljava/lang/Object;)Z", f!(setn_equals)),
        m("hashCode", "()I", f!(setn_hash)),
        m(
            "forEach",
            "(Ljava/util/function/Consumer;)V",
            f!(fl_for_each),
        ),
        m("toArray", "()[Ljava/lang/Object;", f!(fl_to_array)),
    ];
    set_n.extend(immutable());
    let mut list_static = vec![
        s("of", "()Ljava/util/List;", f!(list_of0)),
        s("of", "(Ljava/lang/Object;)Ljava/util/List;", f!(list_of1)),
        s(
            "of",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;",
            f!(list_of2),
        ),
        s(
            "of",
            "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;",
            f!(list_of3),
        ),
        s(
            "of",
            "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;",
            f!(list_of4),
        ),
        s(
            "of",
            "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;",
            f!(list_of5),
        ),
        s(
            "of",
            "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/List;",
            f!(list_of6),
        ),
        s(
            "of",
            "([Ljava/lang/Object;)Ljava/util/List;",
            f!(list_of_array),
        ),
        s(
            "copyOf",
            "(Ljava/util/Collection;)Ljava/util/List;",
            f!(list_copy_of),
        ),
    ];
    list_static.push(m(
        "forEach",
        "(Ljava/util/function/Consumer;)V",
        f!(iterable_for_each),
    ));
    list_static.push(m("isEmpty", "()Z", f!(collection_is_empty)));
    vec![
        class(
            ARRAY_LIST,
            O,
            LIST,
            32,
            vec![
                m("<init>", "()V", f!(al_init)),
                m("<init>", "(I)V", f!(al_init_cap)),
                m("<init>", "(Ljava/util/Collection;)V", f!(al_init_from)),
                m("add", "(Ljava/lang/Object;)Z", f!(al_add)),
                m("add", "(ILjava/lang/Object;)V", f!(al_add_at)),
                m("get", "(I)Ljava/lang/Object;", f!(al_get)),
                m("set", "(ILjava/lang/Object;)Ljava/lang/Object;", f!(al_set)),
                m("remove", "(I)Ljava/lang/Object;", f!(al_remove_at)),
                m("remove", "(Ljava/lang/Object;)Z", f!(al_remove_object)),
                m("clear", "()V", f!(al_clear)),
                m("size", "()I", f!(al_size_c)),
                m("isEmpty", "()Z", f!(al_is_empty)),
                m("contains", "(Ljava/lang/Object;)Z", f!(al_contains)),
                m("indexOf", "(Ljava/lang/Object;)I", f!(al_index_of)),
                m("addAll", "(Ljava/util/Collection;)Z", f!(al_add_all)),
                m("iterator", "()Ljava/util/Iterator;", f!(al_iterator)),
                m(
                    "forEach",
                    "(Ljava/util/function/Consumer;)V",
                    f!(al_for_each),
                ),
                m(
                    "removeIf",
                    "(Ljava/util/function/Predicate;)Z",
                    f!(al_remove_if),
                ),
                m("toString", "()Ljava/lang/String;", f!(al_to_string)),
                m("equals", "(Ljava/lang/Object;)Z", f!(al_equals)),
                m("hashCode", "()I", f!(al_hash)),
                m("toArray", "()[Ljava/lang/Object;", f!(al_to_array)),
                m("sort", "(Ljava/util/Comparator;)V", f!(al_sort)),
            ],
        ),
        class(
            "java/util/ArrayList$Itr",
            O,
            ITER,
            40,
            vec![
                m("hasNext", "()Z", f!(itr_has_next)),
                m("next", "()Ljava/lang/Object;", f!(itr_next)),
                m("remove", "()V", f!(itr_remove)),
                m(
                    "forEachRemaining",
                    "(Ljava/util/function/Consumer;)V",
                    f!(iterator_for_each_remaining),
                ),
            ],
        ),
        class(
            "java/util/ImmutableCollections$ListItr",
            O,
            ITER,
            24,
            vec![
                m("hasNext", "()Z", f!(arr_it_has_next)),
                m("next", "()Ljava/lang/Object;", f!(arr_it_next)),
                m("remove", "()V", f!(iterator_remove)),
                m(
                    "forEachRemaining",
                    "(Ljava/util/function/Consumer;)V",
                    f!(iterator_for_each_remaining),
                ),
            ],
        ),
        class(LIST_N, O, LIST, 16, list_n),
        class(AS_LIST, O, LIST, 16, as_list),
        class(SET_N, O, SET, 16, set_n),
        class(
            HASH_MAP,
            O,
            MAP,
            40,
            vec![
                m("<init>", "()V", f!(hm_init)),
                m("<init>", "(I)V", f!(hm_init_cap)),
                m("get", "(Ljava/lang/Object;)Ljava/lang/Object;", f!(hm_get)),
                m(
                    "getOrDefault",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(hm_get_or_default),
                ),
                m("containsKey", "(Ljava/lang/Object;)Z", f!(hm_contains_key)),
                m(
                    "containsValue",
                    "(Ljava/lang/Object;)Z",
                    f!(hm_contains_value),
                ),
                m(
                    "put",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(hm_put_c),
                ),
                m(
                    "putIfAbsent",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(hm_put_if_absent),
                ),
                m(
                    "remove",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(hm_remove),
                ),
                m("size", "()I", f!(hm_size)),
                m("isEmpty", "()Z", f!(hm_is_empty)),
                m("clear", "()V", f!(hm_clear)),
                m(
                    "computeIfAbsent",
                    "(Ljava/lang/Object;Ljava/util/function/Function;)Ljava/lang/Object;",
                    f!(hm_compute_if_absent),
                ),
                m(
                    "compute",
                    "(Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
                    f!(hm_compute),
                ),
                m(
                    "merge",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/util/function/BiFunction;)Ljava/lang/Object;",
                    f!(hm_merge),
                ),
                m(
                    "forEach",
                    "(Ljava/util/function/BiConsumer;)V",
                    f!(hm_for_each),
                ),
                m("keySet", "()Ljava/util/Set;", f!(hm_key_set)),
                m("values", "()Ljava/util/Collection;", f!(hm_values)),
                m("entrySet", "()Ljava/util/Set;", f!(hm_entry_set)),
                m("toString", "()Ljava/lang/String;", f!(hm_to_string)),
                m("equals", "(Ljava/lang/Object;)Z", f!(hm_equals)),
                m("hashCode", "()I", f!(hm_hash)),
            ],
        ),
        class(
            NODE,
            O,
            &["java/util/Map$Entry"],
            40,
            vec![
                m("getKey", "()Ljava/lang/Object;", f!(node_key)),
                m("getValue", "()Ljava/lang/Object;", f!(node_value)),
                m(
                    "setValue",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(node_set_value),
                ),
                m("hashCode", "()I", f!(node_hash_code)),
                m("equals", "(Ljava/lang/Object;)Z", f!(node_equals)),
                m("toString", "()Ljava/lang/String;", f!(node_to_string)),
            ],
        ),
        class(
            KV,
            O,
            &["java/util/Map$Entry"],
            24,
            vec![
                m("getKey", "()Ljava/lang/Object;", f!(kv_key)),
                m("getValue", "()Ljava/lang/Object;", f!(kv_value)),
                m("hashCode", "()I", f!(node_hash_code)),
                m("equals", "(Ljava/lang/Object;)Z", f!(node_equals)),
                m("toString", "()Ljava/lang/String;", f!(node_to_string)),
            ],
        ),
        class(
            "java/util/HashMap$KeySet",
            O,
            SET,
            16,
            vec![
                m("size", "()I", f!(view_size)),
                m("isEmpty", "()Z", f!(view_is_empty)),
                m("clear", "()V", f!(view_clear)),
                m("contains", "(Ljava/lang/Object;)Z", f!(keys_contains)),
                m("remove", "(Ljava/lang/Object;)Z", f!(keys_remove)),
                m("iterator", "()Ljava/util/Iterator;", f!(keys_iterator)),
                m("toString", "()Ljava/lang/String;", f!(keys_to_string)),
                m(
                    "forEach",
                    "(Ljava/util/function/Consumer;)V",
                    f!(keys_for_each),
                ),
                m("hashCode", "()I", f!(keys_hash)),
                m("equals", "(Ljava/lang/Object;)Z", f!(keys_equals)),
            ],
        ),
        class(
            "java/util/HashMap$Values",
            O,
            &["java/util/Collection", "java/lang/Iterable"],
            16,
            vec![
                m("size", "()I", f!(view_size)),
                m("isEmpty", "()Z", f!(view_is_empty)),
                m("clear", "()V", f!(view_clear)),
                m("contains", "(Ljava/lang/Object;)Z", f!(values_contains)),
                m("iterator", "()Ljava/util/Iterator;", f!(values_iterator)),
                m("toString", "()Ljava/lang/String;", f!(values_to_string)),
                m(
                    "forEach",
                    "(Ljava/util/function/Consumer;)V",
                    f!(values_for_each),
                ),
            ],
        ),
        class(
            "java/util/HashMap$EntrySet",
            O,
            SET,
            16,
            vec![
                m("size", "()I", f!(view_size)),
                m("isEmpty", "()Z", f!(view_is_empty)),
                m("clear", "()V", f!(view_clear)),
                m("contains", "(Ljava/lang/Object;)Z", f!(entries_contains)),
                m("iterator", "()Ljava/util/Iterator;", f!(entries_iterator)),
                m("toString", "()Ljava/lang/String;", f!(entries_to_string)),
                m(
                    "forEach",
                    "(Ljava/util/function/Consumer;)V",
                    f!(entries_for_each),
                ),
            ],
        ),
        class(
            "java/util/HashMap$KeyIterator",
            O,
            ITER,
            48,
            vec![
                m("hasNext", "()Z", f!(hi_has_next)),
                m("next", "()Ljava/lang/Object;", f!(key_it_next)),
                m("remove", "()V", f!(hi_remove)),
                m(
                    "forEachRemaining",
                    "(Ljava/util/function/Consumer;)V",
                    f!(iterator_for_each_remaining),
                ),
            ],
        ),
        class(
            "java/util/HashMap$ValueIterator",
            O,
            ITER,
            48,
            vec![
                m("hasNext", "()Z", f!(hi_has_next)),
                m("next", "()Ljava/lang/Object;", f!(value_it_next)),
                m("remove", "()V", f!(hi_remove)),
                m(
                    "forEachRemaining",
                    "(Ljava/util/function/Consumer;)V",
                    f!(iterator_for_each_remaining),
                ),
            ],
        ),
        class(
            "java/util/HashMap$EntryIterator",
            O,
            ITER,
            48,
            vec![
                m("hasNext", "()Z", f!(hi_has_next)),
                m("next", "()Ljava/lang/Object;", f!(entry_it_next)),
                m("remove", "()V", f!(hi_remove)),
                m(
                    "forEachRemaining",
                    "(Ljava/util/function/Consumer;)V",
                    f!(iterator_for_each_remaining),
                ),
            ],
        ),
        class(
            "java/util/HashSet",
            O,
            SET,
            16,
            vec![
                m("<init>", "()V", f!(hs_init)),
                m("<init>", "(I)V", f!(hs_init_cap)),
                m("<init>", "(Ljava/util/Collection;)V", f!(hs_init_from)),
                m("add", "(Ljava/lang/Object;)Z", f!(hs_add)),
                m("addAll", "(Ljava/util/Collection;)Z", f!(hs_add_all)),
                m("contains", "(Ljava/lang/Object;)Z", f!(hs_contains)),
                m("remove", "(Ljava/lang/Object;)Z", f!(hs_remove)),
                m("size", "()I", f!(hs_size)),
                m("isEmpty", "()Z", f!(hs_is_empty)),
                m("clear", "()V", f!(hs_clear)),
                m("iterator", "()Ljava/util/Iterator;", f!(hs_iterator)),
                m("toString", "()Ljava/lang/String;", f!(hs_to_string)),
                m("hashCode", "()I", f!(hs_hash)),
                m("equals", "(Ljava/lang/Object;)Z", f!(hs_equals)),
                m(
                    "forEach",
                    "(Ljava/util/function/Consumer;)V",
                    f!(hs_for_each),
                ),
            ],
        ),
        class(
            MAP_N,
            O,
            MAP,
            24,
            vec![
                m(
                    "get",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(mapn_get),
                ),
                m(
                    "getOrDefault",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(mapn_get_or_default),
                ),
                m(
                    "containsKey",
                    "(Ljava/lang/Object;)Z",
                    f!(mapn_contains_key),
                ),
                m("size", "()I", f!(mapn_size)),
                m("isEmpty", "()Z", f!(mapn_is_empty)),
                m("keySet", "()Ljava/util/Set;", f!(mapn_key_set)),
                m("values", "()Ljava/util/Collection;", f!(mapn_values)),
                m("entrySet", "()Ljava/util/Set;", f!(mapn_entry_set)),
                m(
                    "forEach",
                    "(Ljava/util/function/BiConsumer;)V",
                    f!(mapn_for_each),
                ),
                m("toString", "()Ljava/lang/String;", f!(mapn_to_string)),
                m("hashCode", "()I", f!(mapn_hash)),
                m("equals", "(Ljava/lang/Object;)Z", f!(mapn_equals)),
                m(
                    "put",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(uoe_put),
                ),
                m(
                    "remove",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(uoe_remove),
                ),
                m("clear", "()V", f!(uoe_void)),
            ],
        ),
        Builtin {
            name: OPTIONAL,
            super_name: Some(O),
            interfaces: &[],
            size: 16,
            is_final: true,
            is_interface: false,
            methods: vec![
                s("empty", "()Ljava/util/Optional;", f!(optional_empty)),
                s(
                    "of",
                    "(Ljava/lang/Object;)Ljava/util/Optional;",
                    f!(optional_of),
                ),
                s(
                    "ofNullable",
                    "(Ljava/lang/Object;)Ljava/util/Optional;",
                    f!(optional_of_nullable),
                ),
                m("isPresent", "()Z", f!(optional_is_present)),
                m("isEmpty", "()Z", f!(optional_is_empty)),
                m("get", "()Ljava/lang/Object;", f!(optional_get)),
                m("orElseThrow", "()Ljava/lang/Object;", f!(optional_get)),
                m(
                    "orElseThrow",
                    "(Ljava/util/function/Supplier;)Ljava/lang/Object;",
                    f!(optional_or_else_throw),
                ),
                m(
                    "ifPresentOrElse",
                    "(Ljava/util/function/Consumer;Ljava/lang/Runnable;)V",
                    f!(optional_if_present_or_else),
                ),
                m(
                    "or",
                    "(Ljava/util/function/Supplier;)Ljava/util/Optional;",
                    f!(optional_or),
                ),
                m("stream", "()Ljava/util/stream/Stream;", f!(optional_stream)),
                m(
                    "orElse",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(optional_or_else),
                ),
                m(
                    "orElseGet",
                    "(Ljava/util/function/Supplier;)Ljava/lang/Object;",
                    f!(optional_or_else_get),
                ),
                m(
                    "map",
                    "(Ljava/util/function/Function;)Ljava/util/Optional;",
                    f!(optional_map),
                ),
                m(
                    "flatMap",
                    "(Ljava/util/function/Function;)Ljava/util/Optional;",
                    f!(optional_flat_map),
                ),
                m(
                    "filter",
                    "(Ljava/util/function/Predicate;)Ljava/util/Optional;",
                    f!(optional_filter),
                ),
                m(
                    "ifPresent",
                    "(Ljava/util/function/Consumer;)V",
                    f!(optional_if_present),
                ),
                m("equals", "(Ljava/lang/Object;)Z", f!(optional_equals)),
                m("hashCode", "()I", f!(optional_hash)),
                m("toString", "()Ljava/lang/String;", f!(optional_to_string)),
            ],
        },
        interface("java/util/List", &["java/util/Collection"], list_static),
        interface(
            "java/util/Collection",
            &["java/lang/Iterable"],
            vec![
                m("isEmpty", "()Z", f!(collection_is_empty)),
                m(
                    "stream",
                    "()Ljava/util/stream/Stream;",
                    f!(streams::collection_stream),
                ),
            ],
        ),
        interface(
            "java/lang/Iterable",
            &[],
            vec![m(
                "forEach",
                "(Ljava/util/function/Consumer;)V",
                f!(iterable_for_each),
            )],
        ),
        interface(
            "java/util/Iterator",
            &[],
            vec![
                m("remove", "()V", f!(iterator_remove)),
                m(
                    "forEachRemaining",
                    "(Ljava/util/function/Consumer;)V",
                    f!(iterator_for_each_remaining),
                ),
            ],
        ),
        interface(
            "java/util/Set",
            &["java/util/Collection"],
            vec![
                s("of", "()Ljava/util/Set;", f!(set_of0)),
                s("of", "(Ljava/lang/Object;)Ljava/util/Set;", f!(set_of1)),
                s(
                    "of",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Set;",
                    f!(set_of2),
                ),
                s(
                    "of",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Set;",
                    f!(set_of3),
                ),
                s(
                    "of",
                    "([Ljava/lang/Object;)Ljava/util/Set;",
                    f!(set_of_array),
                ),
            ],
        ),
        interface(
            "java/util/Map",
            &[],
            vec![
                s("of", "()Ljava/util/Map;", f!(map_of0)),
                s(
                    "of",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;",
                    f!(map_of1),
                ),
                s(
                    "of",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;",
                    f!(map_of2),
                ),
                s(
                    "of",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;",
                    f!(map_of3),
                ),
                s(
                    "of",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;",
                    f!(map_of4),
                ),
                s(
                    "of",
                    "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;",
                    f!(map_of5),
                ),
                m(
                    "getOrDefault",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(map_get_or_default),
                ),
            ],
        ),
        interface("java/util/Map$Entry", &[], vec![]),
        interface(
            "java/util/SequencedCollection",
            &["java/util/Collection"],
            vec![],
        ),
        Builtin {
            name: "java/util/Collections",
            super_name: Some(O),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                s("emptyList", "()Ljava/util/List;", f!(list_of0)),
                s("emptySet", "()Ljava/util/Set;", f!(set_of0)),
                s("emptyMap", "()Ljava/util/Map;", f!(map_of0)),
                s(
                    "max",
                    "(Ljava/util/Collection;)Ljava/lang/Object;",
                    f!(collections_max),
                ),
                s(
                    "max",
                    "(Ljava/util/Collection;Ljava/util/Comparator;)Ljava/lang/Object;",
                    f!(collections_max_by),
                ),
                s(
                    "min",
                    "(Ljava/util/Collection;)Ljava/lang/Object;",
                    f!(collections_min),
                ),
                s(
                    "min",
                    "(Ljava/util/Collection;Ljava/util/Comparator;)Ljava/lang/Object;",
                    f!(collections_min_by),
                ),
                s("sort", "(Ljava/util/List;)V", f!(collections_sort)),
                s(
                    "sort",
                    "(Ljava/util/List;Ljava/util/Comparator;)V",
                    f!(collections_sort_by),
                ),
                s("reverse", "(Ljava/util/List;)V", f!(collections_reverse)),
                s(
                    "frequency",
                    "(Ljava/util/Collection;Ljava/lang/Object;)I",
                    f!(collections_frequency),
                ),
                s(
                    "singletonList",
                    "(Ljava/lang/Object;)Ljava/util/List;",
                    f!(collections_singleton_list),
                ),
                s(
                    "singleton",
                    "(Ljava/lang/Object;)Ljava/util/Set;",
                    f!(collections_singleton),
                ),
                s(
                    "singletonMap",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/util/Map;",
                    f!(collections_singleton_map),
                ),
                s(
                    "nCopies",
                    "(ILjava/lang/Object;)Ljava/util/List;",
                    f!(collections_n_copies),
                ),
                s(
                    "unmodifiableList",
                    "(Ljava/util/List;)Ljava/util/List;",
                    f!(collections_same),
                ),
                s(
                    "unmodifiableSet",
                    "(Ljava/util/Set;)Ljava/util/Set;",
                    f!(collections_same),
                ),
                s(
                    "unmodifiableMap",
                    "(Ljava/util/Map;)Ljava/util/Map;",
                    f!(collections_same),
                ),
                s(
                    "unmodifiableCollection",
                    "(Ljava/util/Collection;)Ljava/util/Collection;",
                    f!(collections_same),
                ),
            ],
        },
        Builtin {
            name: "java/util/Arrays",
            super_name: Some(O),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                s(
                    "stream",
                    "([Ljava/lang/Object;)Ljava/util/stream/Stream;",
                    f!(streams::stream_of_array),
                ),
                s(
                    "asList",
                    "([Ljava/lang/Object;)Ljava/util/List;",
                    f!(arrays_as_list),
                ),
                s(
                    "toString",
                    "([I)Ljava/lang/String;",
                    f!(arrays_to_string_int),
                ),
                s(
                    "toString",
                    "([J)Ljava/lang/String;",
                    f!(arrays_to_string_long),
                ),
                s(
                    "toString",
                    "([D)Ljava/lang/String;",
                    f!(arrays_to_string_double),
                ),
                s(
                    "toString",
                    "([F)Ljava/lang/String;",
                    f!(arrays_to_string_float),
                ),
                s(
                    "toString",
                    "([B)Ljava/lang/String;",
                    f!(arrays_to_string_byte),
                ),
                s(
                    "toString",
                    "([Z)Ljava/lang/String;",
                    f!(arrays_to_string_bool),
                ),
                s(
                    "toString",
                    "([Ljava/lang/Object;)Ljava/lang/String;",
                    f!(arrays_to_string_objects),
                ),
                s(
                    "hashCode",
                    "([Ljava/lang/Object;)I",
                    f!(arrays_hash_objects),
                ),
                s("hashCode", "([I)I", f!(arrays_hash_int)),
                s("equals", "([I[I)Z", f!(arrays_equals_int)),
                s(
                    "equals",
                    "([Ljava/lang/Object;[Ljava/lang/Object;)Z",
                    f!(arrays_equals_objects),
                ),
                s("fill", "([II)V", f!(arrays_fill_int)),
                s(
                    "fill",
                    "([Ljava/lang/Object;Ljava/lang/Object;)V",
                    f!(arrays_fill_objects),
                ),
                s("sort", "([I)V", f!(arrays_sort_int)),
                s("sort", "([Ljava/lang/Object;)V", f!(arrays_sort_objects)),
                s(
                    "sort",
                    "([Ljava/lang/Object;Ljava/util/Comparator;)V",
                    f!(arrays_sort_by),
                ),
                s(
                    "copyOf",
                    "([Ljava/lang/Object;I)[Ljava/lang/Object;",
                    f!(arrays_copy_of_objects),
                ),
                s("copyOf", "([II)[I", f!(arrays_copy_of_int)),
                s("copyOf", "([JI)[J", f!(arrays_copy_of)),
                s("copyOf", "([DI)[D", f!(arrays_copy_of)),
                s("copyOf", "([FI)[F", f!(arrays_copy_of)),
                s("copyOf", "([BI)[B", f!(arrays_copy_of)),
                s("copyOf", "([SI)[S", f!(arrays_copy_of)),
                s("copyOf", "([CI)[C", f!(arrays_copy_of)),
                s("copyOf", "([ZI)[Z", f!(arrays_copy_of)),
                s(
                    "copyOfRange",
                    "([Ljava/lang/Object;II)[Ljava/lang/Object;",
                    f!(arrays_copy_of_range),
                ),
                s("copyOfRange", "([III)[I", f!(arrays_copy_of_range)),
                s("copyOfRange", "([JII)[J", f!(arrays_copy_of_range)),
                s("copyOfRange", "([DII)[D", f!(arrays_copy_of_range)),
                s("copyOfRange", "([FII)[F", f!(arrays_copy_of_range)),
                s("copyOfRange", "([BII)[B", f!(arrays_copy_of_range)),
                s("copyOfRange", "([SII)[S", f!(arrays_copy_of_range)),
                s("copyOfRange", "([CII)[C", f!(arrays_copy_of_range)),
                s("copyOfRange", "([ZII)[Z", f!(arrays_copy_of_range)),
                s("equals", "([J[J)Z", f!(arrays_equals_prim)),
                s("equals", "([D[D)Z", f!(arrays_equals_prim)),
                s("equals", "([F[F)Z", f!(arrays_equals_prim)),
                s("equals", "([B[B)Z", f!(arrays_equals_prim)),
                s("equals", "([S[S)Z", f!(arrays_equals_prim)),
                s("equals", "([C[C)Z", f!(arrays_equals_prim)),
                s("equals", "([Z[Z)Z", f!(arrays_equals_prim)),
                s("fill", "([JJ)V", f!(arrays_fill_long)),
                s("fill", "([DD)V", f!(arrays_fill_double)),
                s("fill", "([FF)V", f!(arrays_fill_float)),
                s("fill", "([BB)V", f!(arrays_fill_narrow)),
                s("fill", "([SS)V", f!(arrays_fill_narrow)),
                s("fill", "([CC)V", f!(arrays_fill_narrow)),
                s("fill", "([ZZ)V", f!(arrays_fill_narrow)),
                s("fill", "([IIII)V", f!(arrays_fill_narrow_range)),
                s("fill", "([JIIJ)V", f!(arrays_fill_long_range)),
                s("fill", "([DIID)V", f!(arrays_fill_double_range)),
                s("fill", "([FIIF)V", f!(arrays_fill_float_range)),
                s("fill", "([BIIB)V", f!(arrays_fill_narrow_range)),
                s("fill", "([SIIS)V", f!(arrays_fill_narrow_range)),
                s("fill", "([CIIC)V", f!(arrays_fill_narrow_range)),
                s("fill", "([ZIIZ)V", f!(arrays_fill_narrow_range)),
                s(
                    "fill",
                    "([Ljava/lang/Object;IILjava/lang/Object;)V",
                    f!(arrays_fill_objects_range),
                ),
                s("hashCode", "([J)I", f!(arrays_hash_long)),
                s("hashCode", "([D)I", f!(arrays_hash_double)),
                s("hashCode", "([F)I", f!(arrays_hash_float)),
                s("hashCode", "([B)I", f!(arrays_hash_byte)),
                s("hashCode", "([S)I", f!(arrays_hash_short)),
                s("hashCode", "([C)I", f!(arrays_hash_char)),
                s("hashCode", "([Z)I", f!(arrays_hash_bool)),
            ],
        },
        Builtin {
            name: "java/lang/System",
            super_name: Some(O),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![s(
                "arraycopy",
                "(Ljava/lang/Object;ILjava/lang/Object;II)V",
                f!(system_arraycopy),
            )],
        },
        Builtin {
            name: "java/lang/Math",
            super_name: Some(O),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                s("round", "(D)J", f!(math_round)),
                s("round", "(F)I", f!(math_round_f)),
                s("floorDiv", "(II)I", f!(math_floor_div)),
                s("floorDiv", "(JJ)J", f!(math_floor_div_l)),
                s("floorMod", "(II)I", f!(math_floor_mod)),
                s("floorMod", "(JJ)J", f!(math_floor_mod_l)),
                s("signum", "(D)D", f!(math_signum)),
                s("pow", "(DD)D", f!(math_pow)),
                s("atan2", "(DD)D", f!(math_atan2)),
                s("clamp", "(JII)I", f!(math_clamp)),
            ],
        },
        Builtin {
            name: "java/lang/Class",
            super_name: Some(O),
            interfaces: &[],
            size: 24,
            is_final: true,
            is_interface: false,
            methods: vec![
                m("getName", "()Ljava/lang/String;", f!(class_get_name)),
                m(
                    "getSimpleName",
                    "()Ljava/lang/String;",
                    f!(class_get_simple_name),
                ),
                m("toString", "()Ljava/lang/String;", f!(class_to_string)),
                m("desiredAssertionStatus", "()Z", f!(class_assertions)),
                m("isInstance", "(Ljava/lang/Object;)Z", f!(class_is_instance)),
                m(
                    "isAssignableFrom",
                    "(Ljava/lang/Class;)Z",
                    f!(class_is_assignable_from),
                ),
                m("isArray", "()Z", f!(class_is_array)),
                m("isPrimitive", "()Z", f!(class_is_primitive)),
                m("isInterface", "()Z", f!(class_is_interface)),
                m("isEnum", "()Z", f!(class_is_enum)),
                m(
                    "getComponentType",
                    "()Ljava/lang/Class;",
                    f!(class_get_component_type),
                ),
                m(
                    "componentType",
                    "()Ljava/lang/Class;",
                    f!(class_get_component_type),
                ),
                m(
                    "getSuperclass",
                    "()Ljava/lang/Class;",
                    f!(class_get_superclass),
                ),
                s(
                    "forName",
                    "(Ljava/lang/String;)Ljava/lang/Class;",
                    f!(class_for_name),
                ),
                s(
                    "forName",
                    "(Ljava/lang/String;ZLjava/lang/ClassLoader;)Ljava/lang/Class;",
                    f!(class_for_name_loader),
                ),
            ],
        },
        Builtin {
            name: "java/lang/Enum",
            super_name: Some(O),
            interfaces: &["java/lang/Comparable"],
            size: 24,
            is_final: false,
            is_interface: false,
            methods: vec![
                m("<init>", "(Ljava/lang/String;I)V", f!(enum_init)),
                m("name", "()Ljava/lang/String;", f!(enum_name)),
                m("ordinal", "()I", f!(enum_ordinal)),
                m("toString", "()Ljava/lang/String;", f!(enum_name)),
                m("compareTo", "(Ljava/lang/Enum;)I", f!(enum_compare)),
                m("compareTo", "(Ljava/lang/Object;)I", f!(enum_compare)),
                m(
                    "getDeclaringClass",
                    "()Ljava/lang/Class;",
                    f!(enum_declaring_class),
                ),
                s(
                    "valueOf",
                    "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
                    f!(enum_value_of),
                ),
            ],
        },
    ]
}
