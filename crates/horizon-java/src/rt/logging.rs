//! Logging (SLF4J's `Logger` and `LoggerFactory`, Mojang's `LogUtils`), for
//! the native backend's runtime library: vanilla's classes take a logger in
//! their static setup. Info, warnings and errors go to standard error as a
//! game's log does (SLF4J's `{}` placeholders filled in); debug and trace
//! are quiet. What the compiled code computes never depends on it.
use super::{A_DATA, A_LEN, Builtin, Method, alloc, instance_of, to_string, units, vt, word};

const LOGGER: &str = "org/slf4j/Logger";
/// The one logger class (the interface's methods are its defaults).
const QUIET: &str = "horizon/rt/QuietLogger";

extern "C" fn get_logger() -> u64 {
    alloc(8, vt(QUIET))
}

extern "C" fn get_logger_named(_name: u64) -> u64 {
    get_logger()
}

fn text(o: u64) -> String {
    if o == 0 {
        "null".to_owned()
    } else {
        String::from_utf16_lossy(units(to_string(o)))
    }
}

/// An SLF4J message: each `{}` the next argument's `String.valueOf`; a
/// throwable left over after the placeholders follows the message.
fn emit(level: &str, format: u64, args: &[u64]) {
    let format = text(format);
    let mut out = String::new();
    let mut used = 0;
    let mut rest = format.as_str();
    while let Some(at) = rest.find("{}") {
        let escaped = rest[..at].ends_with('\\');
        out.push_str(&rest[..if escaped { at - 1 } else { at }]);
        if escaped || used == args.len() {
            out.push_str("{}");
        } else {
            out.push_str(&text(args[used]));
            used += 1;
        }
        rest = &rest[at + 2..];
    }
    out.push_str(rest);
    if let Some(&last) = args.get(used..).and_then(|a| a.last())
        && instance_of(last, "java/lang/Throwable")
    {
        out.push('\n');
        out.push_str(&text(last));
    }
    eprintln!("[{level}] {out}");
}

/// The elements of an `Object[]`.
fn elements(a: u64) -> Vec<u64> {
    if a == 0 {
        return Vec::new();
    }
    // SAFETY: a reference array: its length, then its elements.
    unsafe {
        (0..word(a, A_LEN) as usize)
            .map(|i| word(a, A_DATA + 8 * i))
            .collect()
    }
}

macro_rules! level {
    ($name:literal, $f0:ident, $f1:ident, $f2:ident, $fa:ident) => {
        extern "C" fn $f0(_l: u64, f: u64) {
            emit($name, f, &[]);
        }
        extern "C" fn $f1(_l: u64, f: u64, a: u64) {
            emit($name, f, &[a]);
        }
        extern "C" fn $f2(_l: u64, f: u64, a: u64, b: u64) {
            emit($name, f, &[a, b]);
        }
        extern "C" fn $fa(_l: u64, f: u64, a: u64) {
            emit($name, f, &elements(a));
        }
    };
}
level!("INFO", info0, info1, info2, info_array);
level!("WARN", warn0, warn1, warn2, warn_array);
level!("ERROR", error0, error1, error2, error_array);

extern "C" fn enabled(_logger: u64) -> i32 {
    1
}

/// Every message method: the receiver and up to three references (the C
/// convention lets a function ignore arguments it doesn't take).
extern "C" fn ignore(_logger: u64) {}

extern "C" fn disabled(_logger: u64) -> i32 {
    0
}

pub fn builtins() -> Vec<Builtin> {
    let m = |name, desc, f: usize| Method {
        name,
        desc,
        is_static: false,
        f,
    };
    let quiet = ignore as *const () as usize;
    let off = disabled as *const () as usize;
    let on = enabled as *const () as usize;
    let f = |x: extern "C" fn(u64, u64)| x as *const () as usize;
    let f1 = |x: extern "C" fn(u64, u64, u64)| x as *const () as usize;
    let f2 = |x: extern "C" fn(u64, u64, u64, u64)| x as *const () as usize;
    let mut methods = Vec::new();
    const S: &str = "(Ljava/lang/String;)V";
    const S1: &str = "(Ljava/lang/String;Ljava/lang/Object;)V";
    const S2: &str = "(Ljava/lang/String;Ljava/lang/Object;Ljava/lang/Object;)V";
    const SA: &str = "(Ljava/lang/String;[Ljava/lang/Object;)V";
    const ST: &str = "(Ljava/lang/String;Ljava/lang/Throwable;)V";
    for level in ["trace", "debug"] {
        for desc in [S, S1, S2, SA, ST] {
            methods.push(m(level, desc, quiet));
        }
    }
    for (level, a, b, c, d) in [
        ("info", f(info0), f1(info1), f2(info2), f1(info_array)),
        ("warn", f(warn0), f1(warn1), f2(warn2), f1(warn_array)),
        ("error", f(error0), f1(error1), f2(error2), f1(error_array)),
    ] {
        methods.push(m(level, S, a));
        methods.push(m(level, S1, b));
        methods.push(m(level, S2, c));
        methods.push(m(level, SA, d));
        // (A throwable is a last argument with no placeholder.)
        methods.push(m(level, ST, b));
    }
    for name in ["isTraceEnabled", "isDebugEnabled"] {
        methods.push(m(name, "()Z", off));
    }
    for name in ["isInfoEnabled", "isWarnEnabled", "isErrorEnabled"] {
        methods.push(m(name, "()Z", on));
    }
    vec![
        Builtin {
            name: LOGGER,
            super_name: Some("java/lang/Object"),
            interfaces: &[],
            size: 8,
            is_final: false,
            is_interface: true,
            methods,
        },
        Builtin {
            name: QUIET,
            super_name: Some("java/lang/Object"),
            interfaces: &[LOGGER],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: Vec::new(),
        },
        Builtin {
            name: "org/slf4j/LoggerFactory",
            super_name: Some("java/lang/Object"),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                Method {
                    name: "getLogger",
                    desc: "(Ljava/lang/String;)Lorg/slf4j/Logger;",
                    is_static: true,
                    f: get_logger_named as *const () as usize,
                },
                Method {
                    name: "getLogger",
                    desc: "(Ljava/lang/Class;)Lorg/slf4j/Logger;",
                    is_static: true,
                    f: get_logger_named as *const () as usize,
                },
            ],
        },
        Builtin {
            name: "com/mojang/logging/LogUtils",
            super_name: Some("java/lang/Object"),
            interfaces: &[],
            size: 8,
            is_final: true,
            is_interface: false,
            methods: vec![
                Method {
                    name: "getLogger",
                    desc: "()Lorg/slf4j/Logger;",
                    is_static: true,
                    f: get_logger as *const () as usize,
                },
                Method {
                    name: "getLogger",
                    desc: "(Ljava/lang/String;)Lorg/slf4j/Logger;",
                    is_static: true,
                    f: get_logger_named as *const () as usize,
                },
            ],
        },
    ]
}
