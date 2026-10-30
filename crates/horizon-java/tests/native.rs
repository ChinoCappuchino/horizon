//! The native backend against Java's own results: test classes in
//! tests/java built with the local `javac`, compiled with Cranelift and
//! called directly. Skips when no `javac` is on the PATH.
#![cfg(feature = "native")]
use horizon_java::{ClassFile, native::Native};
use std::{path::PathBuf, process::Command};

/// Builds `t/NativeOps` and its nested classes; None without a `javac`.
fn native_ops() -> Option<Native> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("horizon-java-native");
    let status = Command::new("javac")
        .args(["--release", "21", "-d"])
        .arg(&out)
        .arg(src.join("t/NativeOps.java"))
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
    for e in std::fs::read_dir(out.join("t")).unwrap() {
        let p = e.unwrap().path();
        if p.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("NativeOps")
        {
            classes.push(ClassFile::parse(&std::fs::read(&p).unwrap()).unwrap());
        }
    }
    Some(Native::compile(&classes).unwrap())
}

#[test]
fn wide_array_post_increments_keep_both_values() {
    let Some(native) = native_ops() else { return };
    assert!(native.unsupported.is_empty(), "{:?}", native.unsupported);
    let longs = native.function("t/NativeOps.postIncLongs(I)J").unwrap();
    let doubles = native.function("t/NativeOps.postIncDoubles(I)D").unwrap();
    // SAFETY: the compiled methods have these signatures.
    let longs: extern "C" fn(i32) -> i64 = unsafe { std::mem::transmute(longs) };
    let doubles: extern "C" fn(i32) -> f64 = unsafe { std::mem::transmute(doubles) };
    // before * 1000 + after, plus the element after.
    assert_eq!(longs(1), 20 * 1000 + 21 + 21);
    assert_eq!(doubles(2), 3.5 * 1000.0 + 4.5 + 4.5);
}

#[test]
fn a_lambda_in_an_abstract_class_calls_its_private_body() {
    let Some(native) = native_ops() else { return };
    assert!(native.unsupported.is_empty(), "{:?}", native.unsupported);
    let f = native.function("t/NativeOps.lambdaInAbstract(I)I").unwrap();
    // SAFETY: as above.
    let f: extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(f) };
    assert_eq!(f(4), (4 * 3 + 1) * 100 + (4 * 5 + 2));
}
