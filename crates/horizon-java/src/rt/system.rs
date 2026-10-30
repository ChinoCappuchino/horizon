//! `java.lang.Runtime` and `java.util.concurrent.TimeUnit` for the native
//! backend's runtime library, and the JDK's static fields it provides
//! ([`static_getter`]): compiled code reads one by calling its getter.
use super::{Builtin, Method, alloc, collections::once, ctx, str_of, units as text, vt, word};
use std::time::Duration;

// ---- Runtime ----------------------------------------------------------------

extern "C" fn runtime_get() -> u64 {
    once("Runtime", || alloc(8, vt("java/lang/Runtime")))
}
extern "C" fn runtime_processors(_r: u64) -> i32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as i32)
}
/// The heap's reserved size: the most it can grow to.
extern "C" fn runtime_max_memory(_r: u64) -> i64 {
    crate::heap::RESERVE as i64
}
extern "C" fn runtime_total_memory(_r: u64) -> i64 {
    // SAFETY: this thread's context and its heap.
    unsafe { (*(*ctx()).heap).used() as i64 }
}
extern "C" fn runtime_free_memory(_r: u64) -> i64 {
    // Nothing is collected yet: what is handed out stays in use.
    0
}
extern "C" fn runtime_gc(_r: u64) {}

// ---- TimeUnit ---------------------------------------------------------------
//
// [vtable][ordinal]; one object per unit, made once.

const UNITS: [(&str, i64); 7] = [
    ("NANOSECONDS", 1),
    ("MICROSECONDS", 1_000),
    ("MILLISECONDS", 1_000_000),
    ("SECONDS", 1_000_000_000),
    ("MINUTES", 60_000_000_000),
    ("HOURS", 3_600_000_000_000),
    ("DAYS", 86_400_000_000_000),
];

fn units() -> [u64; 7] {
    let all = once("TimeUnit", || {
        let array = alloc(16 + 8 * 7, 0);
        for (k, _) in UNITS.iter().enumerate() {
            let u = alloc(16, vt("java/util/concurrent/TimeUnit"));
            // SAFETY: fresh objects of these sizes.
            unsafe {
                *((u + 8) as *mut u64) = k as u64;
                *((array + 16 + 8 * k as u64) as *mut u64) = u;
            }
        }
        array
    });
    // SAFETY: the array of seven units made above.
    std::array::from_fn(|k| unsafe { word(all, 16 + 8 * k) })
}

fn ordinal(u: u64) -> usize {
    // SAFETY: u is a TimeUnit.
    unsafe { word(u, 8) as usize }
}

/// `d` of `from` in `to`, saturating as Java's does.
fn convert(d: i64, from: usize, to: usize) -> i64 {
    let (a, b) = (UNITS[from].1, UNITS[to].1);
    if a >= b {
        d.saturating_mul(a / b)
    } else {
        d / (b / a)
    }
}

macro_rules! unit_to {
    ($name:ident, $to:expr) => {
        extern "C" fn $name(u: u64, d: i64) -> i64 {
            convert(d, ordinal(u), $to)
        }
    };
}
unit_to!(unit_to_nanos, 0);
unit_to!(unit_to_micros, 1);
unit_to!(unit_to_millis, 2);
unit_to!(unit_to_seconds, 3);
unit_to!(unit_to_minutes, 4);
unit_to!(unit_to_hours, 5);
unit_to!(unit_to_days, 6);

extern "C" fn unit_convert(u: u64, d: i64, from: u64) -> i64 {
    convert(d, ordinal(from), ordinal(u))
}
extern "C" fn unit_ordinal(u: u64) -> i32 {
    ordinal(u) as i32
}
extern "C" fn unit_name(u: u64) -> u64 {
    str_of(UNITS[ordinal(u)].0)
}
extern "C" fn unit_sleep(u: u64, d: i64) {
    let nanos = convert(d, ordinal(u), 0).max(0);
    std::thread::sleep(Duration::from_nanos(nanos as u64));
}

macro_rules! unit_get {
    ($name:ident, $k:expr) => {
        extern "C" fn $name() -> u64 {
            units()[$k]
        }
    };
}
unit_get!(nanoseconds, 0);
unit_get!(microseconds, 1);
unit_get!(milliseconds, 2);
unit_get!(seconds, 3);
unit_get!(minutes, 4);
unit_get!(hours, 5);
unit_get!(days, 6);

// ---- System -------------------------------------------------------------------

static START: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

extern "C" fn current_time_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
extern "C" fn nano_time() -> i64 {
    START.elapsed().as_nanos() as i64
}

