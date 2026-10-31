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

/// `d` of TimeUnit `unit` in nanoseconds (for timeouts).
pub(super) fn to_nanos(d: i64, unit: u64) -> i64 {
    if unit == 0 {
        d
    } else {
        convert(d, ordinal(unit), 0)
    }
}

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

// ---- Locale ----------------------------------------------------------------------
//
// [vtable][language String][country String]; the constants made once.

const LOCALE: &str = "java/util/Locale";
const LOCALES: [(&str, &str, &str); 6] = [
    ("ROOT", "", ""),
    ("ENGLISH", "en", ""),
    ("US", "en", "US"),
    ("UK", "en", "GB"),
    ("GERMAN", "de", ""),
    ("FRENCH", "fr", ""),
];

fn locale(k: usize) -> u64 {
    let all = once("Locale", || {
        let array = alloc(16 + 8 * LOCALES.len(), 0);
        for (i, (_, language, country)) in LOCALES.iter().enumerate() {
            let l = alloc(24, vt(LOCALE));
            // SAFETY: fresh objects of these sizes.
            unsafe {
                *((l + 8) as *mut u64) = str_of(language);
                *((l + 16) as *mut u64) = str_of(country);
                *((array + 16 + 8 * i as u64) as *mut u64) = l;
            }
        }
        array
    });
    // SAFETY: the array of locales made above.
    unsafe { word(all, 16 + 8 * k) }
}

macro_rules! locale_get {
    ($($name:ident = $k:expr;)*) => {
        $(extern "C" fn $name() -> u64 {
            locale($k)
        })*
    };
}
locale_get! {
    locale_root = 0;
    locale_english = 1;
    locale_us = 2;
    locale_uk = 3;
    locale_german = 4;
    locale_french = 5;
}
extern "C" fn locale_language(l: u64) -> u64 {
    // SAFETY: l is a Locale.
    unsafe { word(l, 8) }
}
extern "C" fn locale_country(l: u64) -> u64 {
    // SAFETY: as above.
    unsafe { word(l, 16) }
}
fn locale_text(l: u64, sep: &str) -> String {
    let language = String::from_utf16_lossy(text(locale_language(l)));
    let country = String::from_utf16_lossy(text(locale_country(l)));
    if country.is_empty() {
        language
    } else {
        format!("{language}{sep}{country}")
    }
}
extern "C" fn locale_to_string(l: u64) -> u64 {
    str_of(&locale_text(l, "_"))
}
extern "C" fn locale_tag(l: u64) -> u64 {
    let tag = locale_text(l, "-");
    str_of(if tag.is_empty() { "und" } else { &tag })
}

// ---- DateTimeFormatter -----------------------------------------------------------
//
// [vtable][pattern String][locale][zone]: the pattern kept for formatting,
// which comes with java.time's types.

const FORMATTER: &str = "java/time/format/DateTimeFormatter";

fn formatter(pattern: u64, locale: u64, zone: u64) -> u64 {
    let f = alloc(32, vt(FORMATTER));
    // SAFETY: a fresh formatter.
    unsafe {
        *((f + 8) as *mut u64) = pattern;
        *((f + 16) as *mut u64) = locale;
        *((f + 24) as *mut u64) = zone;
    }
    f
}
extern "C" fn formatter_of_pattern(pattern: u64) -> u64 {
    formatter(pattern, locale_root(), 0)
}
extern "C" fn formatter_of_pattern_locale(pattern: u64, locale: u64) -> u64 {
    formatter(pattern, locale, 0)
}
/// The JDK's predefined formatters, by name (their name is kept as the
/// pattern until formatting is here).
fn predefined(name: &'static str) -> u64 {
    once(name, || formatter(str_of(name), locale_root(), 0))
}
macro_rules! predefined {
    ($($fn_name:ident = $name:expr;)*) => {
        $(extern "C" fn $fn_name() -> u64 {
            predefined($name)
        })*
    };
}
predefined! {
    iso_instant = "ISO_INSTANT";
    iso_local_date = "ISO_LOCAL_DATE";
    iso_local_time = "ISO_LOCAL_TIME";
    iso_local_date_time = "ISO_LOCAL_DATE_TIME";
    iso_offset_date_time = "ISO_OFFSET_DATE_TIME";
    iso_date_time = "ISO_DATE_TIME";
    basic_iso_date = "BASIC_ISO_DATE";
    rfc_1123 = "RFC_1123_DATE_TIME";
}

