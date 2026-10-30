//! The functional interfaces' static and default methods, for the native
//! backend's runtime library, written against the JDK's public API:
//! `Function.identity/andThen/compose`, `Predicate.and/or/negate/not/
//! isEqual`, `Consumer.andThen`, `BiFunction.andThen` and `Comparator`'s
//! `comparing*`, `thenComparing*`, `reversed`, `naturalOrder`,
//! `reverseOrder`, `nullsFirst/Last`.
//!
//! The interfaces are the runtime's (a lambda of the program implements
//! them and so has their default methods); what the methods make is one
//! class, [`COMPOSED`]: [vtable][kind][a][b], whose `apply`, `test`,
//! `accept` and `compare` do what its kind says with its parts.
use super::{
    ACCEPT, APPLY, COMPARE, COMPARE_TO, TEST, call1, call1_i, call2, call2_i, field, new_of, npe,
    objects_equals, set,
};
use crate::rt::{Builtin, Method, pending};

const COMPOSED: &str = "horizon/rt/Composed";
const APPLY_INT: &str = "applyAsInt (Ljava/lang/Object;)I";
const APPLY_LONG: &str = "applyAsLong (Ljava/lang/Object;)J";
const APPLY_DOUBLE: &str = "applyAsDouble (Ljava/lang/Object;)D";

// Functions.
const IDENTITY: u64 = 0;
const AND_THEN: u64 = 1;
const COMPOSE: u64 = 2;
// Predicates.
const AND: u64 = 10;
const OR: u64 = 11;
const NEGATE: u64 = 12;
const IS_EQUAL: u64 = 13;
// Comparators.
const NATURAL: u64 = 20;
const REVERSE: u64 = 21;
const COMPARING: u64 = 22;
const COMPARING_BY: u64 = 23;
const COMPARING_INT: u64 = 24;
const COMPARING_LONG: u64 = 25;
const COMPARING_DOUBLE: u64 = 26;
const THEN: u64 = 27;
const REVERSED: u64 = 28;
const NULLS_FIRST: u64 = 29;
const NULLS_LAST: u64 = 30;
// Consumers, then BiFunction.andThen.
const CONSUMER_THEN: u64 = 40;
const BI_THEN: u64 = 41;

fn composed(kind: u64, a: u64, b: u64) -> u64 {
    let c = new_of(COMPOSED, 32);
    set(c, 8, kind);
    set(c, 16, a);
    set(c, 24, b);
    c
}
fn parts(c: u64) -> (u64, u64, u64) {
    (field(c, 8), field(c, 16), field(c, 24))
}
/// `composed`, its parts tested for null first (Java's requireNonNull).
fn make(kind: u64, a: u64, b: u64, need: usize) -> u64 {
    if (need >= 1 && a == 0) || (need >= 2 && b == 0) {
        npe();
        return 0;
    }
    composed(kind, a, b)
}

// ---- What a composed object does ----------------------------------------------

