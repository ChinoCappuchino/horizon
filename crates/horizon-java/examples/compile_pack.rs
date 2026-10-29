//! Compiles every jar of a pack and counts what does not compile yet, by
//! reason (docs/java-compat/plan.md). Structure only: no code is read.
//!
//!     compile_pack <mods folder>
use horizon_java::{ClassFile, compile::compile};
use std::{collections::BTreeMap, io::Read, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .ok_or("usage: compile_pack <mods folder>")?;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jar"))
        .collect();
    paths.sort();
    let (mut methods, mut skipped) = (0usize, 0usize);
    let mut reasons = BTreeMap::<String, usize>::new();
    let mut whole = Vec::new();
    let mut failed = Vec::new();
    for path in &paths {
        let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(path)?) else {
            continue;
        };
        let mut classes = Vec::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.name().ends_with(".class") {
                let mut data = Vec::new();
                entry.read_to_end(&mut data)?;
                classes.push(ClassFile::parse(&data)?);
            }
        }
        let n: usize = classes
            .iter()
            .map(|c| c.methods.iter().filter(|m| m.code().is_some()).count())
            .sum();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match compile(&classes) {
            Ok(c) => {
                methods += n;
                skipped += c.skipped.len();
                if c.skipped.is_empty() {
                    whole.push(name);
                }
                for (_, why) in &c.skipped {
                    *reasons.entry(category(why)).or_default() += 1;
                }
            }
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    println!(
        "{} jars: {methods} methods, {} compiled ({:.1} %), {skipped} skipped",
        paths.len(),
        methods - skipped,
        100.0 * (methods - skipped) as f64 / methods.max(1) as f64
    );
    println!("{} jars compile whole: {}", whole.len(), whole.join(", "));
    let mut by_count: Vec<_> = reasons.into_iter().collect();
    by_count.sort_by_key(|r| std::cmp::Reverse(r.1));
    println!("skipped, by reason:");
    for (r, n) in by_count.iter().take(40) {
        println!("  {n:7}  {r}");
    }
    for f in &failed {
        println!("failed: {f}");
    }
    Ok(())
}

/// A reason without the member names in it.
fn category(why: &str) -> String {
    if let Some(i) = why.find(" is not compiled yet") {
        let head = &why[..i];
        return format!(
            "{} is not compiled yet",
            head.rsplit(": ").next().unwrap_or(head)
        );
    }
    if why.contains("classes that extend game classes") {
        return "a constructor of a class that extends a game class".into();
    }
    if why.contains("is overridden") {
        return "a virtual call to an overridden method of the jar".into();
    }
    if let Some(i) = why.find("through ") {
        return format!(
            "invokedynamic through {}",
            why[i + 8..].split(' ').next().unwrap_or("")
        );
    }
    why.split(':')
        .next()
        .unwrap_or(why)
        .chars()
        .filter(|c| !c.is_ascii_digit())
        .collect()
}