extern "C" fn formatter_with_locale(f: u64, locale: u64) -> u64 {
    // SAFETY: f is a formatter.
    unsafe { formatter(word(f, 8), locale, word(f, 24)) }
}
extern "C" fn formatter_with_zone(f: u64, zone: u64) -> u64 {
    // SAFETY: as above.
    unsafe { formatter(word(f, 8), word(f, 16), zone) }
}

// ---- UUID ---------------------------------------------------------------------------
//
// [vtable][most significant bits][least significant bits].

const UUID: &str = "java/util/UUID";

fn uuid_bits(u: u64) -> (i64, i64) {
    // SAFETY: u is a UUID.
    unsafe { (word(u, 8) as i64, word(u, 16) as i64) }
}
fn new_uuid(msb: i64, lsb: i64) -> u64 {
    let u = alloc(24, vt(UUID));
    uuid_init(u, msb, lsb);
    u
}
extern "C" fn uuid_init(u: u64, msb: i64, lsb: i64) {
    // SAFETY: u is a UUID.
    unsafe {
        *((u + 8) as *mut i64) = msb;
        *((u + 16) as *mut i64) = lsb;
    }
}
extern "C" fn uuid_msb(u: u64) -> i64 {
    uuid_bits(u).0
}
extern "C" fn uuid_lsb(u: u64) -> i64 {
    uuid_bits(u).1
}
extern "C" fn uuid_hash(u: u64) -> i32 {
    let (m, l) = uuid_bits(u);
    let x = m ^ l;
    ((x >> 32) as i32) ^ (x as i32)
}
extern "C" fn uuid_equals(u: u64, o: u64) -> i32 {
    // SAFETY: o is an object (or null).
    (o != 0 && unsafe { word(o, 0) } == vt(UUID) && uuid_bits(u) == uuid_bits(o)).into()
}
extern "C" fn uuid_compare(u: u64, o: u64) -> i32 {
    match uuid_bits(u).cmp(&uuid_bits(o)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}
fn uuid_text(msb: i64, lsb: i64) -> String {
    let (m, l) = (msb as u64, lsb as u64);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        m >> 32,
        (m >> 16) & 0xFFFF,
        m & 0xFFFF,
        l >> 48,
        l & 0xFFFF_FFFF_FFFF
    )
}
extern "C" fn uuid_to_string(u: u64) -> u64 {
    let (m, l) = uuid_bits(u);
    str_of(&uuid_text(m, l))
}
/// The five dash-separated hex groups (Java accepts shorter groups too).
fn parse_uuid(text: &str) -> Option<(i64, i64)> {
    let parts: Vec<u64> = text
        .split('-')
        .map(|p| u64::from_str_radix(p, 16).ok())
        .collect::<Option<_>>()?;
    let [a, b, c, d, e] = parts[..] else {
        return None;
    };
    let msb = (a << 32) | ((b & 0xFFFF) << 16) | (c & 0xFFFF);
    let lsb = ((d & 0xFFFF) << 48) | (e & 0xFFFF_FFFF_FFFF);
    Some((msb as i64, lsb as i64))
}
extern "C" fn uuid_from_string(s: u64) -> u64 {
    let text = String::from_utf16_lossy(text(s));
    match parse_uuid(&text) {
        Some((m, l)) => new_uuid(m, l),
        None => {
            super::throw(
                "java/lang/IllegalArgumentException",
                Some(&format!("Invalid UUID string: {text}")),
            );
            0
        }
    }
}
/// 16 random bytes from the system.
fn random_bytes() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    #[cfg(unix)]
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok()
    {
        return bytes;
    }
    // Elsewhere: the standard library's per-process random keys.
    for (k, chunk) in bytes.chunks_mut(8).enumerate() {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(k);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes
}
extern "C" fn uuid_random() -> u64 {
    let mut b = random_bytes();
    b[6] = (b[6] & 0x0F) | 0x40; // version 4
    b[8] = (b[8] & 0x3F) | 0x80; // IETF variant
    let msb = i64::from_be_bytes(b[..8].try_into().unwrap());
    let lsb = i64::from_be_bytes(b[8..].try_into().unwrap());
    new_uuid(msb, lsb)
}
extern "C" fn uuid_version(u: u64) -> i32 {
    ((uuid_bits(u).0 >> 12) & 0x0F) as i32
}

