//! Types every method of some jars (docs/java-compat/compiler.md, step 1)
//! and tests the types against the class files' own `StackMapTable`s.
//! Structure only: counts and the first disagreements, no code.
//!
//!     typecheck_jar <jar>...
use horizon_java::{
    ClassFile,
    cfg::Cfg,
    decode,
    types::{Agreement, Classes, check, infer, stack_map},
};
use std::{collections::BTreeMap, io::Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut classes = Vec::new();
    for path in std::env::args().skip(1) {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&path)?)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.name().ends_with(".class") && !entry.name().starts_with("META-INF/") {
                let mut data = Vec::new();
                entry.read_to_end(&mut data)?;
                classes.push(ClassFile::parse(&data)?);
            }
        }
    }
    let hierarchy = Classes::new(&classes);
    let started = std::time::Instant::now();
    let (mut methods, mut typed, mut mapped, mut clean) = (0usize, 0usize, 0usize, 0usize);
    let (mut blocks, mut loops, mut irreducible, mut with_handlers) =
        (0usize, 0usize, 0usize, 0usize);
    let mut total = Agreement::default();
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut examples: Vec<String> = Vec::new();
    for c in &classes {
        let owner = c.name()?;
        for m in &c.methods {
            let Some(code) = m.code() else { continue };
            methods += 1;
            let insns = decode(&code.code)?;
            let cfg = Cfg::new(&insns, &code.handlers);
            blocks += cfg.blocks.len();
            loops += cfg.loops.len();
            if !cfg.reducible {
                irreducible += 1;
            }
            if !code.handlers.is_empty() {
                with_handlers += 1;
            }
            let frames = match infer(&hierarchy, c, m, &insns) {
                Ok(f) => f,
                Err(e) => {
                    // The reason without positions: "at 12 (182): pop of..." → "pop of...".
                    let reason = e.rsplit(": ").next().unwrap_or(&e).to_owned();
                    *failures.entry(reason).or_default() += 1;
                    continue;
                }
            };
            typed += 1;
            let Some(map) = stack_map(c, m)? else {
                continue;
            };
            mapped += 1;
            let (a, first) = check(&hierarchy, &insns, &frames, &map);
            total.agree += a.agree;
            total.disagree += a.disagree;
            total.unknown += a.unknown;
            match first {
                None => clean += 1,
                Some(f) if examples.len() < 15 => {
                    examples.push(format!("{owner}.{}{}: {f}", m.name, m.descriptor))
                }
                Some(_) => {}
            }
        }
    }
    println!(
        "{} classes, {methods} methods with code in {:.1} s: {typed} typed, {} not",
        classes.len(),
        started.elapsed().as_secs_f64(),
        methods - typed
    );
    println!(
        "{mapped} with a StackMapTable: {clean} agree in every slot; slots {} agree, {} disagree, {} unknown",
        total.agree, total.disagree, total.unknown
    );
    println!(
        "control flow: {blocks} blocks, {loops} loops; {irreducible} methods irreducible; {with_handlers} with exception handlers"
    );
    for (reason, n) in &failures {
        println!("  not typed ({n}): {reason}");
    }
    for e in &examples {
        println!("  disagrees: {e}");
    }
    Ok(())
}
