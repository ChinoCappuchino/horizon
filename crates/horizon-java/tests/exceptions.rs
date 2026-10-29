//! Throwing and catching (a mod exception that extends the JDK's
//! RuntimeException, and one of the host's), finally, synchronized and
//! arrays of arrays: tests/java/t/Errors.java, built with the local
//! `javac`. Skips when no `javac` is on the PATH.
use horizon_java::{
    ClassFile,
    compile::compile,
    runtime::{Context, Host, HostObject, Import, Runtime, Value},
};
use std::{path::PathBuf, process::Command, rc::Rc, sync::Arc};
use wasmtime::{Result, bail};

fn javac() -> Option<Vec<ClassFile>> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("horizon-java-errors");
    match Command::new("javac")
        .args(["--release", "21", "-d"])
        .arg(&out)
        .arg(src.join("t/Errors.java"))
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("javac failed: {s}"),
        Err(_) => {
            eprintln!("skipping: no javac");
            return None;
        }
    }
    let mut classes = Vec::new();
    for e in std::fs::read_dir(out.join("t")).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("Errors") && name.ends_with(".class") {
            classes.push(ClassFile::parse(&std::fs::read(e.path()).unwrap()).unwrap());
        }
    }
    Some(classes)
}

/// A JDK exception: its class.
struct Exception(&'static str);

/// The JDK exception classes the test knows, each above the one before.
const ABOVE: [(&str, &str); 3] = [
    (
        "java/lang/IllegalStateException",
        "java/lang/RuntimeException",
    ),
    ("java/lang/RuntimeException", "java/lang/Exception"),
    ("java/lang/Exception", "java/lang/Throwable"),
];

fn is_a(mut class: &str, target: &str) -> bool {
    loop {
        if class == target {
            return true;
        }
        match ABOVE.iter().find(|(c, _)| *c == class) {
            Some((_, up)) => class = up,
            None => return false,
        }
    }
}

struct Jdk;

impl Host for Jdk {
    fn call(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value> {
        let member = format!("{}.{}", import.owner, import.name);
        Ok(match (import.op.as_str(), member.as_str()) {
            ("constant", _) => Value::Null,
            ("new", "java/lang/IllegalStateException.<init>") => {
                Value::Host(Arc::new(Exception("java/lang/IllegalStateException")) as HostObject)
            }
            ("extend", "java/lang/RuntimeException.<init>") => {
                Value::Host(Arc::new(Exception("java/lang/RuntimeException")) as HostObject)
            }
            ("instanceof", _) => {
                let class = match &args[0] {
                    Value::Host(o) => o.downcast_ref::<Exception>().map(|e| e.0.to_owned()),
                    v @ Value::Mod(_) => {
                        // A mod object: its game superclass.
                        let name = cx.class(v)?;
                        let compiled = cx.compiled();
                        name.and_then(|n| {
                            compiled
                                .info
                                .iter()
                                .find(|i| i.name == n)
                                .and_then(|i| i.game_superclass.clone())
                        })
                    }
                    _ => None,
                };
                Value::Int(class.is_some_and(|c| is_a(&c, &import.owner)) as i32)
            }
            _ => bail!("unexpected import {import}"),
        })
    }
}

#[test]
fn exceptions_finally_and_arrays_of_arrays() {
    let Some(classes) = javac() else { return };
    let compiled = compile(&classes).unwrap();
    for (method, why) in &compiled.skipped {
        eprintln!("skipped {method}: {why}");
    }
    assert!(compiled.skipped.is_empty());
    let mut rt = Runtime::new(compiled, Rc::new(Jdk)).unwrap();
    let mut cx = rt.context();
    let mut call =
        |name: &str, x: i32| match cx.call(&format!("t/Errors.{name}(I)I"), &[Value::Int(x)]) {
            Ok(Value::Int(i)) => Ok(i),
            Ok(v) => panic!("{name}: {v:?}"),
            Err(e) => Err(format!("{e:?}")),
        };
    assert_eq!(call("catchMine", 4), Ok(4));
    assert_eq!(call("catchMine", -3), Ok(-30));
    // Not caught there: it leaves the jar as an error.
    assert!(call("catchMine", 0).is_err());
    assert_eq!(call("catchEither", -3), Ok(-1));
    assert_eq!(call("catchEither", 0), Ok(-2));
    assert_eq!(call("withFinally", 5), Ok(1005));
    assert_eq!(call("withFinally", 0), Ok(1100));
    assert_eq!(call("nested", 9), Ok(9));
    assert_eq!(call("nested", -1), Ok(7));
    let mut cx = rt.context();
    let locked = cx
        .call("t/Errors.locked(Ljava/lang/Object;)I", &[Value::Null])
        .unwrap();
    assert!(matches!(locked, Value::Int(5)));
    let grid = cx.call("t/Errors.grid()I", &[]).unwrap();
    assert!(matches!(grid, Value::Int(1347)), "{grid:?}");
}