// ---- File system providers ------------------------------------------------------
//
// The installed ones, by scheme ([vtable][scheme String]): opening their
// file systems comes with java.nio's file system.

const PROVIDER: &str = "java/nio/file/spi/FileSystemProvider";

extern "C" fn installed_providers() -> u64 {
    once("FileSystemProvider.installedProviders", || {
        let list = super::collections::new_array_list();
        for scheme in ["file", "jar"] {
            let p = alloc(16, vt(PROVIDER));
            // SAFETY: a fresh provider.
            unsafe { *((p + 8) as *mut u64) = str_of(scheme) };
            super::collections::al_add(list, p);
        }
        list
    })
}
extern "C" fn provider_scheme(p: u64) -> u64 {
    // SAFETY: p is a provider.
    unsafe { word(p, 8) }
}

// ---- HexFormat ------------------------------------------------------------------------
//
// [vtable][upper case]; two made once.

const HEX: &str = "java/util/HexFormat";

fn hex_format(upper: bool) -> u64 {
    once(
        if upper {
            "HexFormat.upper"
        } else {
            "HexFormat.lower"
        },
        || {
            let h = alloc(16, vt(HEX));
            // SAFETY: a fresh HexFormat.
            unsafe { *((h + 8) as *mut u64) = u64::from(upper) };
            h
        },
    )
}
fn hex_upper(h: u64) -> bool {
    // SAFETY: h is a HexFormat.
    unsafe { word(h, 8) != 0 }
}
/// `v`'s low `digits` hex digits.
fn hex_digits(h: u64, v: u64, digits: u32) -> u64 {
    let text = if hex_upper(h) {
        format!("{:0w$X}", v, w = digits as usize)
    } else {
        format!("{:0w$x}", v, w = digits as usize)
    };
    let keep = text.len() - digits as usize;
    str_of(&text[keep..])
}
extern "C" fn hex_of() -> u64 {
    hex_format(false)
}
extern "C" fn hex_with_upper(_h: u64) -> u64 {
    hex_format(true)
}
extern "C" fn hex_with_lower(_h: u64) -> u64 {
    hex_format(false)
}
extern "C" fn hex_is_upper(h: u64) -> i32 {
    hex_upper(h).into()
}
extern "C" fn hex_byte(h: u64, v: i32) -> u64 {
    hex_digits(h, (v as u8).into(), 2)
}
extern "C" fn hex_char(h: u64, v: i32) -> u64 {
    hex_digits(h, (v as u16).into(), 4)
}
extern "C" fn hex_int(h: u64, v: i32) -> u64 {
    hex_digits(h, (v as u32).into(), 8)
}
extern "C" fn hex_long(h: u64, v: i64) -> u64 {
    hex_digits(h, v as u64, 16)
}
/// `toHexDigits(long, digits)`: the low `digits` (0 to 16) hex digits.
extern "C" fn hex_long_digits(h: u64, v: i64, digits: i32) -> u64 {
    if !(0..=16).contains(&digits) {
        super::throw(
            "java/lang/IllegalArgumentException",
            Some(&format!("number of digits: {digits}")),
        );
        return 0;
    }
    if digits == 0 {
        return str_of("");
    }
    hex_digits(h, v as u64, digits as u32)
}
/// `formatHex(byte[])`: two digits a byte.
extern "C" fn hex_format_bytes(h: u64, bytes: u64) -> u64 {
    if bytes == 0 {
        super::throw("java/lang/NullPointerException", None);
        return 0;
    }
    // SAFETY: a byte array: its length, then its bytes.
    let data = unsafe {
        let n = word(bytes, 8) as usize;
        std::slice::from_raw_parts((bytes + 16) as *const u8, n)
    };
    let upper = hex_upper(h);
    let text: String = data
        .iter()
        .map(|b| {
            if upper {
                format!("{b:02X}")
            } else {
                format!("{b:02x}")
            }
        })
        .collect();
    str_of(&text)
}

// ---- Character ------------------------------------------------------------------------
//
// Its static methods (a char and a code point are both an int here).

