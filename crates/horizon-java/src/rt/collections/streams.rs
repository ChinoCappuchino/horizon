//! `java.util.stream.Stream` and `Collectors` for the native backend's
//! runtime library, written against the JDK's public API.
//!
//! A stream is a chain of stages in the heap, each one
//! [vtable][previous stage][operation][argument][count], built by the
//! intermediate operations. A terminal operation pulls the elements through
//! the chain one at a time, as Java's do: `findFirst`, `anyMatch` and
//! `limit` stop early (an infinite `iterate` or `generate` source is fine
//! with a limit); `sorted` and `distinct` take in everything before they go
//! on. Elements are references (a primitive stream is not here yet).
use super::{
    ACCEPT, APPLY, APPLY2, COMPARE, SUPPLY, TEST, al_add, call0, call1, call1_i, call2, call2_i,
    elements, field, get, hm_put_c, hs_add, hs_init, len, new_array_list, new_hash_map, new_of,
    npe, obj_array, objects_equals, objects_hash, optional, optional_empty, put, set, sort_items,
};
use crate::rt::{Builtin, Method, new_string, pending, throw, to_string, units};

pub(super) const STREAM: &str = "horizon/rt/Stream";
const COLLECTOR: &str = "horizon/rt/Collector";

// A stage: [vtable][previous][operation][argument][count].
const PREV: usize = 8;
const OP: usize = 16;
const ARG: usize = 24;
const COUNT: usize = 32;
const STAGE_SIZE: usize = 40;

// Sources (no previous stage), then operations on the previous one.
const OF_ARRAY: u64 = 0;
const ITERATE: u64 = 1;
const GENERATE: u64 = 2;
const CONCAT: u64 = 3;
const FILTER: u64 = 10;
const MAP: u64 = 11;
const FLAT_MAP: u64 = 12;
const PEEK: u64 = 13;
const LIMIT: u64 = 14;
const SKIP: u64 = 15;
const DISTINCT: u64 = 16;
const SORTED: u64 = 17;
const TAKE_WHILE: u64 = 18;
const DROP_WHILE: u64 = 19;

fn stage(prev: u64, op: u64, arg: u64, count: u64) -> u64 {
    let s = new_of(STREAM, STAGE_SIZE);
    set(s, PREV, prev);
    set(s, OP, op);
    set(s, ARG, arg);
    set(s, COUNT, count);
    s
}

/// A stream of an array's elements.
pub(super) fn of_array(a: u64) -> u64 {
    stage(0, OF_ARRAY, a, 0)
}

/// A stream of a Collection's elements (`Collection.stream()`).
pub(super) extern "C" fn collection_stream(c: u64) -> u64 {
    let Some(items) = elements(c) else { return 0 };
    let a = obj_array(items.len());
    for (i, e) in items.iter().enumerate() {
        put(a, i, *e);
    }
    of_array(a)
}