extern "C" fn composed_apply(c: u64, x: u64) -> u64 {
    let (kind, a, b) = parts(c);
    match kind {
        IDENTITY => x,
        AND_THEN => {
            let y = call1(a, APPLY, x);
            if pending() { 0 } else { call1(b, APPLY, y) }
        }
        COMPOSE => {
            let y = call1(b, APPLY, x);
            if pending() { 0 } else { call1(a, APPLY, y) }
        }
        _ => 0,
    }
}
extern "C" fn composed_apply2(c: u64, x: u64, y: u64) -> u64 {
    let (kind, a, b) = parts(c);
    if kind != BI_THEN {
        return 0;
    }
    let r = call2(
        a,
        "apply (Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        x,
        y,
    );
    if pending() { 0 } else { call1(b, APPLY, r) }
}
extern "C" fn composed_test(c: u64, x: u64) -> i32 {
    let (kind, a, b) = parts(c);
    let test = |f: u64| call1_i(f, TEST, x) != 0;
    match kind {
        AND => (test(a) && !pending() && test(b)).into(),
        OR => (test(a) || (!pending() && test(b))).into(),
        NEGATE => (!test(a)).into(),
        IS_EQUAL => objects_equals(a, x).into(),
        _ => 0,
    }
}
extern "C" fn composed_accept(c: u64, x: u64) {
    let (kind, a, b) = parts(c);
    if kind == CONSUMER_THEN {
        call1(a, ACCEPT, x);
        if !pending() {
            call1(b, ACCEPT, x);
        }
    }
}
/// Natural order: `x.compareTo(y)` (null is an error, as in Java).
fn natural(x: u64, y: u64) -> i32 {
    if x == 0 || y == 0 {
        npe();
        return 0;
    }
    call1_i(x, COMPARE_TO, y)
}
/// Java's `Double.compare`: -0.0 below 0.0, NaN above everything.
fn double_compare(x: f64, y: f64) -> i32 {
    let key = |v: f64| {
        let bits = if v.is_nan() {
            f64::NAN.to_bits()
        } else {
            v.to_bits()
        } as i64;
        bits ^ (((bits >> 63) as u64) >> 1) as i64
    };
    key(x).cmp(&key(y)) as i32
}
extern "C" fn composed_compare(c: u64, x: u64, y: u64) -> i32 {
    let (kind, a, b) = parts(c);
    match kind {
        NATURAL => natural(x, y),
        REVERSE => natural(y, x),
        COMPARING => {
            let (kx, ky) = (call1(a, APPLY, x), call1(a, APPLY, y));
            if pending() { 0 } else { natural(kx, ky) }
        }
        COMPARING_BY => {
            let (kx, ky) = (call1(a, APPLY, x), call1(a, APPLY, y));
            if pending() {
                0
            } else {
                call2_i(b, COMPARE, kx, ky)
            }
        }
        COMPARING_INT => call1_i(a, APPLY_INT, x).cmp(&call1_i(a, APPLY_INT, y)) as i32,
        COMPARING_LONG => {
            let (kx, ky) = (
                call1(a, APPLY_LONG, x) as i64,
                call1(a, APPLY_LONG, y) as i64,
            );
            kx.cmp(&ky) as i32
        }
        COMPARING_DOUBLE => {
            let (kx, ky) = (
                super::call1_d(a, APPLY_DOUBLE, x),
                super::call1_d(a, APPLY_DOUBLE, y),
            );
            if pending() { 0 } else { double_compare(kx, ky) }
        }
        THEN => {
            let r = call2_i(a, COMPARE, x, y);
            if r != 0 || pending() {
                r
            } else {
                call2_i(b, COMPARE, x, y)
            }
        }
        REVERSED => call2_i(a, COMPARE, y, x),
        NULLS_FIRST | NULLS_LAST => {
            let first = if kind == NULLS_FIRST { -1 } else { 1 };
            match (x == 0, y == 0) {
                (true, true) => 0,
                (true, false) => first,
                (false, true) => -first,
                _ if a == 0 => 0,
                _ => call2_i(a, COMPARE, x, y),
            }
        }
        _ => 0,
    }
}

// ---- The interfaces' methods ------------------------------------------------------

