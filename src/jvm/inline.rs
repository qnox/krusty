//! Inline-function expansion (JVM): splice a classpath `inline` function's compiled body into a call
//! site. The body's bytecode references *its own* class's constant pool, so every pool index in it
//! must be **relocated** into the target class's pool — that relocation is the core primitive here.
//! Built on the lazily-read [`MethodCode`](super::classreader::MethodCode); the instruction walk,
//! local remapping, and `reifiedOperationMarker` handling layer on top in later phases.

use super::classfile::ClassWriter;
use super::classreader::C;

fn utf8(cp: &[C], i: u16) -> Option<&str> {
    match cp.get(i as usize)? {
        C::Utf8(s) => Some(s),
        _ => None,
    }
}

fn class_name(cp: &[C], i: u16) -> Option<&str> {
    match cp.get(i as usize)? {
        C::Class(n) => utf8(cp, *n),
        _ => None,
    }
}

fn name_and_type(cp: &[C], i: u16) -> Option<(&str, &str)> {
    match cp.get(i as usize)? {
        C::NameAndType(n, d) => Some((utf8(cp, *n)?, utf8(cp, *d)?)),
        _ => None,
    }
}

/// Re-intern the source constant-pool entry at `idx` (from the inline body's defining class, `src_cp`)
/// into the target class's pool (`cw`), returning the new pool index. Resolving each entry to its
/// semantic form (class/method/field names, descriptors, constant values) and re-interning is what
/// lets a body compiled against one class run inside another. `None` for an entry kind not yet
/// relocatable (`invokedynamic`/method handles — those need bootstrap-method relocation too).
pub fn relocate_const(src_cp: &[C], idx: u16, cw: &mut ClassWriter) -> Option<u16> {
    match src_cp.get(idx as usize)? {
        C::Class(n) => Some(cw.class_ref(utf8(src_cp, *n)?)),
        C::String(u) => Some(cw.const_string(utf8(src_cp, *u)?)),
        C::Integer(v) => Some(cw.const_int(*v)),
        C::Float(b) => Some(cw.const_float(f32::from_bits(*b))),
        C::Long(v) => Some(cw.const_long(*v)),
        C::Double(b) => Some(cw.const_double(f64::from_bits(*b))),
        C::Methodref(c, nt) => {
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            Some(cw.methodref(&cn, &n.to_string(), &d.to_string()))
        }
        C::Fieldref(c, nt) => {
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            Some(cw.fieldref(&cn, &n.to_string(), &d.to_string()))
        }
        C::InterfaceMethodref(c, nt) => {
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            Some(cw.interface_methodref(&cn, &n.to_string(), &d.to_string()))
        }
        _ => None,
    }
}

/// The length in bytes of the instruction at `pc` (opcode + operands), including the variable-length
/// `tableswitch`/`lookupswitch`/`wide` forms. `None` if `pc` is out of range or the opcode is
/// malformed/truncated. Lets the relocation walk step instruction-by-instruction without a disassembler.
pub fn instruction_len(code: &[u8], pc: usize) -> Option<usize> {
    let op = *code.get(pc)?;
    let len = match op {
        // wide: a 2-byte-index load/store, or `wide iinc` (2-byte index + 2-byte const).
        0xc4 => match code.get(pc + 1)? {
            0x84 => 6, // wide iinc
            _ => 4,    // wide iload/istore/…/ret
        },
        // tableswitch: 0-3 bytes pad to a 4-byte boundary, then default/low/high + (high-low+1) offsets.
        0xaa => {
            let base = pc + 1;
            let pad = (4 - (base % 4)) % 4;
            let p = base + pad;
            let low = i32::from_be_bytes(code.get(p + 4..p + 8)?.try_into().ok()?);
            let high = i32::from_be_bytes(code.get(p + 8..p + 12)?.try_into().ok()?);
            let n = (high - low + 1).max(0) as usize;
            (p + 12 + n * 4) - pc
        }
        // lookupswitch: pad, then default + npairs + npairs*(match,offset).
        0xab => {
            let base = pc + 1;
            let pad = (4 - (base % 4)) % 4;
            let p = base + pad;
            let npairs = i32::from_be_bytes(code.get(p + 4..p + 8)?.try_into().ok()?).max(0) as usize;
            (p + 8 + npairs * 8) - pc
        }
        // 1 operand byte.
        0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
        // 2 operand bytes.
        0x11 | 0x13 | 0x14 | 0x84 | 0x99..=0xa8 | 0xb2..=0xb8 | 0xbb | 0xbd | 0xc0 | 0xc1 | 0xc6 | 0xc7 => 3,
        // multianewarray: 2-byte index + 1 dim byte.
        0xc5 => 4,
        // invokeinterface / invokedynamic: 2-byte index + 2 trailing bytes.
        0xb9 | 0xba => 5,
        // goto_w / jsr_w.
        0xc8 | 0xc9 => 5,
        // everything else is a single byte (no operands).
        _ => 1,
    };
    if pc + len <= code.len() {
        Some(len)
    } else {
        None
    }
}