fn ch(c: i32) -> Option<char> {
    char::from_u32(c as u32)
}
macro_rules! char_test {
    ($($name:ident = $test:expr;)*) => {
        $(extern "C" fn $name(c: i32) -> i32 {
            let test: fn(char) -> bool = $test;
            ch(c).is_some_and(test).into()
        })*
    };
}
char_test! {
    char_is_digit = |c| c.is_ascii_digit() || (!c.is_ascii() && c.is_numeric());
    char_is_letter = |c| c.is_alphabetic() && !c.is_numeric();
    char_is_letter_or_digit = |c| c.is_alphanumeric();
    char_is_alphabetic = |c| c.is_alphabetic();
    char_is_upper = |c| c.is_uppercase();
    char_is_lower = |c| c.is_lowercase();
    // Java's: no non-breaking spaces; the separators below space count.
    char_is_whitespace = |c| match c {
        '\u{A0}' | '\u{2007}' | '\u{202F}' | '\u{85}' => false,
        '\t'..='\r' | '\u{1C}'..='\u{1F}' => true,
        c => c.is_whitespace(),
    };
    char_is_space_char = |c| matches!(c,
        ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}'
        | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}');
    char_is_identifier_start = |c| c.is_alphabetic() || c == '_' || c == '$';
    char_is_identifier_part = |c| c.is_alphanumeric() || c == '_' || c == '$';
}
extern "C" fn char_is_iso_control(c: i32) -> i32 {
    matches!(c, 0..=0x1F | 0x7F..=0x9F).into()
}
extern "C" fn char_is_high_surrogate(c: i32) -> i32 {
    (0xD800..=0xDBFF).contains(&c).into()
}
extern "C" fn char_is_low_surrogate(c: i32) -> i32 {
    (0xDC00..=0xDFFF).contains(&c).into()
}
extern "C" fn char_is_surrogate(c: i32) -> i32 {
    (0xD800..=0xDFFF).contains(&c).into()
}
/// The one-char mapping, or the char itself when it maps to several.
fn single(mut mapped: impl Iterator<Item = char>, c: i32) -> i32 {
    match (mapped.next(), mapped.next()) {
        (Some(m), None) => m as i32,
        _ => c,
    }
}
extern "C" fn char_to_upper(c: i32) -> i32 {
    ch(c).map_or(c, |x| single(x.to_uppercase(), c))
}
extern "C" fn char_to_lower(c: i32) -> i32 {
    ch(c).map_or(c, |x| single(x.to_lowercase(), c))
}
extern "C" fn char_to_upper_c(c: i32) -> i32 {
    char_to_upper(c) & 0xFFFF
}
extern "C" fn char_to_lower_c(c: i32) -> i32 {
    char_to_lower(c) & 0xFFFF
}
extern "C" fn char_digit(c: i32, radix: i32) -> i32 {
    if !(2..=36).contains(&radix) {
        return -1;
    }
    ch(c)
        .and_then(|x| x.to_digit(radix as u32))
        .map_or(-1, |d| d as i32)
}
extern "C" fn char_for_digit(d: i32, radix: i32) -> i32 {
    if !(2..=36).contains(&radix) || !(0..radix).contains(&d) {
        return 0;
    }
    char::from_digit(d as u32, radix as u32).map_or(0, |x| x as i32)
}
extern "C" fn char_numeric_value(c: i32) -> i32 {
    ch(c).and_then(|x| x.to_digit(36)).map_or(-1, |d| d as i32)
}
extern "C" fn char_count(c: i32) -> i32 {
    if c >= 0x10000 { 2 } else { 1 }
}
extern "C" fn char_compare(a: i32, b: i32) -> i32 {
    a - b
}
extern "C" fn char_hash(c: i32) -> i32 {
    c
}
extern "C" fn char_string(c: i32) -> u64 {
    super::new_string(&[c as u16])
}

/// `Character.compareTo`: the difference of the two chars.
extern "C" fn char_compare_to(a: u64, b: u64) -> i32 {
    if b == 0 {
        super::throw("java/lang/NullPointerException", None);
        return 0;
    }
    super::box_int(a) - super::box_int(b)
}

