//! Runs a static benchmark method of compiled Java code and times it, to
//! compare the compiler's output with the JVM (fork `afterwinter`, vanilla
//! feasibility: docs/java-compat/plan.md). Structure only: no code is read;
//! the classes are compiled as they are and the host answers the JDK calls
//! a benchmark needs (math, `Object`), failing by name on any other.
//!
//!     jar_bench [--only <prefix>,...] [--imports] [--native <out.cwasm>] <jar or class dir>... -- <Class> <n>
//!
//! `--only` keeps the classes of the jars whose names start with a prefix
//! (class directories are kept whole); `--imports` lists what the compiled
//! classes need and stops. The benchmark class has `make(J)Ljava/lang/Object;`
//! style setup left to it: `<Class>.make(J)` is called with seed 42, then
//! `<Class>.run(<made>, n)` five times.
use horizon_java::{
    ClassFile,
    compile::compile,
    runtime::{Context, Host, Import, Runtime, Value},
};
use std::{
    io::Read,
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicI64, Ordering::SeqCst},
    time::Instant,
};
use wasmtime::{Error, Result, bail};

fn anyhow(m: impl std::fmt::Display) -> Error {
    Error::msg(m.to_string())
}

/// The first `roots` classes and those whose names start with one of
/// `only`, with their supertypes among `all`.
fn with_supertypes(all: Vec<ClassFile>, roots: usize, only: &[String]) -> Vec<ClassFile> {
    let names: Vec<String> = all
        .iter()
        .map(|c| c.name().unwrap_or_default().to_owned())
        .collect();
    let index: std::collections::HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut keep = vec![false; all.len()];
    let mut todo: Vec<usize> = (0..all.len())
        .filter(|&i| i < roots || only.iter().any(|p| names[i].starts_with(p.as_str())))
        .collect();
    while let Some(i) = todo.pop() {
        if std::mem::replace(&mut keep[i], true) {
            continue;
        }
        let c = &all[i];
        let supers = c
            .super_name()
            .ok()
            .flatten()
            .into_iter()
            .chain(c.interfaces.iter().filter_map(|&k| c.class_name(k).ok()));
        todo.extend(supers.filter_map(|s| index.get(s).copied()));
    }
    all.into_iter()
        .zip(keep)
        .filter_map(|(c, k)| k.then_some(c))
        .collect()
}

fn read(path: &Path, only: &[String], out: &mut Vec<ClassFile>) -> Result<()> {
    if path.is_dir() {
        for e in std::fs::read_dir(path)? {
            let p = e?.path();
            if p.is_dir() {
                read(&p, &[], out)?;
            } else if p.extension().is_some_and(|e| e == "class") {
                out.push(ClassFile::parse(&std::fs::read(&p)?).map_err(anyhow)?);
            }
        }
        return Ok(());
    }
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_owned();
        if !name.ends_with(".class") || name.starts_with("META-INF/") {
            continue;
        }
        if !only.is_empty() && !only.iter().any(|p| name.starts_with(p.as_str())) {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        out.push(ClassFile::parse(&data).map_err(|e| anyhow(format!("{name}: {e}")))?);
    }
    Ok(())
}

/// The JDK a numeric benchmark needs.
#[derive(Default)]
struct Jdk {
    /// Calls answered, by import (with HORIZON_BENCH_COUNT set).
    counts: std::cell::RefCell<std::collections::BTreeMap<String, u64>>,
}

fn d(v: &Value) -> f64 {
    match v {
        Value::Double(x) => *x,
        Value::Float(x) => f64::from(*x),
        Value::Int(x) => f64::from(*x),
        Value::Long(x) => *x as f64,
        _ => f64::NAN,
    }
}