/// Feeds stage `s`'s elements to `sink` until it says stop (false) or an
/// exception is pending; false when it stopped.
fn pull(s: u64, sink: &mut dyn FnMut(u64) -> bool) -> bool {
    let arg = field(s, ARG);
    let count = field(s, COUNT);
    let prev = field(s, PREV);
    match field(s, OP) {
        OF_ARRAY => {
            for i in 0..len(arg) {
                if !sink(get(arg, i)) || pending() {
                    return false;
                }
            }
            true
        }
        ITERATE => {
            // [arg: the function][count: the seed]
            let mut x = count;
            loop {
                if !sink(x) || pending() {
                    return false;
                }
                x = call1(arg, APPLY, x);
                if pending() {
                    return false;
                }
            }
        }
        GENERATE => loop {
            let x = call0(arg, SUPPLY);
            if pending() || !sink(x) || pending() {
                return false;
            }
        },
        CONCAT => pull(prev, sink) && pull(arg, sink),
        FILTER => pull(prev, &mut |e| {
            let keep = call1_i(arg, TEST, e) != 0;
            pending() || !keep || sink(e)
        }),
        MAP => pull(prev, &mut |e| {
            let x = call1(arg, APPLY, e);
            !pending() && sink(x)
        }),
        FLAT_MAP => pull(prev, &mut |e| {
            let inner = call1(arg, APPLY, e);
            if pending() {
                return false;
            }
            inner == 0 || pull(inner, sink)
        }),
        PEEK => pull(prev, &mut |e| {
            call1(arg, ACCEPT, e);
            !pending() && sink(e)
        }),
        LIMIT => {
            if count == 0 {
                return true;
            }
            let mut taken = 0;
            let mut stopped_early = false;
            let done = pull(prev, &mut |e| {
                taken += 1;
                if !sink(e) {
                    stopped_early = true;
                    return false;
                }
                taken < count
            });
            // The limit ending the pull is not the consumer stopping.
            done || !stopped_early
        }
        SKIP => {
            let mut seen = 0;
            pull(prev, &mut |e| {
                seen += 1;
                seen <= count || sink(e)
            })
        }
        TAKE_WHILE => {
            let mut stopped_early = false;
            let done = pull(prev, &mut |e| {
                let keep = call1_i(arg, TEST, e) != 0;
                if pending() || !keep {
                    return false;
                }
                if !sink(e) {
                    stopped_early = true;
                    return false;
                }
                true
            });
            done || (!stopped_early && !pending())
        }
        DROP_WHILE => {
            let mut dropping = true;
            pull(prev, &mut |e| {
                if dropping {
                    let drop = call1_i(arg, TEST, e) != 0;
                    if pending() {
                        return false;
                    }
                    if drop {
                        return true;
                    }
                    dropping = false;
                }
                sink(e)
            })
        }
        DISTINCT => {
            let Some(items) = gather(prev) else {
                return false;
            };
            // The first of each equal group, by hash then equals.
            let mut kept: std::collections::HashMap<i32, Vec<u64>> = Default::default();
            for e in items {
                let bucket = kept.entry(objects_hash(e)).or_default();
                if bucket.iter().any(|&k| objects_equals(k, e)) {
                    continue;
                }
                bucket.push(e);
                if !sink(e) || pending() {
                    return false;
                }
            }
            true
        }
        SORTED => {
            let Some(mut items) = gather(prev) else {
                return false;
            };
            sort_items(&mut items, arg);
            if pending() {
                return false;
            }
            items.into_iter().all(|e| sink(e) && !pending())
        }
        _ => true,
    }
}

/// Every element of stage `s`; None when an exception is pending.
fn gather(s: u64) -> Option<Vec<u64>> {
    let mut out = Vec::new();
    pull(s, &mut |e| {
        out.push(e);
        true
    });
    (!pending()).then_some(out)
}

// ---- Intermediate operations --------------------------------------------------

fn needs(f: u64) -> bool {
    if f == 0 {
        npe();
    }
    f != 0
}
macro_rules! with_fn {
    ($($name:ident = $op:expr;)*) => {
        $(extern "C" fn $name(s: u64, f: u64) -> u64 {
            if !needs(f) {
                return 0;
            }
            stage(s, $op, f, 0)
        })*
    };
}
with_fn! {
    stream_filter = FILTER;
    stream_map = MAP;
    stream_flat_map = FLAT_MAP;
    stream_peek = PEEK;
    stream_take_while = TAKE_WHILE;
    stream_drop_while = DROP_WHILE;
}
extern "C" fn stream_limit(s: u64, n: i64) -> u64 {
    if n < 0 {
        throw("java/lang/IllegalArgumentException", Some(&n.to_string()));
        return 0;
    }
    stage(s, LIMIT, 0, n as u64)
}
extern "C" fn stream_skip(s: u64, n: i64) -> u64 {
    if n < 0 {
        throw("java/lang/IllegalArgumentException", Some(&n.to_string()));
        return 0;
    }
    stage(s, SKIP, 0, n as u64)
}
extern "C" fn stream_distinct(s: u64) -> u64 {
    stage(s, DISTINCT, 0, 0)
}
extern "C" fn stream_sorted(s: u64) -> u64 {
    stage(s, SORTED, 0, 0)
}
extern "C" fn stream_sorted_by(s: u64, cmp: u64) -> u64 {
    stage(s, SORTED, cmp, 0)
}
/// Streams are sequential here: these change nothing.
extern "C" fn stream_same(s: u64) -> u64 {
    s
}

