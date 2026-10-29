//! Library survey (council survey "library"): every reference from app code
//! (mods or vanilla) to a library member (JDK or third-party), counted per
//! jar as call sites. Structure only: opcodes, constant pools, descriptors
//! and bootstrap handles, by name and count; no code is printed.
//!
//!     survey_lib <out.tsv> <jar or dir>...
//!
//! Output rows: jar, kind, owner, name, descriptor, sites. Kinds: the
//! invoke/field/type opcodes by name; `indy` (owner.name = bootstrap,
//! descriptor = call-site name); `lambda-iface` (owner = functional
//! interface, name = its method, descriptor = erased method type);
//! `mref` (a lambda's implementation handle when it is a library member);
//! `condy`; `super`/`implements` (library supertypes of app classes);
//! `objproto` (Object/Enum protocol called on a non-library receiver);
//! `shaded-class` (classes under a library prefix inside an app jar, not
//! scanned). A final `#stats` row per jar holds class/method/code counts.
use horizon_java::{
    ClassFile, Constant,
    bytecode::{Operand, name as opname},
    decode,
};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::PathBuf,
};

/// Libraries by package prefix, wherever their classes are defined.
const LIB_PREFIXES: &[&str] = &[
    "java/",
    "javax/",
    "jdk/",
    "sun/",
    "com/sun/",
    "com/google/",
    "it/unimi/dsi/fastutil/",
    "com/mojang/serialization/",
    "com/mojang/datafixers/",
    "com/mojang/brigadier/",
    "com/mojang/authlib/",
    "com/mojang/logging/",
    "com/mojang/jtracy/",
    "com/mojang/bridge/",
    "com/mojang/text2speech/",
    "org/apache/",
    "io/netty/",
    "org/slf4j/",
    "org/joml/",
    "org/lwjgl/",
    "org/jetbrains/",
    "org/jspecify/",
    "org/intellij/",
    "kotlin/",
    "kotlinx/",
    "org/spongepowered/",
    "com/llamalad7/",
    "org/objectweb/asm/",
    "com/electronwill/",
    "oshi/",
    "com/sun/jna/",
    "net/jodah/",
    "org/yaml/",
    "com/github/benmanes/",
    "org/tukaani/",
    "org/sqlite/",
    "io/github/classgraph/",
    "joptsimple/",
    "net/jpountz/",
    "org/lz4/",
    "com/azure/",
    "com/microsoft/",
    "org/checkerframework/",
    "javassist/",
    "org/openjdk/",
    "com/fasterxml/",
    "org/antlr/",
    "org/mozilla/javascript/",
    "dev/latvian/mods/rhino/",
];
/// The game and loader: not library, not counted.
const GAME_PREFIXES: &[&str] = &[
    "net/minecraft/",
    "com/mojang/blaze3d/",
    "com/mojang/math/",
    "com/mojang/realmsclient/",
    "net/neoforged/",
    "cpw/mods/",
];