/// A system property as the JDK names it; None when it has no value here.
fn property(key: &str) -> Option<String> {
    let os = match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Mac OS X",
        "windows" => "Windows 10",
        other => other,
    };
    Some(match key {
        "line.separator" => if cfg!(windows) { "\r\n" } else { "\n" }.to_owned(),
        "file.separator" => std::path::MAIN_SEPARATOR.to_string(),
        "path.separator" => if cfg!(windows) { ";" } else { ":" }.to_owned(),
        "file.encoding" | "native.encoding" | "stdout.encoding" => "UTF-8".to_owned(),
        "os.name" => os.to_owned(),
        "os.arch" => match std::env::consts::ARCH {
            "x86_64" => "amd64",
            other => other,
        }
        .to_owned(),
        "java.version" | "java.specification.version" => "21".to_owned(),
        "java.vendor" => "Horizon".to_owned(),
        "user.dir" => std::env::current_dir().ok()?.display().to_string(),
        "user.home" => std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?,
        "user.name" => std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok()?,
        "java.io.tmpdir" => std::env::temp_dir().display().to_string(),
        _ => return None,
    })
}

fn key_of(k: u64) -> Option<String> {
    (k != 0).then(|| String::from_utf16_lossy(text(k)))
}

extern "C" fn get_property(k: u64) -> u64 {
    key_of(k)
        .and_then(|k| property(&k))
        .map_or(0, |v| str_of(&v))
}
extern "C" fn get_property_or(k: u64, default: u64) -> u64 {
    key_of(k)
        .and_then(|k| property(&k))
        .map_or(default, |v| str_of(&v))
}
extern "C" fn get_env(k: u64) -> u64 {
    key_of(k)
        .and_then(|k| std::env::var(k).ok())
        .map_or(0, |v| str_of(&v))
}
extern "C" fn line_separator() -> u64 {
    str_of(if cfg!(windows) { "\r\n" } else { "\n" })
}

/// `System`'s methods beyond `arraycopy` (rt/collections.rs).
pub fn system_methods() -> Vec<Method> {
    let s = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: true,
        f,
    };
    vec![
        s(
            "currentTimeMillis",
            "()J",
            current_time_millis as *const () as usize,
        ),
        s("nanoTime", "()J", nano_time as *const () as usize),
        s(
            "getProperty",
            "(Ljava/lang/String;)Ljava/lang/String;",
            get_property as *const () as usize,
        ),
        s(
            "getProperty",
            "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            get_property_or as *const () as usize,
        ),
        s(
            "getenv",
            "(Ljava/lang/String;)Ljava/lang/String;",
            get_env as *const () as usize,
        ),
        s(
            "lineSeparator",
            "()Ljava/lang/String;",
            line_separator as *const () as usize,
        ),
        s(
            "identityHashCode",
            "(Ljava/lang/Object;)I",
            super::identity_hash as *const () as usize,
        ),
    ]
}

// ---- Math --------------------------------------------------------------------

macro_rules! unary {
    ($($name:ident = $e:expr;)*) => {
        $(extern "C" fn $name(x: f64) -> f64 {
            let f: fn(f64) -> f64 = $e;
            f(x)
        })*
    };
}
unary! {
    math_asin = f64::asin;
    math_acos = f64::acos;
    math_atan = f64::atan;
    math_tan = f64::tan;
    math_exp = f64::exp;
    math_log = f64::ln;
    math_log10 = f64::log10;
    math_log1p = f64::ln_1p;
    math_expm1 = f64::exp_m1;
    math_cbrt = f64::cbrt;
    math_sinh = f64::sinh;
    math_cosh = f64::cosh;
    math_tanh = f64::tanh;
    // The JDK's own constants, so the results are its.
    math_to_radians = |x| x * 0.017453292519943295;
    math_to_degrees = |x| x * 57.29577951308232;
}
extern "C" fn math_hypot(x: f64, y: f64) -> f64 {
    x.hypot(y)
}
extern "C" fn math_abs_l(x: i64) -> i64 {
    x.wrapping_abs()
}
extern "C" fn math_abs_f(x: f32) -> f32 {
    x.abs()
}
extern "C" fn math_max_l(a: i64, b: i64) -> i64 {
    a.max(b)
}
extern "C" fn math_min_l(a: i64, b: i64) -> i64 {
    a.min(b)
}
/// Java's float max: NaN wins, and 0.0 is greater than -0.0.
extern "C" fn math_max_f(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { b } else { a }
    } else {
        a.max(b)
    }
}
extern "C" fn math_min_f(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { a } else { b }
    } else {
        a.min(b)
    }
}
extern "C" fn math_signum_f(x: f32) -> f32 {
    if x == 0.0 || x.is_nan() {
        x
    } else {
        x.signum()
    }
}