// ---- Sources --------------------------------------------------------------------

pub(super) extern "C" fn stream_of_array(a: u64) -> u64 {
    if a == 0 {
        npe();
        return 0;
    }
    of_array(a)
}
extern "C" fn stream_of_one(e: u64) -> u64 {
    let a = obj_array(1);
    put(a, 0, e);
    of_array(a)
}
extern "C" fn stream_empty() -> u64 {
    of_array(obj_array(0))
}
extern "C" fn stream_iterate(seed: u64, f: u64) -> u64 {
    if !needs(f) {
        return 0;
    }
    stage(0, ITERATE, f, seed)
}
extern "C" fn stream_generate(f: u64) -> u64 {
    if !needs(f) {
        return 0;
    }
    stage(0, GENERATE, f, 0)
}
extern "C" fn stream_concat(a: u64, b: u64) -> u64 {
    if a == 0 || b == 0 {
        npe();
        return 0;
    }
    stage(a, CONCAT, b, 0)
}

// ---- Terminal operations -----------------------------------------------------

extern "C" fn stream_for_each(s: u64, f: u64) {
    if needs(f) {
        pull(s, &mut |e| {
            call1(f, ACCEPT, e);
            !pending()
        });
    }
}
fn list_of(items: &[u64]) -> u64 {
    let l = new_array_list();
    for &e in items {
        al_add(l, e);
    }
    l
}
extern "C" fn stream_to_list(s: u64) -> u64 {
    gather(s).map_or(0, |items| list_of(&items))
}
extern "C" fn stream_to_array(s: u64) -> u64 {
    let Some(items) = gather(s) else { return 0 };
    let a = obj_array(items.len());
    for (i, e) in items.iter().enumerate() {
        put(a, i, *e);
    }
    a
}
extern "C" fn stream_count(s: u64) -> i64 {
    let mut n = 0i64;
    pull(s, &mut |_| {
        n += 1;
        true
    });
    n
}
/// The first element, if any (null is an error, as in Java).
fn first(s: u64) -> Option<u64> {
    let mut found = None;
    pull(s, &mut |e| {
        found = Some(e);
        false
    });
    found
}
extern "C" fn stream_find_first(s: u64) -> u64 {
    match first(s) {
        _ if pending() => 0,
        Some(0) => {
            npe();
            0
        }
        Some(e) => optional(e),
        None => optional_empty(),
    }
}
/// anyMatch (wanted: true), allMatch (true, negated), noneMatch.
fn matches(s: u64, f: u64, stop_on: bool) -> bool {
    if !needs(f) {
        return false;
    }
    let mut hit = false;
    pull(s, &mut |e| {
        let r = call1_i(f, TEST, e) != 0;
        if pending() {
            return false;
        }
        if r == stop_on {
            hit = true;
            return false;
        }
        true
    });
    hit
}
extern "C" fn stream_any_match(s: u64, f: u64) -> i32 {
    matches(s, f, true).into()
}
extern "C" fn stream_all_match(s: u64, f: u64) -> i32 {
    (!matches(s, f, false) && !pending()).into()
}
extern "C" fn stream_none_match(s: u64, f: u64) -> i32 {
    (!matches(s, f, true) && !pending()).into()
}
extern "C" fn stream_reduce_identity(s: u64, identity: u64, f: u64) -> u64 {
    if !needs(f) {
        return 0;
    }
    let mut acc = identity;
    pull(s, &mut |e| {
        acc = call2(f, APPLY2, acc, e);
        !pending()
    });
    acc
}
extern "C" fn stream_reduce(s: u64, f: u64) -> u64 {
    if !needs(f) {
        return 0;
    }
    let mut acc: Option<u64> = None;
    pull(s, &mut |e| {
        acc = Some(match acc {
            None => e,
            Some(a) => call2(f, APPLY2, a, e),
        });
        !pending()
    });
    match acc {
        _ if pending() => 0,
        Some(a) => optional(a),
        None => optional_empty(),
    }
}
/// min (sign -1) or max (sign 1) by comparator: the first of equals wins
/// for min, the last for max (as Java's reduce does).
fn extreme(s: u64, cmp: u64, sign: i32) -> u64 {
    if !needs(cmp) {
        return 0;
    }
    let mut best: Option<u64> = None;
    pull(s, &mut |e| {
        best = Some(match best {
            None => e,
            Some(b) => {
                let c = call2_i(cmp, COMPARE, b, e);
                if (sign < 0 && c > 0) || (sign > 0 && c <= 0) {
                    e
                } else {
                    b
                }
            }
        });
        !pending()
    });
    match best {
        _ if pending() => 0,
        Some(b) => optional(b),
        None => optional_empty(),
    }
}
extern "C" fn stream_min(s: u64, cmp: u64) -> u64 {
    extreme(s, cmp, -1)
}
extern "C" fn stream_max(s: u64, cmp: u64) -> u64 {
    extreme(s, cmp, 1)
}

