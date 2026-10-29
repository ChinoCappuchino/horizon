//! Classes that extend a game class, virtual calls, interfaces, records,
//! enums, arrays, instanceof and string concatenation:
//! tests/java/t/Shapes.java, built with the local `javac`, compiled and run
//! against a host that plays the game class tests/java/g/Block.java (which
//! is not compiled) and the JDK. Skips when no `javac` is on the PATH.
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

/// Builds the test sources; the classes of `t/Shapes` (and its nested
/// classes) are the jar, `g/Block` stays the game's.
fn javac() -> Option<Vec<ClassFile>> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("horizon-java-extend");
    let status = Command::new("javac")
        .args(["--release", "21", "-d"])
        .arg(&out)
        .arg(src.join("g/Block.java"))
        .arg(src.join("t/Shapes.java"))
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
    for e in std::fs::read_dir(out.join("t")).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("Shapes") && name.ends_with(".class") {
            classes.push(ClassFile::parse(&std::fs::read(e.path()).unwrap()).unwrap());
        }
    }
    Some(classes)
}

enum Obj {
    Str(String),
    Block(Mutex<i32>),
    Enum(#[allow(dead_code)] String, i32),
    Facing,
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
    match v {
        Value::Int(i) => *i,
        _ => panic!("not an int: {v:?}"),
    }
}

/// A double as Java prints it.
fn java_double(d: f64) -> String {
    if d.fract() == 0.0 && d.is_finite() {
        format!("{d:.1}")
    } else {
        format!("{d}")
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Double(d) => java_double(*d),
        _ => string(v),
    }
}

struct Game {
    facing: Value,
    strings: Mutex<Vec<(String, Value)>>,
}

impl Game {
    /// A record's field names, and its values.
    fn fields(cx: &mut Context<'_>, class: &str, v: &Value) -> Result<Vec<(String, Value)>> {
        let compiled = cx.compiled();
        let Some(info) = compiled.info.iter().find(|i| i.name == class) else {
            bail!("no class {class}");
        };
        let mut out = Vec::new();
        for (i, f) in info.fields.iter().enumerate() {
            out.push((f.clone(), cx.field(v, i + 2)?));
        }
        Ok(out)
    }
}

impl Host for Game {
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
            ("extend", "g/Block.<init>") => host(Obj::Block(Mutex::new(int(&args[1])))),
            ("extend", "java/lang/Enum.<init>") => host(Obj::Enum(string(&args[1]), int(&args[2]))),
            ("getfield", "g/Block.light") => match obj(&cx.peer(&args[0])?) {
                Some(Obj::Block(light)) => Value::Int(*light.lock().unwrap()),
                _ => bail!("not a block"),
            },
            ("getstatic", "g/Block.FACING") => self.facing.clone(),
            // Block.use, the game's own (`super.use(x)`).
            ("invokespecial", "g/Block.use") => args[1].clone(),
            ("invokevirtual", "java/lang/Enum.ordinal") => match obj(&cx.peer(&args[0])?) {
                Some(Obj::Enum(_, ordinal)) => Value::Int(*ordinal),
                _ => bail!("not an enum"),
            },
            ("concat", _) => {
                let mut args = args.iter();
                let mut out = String::new();
                for c in import.name.chars() {
                    match c {
                        '\u{1}' => out.push_str(&text(args.next().unwrap())),
                        c => out.push(c),
                    }
                }
                host(Obj::Str(out))
            }
            ("record", m) if m.ends_with(".toString") => {
                let simple = import.owner.rsplit(['/', '$']).next().unwrap();
                let fields: Vec<String> = Self::fields(cx, &import.owner, &args[0])?
                    .into_iter()
                    .map(|(n, v)| format!("{n}={}", text(&v)))
                    .collect();
                host(Obj::Str(format!("{simple}[{}]", fields.join(", "))))
            }
            ("record", m) if m.ends_with(".equals") => {
                let same = cx.id(&args[0])? == cx.id(&args[1])? && {
                    let a = Self::fields(cx, &import.owner, &args[0])?;
                    let b = Self::fields(cx, &import.owner, &args[1])?;
                    a.iter()
                        .zip(&b)
                        .all(|((_, x), (_, y))| format!("{x:?}") == format!("{y:?}"))
                };
                Value::Int(same as i32)
            }
            _ => bail!("unexpected import {import}"),
        })
    }
}

