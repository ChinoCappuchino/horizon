//! JVM bytecode decoding (JVMS chapter 6): a method's code as instructions,
//! with branch targets as absolute positions in the code.

use crate::classfile::{Error, Result};

/// One instruction at a position of the code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub at: u32,
    pub opcode: u8,
    pub operand: Operand,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    None,
    /// `bipush`, `sipush`: the pushed value.
    Value(i32),
    /// Loads, stores and `ret`: the local variable (two bytes after `wide`).
    Local(u16),
    /// `iinc`: the local and the increment.
    Increment(u16, i32),
    /// `ldc` and every instruction naming a constant: its pool index.
    Constant(u16),
    /// `invokeinterface`: the method and its argument count.
    Interface(u16, u8),
    /// `multianewarray`: the class and its dimensions.
    Dimensions(u16, u8),
    /// `newarray`: the primitive type (4 boolean to 11 long).
    ArrayType(u8),
    /// Branches: the target's position.
    Target(u32),
    /// `tableswitch`: the default, the lowest key, and a target per key.
    Table {
        default: u32,
        low: i32,
        targets: Vec<u32>,
    },
    /// `lookupswitch`: the default and (key, target) pairs.
    Lookup {
        default: u32,
        pairs: Vec<(i32, u32)>,
    },
}

/// Opcode names, for logs and tests.
pub const NAMES: [&str; 202] = [
    "nop",
    "aconst_null",
    "iconst_m1",
    "iconst_0",
    "iconst_1",
    "iconst_2",
    "iconst_3",
    "iconst_4",
    "iconst_5",
    "lconst_0",
    "lconst_1",
    "fconst_0",
    "fconst_1",
    "fconst_2",
    "dconst_0",
    "dconst_1",
    "bipush",
    "sipush",
    "ldc",
    "ldc_w",
    "ldc2_w",
    "iload",
    "lload",
    "fload",
    "dload",
    "aload",
    "iload_0",
    "iload_1",
    "iload_2",
    "iload_3",
    "lload_0",
    "lload_1",
    "lload_2",
    "lload_3",
    "fload_0",
    "fload_1",
    "fload_2",
    "fload_3",
    "dload_0",
    "dload_1",
    "dload_2",
    "dload_3",
    "aload_0",
    "aload_1",
    "aload_2",
    "aload_3",
    "iaload",
    "laload",
    "faload",
    "daload",
    "aaload",
    "baload",
    "caload",
    "saload",
    "istore",
    "lstore",
    "fstore",
    "dstore",
    "astore",
    "istore_0",
    "istore_1",
    "istore_2",
    "istore_3",
    "lstore_0",
    "lstore_1",
    "lstore_2",
    "lstore_3",
    "fstore_0",
    "fstore_1",
    "fstore_2",
    "fstore_3",
    "dstore_0",
    "dstore_1",
    "dstore_2",
    "dstore_3",
    "astore_0",
    "astore_1",
    "astore_2",
    "astore_3",
    "iastore",
    "lastore",
    "fastore",
    "dastore",
    "aastore",
    "bastore",
    "castore",
    "sastore",
    "pop",
    "pop2",
    "dup",
    "dup_x1",
    "dup_x2",
    "dup2",
    "dup2_x1",
    "dup2_x2",
    "swap",
    "iadd",
    "ladd",
    "fadd",
    "dadd",
    "isub",
    "lsub",
    "fsub",
    "dsub",
    "imul",
    "lmul",
    "fmul",
    "dmul",
    "idiv",
    "ldiv",
    "fdiv",
    "ddiv",
    "irem",
    "lrem",
    "frem",
    "drem",
    "ineg",
    "lneg",
    "fneg",
    "dneg",
    "ishl",
    "lshl",
    "ishr",
    "lshr",
    "iushr",
    "lushr",
    "iand",
    "land",
    "ior",
    "lor",
    "ixor",
    "lxor",
    "iinc",
    "i2l",
    "i2f",
    "i2d",
    "l2i",
    "l2f",
    "l2d",
    "f2i",
    "f2l",
    "f2d",
    "d2i",
    "d2l",
    "d2f",
    "i2b",
    "i2c",
    "i2s",
    "lcmp",
    "fcmpl",
    "fcmpg",
    "dcmpl",
    "dcmpg",
    "ifeq",
    "ifne",
    "iflt",
    "ifge",
    "ifgt",
    "ifle",
    "if_icmpeq",
    "if_icmpne",
    "if_icmplt",
    "if_icmpge",
    "if_icmpgt",
    "if_icmple",
    "if_acmpeq",
    "if_acmpne",
    "goto",
    "jsr",
    "ret",
    "tableswitch",
    "lookupswitch",
    "ireturn",
    "lreturn",
    "freturn",
    "dreturn",
    "areturn",
    "return",
    "getstatic",
    "putstatic",
    "getfield",
    "putfield",
    "invokevirtual",
    "invokespecial",
    "invokestatic",
    "invokeinterface",
    "invokedynamic",
    "new",
    "newarray",
    "anewarray",
    "arraylength",
    "athrow",
    "checkcast",
    "instanceof",
    "monitorenter",
    "monitorexit",
    "wide",
    "multianewarray",
    "ifnull",
    "ifnonnull",
    "goto_w",
    "jsr_w",
];

