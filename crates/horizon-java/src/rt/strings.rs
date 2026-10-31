//! `String`'s methods beyond rt.rs's basics, for the native backend's runtime
//! library, written against the JDK's public API: case mapping, searching
//! from a position, splitting (literal patterns; a regular expression
//! throws until java.util.regex is here), joining, stripping, repeating,
//! bytes, and `String.format` (`%s %S %d %x %X %o %c %b %f %e %n %%` with
//! flags, width, precision and argument indexes, `%f` rounding half up on
//! the double's exact value as Java's Formatter does).
use super::{
    A_DATA, A_LEN, Method, alloc, box_double, box_float, box_int, box_long, instance_of,
    new_string, pending, set_word, throw, to_string, units, word,
};

fn text(s: u64) -> String {
    String::from_utf16_lossy(units(s))
}
fn string(t: &str) -> u64 {
    new_string(&t.encode_utf16().collect::<Vec<_>>())
}
fn npe() {
    throw("java/lang/NullPointerException", None);
}
/// A new array of `n` elements of `size` bytes (the compiler's layout).
fn array(n: usize, size: usize) -> u64 {
    let a = alloc(A_DATA + size * n, size as u64);
    // SAFETY: a fresh array.
    unsafe { set_word(a, A_LEN, n as u64) };
    a
}
fn string_array(items: &[u64]) -> u64 {
    let a = array(items.len(), 8);
    for (i, &s) in items.iter().enumerate() {
        // SAFETY: within the array.
        unsafe { set_word(a, A_DATA + 8 * i, s) };
    }
    a
}

extern "C" fn to_char_array(s: u64) -> u64 {
    let u = units(s);
    let a = array(u.len(), 2);
    // SAFETY: room for the units.
    unsafe {
        std::ptr::copy_nonoverlapping(u.as_ptr(), (a as usize + A_DATA) as *mut u16, u.len())
    };
    a
}
extern "C" fn value_of_chars(a: u64) -> u64 {
    if a == 0 {
        npe();
        return 0;
    }
    // SAFETY: a char array: its length, then its units.
    let u = unsafe {
        std::slice::from_raw_parts((a as usize + A_DATA) as *const u16, word(a, A_LEN) as usize)
    };
    new_string(u)
}
extern "C" fn to_lower(s: u64) -> u64 {
    string(&text(s).to_lowercase())
}
extern "C" fn to_upper(s: u64) -> u64 {
    string(&text(s).to_uppercase())
}
extern "C" fn to_lower_locale(s: u64, _l: u64) -> u64 {
    to_lower(s)
}
extern "C" fn to_upper_locale(s: u64, _l: u64) -> u64 {
    to_upper(s)
}