// ---- Collectors -------------------------------------------------------------------
//
// A collector: [vtable][kind][a][b][c] (its functions and texts).

const TO_LIST: u64 = 0;
const TO_SET: u64 = 1;
const TO_MAP: u64 = 2;
const JOINING: u64 = 3;
const GROUPING_BY: u64 = 4;
const COUNTING: u64 = 5;
const MAPPING: u64 = 6;

fn collector(kind: u64, a: u64, b: u64, c: u64) -> u64 {
    let k = new_of(COLLECTOR, 40);
    set(k, 8, kind);
    set(k, 16, a);
    set(k, 24, b);
    set(k, 32, c);
    k
}
macro_rules! collectors {
    ($($name:ident() = $kind:expr;)*) => {
        $(extern "C" fn $name() -> u64 {
            collector($kind, 0, 0, 0)
        })*
    };
}
collectors! {
    collectors_to_list() = TO_LIST;
    collectors_to_set() = TO_SET;
    collectors_counting() = COUNTING;
    collectors_joining() = JOINING;
}
extern "C" fn collectors_to_map(k: u64, v: u64) -> u64 {
    collector(TO_MAP, k, v, 0)
}
extern "C" fn collectors_to_map_merge(k: u64, v: u64, merge: u64) -> u64 {
    collector(TO_MAP, k, v, merge)
}
extern "C" fn collectors_joining_with(sep: u64) -> u64 {
    collector(JOINING, sep, 0, 0)
}
extern "C" fn collectors_joining_full(sep: u64, prefix: u64, suffix: u64) -> u64 {
    collector(JOINING, sep, prefix, suffix)
}
extern "C" fn collectors_grouping_by(f: u64) -> u64 {
    collector(GROUPING_BY, f, 0, 0)
}
extern "C" fn collectors_grouping_by_with(f: u64, downstream: u64) -> u64 {
    collector(GROUPING_BY, f, downstream, 0)
}
extern "C" fn collectors_mapping(f: u64, downstream: u64) -> u64 {
    collector(MAPPING, f, downstream, 0)
}

