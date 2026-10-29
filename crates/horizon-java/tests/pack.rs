//! Every class of a real mod pack reads, and every method's code decodes
//! (docs/java-compat/plan.md, phase 0, step 1). Set HORIZON_JAVA_MODS to a
//! folder of mod jars (an instance's `mods/`); skips without it.
use horizon_java::{ClassFile, bytecode::decode};
use std::{io::Read, path::PathBuf};

#[test]
fn every_class_of_a_pack_reads_and_its_code_decodes() {
    let Some(dir) = std::env::var_os("HORIZON_JAVA_MODS").map(PathBuf::from) else {
        eprintln!("skipping: HORIZON_JAVA_MODS is not set");
        return;
    };
    let (mut jars, mut classes, mut methods, mut instructions) = (0, 0, 0, 0usize);
    let mut versions = std::collections::BTreeMap::<u16, usize>::new();
    let mut failures = Vec::new();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jar"))
        .collect();
    paths.sort();
    for path in paths {
        let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()) else {
            continue;
        };
        jars += 1;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            if !entry.name().ends_with(".class") {
                continue;
            }
            let name = entry.name().to_owned();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            match ClassFile::parse(&data) {
                Ok(class) => {
                    classes += 1;
                    *versions.entry(class.major).or_default() += 1;
                    for m in &class.methods {
                        let Some(code) = m.code() else { continue };
                        methods += 1;
                        match decode(&code.code) {
                            Ok(insns) => instructions += insns.len(),
                            Err(e) => {
                                failures.push(format!("{}: {name} {}: {e}", path.display(), m.name))
                            }
                        }
                    }
                }
                Err(e) => failures.push(format!("{}: {name}: {e}", path.display())),
            }
        }
    }
    eprintln!(
        "{jars} jars, {classes} classes, {methods} methods with code, {instructions} instructions; \
         class file versions {versions:?}"
    );
    assert!(
        failures.is_empty(),
        "{} failures, e.g. {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
    assert!(classes > 0);
}
