//! How far the game API that mods use moved between two game versions
//! (docs/java-compat/compiler.md, version bridge): every game member the
//! mods reference, sorted into unchanged, class renamed or moved (remapped
//! mechanically), member renamed, signature changed, and removed; weighted
//! by references and by mods. Names and descriptors only.
//!
//!     api_diff <old game jar> <new game jar> <mods dir> [--top N]
use horizon_java::{ClassFile, Constant};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
};

#[derive(Default)]
struct Class {
    super_name: Option<String>,
    interfaces: Vec<String>,
    methods: HashSet<(String, String)>,
    fields: HashSet<(String, String)>,
}

type Api = HashMap<String, Class>;

fn read_api(path: &str) -> Result<Api, Box<dyn std::error::Error>> {
    let mut api = Api::new();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        if !e.name().ends_with(".class") || e.name().starts_with("META-INF/") {
            continue;
        }
        let mut data = Vec::new();
        e.read_to_end(&mut data)?;
        let Ok(c) = ClassFile::parse(&data) else {
            continue;
        };
        let name = c.name()?.to_owned();
        api.insert(
            name,
            Class {
                super_name: c.super_name()?.map(str::to_owned),
                interfaces: c
                    .interfaces
                    .iter()
                    .filter_map(|&i| c.class_name(i).ok().map(str::to_owned))
                    .collect(),
                methods: c
                    .methods
                    .iter()
                    .map(|m| (m.name.clone(), m.descriptor.clone()))
                    .collect(),
                fields: c
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.descriptor.clone()))
                    .collect(),
            },
        );
    }
    Ok(api)
}

fn is_game(c: &str) -> bool {
    c.starts_with("net/minecraft/")
        || c.starts_with("com/mojang/blaze3d/")
        || c.starts_with("com/mojang/math/")
}

