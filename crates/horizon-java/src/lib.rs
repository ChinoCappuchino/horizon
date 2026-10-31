//! Java mod compatibility (fork `NeuHorizon`, docs/java-compat/README.md):
//! the compiler's front end. It reads JVM class files (`classfile`) and
//! decodes their bytecode (`bytecode`), from any Java version mods were built
//! with (Java 6 for Minecraft 1.7.10 to Java 21 for 1.21.1).
//!
//! Mod code is processed, never read by a person or an agent: this crate is
//! the clean-room boundary.
pub mod bytecode;
pub mod cfg;
pub mod classfile;
pub mod compile;
pub mod descriptor;
#[cfg(feature = "native")]
pub mod heap;
#[cfg(feature = "native")]
pub mod native;
#[cfg(feature = "native")]
pub mod rt;
#[cfg(feature = "runtime")]
pub mod runtime;
pub mod types;

pub use bytecode::{Instruction, decode};
pub use classfile::{ClassFile, Constant, Error};