/// `Character`'s static methods beyond rt.rs's boxing.
pub fn character_methods() -> Vec<Method> {
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
    let mut out = Vec::new();
    for (name, f) in [
        ("isDigit", f!(char_is_digit)),
        ("isLetter", f!(char_is_letter)),
        ("isLetterOrDigit", f!(char_is_letter_or_digit)),
        ("isUpperCase", f!(char_is_upper)),
        ("isLowerCase", f!(char_is_lower)),
        ("isWhitespace", f!(char_is_whitespace)),
        ("isSpaceChar", f!(char_is_space_char)),
        ("isISOControl", f!(char_is_iso_control)),
        ("isJavaIdentifierStart", f!(char_is_identifier_start)),
        ("isJavaIdentifierPart", f!(char_is_identifier_part)),
    ] {
        out.push(s(name, "(C)Z", f));
        out.push(s(name, "(I)Z", f));
    }
    out.extend([
        s("isAlphabetic", "(I)Z", f!(char_is_alphabetic)),
        s("isHighSurrogate", "(C)Z", f!(char_is_high_surrogate)),
        s("isLowSurrogate", "(C)Z", f!(char_is_low_surrogate)),
        s("isSurrogate", "(C)Z", f!(char_is_surrogate)),
        s("toUpperCase", "(C)C", f!(char_to_upper_c)),
        s("toLowerCase", "(C)C", f!(char_to_lower_c)),
        s("toUpperCase", "(I)I", f!(char_to_upper)),
        s("toLowerCase", "(I)I", f!(char_to_lower)),
        s("digit", "(CI)I", f!(char_digit)),
        s("digit", "(II)I", f!(char_digit)),
        s("forDigit", "(II)C", f!(char_for_digit)),
        s("getNumericValue", "(C)I", f!(char_numeric_value)),
        s("getNumericValue", "(I)I", f!(char_numeric_value)),
        s("charCount", "(I)I", f!(char_count)),
        s("compare", "(CC)I", f!(char_compare)),
        s("hashCode", "(C)I", f!(char_hash)),
        s("toString", "(C)Ljava/lang/String;", f!(char_string)),
    ]);
    for desc in ["(Ljava/lang/Character;)I", "(Ljava/lang/Object;)I"] {
        out.push(Method {
            name: "compareTo",
            desc,
            is_static: false,
            f: f!(char_compare_to),
        });
    }
    out
}

/// The JDK's static fields this library provides: `owner name` → its
/// getter and the getter's descriptor.
// ---- PrintStream (System.out and System.err) ----------------------------------

