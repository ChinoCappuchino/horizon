//! Logging (SLF4J's `Logger`, Mojang's `LogUtils`), for the native backend's
//! runtime library: vanilla's classes take a logger in their static setup.
//! The logger is quiet: what the compiled code computes never depends on it.
use super::{Builtin, Method, alloc, vt};

const LOGGER: &str = "org/slf4j/Logger";
/// The one logger class (the interface's methods are its defaults).
const QUIET: &str = "horizon/rt/QuietLogger";

extern "C" fn get_logger() -> u64 {
    alloc(8, vt(QUIET))
}

extern "C" fn get_logger_named(_name: u64) -> u64 {
    get_logger()
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
    let mut methods = Vec::new();
    for level in ["trace", "debug", "info", "warn", "error"] {
        for desc in [
            "(Ljava/lang/String;)V",
            "(Ljava/lang/String;Ljava/lang/Object;)V",
            "(Ljava/lang/String;Ljava/lang/Object;Ljava/lang/Object;)V",
            "(Ljava/lang/String;[Ljava/lang/Object;)V",
            "(Ljava/lang/String;Ljava/lang/Throwable;)V",
        ] {
            methods.push(m(level, desc, quiet));
        }
    }
    for enabled in [
        "isTraceEnabled",
        "isDebugEnabled",
        "isInfoEnabled",
        "isWarnEnabled",
        "isErrorEnabled",
    ] {
        methods.push(m(enabled, "()Z", off));
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