/// What collector `k` makes of `items`.
fn finish(k: u64, items: &[u64]) -> u64 {
    let (a, b, c) = (field(k, 16), field(k, 24), field(k, 32));
    match field(k, 8) {
        TO_LIST => list_of(items),
        TO_SET => {
            let set = new_of("java/util/HashSet", 16);
            hs_init(set);
            for &e in items {
                hs_add(set, e);
            }
            set
        }
        COUNTING => {
            let (_, value_of, _) = crate::rt::boxing("J").expect("Long's boxing");
            // SAFETY: Long.valueOf(long).
            let value_of: extern "C" fn(i64) -> u64 = unsafe { std::mem::transmute(value_of) };
            value_of(items.len() as i64)
        }
        JOINING => {
            let text = |s: u64| {
                if s == 0 {
                    Vec::new()
                } else {
                    units(s).to_vec()
                }
            };
            let mut out = text(b);
            for (i, &e) in items.iter().enumerate() {
                if i > 0 {
                    out.extend(text(a));
                }
                out.extend_from_slice(units(e));
            }
            out.extend(text(c));
            new_string(&out)
        }
        TO_MAP => {
            let m = new_hash_map();
            for &e in items {
                let key = call1(a, APPLY, e);
                let value = call1(b, APPLY, e);
                if pending() {
                    return 0;
                }
                if value == 0 {
                    npe();
                    return 0;
                }
                let old = super::hm_get(m, key);
                let value = if old == 0 {
                    value
                } else if c != 0 {
                    call2(c, APPLY2, old, value)
                } else {
                    let message = format!(
                        "Duplicate key {} (attempted merging values {} and {})",
                        String::from_utf16_lossy(units(to_string(key))),
                        String::from_utf16_lossy(units(to_string(old))),
                        String::from_utf16_lossy(units(to_string(value)))
                    );
                    throw("java/lang/IllegalStateException", Some(&message));
                    return 0;
                };
                hm_put_c(m, key, value);
            }
            m
        }
        GROUPING_BY | MAPPING => {
            if field(k, 8) == MAPPING {
                let mut mapped = Vec::with_capacity(items.len());
                for &e in items {
                    mapped.push(call1(a, APPLY, e));
                    if pending() {
                        return 0;
                    }
                }
                let downstream = if b == 0 {
                    collector(TO_LIST, 0, 0, 0)
                } else {
                    b
                };
                return finish(downstream, &mapped);
            }
            // Groups in the order their keys first came, as a HashMap.
            let mut keys: Vec<u64> = Vec::new();
            let mut groups: Vec<Vec<u64>> = Vec::new();
            for &e in items {
                let key = call1(a, APPLY, e);
                if pending() {
                    return 0;
                }
                if key == 0 {
                    throw(
                        "java/lang/NullPointerException",
                        Some("element cannot be mapped to a null key"),
                    );
                    return 0;
                }
                match keys.iter().position(|&x| objects_equals(x, key)) {
                    Some(i) => groups[i].push(e),
                    None => {
                        keys.push(key);
                        groups.push(vec![e]);
                    }
                }
            }
            let downstream = if b == 0 {
                collector(TO_LIST, 0, 0, 0)
            } else {
                b
            };
            let m = new_hash_map();
            for (key, group) in keys.iter().zip(&groups) {
                let v = finish(downstream, group);
                if pending() {
                    return 0;
                }
                hm_put_c(m, *key, v);
            }
            m
        }
        _ => 0,
    }
}

extern "C" fn stream_collect(s: u64, k: u64) -> u64 {
    if !needs(k) {
        return 0;
    }
    let Some(items) = gather(s) else { return 0 };
    finish(k, &items)
}