/// A descriptor with its class names put through `rename`.
fn remap_desc(desc: &str, rename: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = desc;
    while let Some(i) = rest.find('L') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find(';') else { break };
        let class = &rest[i + 1..i + j];
        out.push('L');
        out.push_str(rename.get(class).map_or(class, |s| s.as_str()));
        out.push(';');
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

/// Whether `class` (or a supertype) has the member: `None` when it does
/// not, but a supertype is a library class this tool does not read (the
/// member may be inherited from it, as `Enum.ordinal`).
fn has(api: &Api, class: &str, member: &(String, String), method: bool) -> Option<bool> {
    let mut todo = vec![class.to_owned()];
    let mut seen = HashSet::new();
    let mut library = false;
    while let Some(c) = todo.pop() {
        if !seen.insert(c.clone()) {
            continue;
        }
        let Some(k) = api.get(&c) else {
            library |= !is_game(&c);
            continue;
        };
        if (method && k.methods.contains(member)) || (!method && k.fields.contains(member)) {
            return Some(true);
        }
        todo.extend(k.super_name.clone());
        todo.extend(k.interfaces.iter().cloned());
    }
    (!library).then_some(false)
}

fn simple(c: &str) -> &str {
    c.rsplit('/').next().unwrap_or(c)
}

/// Old classes that are gone, matched to new classes that appeared: by the
/// same simple name, else by the most similar members.
fn class_renames(old: &Api, new: &Api) -> HashMap<String, String> {
    let gone: Vec<&String> = old
        .keys()
        .filter(|c| is_game(c) && !new.contains_key(*c))
        .collect();
    let added: Vec<&String> = new
        .keys()
        .filter(|c| is_game(c) && !old.contains_key(*c))
        .collect();
    let mut by_simple: HashMap<&str, Vec<&String>> = HashMap::new();
    for a in &added {
        by_simple.entry(simple(a)).or_default().push(a);
    }
    let shape = |k: &Class| -> HashSet<String> {
        k.methods
            .iter()
            .map(|(n, d)| format!("{n}/{}", d.matches(';').count() + d.len()))
            .chain(k.fields.iter().map(|(n, _)| format!("f {n}")))
            .collect()
    };
    let mut out = HashMap::new();
    let mut taken = HashSet::new();
    for g in &gone {
        if let Some(v) = by_simple.get(simple(g))
            && v.len() == 1
        {
            out.insert((*g).clone(), v[0].clone());
            taken.insert(v[0].clone());
        }
    }
    for g in &gone {
        if out.contains_key(*g) {
            continue;
        }
        let gs = shape(&old[*g]);
        if gs.len() < 3 {
            continue;
        }
        let mut best: Option<(f64, &String)> = None;
        for a in &added {
            if taken.contains(*a) {
                continue;
            }
            let s = shape(&new[*a]);
            let inter = gs.intersection(&s).count() as f64;
            let j = inter / (gs.len() + s.len()) as f64 * 2.0;
            if j > best.map_or(0.6, |b| b.0) {
                best = Some((j, a));
            }
        }
        if let Some((_, a)) = best {
            out.insert((*g).clone(), a.clone());
            taken.insert(a.clone());
        }
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let top = match args.iter().position(|a| a == "--top") {
        Some(i) => {
            let n = args[i + 1].parse()?;
            args.drain(i..=i + 1);
            n
        }
        None => 15,
    };
    let [old_jar, new_jar, mods] = &args[..] else {
        return Err("usage: api_diff <old game jar> <new game jar> <mods dir>".into());
    };
    let old = read_api(old_jar)?;
    let new = read_api(new_jar)?;
    let renames = class_renames(&old, &new);
    let moved = renames.iter().filter(|(a, b)| a != b).count();
    println!(
        "{} old game classes, {} new; {} gone, {} matched to a new class",
        old.keys().filter(|c| is_game(c)).count(),
        new.keys().filter(|c| is_game(c)).count(),
        old.keys()
            .filter(|c| is_game(c) && !new.contains_key(*c))
            .count(),
        moved
    );
    // What the mods reference: (owner, name, desc, method?) → (refs, mods).
    let mut demand: HashMap<(String, String, String, bool), (usize, HashSet<String>)> =
        HashMap::new();
    for e in std::fs::read_dir(mods)? {
        let path = e?.path();
        if path.extension().is_none_or(|x| x != "jar") {
            continue;
        }
        let modname = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(&path)?) else {
            continue;
        };
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
                let method = match constant {
                    Constant::Methodref(..) | Constant::InterfaceMethodref(..) => true,
                    Constant::Fieldref(..) => false,
                    _ => continue,
                };
                let Ok((o, n, d)) = c.member_ref(k as u16) else {
                    continue;
                };
                if !is_game(o) {
                    continue;
                }
                let v = demand
                    .entry((o.to_owned(), n.to_owned(), d.to_owned(), method))
                    .or_default();
                v.0 += 1;
                v.1.insert(modname.clone());
            }
        }
    }
    // Sort each referenced member.
    let mut cats: BTreeMap<&str, (usize, usize, HashSet<String>, Vec<(usize, String)>)> =
        BTreeMap::new();
    for ((owner, name, desc, method), (refs, mods)) in &demand {
        let owner2 = renames.get(owner).cloned().unwrap_or_else(|| owner.clone());
        let desc2 = remap_desc(desc, &renames);
        let member2 = (name.clone(), desc2.clone());
        let cat = if !old.contains_key(owner) && !new.contains_key(owner) {
            // A class neither version has as such (a mod's own, or synthetic).
            "unknown owner"
        } else if !new.contains_key(&owner2) {
            "class removed"
        } else if has(&new, &owner2, &member2, *method).is_none()
            && has(&old, owner, &(name.clone(), desc.clone()), *method).is_none()
        {
            "inherited from a library"
        } else if has(&new, &owner2, &member2, *method) == Some(true) {
            if owner2 == *owner && desc2 == *desc {
                "unchanged"
            } else {
                "class renamed/moved (auto)"
            }
        } else {
            let k = &new[&owner2];
            let same_name = if *method {
                k.methods.iter().any(|(n, _)| n == name)
            } else {
                k.fields.iter().any(|(n, _)| n == name)
            };
            let same_desc_new_name = if *method {
                k.methods.iter().any(|(n, d)| *d == desc2 && n != name)
            } else {
                k.fields.iter().any(|(n, d)| *d == desc2 && n != name)
            };
            if same_name {
                "signature changed"
            } else if same_desc_new_name {
                "member renamed (candidate)"
            } else {
                "removed"
            }
        };
        let e = cats.entry(cat).or_default();
        e.0 += 1;
        e.1 += refs;
        e.2.extend(mods.iter().cloned());
        e.3.push((*refs, format!("{owner}.{name}{desc}")));
    }
    let total_refs: usize = cats.values().map(|c| c.1).sum();
    println!(
        "\n{:>28} {:>8} {:>9} {:>6}  {:>5}",
        "category", "members", "refs", "%refs", "mods"
    );
    for (cat, (members, refs, mods, _)) in &cats {
        println!(
            "{cat:>28} {members:>8} {refs:>9} {:>5.1}%  {:>5}",
            100.0 * *refs as f64 / total_refs.max(1) as f64,
            mods.len()
        );
    }
    for (cat, (_, _, _, list)) in &mut cats {
        if *cat == "unchanged" || *cat == "unknown owner" {
            continue;
        }
        list.sort_by_key(|x| std::cmp::Reverse(x.0));
        println!("\n{cat}: most referenced");
        for (refs, m) in list.iter().take(top) {
            println!("  {refs:>6}  {m}");
        }
    }
    // The classes behind the changes that need adapters: how few cover most.
    let mut by_class: HashMap<String, usize> = HashMap::new();
    for (cat, (_, _, _, list)) in &cats {
        if matches!(
            *cat,
            "signature changed" | "removed" | "class removed" | "member renamed (candidate)"
        ) {
            for (refs, m) in list {
                let owner = m.split('.').next().unwrap_or(m).to_owned();
                *by_class.entry(owner).or_default() += refs;
            }
        }
    }
    let mut classes: Vec<_> = by_class.into_iter().collect();
    classes.sort_by_key(|x| std::cmp::Reverse(x.1));
    let changed: usize = classes.iter().map(|c| c.1).sum();
    let mut acc = 0;
    let mut marks = vec![50usize, 80, 90, 95];
    println!(
        "\nchanged references by class: {} classes, {changed} refs",
        classes.len()
    );
    for (i, (c, r)) in classes.iter().enumerate() {
        acc += r;
        while let Some(&m) = marks.first() {
            if acc * 100 >= changed * m {
                println!("  {m}% of changed refs in the first {} classes", i + 1);
                marks.remove(0);
            } else {
                break;
            }
        }
        if i < top {
            println!("    {r:>6}  {c}");
        }
    }
    let mut sample: Vec<_> = renames.iter().filter(|(a, b)| a != b).collect();
    sample.sort();
    println!("\nclass renames/moves (first {top}):");
    for (a, b) in sample.into_iter().take(top) {
        println!("  {a} -> {b}");
    }
    Ok(())
}