pub fn name(opcode: u8) -> &'static str {
    NAMES
        .get(usize::from(opcode))
        .copied()
        .unwrap_or("(unknown)")
}

/// Decodes a method's code; errs on an unknown opcode or a cut instruction.
pub fn decode(code: &[u8]) -> Result<Vec<Instruction>> {
    let mut out = Vec::with_capacity(code.len() / 2);
    let mut pc = 0usize;
    let byte = |i: usize| code.get(i).copied().ok_or(Error::Truncated);
    let u16_at = |i: usize| Ok::<u16, Error>(u16::from_be_bytes([byte(i)?, byte(i + 1)?]));
    let i32_at = |i: usize| {
        Ok::<i32, Error>(i32::from_be_bytes([
            byte(i)?,
            byte(i + 1)?,
            byte(i + 2)?,
            byte(i + 3)?,
        ]))
    };
    let target = |pc: usize, offset: i32| -> Result<u32> {
        let t = pc as i64 + i64::from(offset);
        if t < 0 || t >= code.len() as i64 {
            return Err(Error::Malformed("a branch outside the code"));
        }
        Ok(t as u32)
    };
    while pc < code.len() {
        let op = code[pc];
        let (operand, len) = match op {
            0x00..=0x0F
            | 0x1A..=0x35
            | 0x3B..=0x83
            | 0x85..=0x98
            | 0xAC..=0xB1
            | 0xBE
            | 0xBF
            | 0xC2
            | 0xC3 => (Operand::None, 1),
            0x10 => (Operand::Value(i32::from(byte(pc + 1)? as i8)), 2),
            0x11 => (Operand::Value(i32::from(u16_at(pc + 1)? as i16)), 3),
            0x12 => (Operand::Constant(u16::from(byte(pc + 1)?)), 2),
            0x13 | 0x14 | 0xB2..=0xB8 | 0xBB | 0xBD | 0xC0 | 0xC1 => {
                (Operand::Constant(u16_at(pc + 1)?), 3)
            }
            0x15..=0x19 | 0x36..=0x3A | 0xA9 => (Operand::Local(u16::from(byte(pc + 1)?)), 2),
            0x84 => (
                Operand::Increment(u16::from(byte(pc + 1)?), i32::from(byte(pc + 2)? as i8)),
                3,
            ),
            0x99..=0xA8 | 0xC6 | 0xC7 => (
                Operand::Target(target(pc, i32::from(u16_at(pc + 1)? as i16))?),
                3,
            ),
            0xC8 | 0xC9 => (Operand::Target(target(pc, i32_at(pc + 1)?)?), 5),
            0xB9 => {
                let count = byte(pc + 3)?;
                if byte(pc + 4)? != 0 {
                    return Err(Error::Malformed("invokeinterface's fourth byte is not 0"));
                }
                (Operand::Interface(u16_at(pc + 1)?, count), 5)
            }
            0xBA => {
                if u16_at(pc + 3)? != 0 {
                    return Err(Error::Malformed("invokedynamic's last bytes are not 0"));
                }
                (Operand::Constant(u16_at(pc + 1)?), 5)
            }
            0xBC => (Operand::ArrayType(byte(pc + 1)?), 2),
            0xC5 => (Operand::Dimensions(u16_at(pc + 1)?, byte(pc + 3)?), 4),
            0xAA | 0xAB => {
                // Padded so the operands start at a multiple of 4.
                let base = (pc + 4) & !3;
                let default = target(pc, i32_at(base)?)?;
                if op == 0xAA {
                    let low = i32_at(base + 4)?;
                    let high = i32_at(base + 8)?;
                    if high < low {
                        return Err(Error::Malformed("a tableswitch with high < low"));
                    }
                    let n = (i64::from(high) - i64::from(low) + 1) as usize;
                    let mut targets = Vec::with_capacity(n);
                    for i in 0..n {
                        targets.push(target(pc, i32_at(base + 12 + 4 * i)?)?);
                    }
                    (
                        Operand::Table {
                            default,
                            low,
                            targets,
                        },
                        base + 12 + 4 * n - pc,
                    )
                } else {
                    let n = i32_at(base + 4)?;
                    if n < 0 {
                        return Err(Error::Malformed("a lookupswitch with a negative count"));
                    }
                    let mut pairs = Vec::with_capacity(n as usize);
                    for i in 0..n as usize {
                        let at = base + 8 + 8 * i;
                        pairs.push((i32_at(at)?, target(pc, i32_at(at + 4)?)?));
                    }
                    (
                        Operand::Lookup { default, pairs },
                        base + 8 + 8 * n as usize - pc,
                    )
                }
            }
            0xC4 => {
                // wide: a two-byte local, and for iinc a two-byte increment.
                let inner = byte(pc + 1)?;
                let local = u16_at(pc + 2)?;
                let (operand, len) = match inner {
                    0x84 => (
                        Operand::Increment(local, i32::from(u16_at(pc + 4)? as i16)),
                        6,
                    ),
                    0x15..=0x19 | 0x36..=0x3A | 0xA9 => (Operand::Local(local), 4),
                    _ => {
                        return Err(Error::Malformed(
                            "wide before an instruction it can't widen",
                        ));
                    }
                };
                out.push(Instruction {
                    at: pc as u32,
                    opcode: inner,
                    operand,
                });
                pc += len;
                continue;
            }
            _ => return Err(Error::Malformed("an unknown opcode")),
        };
        if pc + len > code.len() {
            return Err(Error::Truncated);
        }
        out.push(Instruction {
            at: pc as u32,
            opcode: op,
            operand,
        });
        pc += len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_length_instructions_decode() {
        // 0: iload_0; 1: tableswitch (pad to 4) default +23, 1..2 -> +23, +23;
        // 24: iinc 1 by -1 under wide; 30: return.
        let mut code = vec![0x1A, 0xAA, 0, 0];
        for v in [23i32, 1, 2, 23, 23] {
            code.extend(v.to_be_bytes());
        }
        code.extend([0xC4, 0x84, 0x01, 0x00, 0xFF, 0xFF, 0xB1]);
        let insns = decode(&code).unwrap();
        assert_eq!(insns.len(), 4);
        assert_eq!(
            insns[1].operand,
            Operand::Table {
                default: 24,
                low: 1,
                targets: vec![24, 24]
            }
        );
        assert_eq!(insns[2].at, 24);
        assert_eq!(insns[2].operand, Operand::Increment(256, -1));
        assert_eq!(name(insns[3].opcode), "return");
    }

    #[test]
    fn unknown_or_cut_code_is_refused() {
        assert!(decode(&[0xCA]).is_err());
        assert!(decode(&[0x11, 0x00]).is_err());
        assert!(
            decode(&[0xA7, 0x00, 0x09]).is_err(),
            "goto outside the code"
        );
    }
}