extern "C" fn identity() -> u64 {
    composed(IDENTITY, 0, 0)
}
extern "C" fn function_and_then(f: u64, after: u64) -> u64 {
    make(AND_THEN, f, after, 2)
}
extern "C" fn function_compose(f: u64, before: u64) -> u64 {
    make(COMPOSE, f, before, 2)
}
extern "C" fn bi_and_then(f: u64, after: u64) -> u64 {
    make(BI_THEN, f, after, 2)
}
extern "C" fn predicate_and(p: u64, q: u64) -> u64 {
    make(AND, p, q, 2)
}
extern "C" fn predicate_or(p: u64, q: u64) -> u64 {
    make(OR, p, q, 2)
}
extern "C" fn predicate_negate(p: u64) -> u64 {
    make(NEGATE, p, 0, 1)
}
extern "C" fn predicate_is_equal(o: u64) -> u64 {
    composed(IS_EQUAL, o, 0)
}
extern "C" fn consumer_and_then(c: u64, after: u64) -> u64 {
    make(CONSUMER_THEN, c, after, 2)
}
extern "C" fn natural_order() -> u64 {
    composed(NATURAL, 0, 0)
}
extern "C" fn reverse_order() -> u64 {
    composed(REVERSE, 0, 0)
}
extern "C" fn comparing(key: u64) -> u64 {
    make(COMPARING, key, 0, 1)
}
extern "C" fn comparing_by(key: u64, cmp: u64) -> u64 {
    make(COMPARING_BY, key, cmp, 2)
}
extern "C" fn comparing_int(key: u64) -> u64 {
    make(COMPARING_INT, key, 0, 1)
}
extern "C" fn comparing_long(key: u64) -> u64 {
    make(COMPARING_LONG, key, 0, 1)
}
extern "C" fn comparing_double(key: u64) -> u64 {
    make(COMPARING_DOUBLE, key, 0, 1)
}
extern "C" fn nulls_first(cmp: u64) -> u64 {
    composed(NULLS_FIRST, cmp, 0)
}
extern "C" fn nulls_last(cmp: u64) -> u64 {
    composed(NULLS_LAST, cmp, 0)
}
extern "C" fn then_comparing(c: u64, other: u64) -> u64 {
    make(THEN, c, other, 2)
}
extern "C" fn then_comparing_key(c: u64, key: u64) -> u64 {
    let other = comparing(key);
    if other == 0 {
        0
    } else {
        composed(THEN, c, other)
    }
}
extern "C" fn then_comparing_key_by(c: u64, key: u64, cmp: u64) -> u64 {
    let other = comparing_by(key, cmp);
    if other == 0 {
        0
    } else {
        composed(THEN, c, other)
    }
}
extern "C" fn then_comparing_int(c: u64, key: u64) -> u64 {
    let other = comparing_int(key);
    if other == 0 {
        0
    } else {
        composed(THEN, c, other)
    }
}
extern "C" fn then_comparing_long(c: u64, key: u64) -> u64 {
    let other = comparing_long(key);
    if other == 0 {
        0
    } else {
        composed(THEN, c, other)
    }
}
extern "C" fn then_comparing_double(c: u64, key: u64) -> u64 {
    let other = comparing_double(key);
    if other == 0 {
        0
    } else {
        composed(THEN, c, other)
    }
}
extern "C" fn reversed(c: u64) -> u64 {
    composed(REVERSED, c, 0)
}

