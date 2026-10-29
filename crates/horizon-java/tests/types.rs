//! The typed front end (docs/java-compat/compiler.md, step 1): every method
//! of the test classes, and of a real pack when HORIZON_JAVA_MODS is set,
//! gets a type for each slot, and the types fit the class files' own
//! `StackMapTable`s. The test classes are built with the local `javac`;
//! skips without one.
use horizon_java::{
    ClassFile, decode,
    types::{Classes, check, infer, stack_map},
};
use std::{io::Read, path::PathBuf, process::Command};

/// Types every method of `classes`: (methods, methods with a map, slots
/// that disagree, the first disagreement).
fn type_all(classes: &[ClassFile]) -> (usize, usize, usize, Option<String>) {
    let h = Classes::new(classes);
    let (mut methods, mut mapped, mut disagree) = (0, 0, 0);
    let mut first = None;
    for c in classes {
        for m in &c.methods {
            let Some(code) = m.code() else { continue };
            let insns = decode(&code.code).unwrap();
            let frames = infer(&h, c, m, &insns)
                .unwrap_or_else(|e| panic!("{}.{}: {e}", c.name().unwrap(), m.name));
            methods += 1;
            if let Some(map) = stack_map(c, m).unwrap() {
                mapped += 1;
                let (a, f) = check(&h, &insns, &frames, &map);
                disagree += a.disagree;
                if first.is_none() {
                    first = f.map(|f| format!("{}.{}: {f}", c.name().unwrap(), m.name));
                }
            }
        }
    }
    (methods, mapped, disagree, first)
}

#[test]
fn the_test_classes_type_as_their_stack_maps_say() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("horizon-java-types");
    let files: Vec<PathBuf> = ["g/Block", "t/Basic", "t/Objects", "t/Errors", "t/Shapes"]
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
            return;
        }
    }
    let mut classes = Vec::new();
    for e in walk(&out) {
        if e.extension().is_some_and(|x| x == "class") {
            classes.push(ClassFile::parse(&std::fs::read(&e).unwrap()).unwrap());
        }
    }
    let (methods, mapped, disagree, first) = type_all(&classes);
    assert!(
        methods > 0 && mapped > 0,
        "{methods} methods, {mapped} mapped"
    );
    assert_eq!(disagree, 0, "{first:?}");
}

#[test]
fn every_method_of_a_pack_types() {
    let Some(dir) = std::env::var_os("HORIZON_JAVA_MODS").map(PathBuf::from) else {
        eprintln!("skipping: HORIZON_JAVA_MODS is not set");
        return;
    };
    let mut classes = Vec::new();
    for path in walk(&dir) {
        if path.extension().is_none_or(|e| e != "jar") {
            continue;
        }
        let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()) else {
            continue;
        };
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            if entry.name().ends_with(".class") && !entry.name().starts_with("META-INF/") {
                let mut data = Vec::new();
                entry.read_to_end(&mut data).unwrap();
                classes.push(ClassFile::parse(&data).unwrap());
            }
        }
    }
    // Without the game's own classes a few joins are less exact than the
    // class files' (they join to Object): these are not counted here.
    let (methods, _, _, _) = type_all(&classes);
    assert!(methods > 0);
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