impl Host for Jdk {
    fn call(&self, cx: &mut Context<'_>, i: &Import, args: &[Value]) -> Result<Value> {
        let a = |k: usize| d(&args[k]);
        if std::env::var_os("HORIZON_BENCH_COUNT").is_some() {
            *self
                .counts
                .borrow_mut()
                .entry(format!("{} {}.{} {}", i.op, i.owner, i.name, i.descriptor))
                .or_default() += 1;
        }
        Ok(
            match (i.owner.as_str(), i.name.as_str(), i.descriptor.as_str()) {
                ("java/lang/Object", "<init>", _) => Value::Void,
                // `Util.make(o, f)`: f.accept(o), then o (Util's own static
                // setup starts thread pools, which a benchmark does not need).
                ("net/minecraft/util/Util", "make", _) => {
                    cx.call_lambda(&args[1], &[args[0].clone()])?;
                    args[0].clone()
                }
                ("java/util/concurrent/atomic/AtomicLong", "<init>", desc) => {
                    let start = match (desc, args.first()) {
                        ("(J)V", Some(Value::Long(v))) => *v,
                        _ => 0,
                    };
                    Value::Host(std::sync::Arc::new(AtomicI64::new(start)))
                }
                ("java/util/concurrent/atomic/AtomicLong", f, _) => {
                    let Some(Value::Host(o)) = args.first() else {
                        bail!("not answered: {i:?}");
                    };
                    let Some(at) = o.downcast_ref::<AtomicI64>() else {
                        bail!("not answered: {i:?}");
                    };
                    let long = |k: usize| match args.get(k) {
                        Some(Value::Long(v)) => *v,
                        _ => 0,
                    };
                    match f {
                        "get" => Value::Long(at.load(SeqCst)),
                        "set" => {
                            at.store(long(1), SeqCst);
                            Value::Void
                        }
                        "compareAndSet" => Value::Int(
                            at.compare_exchange(long(1), long(2), SeqCst, SeqCst)
                                .is_ok() as i32,
                        ),
                        "getAndSet" => Value::Long(at.swap(long(1), SeqCst)),
                        _ => bail!("not answered: {i:?}"),
                    }
                }
                ("java/lang/Float" | "java/lang/Double", "isNaN", _) => {
                    Value::Int(a(0).is_nan() as i32)
                }
                ("java/lang/Float" | "java/lang/Double", "isInfinite", _) => {
                    Value::Int(a(0).is_infinite() as i32)
                }
                ("java/lang/Float" | "java/lang/Double", "isFinite", _) => {
                    Value::Int(a(0).is_finite() as i32)
                }
                ("java/lang/Float", "floatToIntBits" | "floatToRawIntBits", _) => {
                    Value::Int((a(0) as f32).to_bits() as i32)
                }
                ("java/lang/Float", "intBitsToFloat", _) => match args[0] {
                    Value::Int(b) => Value::Float(f32::from_bits(b as u32)),
                    _ => bail!("not answered: {i:?}"),
                },
                ("java/lang/Double", "doubleToLongBits" | "doubleToRawLongBits", _) => {
                    Value::Long(a(0).to_bits() as i64)
                }
                ("java/lang/Double", "longBitsToDouble", _) => match args[0] {
                    Value::Long(b) => Value::Double(f64::from_bits(b as u64)),
                    _ => bail!("not answered: {i:?}"),
                },
                ("java/lang/Float" | "java/lang/Double", "compare", _) => {
                    Value::Int(a(0).partial_cmp(&a(1)).map_or(0, |o| o as i32))
                }
                ("java/lang/Math" | "java/lang/StrictMath", f, desc) => {
                    let r = match f {
                        "floor" => a(0).floor(),
                        "ceil" => a(0).ceil(),
                        "sqrt" => a(0).sqrt(),
                        "abs" => a(0).abs(),
                        "sin" => a(0).sin(),
                        "cos" => a(0).cos(),
                        "exp" => a(0).exp(),
                        "log" => a(0).ln(),
                        "pow" => a(0).powf(a(1)),
                        "atan2" => a(0).atan2(a(1)),
                        "asin" => a(0).asin(),
                        "acos" => a(0).acos(),
                        "atan" => a(0).atan(),
                        "tan" => a(0).tan(),
                        "signum" => a(0).signum(),
                        "round" => a(0).round(),
                        "toRadians" => a(0).to_radians(),
                        "toDegrees" => a(0).to_degrees(),
                        "fma" => a(0).mul_add(a(1), a(2)),
                        "nextDown" if desc.starts_with("(F") => {
                            f64::from((a(0) as f32).next_down())
                        }
                        "nextUp" if desc.starts_with("(F") => f64::from((a(0) as f32).next_up()),
                        "nextDown" => a(0).next_down(),
                        "nextUp" => a(0).next_up(),
                        "max" => a(0).max(a(1)),
                        "min" => a(0).min(a(1)),
                        _ => bail!("not answered: {i:?}"),
                    };
                    match desc.chars().last() {
                        Some('D') => Value::Double(r),
                        Some('F') => Value::Float(r as f32),
                        Some('I') => Value::Int(r as i32),
                        Some('J') => Value::Long(r as i64),
                        _ => bail!("not answered: {i:?}"),
                    }
                }
                _ => bail!(
                    "not answered: {} {}.{} {}",
                    i.op,
                    i.owner,
                    i.name,
                    i.descriptor
                ),
            },
        )
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let (inputs, bench) = args.split_at(split);
    let mut only = Vec::new();
    let mut imports = false;
    let mut native: Option<String> = None;
    let mut run_native = false;
    let mut native_check = false;
    let mut paths = Vec::new();
    let mut it = inputs.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--only" => {
                only = it
                    .next()
                    .ok_or(anyhow("--only needs prefixes"))?
                    .split(',')
                    .map(str::to_owned)
                    .collect()
            }
            "--imports" => imports = true,
            "--native" => native = it.next().cloned(),
            "--run-native" => run_native = true,
            "--native-check" => native_check = true,
            p => paths.push(p.to_owned()),
        }
    }
    let mut classes = Vec::new();
    let mut roots = 0;
    for p in &paths {
        read(Path::new(p), &[], &mut classes)?;
        // Class directories are kept whole.
        if Path::new(p).is_dir() {
            roots = classes.len();
        }
    }
    // `--only`: the classes it names, and every class they extend or
    // implement (a class is never compiled without its supertypes).
    if !only.is_empty() {
        classes = with_supertypes(classes, roots, &only);
    }
    let started = Instant::now();
    let compiled = compile(&classes).map_err(anyhow)?;
    eprintln!(
        "{} classes compiled in {:.2} s, {} not compiled",
        classes.len(),
        started.elapsed().as_secs_f64(),
        compiled.skipped.len()
    );
    for (m, why) in &compiled.skipped {
        eprintln!("not compiled: {m}: {why}");
    }
    if native_check {
        return native_check_report(&classes);
    }
    if run_native {
        return native_bench(&classes, &bench[1..]);
    }
    if let Some(out) = native {
        std::fs::write(&out, horizon_java::runtime::precompile(&compiled)?)?;
        eprintln!("native code in {out}");
        return Ok(());
    }
    if imports {
        let mut config = wasmtime::Config::new();
        config.wasm_gc(true).wasm_function_references(true);
        let module = wasmtime::Module::new(&wasmtime::Engine::new(&config)?, &compiled.wasm)?;
        for i in module.imports() {
            println!("{}", i.name());
        }
        return Ok(());
    }
    let [class, n] = &bench[1..] else {
        bail!("usage: ... -- <Class> <n>");
    };
    let n: i32 = n.parse()?;
    let started = Instant::now();
    let jdk = Rc::new(Jdk::default());
    let mut rt = Runtime::new(compiled, jdk.clone())?;
    eprintln!("instantiated in {:.2} s", started.elapsed().as_secs_f64());
    let mut cx = rt.context();
    // The made object's type: a vanilla noise, or any object.
    let ty = [
        "Lnet/minecraft/world/level/levelgen/synth/SimplexNoise;",
        "Ljava/lang/Object;",
    ]
    .into_iter()
    .find(|t| cx.has(&format!("{class}.make(J){t}")))
    .ok_or(anyhow("no make(J) method"))?;
    let made = cx.call(&format!("{class}.make(J){ty}"), &[Value::Long(42)])?;
    for pass in 0..5 {
        let t = Instant::now();
        let sum = cx.call(
            &format!("{class}.run({ty}I)D"),
            &[made.clone(), Value::Int(n)],
        )?;
        println!(
            "pass {pass}: {:.1} ms sum={sum:?}",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    for (name, n) in jdk.counts.borrow().iter() {
        eprintln!("{n:>12} {name}");
    }
    Ok(())
}

/// The benchmark through the native backend (built with `--features
/// native`): `make(J)` then `run(made, n)`, five times.
#[cfg(feature = "native")]
fn native_bench(classes: &[ClassFile], bench: &[String]) -> Result<()> {
    let [class, n] = bench else {
        bail!("usage: ... -- <Class> <n>");
    };
    let n: i32 = n.parse()?;
    let started = Instant::now();
    let native = horizon_java::native::Native::compile(classes).map_err(anyhow)?;
    eprintln!(
        "compiled natively in {:.2} s; {} methods trap (not in the prototype)",
        started.elapsed().as_secs_f64(),
        native.unsupported.len()
    );
    if std::env::var_os("HORIZON_BENCH_UNSUPPORTED").is_some() {
        for (m, why) in &native.unsupported {
            eprintln!("  {m}: {why}");
        }
    }
    let ty = [
        "Lnet/minecraft/world/level/levelgen/synth/SimplexNoise;",
        "Ljava/lang/Object;",
    ]
    .into_iter()
    .find(|t| native.function(&format!("{class}.make(J){t}")).is_some())
    .ok_or(anyhow("no make(J) method"))?;
    let make = native
        .function(&format!("{class}.make(J){ty}"))
        .expect("found");
    let run = native
        .function(&format!("{class}.run({ty}I)D"))
        .ok_or(anyhow("no run method"))?;
    // SAFETY: the compiled methods have these signatures (a reference is
    // a pointer, an int an i32), in the platform's calling convention.
    // (Each takes the thread's context first.)
    let make: extern "C" fn(*mut horizon_java::heap::Context, i64) -> i64 =
        unsafe { std::mem::transmute(make) };
    let run: extern "C" fn(*mut horizon_java::heap::Context, i64, i32) -> f64 =
        unsafe { std::mem::transmute(run) };
    let ctx = native.context();
    let made = make(ctx, 42);
    for pass in 0..5 {
        let t = Instant::now();
        let sum = run(ctx, made, n);
        println!(
            "pass {pass}: {:.1} ms sum={sum}d",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    Ok(())
}

#[cfg(not(feature = "native"))]
fn native_bench(_: &[ClassFile], _: &[String]) -> Result<()> {
    bail!("built without the native backend: --features native")
}

/// Compiles the classes natively and reports what did not compile and the
/// stops in what did, grouped by package (classes outside the set).
#[cfg(feature = "native")]
fn native_check_report(classes: &[ClassFile]) -> Result<()> {
    let methods: usize = classes
        .iter()
        .map(|c| c.methods.iter().filter(|m| m.code().is_some()).count())
        .sum();
    let started = Instant::now();
    let native = horizon_java::native::Native::compile(classes).map_err(anyhow)?;
    println!(
        "{} classes, {methods} methods with code, compiled in {:.1} s: {} whole methods not compiled",
        classes.len(),
        started.elapsed().as_secs_f64(),
        native.unsupported.len()
    );
    let mut why: std::collections::BTreeMap<String, usize> = Default::default();
    for (_, w) in &native.unsupported {
        *why.entry(w.clone()).or_default() += 1;
    }
    let mut why: Vec<_> = why.into_iter().collect();
    why.sort_by_key(|x| std::cmp::Reverse(x.1));
    for (w, n) in why.iter().take(15) {
        println!("  {n:>6}  {w}");
    }
    // HORIZON_BENCH_STOPS=<text>: each stop whose place or reason has it.
    if let Ok(filter) = std::env::var("HORIZON_BENCH_STOPS") {
        for why in horizon_java::rt::stop_reasons() {
            if why.contains(&filter) {
                println!("  stop: {why}");
            }
        }
    }
    let total: usize = native.stops.values().sum();
    println!("{total} instructions compiled as stops; by kind and package:");
    let mut groups: std::collections::BTreeMap<String, usize> = Default::default();
    for (w, n) in &native.stops {
        // "call to net/minecraft/world/level/Level.getBlockState" -> "call to net/minecraft/world"
        let (kind, rest) = w
            .split_once(" of ")
            .or_else(|| w.split_once(" to "))
            .or_else(|| w.split_once(' '))
            .unwrap_or((w, ""));
        let pkg: Vec<&str> = rest
            .split(['.', ' '])
            .next()
            .unwrap_or("")
            .split('/')
            .take(3)
            .collect();
        *groups
            .entry(format!("{kind}: {}", pkg.join("/")))
            .or_default() += n;
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|x| std::cmp::Reverse(x.1));
    for (g, n) in groups.iter().take(25) {
        println!("  {n:>7}  {g}");
    }
    Ok(())
}

#[cfg(not(feature = "native"))]
fn native_check_report(_: &[ClassFile]) -> Result<()> {
    bail!("built without the native backend: --features native")
}