pub(super) fn builtins() -> Vec<Builtin> {
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
    macro_rules! f {
        ($f:expr) => {
            $f as *const () as usize
        };
    }
    const O: &str = "java/lang/Object";
    let interface = |name: &'static str, interfaces: &'static [&'static str], methods| Builtin {
        name,
        super_name: Some(O),
        interfaces,
        size: 8,
        is_final: false,
        is_interface: true,
        methods,
    };
    vec![
        interface(
            "java/util/function/Function",
            &[],
            vec![
                s("identity", "()Ljava/util/function/Function;", f!(identity)),
                m(
                    "andThen",
                    "(Ljava/util/function/Function;)Ljava/util/function/Function;",
                    f!(function_and_then),
                ),
                m(
                    "compose",
                    "(Ljava/util/function/Function;)Ljava/util/function/Function;",
                    f!(function_compose),
                ),
            ],
        ),
        interface(
            "java/util/function/UnaryOperator",
            &["java/util/function/Function"],
            vec![s(
                "identity",
                "()Ljava/util/function/UnaryOperator;",
                f!(identity),
            )],
        ),
        interface(
            "java/util/function/BiFunction",
            &[],
            vec![m(
                "andThen",
                "(Ljava/util/function/Function;)Ljava/util/function/BiFunction;",
                f!(bi_and_then),
            )],
        ),
        interface(
            "java/util/function/BinaryOperator",
            &["java/util/function/BiFunction"],
            Vec::new(),
        ),
        interface(
            "java/util/function/Predicate",
            &[],
            vec![
                m(
                    "and",
                    "(Ljava/util/function/Predicate;)Ljava/util/function/Predicate;",
                    f!(predicate_and),
                ),
                m(
                    "or",
                    "(Ljava/util/function/Predicate;)Ljava/util/function/Predicate;",
                    f!(predicate_or),
                ),
                m(
                    "negate",
                    "()Ljava/util/function/Predicate;",
                    f!(predicate_negate),
                ),
                s(
                    "not",
                    "(Ljava/util/function/Predicate;)Ljava/util/function/Predicate;",
                    f!(predicate_negate),
                ),
                s(
                    "isEqual",
                    "(Ljava/lang/Object;)Ljava/util/function/Predicate;",
                    f!(predicate_is_equal),
                ),
            ],
        ),
        interface(
            "java/util/function/Consumer",
            &[],
            vec![m(
                "andThen",
                "(Ljava/util/function/Consumer;)Ljava/util/function/Consumer;",
                f!(consumer_and_then),
            )],
        ),
        interface("java/util/function/Supplier", &[], Vec::new()),
        interface("java/util/function/BiConsumer", &[], Vec::new()),
        interface("java/util/function/ToIntFunction", &[], Vec::new()),
        interface("java/util/function/ToLongFunction", &[], Vec::new()),
        interface("java/util/function/ToDoubleFunction", &[], Vec::new()),
        interface(
            "java/util/Comparator",
            &[],
            vec![
                s(
                    "naturalOrder",
                    "()Ljava/util/Comparator;",
                    f!(natural_order),
                ),
                s(
                    "reverseOrder",
                    "()Ljava/util/Comparator;",
                    f!(reverse_order),
                ),
                s(
                    "comparing",
                    "(Ljava/util/function/Function;)Ljava/util/Comparator;",
                    f!(comparing),
                ),
                s(
                    "comparing",
                    "(Ljava/util/function/Function;Ljava/util/Comparator;)Ljava/util/Comparator;",
                    f!(comparing_by),
                ),
                s(
                    "comparingInt",
                    "(Ljava/util/function/ToIntFunction;)Ljava/util/Comparator;",
                    f!(comparing_int),
                ),
                s(
                    "comparingLong",
                    "(Ljava/util/function/ToLongFunction;)Ljava/util/Comparator;",
                    f!(comparing_long),
                ),
                s(
                    "comparingDouble",
                    "(Ljava/util/function/ToDoubleFunction;)Ljava/util/Comparator;",
                    f!(comparing_double),
                ),
                s(
                    "nullsFirst",
                    "(Ljava/util/Comparator;)Ljava/util/Comparator;",
                    f!(nulls_first),
                ),
                s(
                    "nullsLast",
                    "(Ljava/util/Comparator;)Ljava/util/Comparator;",
                    f!(nulls_last),
                ),
                m("reversed", "()Ljava/util/Comparator;", f!(reversed)),
                m(
                    "thenComparing",
                    "(Ljava/util/Comparator;)Ljava/util/Comparator;",
                    f!(then_comparing),
                ),
                m(
                    "thenComparing",
                    "(Ljava/util/function/Function;)Ljava/util/Comparator;",
                    f!(then_comparing_key),
                ),
                m(
                    "thenComparing",
                    "(Ljava/util/function/Function;Ljava/util/Comparator;)Ljava/util/Comparator;",
                    f!(then_comparing_key_by),
                ),
                m(
                    "thenComparingInt",
                    "(Ljava/util/function/ToIntFunction;)Ljava/util/Comparator;",
                    f!(then_comparing_int),
                ),
                m(
                    "thenComparingLong",
                    "(Ljava/util/function/ToLongFunction;)Ljava/util/Comparator;",
                    f!(then_comparing_long),
                ),
                m(
                    "thenComparingDouble",
                    "(Ljava/util/function/ToDoubleFunction;)Ljava/util/Comparator;",
                    f!(then_comparing_double),
                ),
            ],
        ),
        Builtin {
            name: COMPOSED,
            super_name: Some(O),
            interfaces: &[
                "java/util/function/Function",
                "java/util/function/UnaryOperator",
                "java/util/function/BiFunction",
                "java/util/function/Predicate",
                "java/util/function/Consumer",
                "java/util/Comparator",
            ],
            size: 32,
            is_final: true,
            is_interface: false,
            methods: vec![
                m(
                    "apply",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(composed_apply),
                ),
                m(
                    "apply",
                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                    f!(composed_apply2),
                ),
                m("test", "(Ljava/lang/Object;)Z", f!(composed_test)),
                m("accept", "(Ljava/lang/Object;)V", f!(composed_accept)),
                m(
                    "compare",
                    "(Ljava/lang/Object;Ljava/lang/Object;)I",
                    f!(composed_compare),
                ),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::double_compare;

    #[test]
    fn doubles_compare_as_java_does() {
        assert_eq!(double_compare(-0.0, 0.0), -1);
        assert_eq!(double_compare(f64::NAN, f64::INFINITY), 1);
        assert_eq!(double_compare(f64::NAN, f64::NAN), 0);
        assert_eq!(double_compare(1.0, 2.0), -1);
        assert_eq!(double_compare(-3.0, -2.0), -1);
    }
}