fn find(hay: &[u16], needle: &[u16], from: usize) -> i32 {
    if needle.is_empty() {
        return from.min(hay.len()) as i32;
    }
    (from..=hay.len().saturating_sub(needle.len()))
        .find(|&i| hay[i..].starts_with(needle))
        .map_or(-1, |i| i as i32)
}
fn find_last(hay: &[u16], needle: &[u16], from: usize) -> i32 {
    if needle.len() > hay.len() {
        return -1;
    }
    let start = from.min(hay.len() - needle.len());
    (0..=start)
        .rev()
        .find(|&i| hay[i..].starts_with(needle))
        .map_or(-1, |i| i as i32)
}
fn code_units(c: i32) -> Vec<u16> {
    char::from_u32(c as u32).map_or(vec![c as u16], |ch| {
        let mut b = [0u16; 2];
        ch.encode_utf16(&mut b).to_vec()
    })
}
extern "C" fn index_of_char_from(s: u64, c: i32, from: i32) -> i32 {
    find(units(s), &code_units(c), from.max(0) as usize)
}
extern "C" fn index_of_from(s: u64, t: u64, from: i32) -> i32 {
    if t == 0 {
        npe();
        return -1;
    }
    find(units(s), units(t), from.max(0) as usize)
}
extern "C" fn last_index_of_char(s: u64, c: i32) -> i32 {
    find_last(units(s), &code_units(c), usize::MAX)
}
extern "C" fn last_index_of(s: u64, t: u64) -> i32 {
    if t == 0 {
        npe();
        return -1;
    }
    find_last(units(s), units(t), usize::MAX)
}
extern "C" fn last_index_of_char_from(s: u64, c: i32, from: i32) -> i32 {
    if from < 0 {
        return -1;
    }
    find_last(units(s), &code_units(c), from as usize)
}
extern "C" fn starts_with_at(s: u64, t: u64, at: i32) -> i32 {
    let (h, n) = (units(s), units(t));
    (at >= 0 && (at as usize) <= h.len() && h[at as usize..].starts_with(n)).into()
}
extern "C" fn is_blank(s: u64) -> i32 {
    text(s).chars().all(char::is_whitespace).into()
}
extern "C" fn strip(s: u64) -> u64 {
    string(text(s).trim())
}
extern "C" fn strip_leading(s: u64) -> u64 {
    string(text(s).trim_start())
}
extern "C" fn strip_trailing(s: u64) -> u64 {
    string(text(s).trim_end())
}
extern "C" fn repeat(s: u64, n: i32) -> u64 {
    if n < 0 {
        throw(
            "java/lang/IllegalArgumentException",
            Some(&format!("count is negative: {n}")),
        );
        return 0;
    }
    new_string(&units(s).repeat(n as usize))
}
extern "C" fn code_point_at(s: u64, i: i32) -> i32 {
    let u = units(s);
    let Some(&a) = u.get(i as usize).filter(|_| i >= 0) else {
        throw(
            "java/lang/StringIndexOutOfBoundsException",
            Some(&format!("index {i}, length {}", u.len())),
        );
        return 0;
    };
    match u.get(i as usize + 1) {
        Some(&b) if (0xD800..0xDC00).contains(&a) && (0xDC00..0xE000).contains(&b) => {
            0x10000 + ((i32::from(a) - 0xD800) << 10) + (i32::from(b) - 0xDC00)
        }
        _ => i32::from(a),
    }
}
extern "C" fn compare_ignore_case(a: u64, b: u64) -> i32 {
    // Java's: char by char, upper then lower case.
    let fold = |c: u16| {
        let ch = char::from_u32(u32::from(c)).unwrap_or('\u{FFFD}');
        let up = ch.to_uppercase().next().unwrap_or(ch);
        up.to_lowercase().next().unwrap_or(up) as i32
    };
    let (x, y) = (units(a), units(b));
    for (p, q) in x.iter().zip(y) {
        let d = fold(*p) - fold(*q);
        if d != 0 {
            return d;
        }
    }
    x.len() as i32 - y.len() as i32
}
extern "C" fn get_bytes(s: u64) -> u64 {
    let bytes = text(s).into_bytes();
    let a = array(bytes.len(), 1);
    // SAFETY: room for the bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            (a as usize + A_DATA) as *mut u8,
            bytes.len(),
        )
    };
    a
}
extern "C" fn get_bytes_charset(s: u64, _charset: u64) -> u64 {
    get_bytes(s)
}

// ---- join ---------------------------------------------------------------------

fn join_units(sep: u64, parts: &[u64]) -> u64 {
    let mut out = Vec::new();
    for (i, &p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(units(sep));
        }
        if p == 0 {
            out.extend("null".encode_utf16());
        } else {
            out.extend_from_slice(units(to_string(p)));
        }
    }
    new_string(&out)
}
extern "C" fn join_array(sep: u64, a: u64) -> u64 {
    if sep == 0 || a == 0 {
        npe();
        return 0;
    }
    // SAFETY: an array of references.
    let parts: Vec<u64> = unsafe {
        (0..word(a, A_LEN) as usize)
            .map(|i| word(a, A_DATA + 8 * i))
            .collect()
    };
    join_units(sep, &parts)
}
extern "C" fn join_iterable(sep: u64, it: u64) -> u64 {
    if sep == 0 || it == 0 {
        npe();
        return 0;
    }
    let Some(parts) = super::collections::iterate(it) else {
        return 0;
    };
    join_units(sep, &parts)
}