/// A stream: [vtable][1 (stdout) or 2 (stderr)].
fn print_stream(fd: u64) -> u64 {
    let p = alloc(16, vt("java/io/PrintStream"));
    // SAFETY: a fresh 16-byte object.
    unsafe { *((p as usize + 8) as *mut u64) = fd };
    p
}
extern "C" fn system_out() -> u64 {
    once("System.out", || print_stream(1))
}
extern "C" fn system_err() -> u64 {
    once("System.err", || print_stream(2))
}
extern "C" fn boolean_true() -> u64 {
    super::bool_value_of(1)
}
extern "C" fn boolean_false() -> u64 {
    super::bool_value_of(0)
}
fn emit(p: u64, s: &str, line: bool) {
    use std::io::Write;
    // SAFETY: a PrintStream: its target at 8.
    let fd = unsafe { word(p, 8) };
    let end = if line { "\n" } else { "" };
    if fd == 2 {
        let _ = write!(std::io::stderr(), "{s}{end}");
    } else {
        let _ = write!(std::io::stdout(), "{s}{end}");
    }
}
/// `String.valueOf` of an object: "null" or its `toString()`.
fn object_text(o: u64) -> String {
    if o == 0 {
        "null".to_owned()
    } else {
        String::from_utf16_lossy(text(super::to_string(o)))
    }
}
fn string_text(s: u64) -> String {
    if s == 0 {
        "null".to_owned()
    } else {
        String::from_utf16_lossy(text(s))
    }
}
extern "C" fn ps_println(p: u64) {
    emit(p, "", true);
}
extern "C" fn ps_println_string(p: u64, s: u64) {
    emit(p, &string_text(s), true);
}
extern "C" fn ps_print_string(p: u64, s: u64) {
    emit(p, &string_text(s), false);
}
extern "C" fn ps_println_object(p: u64, o: u64) {
    emit(p, &object_text(o), true);
}
extern "C" fn ps_print_object(p: u64, o: u64) {
    emit(p, &object_text(o), false);
}
extern "C" fn ps_println_int(p: u64, v: i32) {
    emit(p, &v.to_string(), true);
}
extern "C" fn ps_print_int(p: u64, v: i32) {
    emit(p, &v.to_string(), false);
}
extern "C" fn ps_println_long(p: u64, v: i64) {
    emit(p, &v.to_string(), true);
}
extern "C" fn ps_print_long(p: u64, v: i64) {
    emit(p, &v.to_string(), false);
}
extern "C" fn ps_println_double(p: u64, v: f64) {
    emit(p, &super::java_double(v), true);
}
extern "C" fn ps_print_double(p: u64, v: f64) {
    emit(p, &super::java_double(v), false);
}
extern "C" fn ps_println_float(p: u64, v: f32) {
    emit(p, &super::java_float(v), true);
}
extern "C" fn ps_print_float(p: u64, v: f32) {
    emit(p, &super::java_float(v), false);
}
extern "C" fn ps_println_char(p: u64, v: i32) {
    emit(p, &String::from_utf16_lossy(&[v as u16]), true);
}
extern "C" fn ps_print_char(p: u64, v: i32) {
    emit(p, &String::from_utf16_lossy(&[v as u16]), false);
}
extern "C" fn ps_println_bool(p: u64, v: i32) {
    emit(p, if v != 0 { "true" } else { "false" }, true);
}
extern "C" fn ps_print_bool(p: u64, v: i32) {
    emit(p, if v != 0 { "true" } else { "false" }, false);
}
extern "C" fn ps_printf(p: u64, f: u64, args: u64) -> u64 {
    let s = super::strings::format(f, args);
    if s != 0 {
        emit(p, &string_text(s), false);
    }
    p
}
extern "C" fn ps_flush(_p: u64) {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}
fn print_stream_class() -> Builtin {
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
    Builtin {
        name: "java/io/PrintStream",
        super_name: Some("java/lang/Object"),
        interfaces: &[],
        size: 16,
        is_final: false,
        is_interface: false,
        methods: vec![
            m("println", "()V", f!(ps_println)),
            m("println", "(Ljava/lang/String;)V", f!(ps_println_string)),
            m("print", "(Ljava/lang/String;)V", f!(ps_print_string)),
            m("println", "(Ljava/lang/Object;)V", f!(ps_println_object)),
            m("print", "(Ljava/lang/Object;)V", f!(ps_print_object)),
            m("println", "(I)V", f!(ps_println_int)),
            m("print", "(I)V", f!(ps_print_int)),
            m("println", "(J)V", f!(ps_println_long)),
            m("print", "(J)V", f!(ps_print_long)),
            m("println", "(D)V", f!(ps_println_double)),
            m("print", "(D)V", f!(ps_print_double)),
            m("println", "(F)V", f!(ps_println_float)),
            m("print", "(F)V", f!(ps_print_float)),
            m("println", "(C)V", f!(ps_println_char)),
            m("print", "(C)V", f!(ps_print_char)),
            m("println", "(Z)V", f!(ps_println_bool)),
            m("print", "(Z)V", f!(ps_print_bool)),
            m(
                "printf",
                "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/io/PrintStream;",
                f!(ps_printf),
            ),
            m(
                "format",
                "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/io/PrintStream;",
                f!(ps_printf),
            ),
            m("flush", "()V", f!(ps_flush)),
        ],
    }
}

