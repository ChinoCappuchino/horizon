//! Which classes code allocates most, and whether anything depends on
//! their identity (docs/java-compat/compiler.md, memory plan): `new` sites
//! per class, and per class the identity uses the typed front end sees
//! (`==`/`!=` on two references of it, `synchronized` on it). Structure
//! only: counts by class name, no code.
//!
//!     alloc_census <jar>... [--top N]
use horizon_java::{
    ClassFile, decode,
    types::{Classes, Ty, infer},
};
use std::{collections::HashMap, io::Read};

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
    let mut classes = Vec::new();
    for path in &args {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.name().ends_with(".class") && !entry.name().starts_with("META-INF/") {
                let mut data = Vec::new();
                entry.read_to_end(&mut data)?;
                classes.push(ClassFile::parse(&data)?);
            }
        }
    }
    let h = Classes::new(&classes);
    let mut news: HashMap<String, usize> = HashMap::new();
    let mut identity: HashMap<String, usize> = HashMap::new();
    // HORIZON_CENSUS_WHERE=Class,...: where the identity uses of those are.
    let watch: Vec<String> = std::env::var("HORIZON_CENSUS_WHERE")
        .map(|v| v.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    for c in &classes {
        for m in &c.methods {
            let Some(code) = m.code() else { continue };
            let insns = decode(&code.code)?;
            for x in &insns {
                if x.opcode == 0xBB
                    && let horizon_java::bytecode::Operand::Constant(i) = x.operand
                {
                    *news.entry(c.class_name(i)?.to_owned()).or_default() += 1;
                }
            }
            let Ok(frames) = infer(&h, c, m, &insns) else {
                continue;
            };
            for (x, f) in insns.iter().zip(&frames) {
                let Some(f) = f else { continue };
                let d = f.stack.len();
                let class = |t: &Ty| match t {
                    Ty::Ref(n) => Some(n.to_string()),
                    _ => None,
                };
                match x.opcode {
                    // if_acmpeq / if_acmpne: both sides of one class.
                    0xA5 | 0xA6 if d >= 2 => {
                        for t in [&f.stack[d - 1], &f.stack[d - 2]] {
                            if let Some(n) = class(t) {
                                if watch.contains(&n) {
                                    println!(
                                        "identity of {n} in {}.{}{}",
                                        c.name()?,
                                        m.name,
                                        m.descriptor
                                    );
                                }
                                *identity.entry(n).or_default() += 1;
                            }
                        }
                    }
                    // monitorenter.
                    0xC2 if d >= 1 => {
                        if let Some(n) = class(&f.stack[d - 1]) {
                            *identity.entry(n).or_default() += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut sorted: Vec<(&String, &usize)> = news.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    println!("{:>7}  {:>8}  class", "new", "identity");
    for (class, n) in sorted.into_iter().take(top) {
        println!(
            "{n:>7}  {:>8}  {class}",
            identity.get(class).copied().unwrap_or(0)
        );
    }
    Ok(())
}