/// The byte offset (from the opcode) and width of an instruction's constant-pool operand, if it has
/// one. `ldc` carries a 1-byte index; the rest carry 2 bytes.
fn pool_operand(op: u8) -> Option<(usize, usize)> {
    match op {
        0x12 => Some((1, 1)), // ldc
        0x13 | 0x14 => Some((1, 2)), // ldc_w / ldc2_w
        0xb2..=0xb8 => Some((1, 2)), // get/put static/field, invoke virtual/special/static
        0xb9 | 0xba => Some((1, 2)), // invokeinterface / invokedynamic (index in first 2 operand bytes)
        0xbb | 0xbd | 0xc0 | 0xc1 => Some((1, 2)), // new / anewarray / checkcast / instanceof
        0xc5 => Some((1, 2)), // multianewarray
        _ => None,
    }
}

/// Relocate every constant-pool reference in an inline body's `code` into `cw`'s pool, returning the
/// rewritten bytecode. Branch offsets are unaffected (instruction lengths are preserved). `None` if a
/// reference can't be relocated (`invokedynamic`, or a relocated `ldc` index exceeding a byte — that
/// would need an `ldc`→`ldc_w` rewrite that shifts offsets), so the caller falls back to a real call.
pub fn relocate_code(code: &[u8], src_cp: &[C], cw: &mut ClassWriter) -> Option<Vec<u8>> {
    let mut out = code.to_vec();
    let mut pc = 0;
    while pc < code.len() {
        let op = code[pc];
        let len = instruction_len(code, pc)?;
        if let Some((off, width)) = pool_operand(op) {
            if op == 0xba {
                return None; // invokedynamic: bootstrap-method relocation not modeled
            }
            let src_idx = if width == 1 {
                code[pc + off] as u16
            } else {
                (code[pc + off] as u16) << 8 | code[pc + off + 1] as u16
            };
            let new = relocate_const(src_cp, src_idx, cw)?;
            if width == 1 {
                if new > 0xff {
                    return None; // would need ldc→ldc_w (offset-shifting) — bail
                }
                out[pc + off] = new as u8;
            } else {
                out[pc + off] = (new >> 8) as u8;
                out[pc + off + 1] = (new & 0xff) as u8;
            }
        }
        pc += len;
    }
    Some(out)
}

/// A decoded instruction with branch targets resolved to *instruction indices*, not byte offsets —
/// so transforms that change an instruction's size (local remap, `ldc`→`ldc_w`) don't invalidate
/// jump targets: the assembler recomputes every offset from the final layout.
#[derive(Clone, Debug)]
pub enum Insn {
    /// Opcode + verbatim operand bytes (the operands carry no branch offset).
    Plain { op: u8, operands: Vec<u8> },
    /// A 2-byte-offset conditional/`goto`/`jsr` branch to instruction `target`.
    Branch { op: u8, target: usize },
    /// A 4-byte-offset `goto_w`/`jsr_w`.
    BranchW { op: u8, target: usize },
    TableSwitch { default: usize, low: i32, targets: Vec<usize> },
    LookupSwitch { default: usize, pairs: Vec<(i32, usize)> },
}