#[test]
fn game_classes_virtual_calls_arrays_and_records() {
    let Some(classes) = javac() else { return };
    let compiled = compile(&classes).unwrap();
    for (method, why) in &compiled.skipped {
        eprintln!("skipped {method}: {why}");
    }
    assert!(compiled.skipped.is_empty());
    let lamp_info = compiled
        .info
        .iter()
        .find(|i| i.name == "t/Shapes$Lamp")
        .unwrap();
    assert_eq!(lamp_info.game_superclass.as_deref(), Some("g/Block"));
    assert_eq!(lamp_info.fields, ["uses"]);
    let facing = host(Obj::Facing);
    let game = Game {
        facing: facing.clone(),
        strings: Mutex::new(Vec::new()),
    };
    let mut rt = Runtime::new(compiled, Rc::new(game)).unwrap();
    let mut cx = rt.context();
    let call = |cx: &mut Context<'_>, name: &str, args: &[Value]| {
        cx.call(&format!("t/Shapes.{name}"), args)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"))
    };

    // Virtual calls: a record, an abstract class and its subclasses, a
    // default method.
    assert!(matches!(call(&mut cx, "total()D", &[]), Value::Double(d) if d == 47.0));
    assert_eq!(
        string(&call(&mut cx, "kinds()Ljava/lang/String;", &[])),
        "square,shape"
    );
    // An enum: its constants' peers are the JDK's Enum; values() clones.
    assert_eq!(int(&call(&mut cx, "colors()I", &[])), 32);
    // instanceof an interface of the jar.
    let square = call(&mut cx, "square()Ljava/lang/Object;", &[]);
    assert_eq!(
        int(&call(&mut cx, "isShape(Ljava/lang/Object;)Z", &[square])),
        1
    );
    let other = host(Obj::Str("no".into()));
    assert_eq!(
        int(&call(&mut cx, "isShape(Ljava/lang/Object;)Z", &[other])),
        0
    );
    // Arrays of every width.
    assert_eq!(int(&call(&mut cx, "arrays()I", &[])), 158);
    // A class that extends the game's: the game's field through the peer,
    // `super.use`, and the game calling the override back.
    assert_eq!(int(&call(&mut cx, "lamp()I", &[])), 108);
    let lamp = call(&mut cx, "makeLamp()Lg/Block;", &[]);
    assert!(matches!(obj(&cx.peer(&lamp).unwrap()), Some(Obj::Block(_))));
    let used = cx
        .call_virtual(&lamp, "use", "(I)I", &[Value::Int(1)])
        .unwrap()
        .unwrap();
    assert_eq!(int(&used) * 10, 1080);
    assert_eq!(int(&cx.field(&lamp, 2).unwrap()), 1);
    // A static field the class inherits from the game.
    let f = call(&mut cx, "facing()Ljava/lang/Object;", &[]);
    assert!(cx.same(&f, &facing).unwrap());
    // Records' methods and string concatenation.
    assert_eq!(
        string(&call(&mut cx, "squareText()Ljava/lang/String;", &[])),
        "Square[side=3.0]"
    );
    assert_eq!(int(&call(&mut cx, "squaresEqual()Z", &[])), 1);
    let t = call(
        &mut cx,
        "text(ILjava/lang/String;D)Ljava/lang/String;",
        &[
            Value::Int(5),
            host(Obj::Str("x".into())),
            Value::Double(2.5),
        ],
    );
    assert_eq!(string(&t), "i=5 s=x d=2.5");
    // A pattern switch: null, a guard (which restarts the switch), class
    // patterns, default.
    let pattern = "pattern(Ljava/lang/Object;)I";
    let big = call(&mut cx, "bigSquare()Ljava/lang/Object;", &[]);
    let small = call(&mut cx, "square()Ljava/lang/Object;", &[]);
    let tri = call(&mut cx, "tri()Ljava/lang/Object;", &[]);
    assert_eq!(int(&call(&mut cx, pattern, &[big])), 50);
    assert_eq!(int(&call(&mut cx, pattern, &[small])), 5);
    assert_eq!(int(&call(&mut cx, pattern, &[tri])), 3);
    assert_eq!(int(&call(&mut cx, pattern, &[Value::Null])), -1);
    assert_eq!(int(&call(&mut cx, pattern, &[host(Obj::Facing)])), 0);
}