// ---- split ----------------------------------------------------------------------

/// The text a regular expression matches when it is a literal (no
/// metacharacters, or one escaped non-alphanumeric character, Java's own
/// fast path); None for a real pattern.
fn literal(regex: &str) -> Option<String> {
    const META: &str = ".$|()[{^?*+\\";
    let chars: Vec<char> = regex.chars().collect();
    match chars[..] {
        ['\\', c] if !c.is_ascii_alphanumeric() => Some(c.to_string()),
        _ if !regex.chars().any(|c| META.contains(c)) => Some(regex.to_owned()),
        _ => None,
    }
}
/// Java's split by a literal: at most `limit` parts when positive; trailing
/// empty strings dropped when 0.
fn split_literal(s: &str, sep: &str, limit: i32) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut parts: Vec<String> = if limit > 0 {
        s.splitn(limit as usize, sep).map(str::to_owned).collect()
    } else {
        s.split(sep).map(str::to_owned).collect()
    };
    if sep.is_empty() {
        // Java splits between every char (no empty first part).
        parts = s.chars().map(String::from).collect();
    }
    if limit == 0 {
        while parts.len() > 1 && parts.last().is_some_and(String::is_empty) {
            parts.pop();
        }
        if parts.len() == 1 && parts[0].is_empty() && !s.is_empty() {
            parts.clear();
        }
    }
    parts
}
extern "C" fn split_limit(s: u64, regex: u64, limit: i32) -> u64 {
    if regex == 0 {
        npe();
        return 0;
    }
    let pattern = text(regex);
    let Some(sep) = literal(&pattern) else {
        throw(
            "java/lang/UnsupportedOperationException",
            Some(&format!("regular expressions are not here yet: {pattern}")),
        );
        return 0;
    };
    let parts: Vec<u64> = split_literal(&text(s), &sep, limit)
        .iter()
        .map(|p| string(p))
        .collect();
    string_array(&parts)
}
extern "C" fn split(s: u64, regex: u64) -> u64 {
    split_limit(s, regex, 0)
}

// ---- format -----------------------------------------------------------------------