/// Decode a method body into [`Insn`]s with branch targets as instruction indices. `None` on
/// malformed/truncated bytecode or a branch into the middle of an instruction.
pub fn disassemble(code: &[u8]) -> Option<Vec<Insn>> {
    // Pass 1: decode each instruction, keeping branch targets as absolute byte offsets.
    let mut offsets: Vec<usize> = Vec::new(); // insn index → byte offset
    let mut insns: Vec<Insn> = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        let op = code[pc];
        let len = instruction_len(code, pc)?;
        let insn = match op {
            0x99..=0xa8 | 0xc6 | 0xc7 => {
                let off = i16::from_be_bytes([code[pc + 1], code[pc + 2]]) as isize;
                Insn::Branch { op, target: (pc as isize + off) as usize }
            }
            0xc8 | 0xc9 => {
                let off = i32::from_be_bytes(code.get(pc + 1..pc + 5)?.try_into().ok()?) as isize;
                Insn::BranchW { op, target: (pc as isize + off) as usize }
            }
            0xaa => {
                let p = pc + 1 + (4 - ((pc + 1) % 4)) % 4;
                let def = (pc as isize + i32::from_be_bytes(code.get(p..p + 4)?.try_into().ok()?) as isize) as usize;
                let low = i32::from_be_bytes(code.get(p + 4..p + 8)?.try_into().ok()?);
                let high = i32::from_be_bytes(code.get(p + 8..p + 12)?.try_into().ok()?);
                let n = (high - low + 1).max(0) as usize;
                let mut targets = Vec::with_capacity(n);
                for k in 0..n {
                    let o = p + 12 + k * 4;
                    targets.push((pc as isize + i32::from_be_bytes(code.get(o..o + 4)?.try_into().ok()?) as isize) as usize);
                }
                Insn::TableSwitch { default: def, low, targets }
            }
            0xab => {
                let p = pc + 1 + (4 - ((pc + 1) % 4)) % 4;
                let def = (pc as isize + i32::from_be_bytes(code.get(p..p + 4)?.try_into().ok()?) as isize) as usize;
                let npairs = i32::from_be_bytes(code.get(p + 4..p + 8)?.try_into().ok()?).max(0) as usize;
                let mut pairs = Vec::with_capacity(npairs);
                for k in 0..npairs {
                    let o = p + 8 + k * 8;
                    let m = i32::from_be_bytes(code.get(o..o + 4)?.try_into().ok()?);
                    let t = (pc as isize + i32::from_be_bytes(code.get(o + 4..o + 8)?.try_into().ok()?) as isize) as usize;
                    pairs.push((m, t));
                }
                Insn::LookupSwitch { default: def, pairs }
            }
            _ => Insn::Plain { op, operands: code[pc + 1..pc + len].to_vec() },
        };
        offsets.push(pc);
        insns.push(insn);
        pc += len;
    }
    // Pass 2: resolve byte-offset targets to instruction indices.
    let idx_of = |byte: usize| offsets.binary_search(&byte).ok();
    for insn in &mut insns {
        match insn {
            Insn::Branch { target, .. } | Insn::BranchW { target, .. } => *target = idx_of(*target)?,
            Insn::TableSwitch { default, targets, .. } => {
                *default = idx_of(*default)?;
                for t in targets {
                    *t = idx_of(*t)?;
                }
            }
            Insn::LookupSwitch { default, pairs } => {
                *default = idx_of(*default)?;
                for (_, t) in pairs {
                    *t = idx_of(*t)?;
                }
            }
            Insn::Plain { .. } => {}
        }
    }
    Some(insns)
}

/// The encoded size of an instruction at byte offset `at` (switch padding depends on position).
fn insn_size(insn: &Insn, at: usize) -> usize {
    match insn {
        Insn::Plain { operands, .. } => 1 + operands.len(),
        Insn::Branch { .. } => 3,
        Insn::BranchW { .. } => 5,
        Insn::TableSwitch { targets, .. } => {
            let pad = (4 - ((at + 1) % 4)) % 4;
            1 + pad + 12 + targets.len() * 4
        }
        Insn::LookupSwitch { pairs, .. } => {
            let pad = (4 - ((at + 1) % 4)) % 4;
            1 + pad + 8 + pairs.len() * 8
        }
    }
}