fn is_lib_prefix(c: &str) -> bool {
    LIB_PREFIXES.iter().any(|p| c.starts_with(p))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (out, inputs) = args
        .split_first()
        .ok_or("usage: survey_lib <out.tsv> <jar|dir>...")?;
    let mut jars: Vec<PathBuf> = Vec::new();
    for a in inputs {
        let p = PathBuf::from(a);
        if p.is_dir() {
            let mut v: Vec<_> = std::fs::read_dir(&p)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jar"))
                .collect();
            v.sort();
            jars.extend(v);
        } else {
            jars.push(p);
        }
    }
    // Pass 1: parse every class of every jar.
    let mut parsed: Vec<(String, Vec<ClassFile>, usize)> = Vec::new();
    let mut defined: HashSet<String> = HashSet::new();
    for path in &jars {
        let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(path)?) else {
            continue;
        };
        let jar = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut classes = Vec::new();
        let mut bad = 0;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if !entry.name().ends_with(".class") || entry.name().starts_with("META-INF/") {
                continue;
            }
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            match ClassFile::parse(&data) {
                Ok(c) => {
                    if let Ok(n) = c.name() {
                        defined.insert(n.to_owned());
                    }
                    classes.push(c);
                }
                Err(_) => bad += 1,
            }
        }
        parsed.push((jar, classes, bad));
    }
    let is_lib = |c: &str| -> bool {
        if c.starts_with('[') {
            return true;
        }
        if is_lib_prefix(c) {
            return true;
        }
        if GAME_PREFIXES.iter().any(|p| c.starts_with(p)) {
            return false;
        }
        !defined.contains(c)
    };
    let mut w = std::io::BufWriter::new(std::fs::File::create(out)?);
    for (jar, classes, bad) in &parsed {
        let mut rows: HashMap<(String, String, String, String), usize> = HashMap::new();
        let mut add = |k: &str, o: &str, n: &str, d: &str| {
            *rows
                .entry((k.to_owned(), o.to_owned(), n.to_owned(), d.to_owned()))
                .or_default() += 1;
        };
        let (mut nclass, mut nmeth, mut ncode, mut shaded, mut decode_err) = (0, 0, 0, 0, 0);
        for c in classes {
            let Ok(cname) = c.name() else { continue };
            if is_lib_prefix(cname) {
                shaded += 1;
                let top: Vec<&str> = cname.split('/').take(2).collect();
                add("shaded-class", &top.join("/"), "", "");
                continue;
            }
            nclass += 1;
            if let Ok(Some(s)) = c.super_name()
                && s != "java/lang/Object"
                && is_lib(s)
            {
                add("super", s, "", "");
            }
            for &i in &c.interfaces {
                if let Ok(n) = c.class_name(i)
                    && is_lib(n)
                {
                    add("implements", n, "", "");
                }
            }
            let bsms = c.bootstrap_methods();
            let handle = |idx: u16| -> Option<(u8, String, String, String)> {
                match c.constants.get(usize::from(idx)) {
                    Some(Constant::MethodHandle(kind, r)) => {
                        let (o, n, d) = c.member_ref(*r).ok()?;
                        Some((*kind, o.to_owned(), n.to_owned(), d.to_owned()))
                    }
                    _ => None,
                }
            };
            for m in &c.methods {
                nmeth += 1;
                let Some(code) = m.code() else { continue };
                ncode += 1;
                let Ok(insns) = decode(&code.code) else {
                    decode_err += 1;
                    continue;
                };
                for x in &insns {
                    let idx = match x.operand {
                        Operand::Constant(i)
                        | Operand::Interface(i, _)
                        | Operand::Dimensions(i, _) => i,
                        _ => continue,
                    };
                    let op = x.opcode;
                    match op {
                        // get/put field/static, invoke virtual/special/static/interface
                        0xB2..=0xB9 => {
                            let Ok((o, n, d)) = c.member_ref(idx) else {
                                continue;
                            };
                            if is_lib(o) {
                                add(opname(op), o, n, d);
                            } else if (0xB6..=0xB9).contains(&op) {
                                let proto = matches!(
                                    (n, d),
                                    ("hashCode", "()I")
                                        | ("equals", "(Ljava/lang/Object;)Z")
                                        | ("toString", "()Ljava/lang/String;")
                                        | ("getClass", "()Ljava/lang/Class;")
                                        | ("ordinal", "()I")
                                        | ("name", "()Ljava/lang/String;")
                                        | ("compareTo", "(Ljava/lang/Enum;)I")
                                        | ("clone", "()Ljava/lang/Object;")
                                );
                                if proto {
                                    add("objproto", "(app)", n, d);
                                }
                            }
                        }
                        // new, anewarray, checkcast, instanceof, multianewarray
                        0xBB | 0xBD | 0xC0 | 0xC1 | 0xC5 => {
                            let Ok(o) = c.class_name(idx) else { continue };
                            if is_lib(o) {
                                add(opname(op), o, "", "");
                            }
                        }
                        // ldc of a dynamic constant / method handle / type
                        0x12..=0x14 => match c.constants.get(usize::from(idx)) {
                            Some(Constant::Dynamic(b, nat)) => {
                                let (bo, bn) = bsms
                                    .get(usize::from(*b))
                                    .and_then(|b| handle(b.method_handle))
                                    .map(|h| (h.1, h.2))
                                    .unwrap_or_default();
                                let n = c.name_and_type(*nat).map(|x| x.0).unwrap_or("");
                                add("condy", &bo, &bn, n);
                            }
                            Some(Constant::MethodHandle(..)) => {
                                if let Some(h) = handle(idx) {
                                    add("ldc-handle", &h.1, &h.2, &h.3);
                                }
                            }
                            Some(Constant::MethodType(_)) => add("ldc-type", "", "", ""),
                            _ => {}
                        },
                        0xBA => {
                            let Some(Constant::InvokeDynamic(b, nat)) =
                                c.constants.get(usize::from(idx))
                            else {
                                continue;
                            };
                            let (iname, idesc) = c.name_and_type(*nat).unwrap_or(("", ""));
                            let Some(bm) = bsms.get(usize::from(*b)) else {
                                continue;
                            };
                            let Some((_, bo, bn, _)) = handle(bm.method_handle) else {
                                continue;
                            };
                            add("indy", &bo, &bn, iname);
                            if bo == "java/lang/invoke/LambdaMetafactory" {
                                let iface = idesc
                                    .rsplit(')')
                                    .next()
                                    .unwrap_or("")
                                    .trim_start_matches('L')
                                    .trim_end_matches(';');
                                let sam = bm
                                    .arguments
                                    .first()
                                    .and_then(|&a| match c.constants.get(usize::from(a)) {
                                        Some(Constant::MethodType(d)) => c.utf8(*d).ok(),
                                        _ => None,
                                    })
                                    .unwrap_or("");
                                add("lambda-iface", iface, iname, sam);
                                if let Some((k, o, n, d)) =
                                    bm.arguments.get(1).and_then(|&a| handle(a))
                                {
                                    if is_lib(&o) {
                                        add("mref", &o, &n, &d);
                                    } else {
                                        // capturing-arity bucket for app lambdas
                                        let cap = desc_args(idesc);
                                        add(
                                            "lambda-impl",
                                            "(app)",
                                            &format!("kind{k}"),
                                            &format!("cap{cap}"),
                                        );
                                    }
                                }
                            } else if bo == "java/lang/invoke/StringConcatFactory" {
                                add("concat-args", "", "", &format!("{}", desc_args(idesc)));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        for ((k, o, n, d), s) in rows {
            writeln!(w, "{jar}\t{k}\t{o}\t{n}\t{d}\t{s}")?;
        }
        writeln!(
            w,
            "{jar}\t#stats\tclasses={nclass} methods={nmeth} code={ncode} shaded={shaded} badclass={bad} decode_err={decode_err}\t\t\t0"
        )?;
    }
    Ok(())
}

/// Number of arguments in a method descriptor.
fn desc_args(d: &str) -> usize {
    let inner = d.trim_start_matches('(').split(')').next().unwrap_or("");
    let b = inner.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < b.len() {
        while b[i] == b'[' {
            i += 1;
        }
        if b[i] == b'L' {
            while b[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        n += 1;
    }
    n
}
