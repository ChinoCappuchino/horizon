//! Objects, strings and lambdas across the host boundary: tests/java/t/
//! Objects.java, built with the local `javac`, compiled and run against a
//! small host that plays the JDK. Skips when no `javac` is on the PATH.
use horizon_java::{
    ClassFile,
    compile::{HostConstant, compile},
    runtime::{Context, Host, HostObject, Import, Runtime, Value},
};
use std::{
    path::PathBuf,
    process::Command,
    rc::Rc,
    sync::{Arc, Mutex},
};
use wasmtime::{Result, bail};

fn javac(names: &[&str]) -> Option<Vec<ClassFile>> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("horizon-java-runtime");
    let files: Vec<PathBuf> = names
        .iter()
        .map(|n| src.join(format!("{n}.java")))
        .collect();
    match Command::new("javac")
        .args(["--release", "21", "-d"])
        .arg(&out)
        .args(&files)
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
    for n in names {
        classes.push(
            ClassFile::parse(&std::fs::read(out.join(format!("{n}.class"))).unwrap()).unwrap(),
        );
    }
    Some(classes)
}

enum Obj {
    Str(String),
    Int(i32),
    List(Mutex<Vec<String>>),
}

fn host(o: Obj) -> Value {
    Value::Host(Arc::new(o) as HostObject)
}

fn obj(v: &Value) -> Option<&Obj> {
    match v {
        Value::Host(o) => o.downcast_ref::<Obj>(),
        _ => None,
    }
}

fn string(v: &Value) -> String {
    match obj(v) {
        Some(Obj::Str(s)) => s.clone(),
        _ => panic!("not a string: {v:?}"),
    }
}

fn int(v: &Value) -> i32 {
    match (v, obj(v)) {
        (Value::Int(i), _) | (_, Some(Obj::Int(i))) => *i,
        _ => panic!("not an int: {v:?}"),
    }
}

/// The JDK, as far as the test class needs it. Strings are interned.
struct Jdk {
    strings: Mutex<Vec<(String, Value)>>,
}

impl Host for Jdk {
    fn call(&self, cx: &mut Context<'_>, import: &Import, args: &[Value]) -> Result<Value> {
        let member = format!("{}.{}", import.owner, import.name);
        Ok(match (import.op.as_str(), member.as_str()) {
            ("constant", _) => {
                let Some(HostConstant::String(s)) = cx.constant(int(&args[0])) else {
                    bail!("not a string constant");
                };
                let mut strings = self.strings.lock().unwrap();
                if let Some((_, v)) = strings.iter().find(|(k, _)| *k == s) {
                    return Ok(v.clone());
                }
                let v = host(Obj::Str(s.clone()));
                strings.push((s, v.clone()));
                v
            }
            ("same", _) => Value::Int(cx.same(&args[0], &args[1])? as i32),
            ("invokevirtual", "java/lang/String.concat") => {
                host(Obj::Str(string(&args[0]) + &string(&args[1])))
            }
            ("invokestatic", "java/lang/Integer.valueOf") => host(Obj::Int(int(&args[0]))),
            ("invokevirtual", "java/lang/Integer.intValue") => Value::Int(int(&args[0])),
            ("new", "java/util/ArrayList.<init>") => host(Obj::List(Mutex::new(Vec::new()))),
            ("invokeinterface", "java/util/List.add") => {
                let Some(Obj::List(l)) = obj(&args[0]) else {
                    bail!("not a list")
                };
                l.lock().unwrap().push(string(&args[1]));
                Value::Int(1)
            }
            // A functional interface on a lambda of the jar: call it back.
            ("invokeinterface", "java/util/function/IntUnaryOperator.applyAsInt")
            | ("invokeinterface", "java/util/function/Supplier.get") => {
                cx.call_lambda(&args[0], &args[1..])?
            }
            _ => bail!("unexpected import {import}"),
        })
    }
}

#[test]
fn objects_strings_and_lambdas_cross_the_host_boundary() {
    let Some(classes) = javac(&["t/Objects"]) else {
        return;
    };
    let compiled = compile(&classes).unwrap();
    for (method, why) in &compiled.skipped {
        eprintln!("skipped {method}: {why}");
    }
    assert!(compiled.skipped.is_empty());
    let mut rt = Runtime::new(
        compiled,
        Rc::new(Jdk {
            strings: Mutex::new(Vec::new()),
        }),
    )
    .unwrap();
    let mut cx = rt.context();
    let call = |cx: &mut Context<'_>, name: &str, args: &[Value]| {
        cx.call(&format!("t/Objects.{name}"), args)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"))
    };

    // Objects of the jar: a constructor, fields, methods.
    assert_eq!(int(&call(&mut cx, "make()I", &[])), 207);
    assert_eq!(
        string(&call(&mut cx, "nameOf()Ljava/lang/String;", &[])),
        "named"
    );
    // Strings: a constant is the same object each time.
    let a = call(&mut cx, "greet()Ljava/lang/String;", &[]);
    let b = call(&mut cx, "greet()Ljava/lang/String;", &[]);
    assert_eq!(string(&a), "hello");
    assert!(cx.same(&a, &b).unwrap());
    assert_eq!(
        int(&call(
            &mut cx,
            "same(Ljava/lang/Object;Ljava/lang/Object;)Z",
            &[a.clone(), b]
        )),
        1
    );
    let other = host(Obj::Str("hello".into()));
    assert_eq!(
        int(&call(
            &mut cx,
            "same(Ljava/lang/Object;Ljava/lang/Object;)Z",
            &[a, other]
        )),
        0
    );
    // The host's objects: a list the jar fills.
    let list = call(&mut cx, "list()Ljava/util/List;", &[]);
    let Some(Obj::List(l)) = obj(&list) else {
        panic!("not a list")
    };
    assert_eq!(*l.lock().unwrap(), ["a", "hello"]);
    // Lambdas: called by the jar through the host, and by the host.
    assert_eq!(int(&call(&mut cx, "useLambda(I)I", &[Value::Int(2)])), 12);
    let inc = call(&mut cx, "boxed()Ljava/util/function/Function;", &[]);
    assert_eq!(cx.class(&inc).unwrap(), None);
    assert_eq!(
        int(&cx.call_lambda(&inc, &[host(Obj::Int(41))]).unwrap()),
        42
    );
    // A method reference to an int method, boxed for Supplier<Integer>.
    let seven = call(&mut cx, "seven()Ljava/util/function/Supplier;", &[]);
    assert_eq!(int(&cx.call_lambda(&seven, &[]).unwrap()), 7);
    // A captured host object.
    let e = call(
        &mut cx,
        "exclaim(Ljava/lang/String;)Ljava/util/function/Supplier;",
        &[host(Obj::Str("hi".into()))],
    );
    assert_eq!(string(&cx.call_lambda(&e, &[]).unwrap()), "hi!");
    // A lambda that makes an object of the jar, passed back in.
    let maker = call(&mut cx, "maker()Ljava/util/function/Supplier;", &[]);
    let made = cx.call_lambda(&maker, &[]).unwrap();
    assert_eq!(cx.class(&made).unwrap().as_deref(), Some("t/Objects"));
    assert_eq!(
        string(&call(
            &mut cx,
            "madeName(Ljava/util/function/Supplier;)Ljava/lang/String;",
            &[maker]
        )),
        "made"
    );
}