/// A double's exact decimal digits rounded half up at `precision` places
/// (Java's Formatter rounds the exact value so).
fn fixed(x: f64, precision: usize) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "NaN".into()
        } else if x > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    // Every digit of a double's binary fraction, then rounded by hand.
    let exact = format!("{:.1100}", x.abs());
    let (int, frac) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits: Vec<u8> = int
        .bytes()
        .chain(frac.bytes().take(precision))
        .map(|b| b - b'0')
        .collect();
    let next = frac.as_bytes().get(precision).map_or(0, |b| b - b'0');
    if next >= 5 {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let int_len = digits.len() - precision;
    let mut out: String = digits[..int_len]
        .iter()
        .map(|d| (b'0' + d) as char)
        .collect();
    if precision > 0 {
        out.push('.');
        out.extend(digits[int_len..].iter().map(|d| (b'0' + d) as char));
    }
    // Java keeps the sign of a negative value that rounds to zero ("-0.00").
    if x.is_sign_negative() {
        out.insert(0, '-');
    }
    out
}
/// Groups of three digits with commas.
fn grouped(digits: &str) -> String {
    let (neg, d) = digits
        .strip_prefix('-')
        .map_or((false, digits), |d| (true, d));
    let (int, rest) = d.split_once('.').map_or((d, None), |(i, r)| (i, Some(r)));
    let mut out = String::new();
    for (k, c) in int.chars().enumerate() {
        if k > 0 && (int.len() - k) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if let Some(r) = rest {
        out.push('.');
        out.push_str(r);
    }
    if neg { format!("-{out}") } else { out }
}

enum Num {
    Int(i64, u32),
    Float(f64),
}
/// A boxed number's value (with an integer's width in bits).
fn number(o: u64) -> Option<Num> {
    if o == 0 {
        return None;
    }
    Some(if instance_of(o, "java/lang/Integer") {
        Num::Int(i64::from(box_int(o)), 32)
    } else if instance_of(o, "java/lang/Long") {
        Num::Int(box_long(o), 64)
    } else if instance_of(o, "java/lang/Short") {
        Num::Int(i64::from(box_int(o) as i16), 16)
    } else if instance_of(o, "java/lang/Byte") {
        Num::Int(i64::from(box_int(o) as i8), 8)
    } else if instance_of(o, "java/lang/Double") {
        Num::Float(box_double(o))
    } else if instance_of(o, "java/lang/Float") {
        Num::Float(f64::from(box_float(o)))
    } else {
        return None;
    })
}

fn format_text(pattern: &str, args: &[u64]) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    let mut next_arg = 0usize;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // %[index$][flags][width][.precision]conversion
        let mut spec = String::new();
        while let Some(&d) = chars.peek() {
            if d.is_ascii_alphabetic() || d == '%' {
                break;
            }
            spec.push(d);
            chars.next();
        }
        let conv = chars.next().ok_or("a format ending in %")?;
        let (index, spec) = match spec.split_once('$') {
            Some((i, rest)) => (
                Some(i.parse::<usize>().map_err(|_| "a bad argument index")?),
                rest.to_owned(),
            ),
            None => (None, spec),
        };
        let flags: String = spec
            .chars()
            .take_while(|c| "-+ 0,#(".contains(*c))
            .collect();
        let rest = &spec[flags.len()..];
        let (width, precision) = match rest.split_once('.') {
            Some((w, p)) => (
                w.parse::<usize>().ok(),
                Some(p.parse::<usize>().map_err(|_| "a bad precision")?),
            ),
            None => (rest.parse::<usize>().ok(), None),
        };
        let mut arg = || -> Result<u64, String> {
            let k = match index {
                Some(i) => i.checked_sub(1).ok_or("argument index 0")?,
                None => {
                    next_arg += 1;
                    next_arg - 1
                }
            };
            args.get(k)
                .copied()
                .ok_or_else(|| format!("Format specifier '%{spec}{conv}'"))
        };
        let body = match conv {
            '%' => "%".to_owned(),
            'n' => "\n".to_owned(),
            's' | 'S' => {
                let a = arg()?;
                let mut s = if a == 0 {
                    "null".to_owned()
                } else {
                    text(to_string(a))
                };
                if let Some(p) = precision {
                    s = s.chars().take(p).collect();
                }
                if conv == 'S' { s.to_uppercase() } else { s }
            }
            'b' | 'B' => {
                let a = arg()?;
                let v = if a == 0 {
                    false
                } else if instance_of(a, "java/lang/Boolean") {
                    box_int(a) != 0
                } else {
                    true
                };
                let s = v.to_string();
                if conv == 'B' { s.to_uppercase() } else { s }
            }
            'c' | 'C' => {
                let a = arg()?;
                let code = if instance_of(a, "java/lang/Character")
                    || instance_of(a, "java/lang/Integer")
                {
                    box_int(a)
                } else {
                    return Err(format!("%{conv} != {}", text(to_string(a))));
                };
                let s = char::from_u32(code as u32).map_or(String::new(), String::from);
                if conv == 'C' { s.to_uppercase() } else { s }
            }
            'd' | 'x' | 'X' | 'o' => {
                let a = arg()?;
                let Some(Num::Int(v, bits)) = number(a) else {
                    return Err(format!(
                        "%{conv} != {}",
                        if a == 0 {
                            "null".into()
                        } else {
                            text(to_string(a))
                        }
                    ));
                };
                let unsigned = if bits == 64 {
                    v as u64
                } else {
                    (v as u64) & ((1u64 << bits) - 1)
                };
                let mut s = match conv {
                    'd' => {
                        let mut s = if flags.contains(',') {
                            grouped(&v.to_string())
                        } else {
                            v.to_string()
                        };
                        if v >= 0 && flags.contains('+') {
                            s.insert(0, '+');
                        } else if v >= 0 && flags.contains(' ') {
                            s.insert(0, ' ');
                        }
                        s
                    }
                    'o' => format!("{unsigned:o}"),
                    'x' => format!("{unsigned:x}"),
                    _ => format!("{unsigned:X}"),
                };
                if flags.contains('#') && conv != 'd' {
                    s.insert_str(
                        0,
                        if conv == 'o' {
                            "0"
                        } else if conv == 'x' {
                            "0x"
                        } else {
                            "0X"
                        },
                    );
                }
                s
            }
            'f' | 'e' | 'E' => {
                let a = arg()?;
                let v = match number(a) {
                    Some(Num::Float(v)) => v,
                    _ => {
                        return Err(format!(
                            "%{conv} != {}",
                            if a == 0 {
                                "null".into()
                            } else {
                                text(to_string(a))
                            }
                        ));
                    }
                };
                let p = precision.unwrap_or(6);
                let mut s = if conv == 'f' {
                    let s = fixed(v, p);
                    if flags.contains(',') { grouped(&s) } else { s }
                } else {
                    let t = format!("{:.*e}", p, v);
                    let (m, e) = t.split_once('e').unwrap_or((&t, "0"));
                    let e: i32 = e.parse().unwrap_or(0);
                    let s = format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
                    if conv == 'E' { s.to_uppercase() } else { s }
                };
                if v >= 0.0 && flags.contains('+') {
                    s.insert(0, '+');
                }
                s
            }
            other => return Err(format!("Conversion = '{other}'")),
        };
        // Width: padded with spaces (right-aligned unless '-'), or zeros.
        let len = body.chars().count();
        match width {
            Some(w) if w > len => {
                let pad = w - len;
                if flags.contains('-') {
                    out.push_str(&body);
                    out.extend(std::iter::repeat_n(' ', pad));
                } else if flags.contains('0') && "dxXofeE".contains(conv) {
                    let (sign, digits) = match body.chars().next() {
                        Some(s @ ('-' | '+' | ' ')) => (s.to_string(), &body[1..]),
                        _ => (String::new(), &body[..]),
                    };
                    out.push_str(&sign);
                    out.extend(std::iter::repeat_n('0', pad));
                    out.push_str(digits);
                } else {
                    out.extend(std::iter::repeat_n(' ', pad));
                    out.push_str(&body);
                }
            }
            _ => out.push_str(&body),
        }
    }
    Ok(out)
}