/// `Math`'s methods beyond rt/collections.rs's and the compiler's own.
pub fn math_methods() -> Vec<Method> {
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
    vec![
        s("asin", "(D)D", f!(math_asin)),
        s("acos", "(D)D", f!(math_acos)),
        s("atan", "(D)D", f!(math_atan)),
        s("tan", "(D)D", f!(math_tan)),
        s("exp", "(D)D", f!(math_exp)),
        s("log", "(D)D", f!(math_log)),
        s("log10", "(D)D", f!(math_log10)),
        s("log1p", "(D)D", f!(math_log1p)),
        s("expm1", "(D)D", f!(math_expm1)),
        s("cbrt", "(D)D", f!(math_cbrt)),
        s("sinh", "(D)D", f!(math_sinh)),
        s("cosh", "(D)D", f!(math_cosh)),
        s("tanh", "(D)D", f!(math_tanh)),
        s("toRadians", "(D)D", f!(math_to_radians)),
        s("toDegrees", "(D)D", f!(math_to_degrees)),
        s("hypot", "(DD)D", f!(math_hypot)),
        s("abs", "(J)J", f!(math_abs_l)),
        s("abs", "(F)F", f!(math_abs_f)),
        s("max", "(JJ)J", f!(math_max_l)),
        s("min", "(JJ)J", f!(math_min_l)),
        s("max", "(FF)F", f!(math_max_f)),
        s("min", "(FF)F", f!(math_min_f)),
        s("signum", "(F)F", f!(math_signum_f)),
    ]
}

/// The JDK's static fields this library provides: `owner name` → its
/// getter and the getter's descriptor.
pub fn static_getter(owner: &str, name: &str) -> Option<(usize, &'static str)> {
    const UNIT: &str = "()Ljava/util/concurrent/TimeUnit;";
    let f = |f: extern "C" fn() -> u64| f as *const () as usize;
    Some(match (owner, name) {
        ("java/util/concurrent/TimeUnit", "NANOSECONDS") => (f(nanoseconds), UNIT),
        ("java/util/concurrent/TimeUnit", "MICROSECONDS") => (f(microseconds), UNIT),
        ("java/util/concurrent/TimeUnit", "MILLISECONDS") => (f(milliseconds), UNIT),
        ("java/util/concurrent/TimeUnit", "SECONDS") => (f(seconds), UNIT),
        ("java/util/concurrent/TimeUnit", "MINUTES") => (f(minutes), UNIT),
        ("java/util/concurrent/TimeUnit", "HOURS") => (f(hours), UNIT),
        ("java/util/concurrent/TimeUnit", "DAYS") => (f(days), UNIT),
        _ => return None,
    })
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
    const O: &str = "java/lang/Object";
    vec![
        Builtin {
            name: "java/lang/Runtime",
            super_name: Some(O),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                Method {
                    name: "getRuntime",
                    desc: "()Ljava/lang/Runtime;",
                    is_static: true,
                    f: f!(runtime_get),
                },
                m("availableProcessors", "()I", f!(runtime_processors)),
                m("maxMemory", "()J", f!(runtime_max_memory)),
                m("totalMemory", "()J", f!(runtime_total_memory)),
                m("freeMemory", "()J", f!(runtime_free_memory)),
                m("gc", "()V", f!(runtime_gc)),
            ],
        },
        Builtin {
            name: "java/util/concurrent/TimeUnit",
            super_name: Some(O),
            interfaces: &[],
            size: 16,
            is_final: true,
            is_interface: false,
            methods: vec![
                m("toNanos", "(J)J", f!(unit_to_nanos)),
                m("toMicros", "(J)J", f!(unit_to_micros)),
                m("toMillis", "(J)J", f!(unit_to_millis)),
                m("toSeconds", "(J)J", f!(unit_to_seconds)),
                m("toMinutes", "(J)J", f!(unit_to_minutes)),
                m("toHours", "(J)J", f!(unit_to_hours)),
                m("toDays", "(J)J", f!(unit_to_days)),
                m(
                    "convert",
                    "(JLjava/util/concurrent/TimeUnit;)J",
                    f!(unit_convert),
                ),
                m("ordinal", "()I", f!(unit_ordinal)),
                m("name", "()Ljava/lang/String;", f!(unit_name)),
                m("toString", "()Ljava/lang/String;", f!(unit_name)),
                m("sleep", "(J)V", f!(unit_sleep)),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::convert;

    #[test]
    fn float_max_and_min_follow_java() {
        use super::{math_max_f, math_min_f};
        assert!(math_max_f(f32::NAN, 1.0).is_nan());
        assert!(math_max_f(-0.0, 0.0).is_sign_positive());
        assert!(math_min_f(0.0, -0.0).is_sign_negative());
        assert_eq!(math_max_f(2.0, 3.0), 3.0);
    }

    #[test]
    fn time_units_convert_as_java_does() {
        assert_eq!(convert(3, 3, 2), 3_000); // seconds to millis
        assert_eq!(convert(1_999, 2, 3), 1); // millis to seconds, truncated
        assert_eq!(convert(i64::MAX / 2, 6, 0), i64::MAX); // saturates
        assert_eq!(convert(-90, 3, 4), -1); // toward zero
    }
}
