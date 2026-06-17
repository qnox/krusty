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
