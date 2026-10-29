//! Runs a static benchmark method of compiled Java code and times it, to
//! compare the compiler's output with the JVM (fork `afterwinter`, vanilla
//! feasibility: docs/java-compat/plan.md). Structure only: no code is read;
//! the classes are compiled as they are and the host answers the JDK calls
//! a benchmark needs (math, `Object`), failing by name on any other.
//!
//!     jar_bench [--only <prefix>,...] [--imports] <jar or class dir>... -- <Class> <n>
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
            p => paths.push(p.to_owned()),
        }
    }
    let mut classes = Vec::new();
    for p in &paths {
        read(Path::new(p), &only, &mut classes)?;
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