/// Re-encode instructions to bytecode, computing every branch/switch offset from the final layout.
/// Instruction-index targets make this robust to size-changing transforms. A round-trip of an
/// untransformed body reproduces it byte-for-byte (switch padding is position-stable).
pub fn assemble(insns: &[Insn]) -> Vec<u8> {
    // Iterate offsets to a fixpoint: a switch's padding depends on its byte position, which depends
    // on earlier sizes. Converges (sizes only depend on positions monotonically).
    let mut offs = vec![0usize; insns.len() + 1];
    loop {
        let mut at = 0;
        let mut changed = false;
        for (i, insn) in insns.iter().enumerate() {
            if offs[i] != at {
                offs[i] = at;
                changed = true;
            }
            at += insn_size(insn, at);
        }
        if offs[insns.len()] != at {
            offs[insns.len()] = at;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    let mut out = Vec::with_capacity(offs[insns.len()]);
    for (i, insn) in insns.iter().enumerate() {
        let here = offs[i];
        match insn {
            Insn::Plain { op, operands } => {
                out.push(*op);
                out.extend_from_slice(operands);
            }
            Insn::Branch { op, target } => {
                out.push(*op);
                out.extend_from_slice(&((offs[*target] as isize - here as isize) as i16).to_be_bytes());
            }
            Insn::BranchW { op, target } => {
                out.push(*op);
                out.extend_from_slice(&((offs[*target] as isize - here as isize) as i32).to_be_bytes());
            }
            Insn::TableSwitch { default, low, targets } => {
                out.push(0xaa);
                while (out.len()) % 4 != 0 {
                    out.push(0);
                }
                out.extend_from_slice(&((offs[*default] as isize - here as isize) as i32).to_be_bytes());
                out.extend_from_slice(&low.to_be_bytes());
                out.extend_from_slice(&(*low + targets.len() as i32 - 1).to_be_bytes());
                for t in targets {
                    out.extend_from_slice(&((offs[*t] as isize - here as isize) as i32).to_be_bytes());
                }
            }
            Insn::LookupSwitch { default, pairs } => {
                out.push(0xab);
                while (out.len()) % 4 != 0 {
                    out.push(0);
                }
                out.extend_from_slice(&((offs[*default] as isize - here as isize) as i32).to_be_bytes());
                out.extend_from_slice(&(pairs.len() as i32).to_be_bytes());
                for (m, t) in pairs {
                    out.extend_from_slice(&m.to_be_bytes());
                    out.extend_from_slice(&((offs[*t] as isize - here as isize) as i32).to_be_bytes());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocates_pool_entries() {
        // A miniature source pool: Object.hashCode()I as a Methodref, plus a String and an Integer.
        let src_cp = vec![
            C::Other,
            C::Utf8("java/lang/Object".into()), // 1
            C::Class(1),                         // 2
            C::Utf8("hashCode".into()),          // 3
            C::Utf8("()I".into()),               // 4
            C::NameAndType(3, 4),                // 5
            C::Methodref(2, 5),                  // 6
            C::Utf8("hi".into()),                // 7
            C::String(7),                        // 8
            C::Integer(42),                      // 9
        ];
        let mut cw = ClassWriter::new("Target", "java/lang/Object");
        let m = relocate_const(&src_cp, 6, &mut cw).expect("methodref");
        let s = relocate_const(&src_cp, 8, &mut cw).expect("string");
        let n = relocate_const(&src_cp, 9, &mut cw).expect("integer");
        assert!(m > 0 && s > 0 && n > 0);
        // Interning is idempotent: relocating the same entry again yields the same index.
        assert_eq!(m, relocate_const(&src_cp, 6, &mut cw).unwrap());
        // An unrelocatable kind (a bare NameAndType / Utf8) returns None.
        assert!(relocate_const(&src_cp, 5, &mut cw).is_none());
        assert!(relocate_const(&src_cp, 1, &mut cw).is_none());
    }

    #[test]
    fn relocates_code_pool_refs() {
        let src_cp = vec![
            C::Other,
            C::Utf8("Foo".into()),    // 1
            C::Class(1),              // 2
            C::Utf8("bar".into()),    // 3
            C::Utf8("()V".into()),    // 4
            C::NameAndType(3, 4),     // 5
            C::Methodref(2, 5),       // 6
        ];
        // invokestatic #6 ; return
        let code = [0xb8, 0x00, 0x06, 0xb1];
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let out = relocate_code(&code, &src_cp, &mut cw).expect("relocate");
        assert_eq!(out.len(), code.len(), "instruction lengths preserved");
        let expected = cw.methodref("Foo", "bar", "()V");
        assert_eq!((out[1] as u16) << 8 | out[2] as u16, expected, "index points at target methodref");
        assert_eq!(out[3], 0xb1, "return opcode unchanged");
    }

    #[test]
    fn instruction_len_covers_switches_and_wide() {
        // bipush(2), invokestatic(3), wide-iinc(6), goto_w(5), single-byte iadd(1).
        assert_eq!(instruction_len(&[0x10, 0x05], 0), Some(2));
        assert_eq!(instruction_len(&[0xb8, 0, 6, 0xb1], 0), Some(3));
        assert_eq!(instruction_len(&[0xc4, 0x84, 0, 1, 0, 1], 0), Some(6));
        assert_eq!(instruction_len(&[0xc8, 0, 0, 0, 4], 0), Some(5));
        assert_eq!(instruction_len(&[0x60], 0), Some(1));
    }
}
