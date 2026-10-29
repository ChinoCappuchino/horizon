//! The compiler against Java's own results: test classes in tests/java are
//! built with the local `javac`, compiled to WASM and run in wasmtime.
//! Skips when no `javac` is on the PATH.
use horizon_java::{ClassFile, compile::compile};
use std::{path::PathBuf, process::Command};
use wasmtime::{Config, Engine, Instance, Module, Store, WasmParams, WasmResults};

/// Builds the named test classes; None without a `javac`.
fn javac(names: &[&str]) -> Option<Vec<ClassFile>> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("horizon-java-classes");
    let files: Vec<PathBuf> = names
        .iter()
        .map(|n| src.join(format!("{n}.java")))
        .collect();
    let status = Command::new("javac")
        .args(["--release", "21", "-d"])
        .arg(&out)
        .args(&files)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("javac failed: {s}"),
        Err(_) => {
            eprintln!("skipping: no javac");
            return None;
        }
    }
    let mut classes = Vec::new();
    for n in names {
        let data = std::fs::read(out.join(format!("{n}.class"))).unwrap();
        classes.push(ClassFile::parse(&data).unwrap());
    }
    Some(classes)
}

struct Run {
    store: Store<()>,
    instance: Instance,
}

impl Run {
    fn new(classes: &[ClassFile]) -> Run {
        let compiled = compile(classes).unwrap();
        for (method, why) in &compiled.skipped {
            eprintln!("skipped {method}: {why}");
        }
        let wasm = compiled.wasm;
        let mut config = Config::new();
        config
            .wasm_gc(true)
            .wasm_function_references(true)
            .wasm_exceptions(true);
        let engine = Engine::new(&config).unwrap();
        let module = Module::new(&engine, &wasm).unwrap();
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        Run { store, instance }
    }
    fn call<P: WasmParams, R: WasmResults>(&mut self, name: &str, params: P) -> R {
        self.instance
            .get_typed_func::<P, R>(&mut self.store, name)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .call(&mut self.store, params)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    }
}

#[test]
fn static_code_runs_as_java_does() {
    let Some(classes) = javac(&["t/Basic"]) else {
        return;
    };
    let mut r = Run::new(&classes);
    assert_eq!(r.call::<_, i32>("t/Basic.add(II)I", (2, 3)), 5);
    assert_eq!(
        r.call::<_, i32>("t/Basic.add(II)I", (i32::MAX, 1)),
        i32::MIN
    );
    assert_eq!(r.call::<_, i32>("t/Basic.sum(I)I", 10), 45);
    assert_eq!(r.call::<_, i32>("t/Basic.max(II)I", (-4, 9)), 9);
    // The initializer runs once, before the first use: counter starts at 5.
    assert_eq!(r.call::<_, i32>("t/Basic.next()I", ()), 16);
    assert_eq!(r.call::<_, i32>("t/Basic.bump()I", ()), 6);
    assert_eq!(r.call::<_, i32>("t/Basic.next()I", ()), 18);
    for (k, v) in [(1, 10), (2, 20), (3, 30), (100, 1000), (7, -1), (-1, -1)] {
        assert_eq!(r.call::<_, i32>("t/Basic.pick(I)I", k), v);
    }
    assert_eq!(
        r.call::<_, i64>("t/Basic.fact(I)J", 20),
        2432902008176640000
    );
    assert_eq!(r.call::<_, i32>("t/Basic.div(II)I", (-7, 2)), -3);
    assert_eq!(
        r.call::<_, i32>("t/Basic.div(II)I", (i32::MIN, -1)),
        i32::MIN
    );
    assert_eq!(r.call::<_, i32>("t/Basic.rem(II)I", (-7, 2)), -1);
    assert_eq!(r.call::<_, i32>("t/Basic.rem(II)I", (i32::MIN, -1)), 0);
    assert_eq!(r.call::<_, i32>("t/Basic.order(FF)I", (1.0f32, 2.0f32)), -1);
    assert_eq!(r.call::<_, i32>("t/Basic.order(FF)I", (3.0f32, 2.0f32)), 1);
    assert_eq!(
        r.call::<_, i32>("t/Basic.order(FF)I", (f32::NAN, 2.0f32)),
        0
    );
    assert_eq!(r.call::<_, f64>("t/Basic.mod(DD)D", (7.5, 2.0)), 1.5);
    assert_eq!(r.call::<_, f64>("t/Basic.mod(DD)D", (-7.5, 2.0)), -1.5);
    assert_eq!(r.call::<_, i32>("t/Basic.toInt(D)I", 1e20), i32::MAX);
    assert_eq!(r.call::<_, i32>("t/Basic.toInt(D)I", f64::NAN), 0);
    assert_eq!(r.call::<_, i64>("t/Basic.shifted()J", ()), 1 << 37);
    assert_eq!(r.call::<_, i32>("t/Basic.fib(I)I", 20), 6765);
    assert_eq!(r.call::<_, i64>("t/Basic.twice(J)J", 4i64), 10);
    assert_eq!(r.call::<_, i32>("t/Basic.nextChar(C)I", 0xFFFF), 0);
    assert_eq!(r.call::<_, i32>("t/Basic.toByte(I)I", 200), -56);
}