pub fn static_getter(owner: &str, name: &str) -> Option<(usize, &'static str)> {
    const UNIT: &str = "()Ljava/util/concurrent/TimeUnit;";
    let f = |f: extern "C" fn() -> u64| f as *const () as usize;
    const L: &str = "()Ljava/util/Locale;";
    const F: &str = "()Ljava/time/format/DateTimeFormatter;";
    const DTF: &str = "java/time/format/DateTimeFormatter";
    Some(match (owner, name) {
        (DTF, "ISO_INSTANT") => (f(iso_instant), F),
        (DTF, "ISO_LOCAL_DATE") => (f(iso_local_date), F),
        (DTF, "ISO_LOCAL_TIME") => (f(iso_local_time), F),
        (DTF, "ISO_LOCAL_DATE_TIME") => (f(iso_local_date_time), F),
        (DTF, "ISO_OFFSET_DATE_TIME") => (f(iso_offset_date_time), F),
        (DTF, "ISO_DATE_TIME") => (f(iso_date_time), F),
        (DTF, "BASIC_ISO_DATE") => (f(basic_iso_date), F),
        (DTF, "RFC_1123_DATE_TIME") => (f(rfc_1123), F),
        ("java/util/Locale", "ROOT") => (f(locale_root), L),
        ("java/util/Locale", "ENGLISH") => (f(locale_english), L),
        ("java/util/Locale", "US") => (f(locale_us), L),
        ("java/util/Locale", "UK") => (f(locale_uk), L),
        ("java/util/Locale", "GERMAN") => (f(locale_german), L),
        ("java/util/Locale", "FRENCH") => (f(locale_french), L),
        ("java/util/concurrent/TimeUnit", "NANOSECONDS") => (f(nanoseconds), UNIT),
        ("java/util/concurrent/TimeUnit", "MICROSECONDS") => (f(microseconds), UNIT),
        ("java/util/concurrent/TimeUnit", "MILLISECONDS") => (f(milliseconds), UNIT),
        ("java/util/concurrent/TimeUnit", "SECONDS") => (f(seconds), UNIT),
        ("java/util/concurrent/TimeUnit", "MINUTES") => (f(minutes), UNIT),
        ("java/util/concurrent/TimeUnit", "HOURS") => (f(hours), UNIT),
        ("java/util/concurrent/TimeUnit", "DAYS") => (f(days), UNIT),
        ("java/lang/System", "out") => (f(system_out), "()Ljava/io/PrintStream;"),
        ("java/lang/System", "err") => (f(system_err), "()Ljava/io/PrintStream;"),
        ("java/lang/Boolean", "TRUE") => (f(boolean_true), "()Ljava/lang/Boolean;"),
        ("java/lang/Boolean", "FALSE") => (f(boolean_false), "()Ljava/lang/Boolean;"),
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
            name: HEX,
            super_name: Some(O),
            interfaces: &[],
            size: 16,
            is_final: true,
            is_interface: false,
            methods: vec![
                Method {
                    name: "of",
                    desc: "()Ljava/util/HexFormat;",
                    is_static: true,
                    f: f!(hex_of),
                },
                m(
                    "withUpperCase",
                    "()Ljava/util/HexFormat;",
                    f!(hex_with_upper),
                ),
                m(
                    "withLowerCase",
                    "()Ljava/util/HexFormat;",
                    f!(hex_with_lower),
                ),
                m("isUpperCase", "()Z", f!(hex_is_upper)),
                m("toHexDigits", "(B)Ljava/lang/String;", f!(hex_byte)),
                m("toHexDigits", "(C)Ljava/lang/String;", f!(hex_char)),
                m("toHexDigits", "(S)Ljava/lang/String;", f!(hex_char)),
                m("toHexDigits", "(I)Ljava/lang/String;", f!(hex_int)),
                m("toHexDigits", "(J)Ljava/lang/String;", f!(hex_long)),
                m("toHexDigits", "(JI)Ljava/lang/String;", f!(hex_long_digits)),
                m("formatHex", "([B)Ljava/lang/String;", f!(hex_format_bytes)),
            ],
        },
        Builtin {
            name: PROVIDER,
            super_name: Some(O),
            interfaces: &[],
            size: 16,
            is_final: false,
            is_interface: false,
            methods: vec![
                Method {
                    name: "installedProviders",
                    desc: "()Ljava/util/List;",
                    is_static: true,
                    f: f!(installed_providers),
                },
                m("getScheme", "()Ljava/lang/String;", f!(provider_scheme)),
            ],
        },
        Builtin {
            name: UUID,
            super_name: Some(O),
            interfaces: &["java/lang/Comparable"],
            size: 24,
            is_final: true,
            is_interface: false,
            methods: vec![
                m("<init>", "(JJ)V", f!(uuid_init)),
                m("getMostSignificantBits", "()J", f!(uuid_msb)),
                m("getLeastSignificantBits", "()J", f!(uuid_lsb)),
                m("hashCode", "()I", f!(uuid_hash)),
                m("equals", "(Ljava/lang/Object;)Z", f!(uuid_equals)),
                m("compareTo", "(Ljava/util/UUID;)I", f!(uuid_compare)),
                m("compareTo", "(Ljava/lang/Object;)I", f!(uuid_compare)),
                m("toString", "()Ljava/lang/String;", f!(uuid_to_string)),
                m("version", "()I", f!(uuid_version)),
                Method {
                    name: "fromString",
                    desc: "(Ljava/lang/String;)Ljava/util/UUID;",
                    is_static: true,
                    f: f!(uuid_from_string),
                },
                Method {
                    name: "randomUUID",
                    desc: "()Ljava/util/UUID;",
                    is_static: true,
                    f: f!(uuid_random),
                },
            ],
        },
        Builtin {
            name: FORMATTER,
            super_name: Some(O),
            interfaces: &[],
            size: 32,
            is_final: true,
            is_interface: false,
            methods: vec![
                Method {
                    name: "ofPattern",
                    desc: "(Ljava/lang/String;)Ljava/time/format/DateTimeFormatter;",
                    is_static: true,
                    f: f!(formatter_of_pattern),
                },
                Method {
                    name: "ofPattern",
                    desc: "(Ljava/lang/String;Ljava/util/Locale;)Ljava/time/format/DateTimeFormatter;",
                    is_static: true,
                    f: f!(formatter_of_pattern_locale),
                },
                m(
                    "withLocale",
                    "(Ljava/util/Locale;)Ljava/time/format/DateTimeFormatter;",
                    f!(formatter_with_locale),
                ),
                m(
                    "withZone",
                    "(Ljava/time/ZoneId;)Ljava/time/format/DateTimeFormatter;",
                    f!(formatter_with_zone),
                ),
            ],
        },
        Builtin {
            name: LOCALE,
            super_name: Some(O),
            interfaces: &[],
            size: 24,
            is_final: true,
            is_interface: false,
            methods: vec![
                // The default: the root locale (the game is not localized
                // through the JDK's).
                Method {
                    name: "getDefault",
                    desc: "()Ljava/util/Locale;",
                    is_static: true,
                    f: f!(locale_root),
                },
                m("getLanguage", "()Ljava/lang/String;", f!(locale_language)),
                m("getCountry", "()Ljava/lang/String;", f!(locale_country)),
                m("toString", "()Ljava/lang/String;", f!(locale_to_string)),
                m("toLanguageTag", "()Ljava/lang/String;", f!(locale_tag)),
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
        print_stream_class(),
    ]
}

#[cfg(test)]
mod tests {
    use super::convert;

    #[test]
    fn characters_classify_as_java_does() {
        use super::*;
        assert_eq!(char_is_whitespace(' ' as i32), 1);
        assert_eq!(char_is_whitespace(0xA0), 0, "a non-breaking space is not");
        assert_eq!(char_is_whitespace(0x1F), 1);
        assert_eq!(char_is_iso_control(0x9F), 1);
        assert_eq!(char_is_iso_control('a' as i32), 0);
        assert_eq!(char_to_upper_c('a' as i32), 'A' as i32);
        assert_eq!(
            char_to_upper_c('\u{DF}' as i32),
            0xDF,
            "ß has no one-char upper case"
        );
        assert_eq!(char_digit('f' as i32, 16), 15);
        assert_eq!(char_digit('g' as i32, 16), -1);
        assert_eq!(char_for_digit(11, 16), 'b' as i32);
        assert_eq!(char_is_digit('7' as i32), 1);
        assert_eq!(char_is_letter('7' as i32), 0);
    }

    #[test]
    fn uuids_print_and_parse_as_java_does() {
        use super::{parse_uuid, uuid_text};
        let (m, l) = (
            0x123e4567_e89b_12d3_u64 as i64,
            0xa456_426614174000_u64 as i64,
        );
        assert_eq!(uuid_text(m, l), "123e4567-e89b-12d3-a456-426614174000");
        assert_eq!(
            parse_uuid("123e4567-e89b-12d3-a456-426614174000"),
            Some((m, l))
        );
        assert_eq!(uuid_text(0, 0), "00000000-0000-0000-0000-000000000000");
        assert_eq!(parse_uuid("not-a-uuid"), None);
    }

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