pub(super) fn builtins() -> Vec<Builtin> {
    let m = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: false,
        f,
    };
    let st = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: true,
        f,
    };
    macro_rules! f {
        ($f:expr) => {
            $f as *const () as usize
        };
    }
    const O: &str = "java/lang/Object";
    let interface = |name: &'static str, methods| Builtin {
        name,
        super_name: Some(O),
        interfaces: &[],
        size: 8,
        is_final: false,
        is_interface: true,
        methods,
    };
    let stream_methods = vec![
        m(
            "filter",
            "(Ljava/util/function/Predicate;)Ljava/util/stream/Stream;",
            f!(stream_filter),
        ),
        m(
            "map",
            "(Ljava/util/function/Function;)Ljava/util/stream/Stream;",
            f!(stream_map),
        ),
        m(
            "flatMap",
            "(Ljava/util/function/Function;)Ljava/util/stream/Stream;",
            f!(stream_flat_map),
        ),
        m(
            "peek",
            "(Ljava/util/function/Consumer;)Ljava/util/stream/Stream;",
            f!(stream_peek),
        ),
        m(
            "takeWhile",
            "(Ljava/util/function/Predicate;)Ljava/util/stream/Stream;",
            f!(stream_take_while),
        ),
        m(
            "dropWhile",
            "(Ljava/util/function/Predicate;)Ljava/util/stream/Stream;",
            f!(stream_drop_while),
        ),
        m("limit", "(J)Ljava/util/stream/Stream;", f!(stream_limit)),
        m("skip", "(J)Ljava/util/stream/Stream;", f!(stream_skip)),
        m(
            "distinct",
            "()Ljava/util/stream/Stream;",
            f!(stream_distinct),
        ),
        m("sorted", "()Ljava/util/stream/Stream;", f!(stream_sorted)),
        m(
            "sorted",
            "(Ljava/util/Comparator;)Ljava/util/stream/Stream;",
            f!(stream_sorted_by),
        ),
        m(
            "sequential",
            "()Ljava/util/stream/BaseStream;",
            f!(stream_same),
        ),
        m(
            "parallel",
            "()Ljava/util/stream/BaseStream;",
            f!(stream_same),
        ),
        m(
            "unordered",
            "()Ljava/util/stream/BaseStream;",
            f!(stream_same),
        ),
        m(
            "forEach",
            "(Ljava/util/function/Consumer;)V",
            f!(stream_for_each),
        ),
        m(
            "forEachOrdered",
            "(Ljava/util/function/Consumer;)V",
            f!(stream_for_each),
        ),
        m("toList", "()Ljava/util/List;", f!(stream_to_list)),
        m("toArray", "()[Ljava/lang/Object;", f!(stream_to_array)),
        m("count", "()J", f!(stream_count)),
        m("findFirst", "()Ljava/util/Optional;", f!(stream_find_first)),
        m("findAny", "()Ljava/util/Optional;", f!(stream_find_first)),
        m(
            "anyMatch",
            "(Ljava/util/function/Predicate;)Z",
            f!(stream_any_match),
        ),
        m(
            "allMatch",
            "(Ljava/util/function/Predicate;)Z",
            f!(stream_all_match),
        ),
        m(
            "noneMatch",
            "(Ljava/util/function/Predicate;)Z",
            f!(stream_none_match),
        ),
        m(
            "reduce",
            "(Ljava/lang/Object;Ljava/util/function/BinaryOperator;)Ljava/lang/Object;",
            f!(stream_reduce_identity),
        ),
        m(
            "reduce",
            "(Ljava/util/function/BinaryOperator;)Ljava/util/Optional;",
            f!(stream_reduce),
        ),
        m(
            "min",
            "(Ljava/util/Comparator;)Ljava/util/Optional;",
            f!(stream_min),
        ),
        m(
            "max",
            "(Ljava/util/Comparator;)Ljava/util/Optional;",
            f!(stream_max),
        ),
        m(
            "collect",
            "(Ljava/util/stream/Collector;)Ljava/lang/Object;",
            f!(stream_collect),
        ),
    ];
    vec![
        interface("java/util/stream/BaseStream", Vec::new()),
        Builtin {
            name: "java/util/stream/Stream",
            super_name: Some(O),
            interfaces: &["java/util/stream/BaseStream"],
            size: 8,
            is_final: false,
            is_interface: true,
            methods: vec![
                st(
                    "of",
                    "([Ljava/lang/Object;)Ljava/util/stream/Stream;",
                    f!(stream_of_array),
                ),
                st(
                    "of",
                    "(Ljava/lang/Object;)Ljava/util/stream/Stream;",
                    f!(stream_of_one),
                ),
                st("empty", "()Ljava/util/stream/Stream;", f!(stream_empty)),
                st(
                    "iterate",
                    "(Ljava/lang/Object;Ljava/util/function/UnaryOperator;)Ljava/util/stream/Stream;",
                    f!(stream_iterate),
                ),
                st(
                    "generate",
                    "(Ljava/util/function/Supplier;)Ljava/util/stream/Stream;",
                    f!(stream_generate),
                ),
                st(
                    "concat",
                    "(Ljava/util/stream/Stream;Ljava/util/stream/Stream;)Ljava/util/stream/Stream;",
                    f!(stream_concat),
                ),
            ],
        },
        Builtin {
            name: STREAM,
            super_name: Some(O),
            interfaces: &["java/util/stream/Stream", "java/util/stream/BaseStream"],
            size: STAGE_SIZE as i32,
            is_final: true,
            is_interface: false,
            methods: stream_methods,
        },
        interface("java/util/stream/Collector", Vec::new()),
        Builtin {
            name: COLLECTOR,
            super_name: Some(O),
            interfaces: &["java/util/stream/Collector"],
            size: 40,
            is_final: true,
            is_interface: false,
            methods: Vec::new(),
        },
        Builtin {
            name: "java/util/stream/Collectors",
            super_name: Some(O),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                st(
                    "toList",
                    "()Ljava/util/stream/Collector;",
                    f!(collectors_to_list),
                ),
                st(
                    "toUnmodifiableList",
                    "()Ljava/util/stream/Collector;",
                    f!(collectors_to_list),
                ),
                st(
                    "toSet",
                    "()Ljava/util/stream/Collector;",
                    f!(collectors_to_set),
                ),
                st(
                    "toUnmodifiableSet",
                    "()Ljava/util/stream/Collector;",
                    f!(collectors_to_set),
                ),
                st(
                    "counting",
                    "()Ljava/util/stream/Collector;",
                    f!(collectors_counting),
                ),
                st(
                    "joining",
                    "()Ljava/util/stream/Collector;",
                    f!(collectors_joining),
                ),
                st(
                    "joining",
                    "(Ljava/lang/CharSequence;)Ljava/util/stream/Collector;",
                    f!(collectors_joining_with),
                ),
                st(
                    "joining",
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Ljava/util/stream/Collector;",
                    f!(collectors_joining_full),
                ),
                st(
                    "toMap",
                    "(Ljava/util/function/Function;Ljava/util/function/Function;)Ljava/util/stream/Collector;",
                    f!(collectors_to_map),
                ),
                st(
                    "toMap",
                    "(Ljava/util/function/Function;Ljava/util/function/Function;Ljava/util/function/BinaryOperator;)Ljava/util/stream/Collector;",
                    f!(collectors_to_map_merge),
                ),
                st(
                    "groupingBy",
                    "(Ljava/util/function/Function;)Ljava/util/stream/Collector;",
                    f!(collectors_grouping_by),
                ),
                st(
                    "groupingBy",
                    "(Ljava/util/function/Function;Ljava/util/stream/Collector;)Ljava/util/stream/Collector;",
                    f!(collectors_grouping_by_with),
                ),
                st(
                    "mapping",
                    "(Ljava/util/function/Function;Ljava/util/stream/Collector;)Ljava/util/stream/Collector;",
                    f!(collectors_mapping),
                ),
            ],
        },
    ]
}
