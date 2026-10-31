//! What a mod jar needs from the compiler and the compatibility layer
//! (docs/java-compat/AGENT.md, step 1): its instructions, its invokedynamic
//! bootstraps, the JDK members it calls, and its classes with their
//! superclasses and annotations. Structure only: no code is read.
//!
//!     inspect <mod.jar>
use horizon_java::{
    ClassFile,
    bytecode::{Operand, decode, name},
    classfile::Constant,
};
use std::{collections::BTreeMap, io::Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: inspect <mod.jar>")?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path)?)?;
    let mut opcodes = BTreeMap::<&str, usize>::new();
    let mut bootstraps = BTreeMap::<String, usize>::new();
    let mut jdk = BTreeMap::<String, usize>::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if !entry.name().ends_with(".class") {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        let class = ClassFile::parse(&data)?;
        let annotations: Vec<&str> = class
            .annotations()
            .map(|a| a.type_descriptor.as_str())
            .collect();
        println!(
            "class {} extends {} (Java class file {}) {:?}",
            class.name()?,
            class.super_name()?.unwrap_or("-"),
            class.major,
            annotations
        );
        for m in &class.methods {
            let Some(code) = m.code() else { continue };
            for insn in decode(&code.code)? {
                *opcodes.entry(name(insn.opcode)).or_default() += 1;
                if let Operand::Constant(index) | Operand::Interface(index, _) = insn.operand {
                    if insn.opcode == 0xBA {
                        if let Some(Constant::InvokeDynamic(bsm, _)) =
                            class.constants.get(usize::from(index))
                        {
                            let handle =
                                &class.bootstrap_methods()[usize::from(*bsm)].method_handle;
                            if let Some(Constant::MethodHandle(_, r)) =
                                class.constants.get(usize::from(*handle))
                            {
                                let (owner, method, _) = class.member_ref(*r)?;
                                *bootstraps.entry(format!("{owner}.{method}")).or_default() += 1;
                            }
                        }
                    } else if let Ok((owner, member, descriptor)) = class.member_ref(index)
                        && owner.starts_with("java/")
                    {
                        *jdk.entry(format!("{owner}.{member}{descriptor}"))
                            .or_default() += 1;
                    }
                }
            }
        }
    }
    println!("\ninstructions ({} kinds): {opcodes:?}", opcodes.len());
    println!("\ninvokedynamic bootstraps: {bootstraps:?}");
    println!("\nJDK members ({}):", jdk.len());
    for (m, n) in &jdk {
        println!("  {n:3}  {m}");
    }
    Ok(())
}
