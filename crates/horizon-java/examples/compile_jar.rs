//! Compiles a mod jar and reports what did not compile, and what the jar
//! needs from the host (docs/java-compat/AGENT.md). Structure only: no
//! code is read.
//!
//!     compile_jar <mod.jar> [out.wasm]
use horizon_java::{ClassFile, compile::compile};
use std::io::Read;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: compile_jar <mod.jar> [out.wasm]")?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path)?)?;
    let mut classes = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.name().ends_with(".class") {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            classes.push(ClassFile::parse(&data)?);
        }
    }
    let started = std::time::Instant::now();
    let compiled = compile(&classes)?;
    let methods: usize = classes
        .iter()
        .map(|c| c.methods.iter().filter(|m| m.code().is_some()).count())
        .sum();
    println!(
        "{} classes, {methods} methods: {} compiled, {} skipped; {} lambdas, {} constants; {} bytes in {:?}",
        classes.len(),
        methods - compiled.skipped.len(),
        compiled.skipped.len(),
        compiled.lambdas.len(),
        compiled.constants.len(),
        compiled.wasm.len(),
        started.elapsed()
    );
    for (m, why) in &compiled.skipped {
        println!("  skipped {m}: {why}");
    }
    // Loading it also validates it.
    let mut config = wasmtime::Config::new();
    config.wasm_gc(true).wasm_function_references(true);
    let module = wasmtime::Module::new(&wasmtime::Engine::new(&config)?, &compiled.wasm)
        .map_err(|e| format!("{e:?}"))?;
    let imports: Vec<String> = module.imports().map(|i| i.name().to_owned()).collect();
    println!("{} imports:", imports.len());
    for i in &imports {
        println!("  {i}");
    }
    if let Some(out) = args.next() {
        std::fs::write(out, &compiled.wasm)?;
    }
    Ok(())
}
