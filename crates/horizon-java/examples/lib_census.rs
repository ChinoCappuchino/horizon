//! Which library members (JDK, Guava, fastutil, DataFixerUpper, Gson...)
//! mods use, by how many mods use each: the candidates for Rust versions
//! (docs/java-compat/compiler.md, library methods). Constant pools only.
//!
//!     lib_census <mods dir> [--top N]
use horizon_java::{ClassFile, Constant};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
};

const LIBRARIES: [&str; 8] = [
    "java/",
    "com/google/common/",
    "it/unimi/dsi/fastutil/",
    "com/mojang/serialization/",
    "com/mojang/datafixers/",
    "com/mojang/brigadier/",
    "com/google/gson/",
    "org/apache/commons/",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let top = match args.iter().position(|a| a == "--top") {
        Some(i) => {
            let n = args[i + 1].parse()?;
            args.drain(i..=i + 1);
            n
        }
        None => 40,
    };
    let dir = args.first().ok_or("usage: lib_census <mods dir>")?;
    let mut methods: HashMap<String, (usize, usize)> = HashMap::new();
    let mut classes: HashMap<String, (usize, usize)> = HashMap::new();
    let mut mods = 0;
    for e in std::fs::read_dir(dir)? {
        let path = e?.path();
        if path.extension().is_none_or(|x| x != "jar") {
            continue;
        }
        let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(&path)?) else {
            continue;
        };
        mods += 1;
        let (mut seen_m, mut seen_c) = (HashSet::new(), HashSet::new());
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if !entry.name().ends_with(".class") || entry.name().starts_with("META-INF/") {
                continue;
            }
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            let Ok(c) = ClassFile::parse(&data) else {
                continue;
            };
            for (k, constant) in c.constants.iter().enumerate() {
                if !matches!(
                    constant,
                    Constant::Methodref(..) | Constant::InterfaceMethodref(..)
                ) {
                    continue;
                }
                let Ok((owner, name, desc)) = c.member_ref(k as u16) else {
                    continue;
                };
                if !LIBRARIES.iter().any(|l| owner.starts_with(l)) {
                    continue;
                }
                let m = format!("{owner}.{name}{desc}");
                let entry = methods.entry(m.clone()).or_default();
                entry.1 += 1;
                if seen_m.insert(m) {
                    entry.0 += 1;
                }
                let ce = classes.entry(owner.to_owned()).or_default();
                ce.1 += 1;
                if seen_c.insert(owner.to_owned()) {
                    ce.0 += 1;
                }
            }
        }
    }
    let show = |title: &str, map: &HashMap<String, (usize, usize)>| {
        let mut v: Vec<_> = map.iter().collect();
        v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(b.1.1.cmp(&a.1.1)));
        println!(
            "{title} ({} distinct; {mods} mods)\n{:>5} {:>6}",
            map.len(),
            "mods",
            "refs"
        );
        for (name, (m, r)) in v.into_iter().take(top) {
            println!("{m:>5} {r:>6}  {name}");
        }
    };
    show("library classes", &classes);
    show("library methods", &methods);
    Ok(())
}