fn format_with(pattern: u64, args: u64) -> u64 {
    if pattern == 0 {
        npe();
        return 0;
    }
    // SAFETY: an array of references (or none: no arguments).
    let list: Vec<u64> = if args == 0 {
        Vec::new()
    } else {
        unsafe {
            (0..word(args, A_LEN) as usize)
                .map(|i| word(args, A_DATA + 8 * i))
                .collect()
        }
    };
    match format_text(&text(pattern), &list) {
        _ if pending() => 0,
        Ok(s) => string(&s),
        Err(why) => {
            throw("java/util/IllegalFormatException", Some(&why));
            0
        }
    }
}
pub(super) extern "C" fn format(pattern: u64, args: u64) -> u64 {
    format_with(pattern, args)
}
extern "C" fn format_locale(_locale: u64, pattern: u64, args: u64) -> u64 {
    format_with(pattern, args)
}
extern "C" fn formatted(s: u64, args: u64) -> u64 {
    format_with(s, args)
}

/// `String`'s methods here, for rt.rs's `String`.
pub fn string_methods() -> Vec<Method> {
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
    vec![
        m("toCharArray", "()[C", f!(to_char_array)),
        m("toLowerCase", "()Ljava/lang/String;", f!(to_lower)),
        m("toUpperCase", "()Ljava/lang/String;", f!(to_upper)),
        m(
            "toLowerCase",
            "(Ljava/util/Locale;)Ljava/lang/String;",
            f!(to_lower_locale),
        ),
        m(
            "toUpperCase",
            "(Ljava/util/Locale;)Ljava/lang/String;",
            f!(to_upper_locale),
        ),
        m("indexOf", "(II)I", f!(index_of_char_from)),
        m("indexOf", "(Ljava/lang/String;I)I", f!(index_of_from)),
        m("lastIndexOf", "(I)I", f!(last_index_of_char)),
        m("lastIndexOf", "(II)I", f!(last_index_of_char_from)),
        m("lastIndexOf", "(Ljava/lang/String;)I", f!(last_index_of)),
        m("startsWith", "(Ljava/lang/String;I)Z", f!(starts_with_at)),
        m("isBlank", "()Z", f!(is_blank)),
        m("strip", "()Ljava/lang/String;", f!(strip)),
        m("stripLeading", "()Ljava/lang/String;", f!(strip_leading)),
        m("stripTrailing", "()Ljava/lang/String;", f!(strip_trailing)),
        m("repeat", "(I)Ljava/lang/String;", f!(repeat)),
        m("codePointAt", "(I)I", f!(code_point_at)),
        m(
            "compareToIgnoreCase",
            "(Ljava/lang/String;)I",
            f!(compare_ignore_case),
        ),
        m("getBytes", "()[B", f!(get_bytes)),
        m(
            "getBytes",
            "(Ljava/nio/charset/Charset;)[B",
            f!(get_bytes_charset),
        ),
        m(
            "split",
            "(Ljava/lang/String;)[Ljava/lang/String;",
            f!(split),
        ),
        m(
            "split",
            "(Ljava/lang/String;I)[Ljava/lang/String;",
            f!(split_limit),
        ),
        m(
            "formatted",
            "([Ljava/lang/Object;)Ljava/lang/String;",
            f!(formatted),
        ),
        s("valueOf", "([C)Ljava/lang/String;", f!(value_of_chars)),
        s("copyValueOf", "([C)Ljava/lang/String;", f!(value_of_chars)),
        s(
            "join",
            "(Ljava/lang/CharSequence;[Ljava/lang/CharSequence;)Ljava/lang/String;",
            f!(join_array),
        ),
        s(
            "join",
            "(Ljava/lang/CharSequence;Ljava/lang/Iterable;)Ljava/lang/String;",
            f!(join_iterable),
        ),
        s(
            "format",
            "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;",
            f!(format),
        ),
        s(
            "format",
            "(Ljava/util/Locale;Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;",
            f!(format_locale),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::{fixed, grouped, literal, split_literal};

    #[test]
    fn split_drops_trailing_empty_strings_as_java_does() {
        assert_eq!(split_literal("a,b,,", ",", 0), ["a", "b"]);
        assert_eq!(split_literal("a,b,,", ",", -1), ["a", "b", "", ""]);
        assert_eq!(split_literal("a,b,c", ",", 2), ["a", "b,c"]);
        assert_eq!(split_literal("", ",", 0), [""]);
        assert_eq!(split_literal(",", ",", 0), Vec::<String>::new());
        assert_eq!(split_literal(",a", ",", 0), ["", "a"]);
        assert_eq!(literal("\\."), Some(".".into()));
        assert_eq!(literal(":"), Some(":".into()));
        assert_eq!(literal("\\s+"), None);
    }

    #[test]
    fn fixed_rounds_the_exact_value_half_up() {
        assert_eq!(fixed(0.125, 2), "0.13");
        assert_eq!(fixed(1.005, 2), "1.00"); // 1.00499999999999989...
        assert_eq!(fixed(2.5, 0), "3");
        assert_eq!(fixed(-0.001, 2), "-0.00");
        assert_eq!(fixed(1234.5678, 3), "1234.568");
        assert_eq!(grouped("1234567.25"), "1,234,567.25");
        assert_eq!(grouped("-1000"), "-1,000");
    }

    #[test]
    fn format_text_follows_javas_formatter() {
        use super::format_text;
        assert_eq!(format_text("%5s|%-5s|%%|%n", &[]).is_err(), true);
        assert_eq!(format_text("100%%", &[]).unwrap(), "100%");
    }
}
