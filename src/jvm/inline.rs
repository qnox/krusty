//! Inline-function expansion (JVM): splice a classpath `inline` function's compiled body into a call
//! site. The body's bytecode references *its own* class's constant pool, so every pool index in it
//! must be **relocated** into the target class's pool — that relocation is the core primitive here.
//! Built on the lazily-read [`MethodCode`](super::classreader::MethodCode); the instruction walk,
//! local remapping, and `reifiedOperationMarker` handling layer on top in later phases.

use super::classfile::ClassWriter;
use super::classreader::{MethodCode, C};
use std::collections::HashMap;

/// The narrow capability the bytecode inliner needs from the classpath (interface segregation /
/// least-knowledge): read a method's compiled body by owner/name/descriptor. *Whether* a callee is
/// `inline` is function metadata that travels with the resolved signature (decoded once, alongside the
/// signature, in `metadata.rs`) and reaches the emitter via the IR — it is not re-queried here. The
/// emitter depends only on this, not on the whole `Classpath` (caches, jimage, type indexes).
pub trait MethodBodies {
    /// The compiled `Code` body of `owner.name descriptor`, or `None` if absent/abstract/native.
    fn body(&self, owner: &str, name: &str, descriptor: &str) -> Option<MethodCode>;
}

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
            let npairs =
                i32::from_be_bytes(code.get(p + 4..p + 8)?.try_into().ok()?).max(0) as usize;
            (p + 8 + npairs * 8) - pc
        }
        // 1 operand byte.
        0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
        // 2 operand bytes.
        0x11
        | 0x13
        | 0x14
        | 0x84
        | 0x99..=0xa8
        | 0xb2..=0xb8
        | 0xbb
        | 0xbd
        | 0xc0
        | 0xc1
        | 0xc6
        | 0xc7 => 3,
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
        0x12 => Some((1, 1)),                      // ldc
        0x13 | 0x14 => Some((1, 2)),               // ldc_w / ldc2_w
        0xb2..=0xb8 => Some((1, 2)), // get/put static/field, invoke virtual/special/static
        0xb9 | 0xba => Some((1, 2)), // invokeinterface / invokedynamic (index in first 2 operand bytes)
        0xbb | 0xbd | 0xc0 | 0xc1 => Some((1, 2)), // new / anewarray / checkcast / instanceof
        0xc5 => Some((1, 2)),        // multianewarray
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
    TableSwitch {
        default: usize,
        low: i32,
        targets: Vec<usize>,
    },
    LookupSwitch {
        default: usize,
        pairs: Vec<(i32, usize)>,
    },
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
                Insn::Branch {
                    op,
                    target: (pc as isize + off) as usize,
                }
            }
            0xc8 | 0xc9 => {
                let off = i32::from_be_bytes(code.get(pc + 1..pc + 5)?.try_into().ok()?) as isize;
                Insn::BranchW {
                    op,
                    target: (pc as isize + off) as usize,
                }
            }
            0xaa => {
                let p = pc + 1 + (4 - ((pc + 1) % 4)) % 4;
                let def = (pc as isize
                    + i32::from_be_bytes(code.get(p..p + 4)?.try_into().ok()?) as isize)
                    as usize;
                let low = i32::from_be_bytes(code.get(p + 4..p + 8)?.try_into().ok()?);
                let high = i32::from_be_bytes(code.get(p + 8..p + 12)?.try_into().ok()?);
                let n = (high - low + 1).max(0) as usize;
                let mut targets = Vec::with_capacity(n);
                for k in 0..n {
                    let o = p + 12 + k * 4;
                    targets.push(
                        (pc as isize
                            + i32::from_be_bytes(code.get(o..o + 4)?.try_into().ok()?) as isize)
                            as usize,
                    );
                }
                Insn::TableSwitch {
                    default: def,
                    low,
                    targets,
                }
            }
            0xab => {
                let p = pc + 1 + (4 - ((pc + 1) % 4)) % 4;
                let def = (pc as isize
                    + i32::from_be_bytes(code.get(p..p + 4)?.try_into().ok()?) as isize)
                    as usize;
                let npairs =
                    i32::from_be_bytes(code.get(p + 4..p + 8)?.try_into().ok()?).max(0) as usize;
                let mut pairs = Vec::with_capacity(npairs);
                for k in 0..npairs {
                    let o = p + 8 + k * 8;
                    let m = i32::from_be_bytes(code.get(o..o + 4)?.try_into().ok()?);
                    let t = (pc as isize
                        + i32::from_be_bytes(code.get(o + 4..o + 8)?.try_into().ok()?) as isize)
                        as usize;
                    pairs.push((m, t));
                }
                Insn::LookupSwitch {
                    default: def,
                    pairs,
                }
            }
            _ => Insn::Plain {
                op,
                operands: code[pc + 1..pc + len].to_vec(),
            },
        };
        offsets.push(pc);
        insns.push(insn);
        pc += len;
    }
    // Pass 2: resolve byte-offset targets to instruction indices.
    let idx_of = |byte: usize| offsets.binary_search(&byte).ok();
    for insn in &mut insns {
        match insn {
            Insn::Branch { target, .. } | Insn::BranchW { target, .. } => {
                *target = idx_of(*target)?
            }
            Insn::TableSwitch {
                default, targets, ..
            } => {
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
/// The byte offset of each instruction (index `i` → its offset; the final entry is the total byte
/// length), computed to a fixpoint since a switch's padding depends on its position. Lets a transform
/// map an instruction index to its byte offset in the assembled layout.
pub fn insn_offsets(insns: &[Insn]) -> Vec<usize> {
    insn_offsets_at(insns, 0)
}

/// Like [`insn_offsets`] but the first instruction sits at byte offset `base` (the position this body
/// will occupy in the final method) — switch padding aligns to 4 bytes from the METHOD start, so a
/// spliced body containing a `tableswitch`/`lookupswitch` must be laid out at its real offset.
pub fn insn_offsets_at(insns: &[Insn], base: usize) -> Vec<usize> {
    let mut offs = vec![base; insns.len() + 1];
    loop {
        let mut at = base;
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
    offs
}

pub fn assemble(insns: &[Insn]) -> Vec<u8> {
    assemble_at(insns, 0)
}

/// Like [`assemble`] but laid out as if starting at byte offset `base` in the method — switch padding
/// aligns to 4 bytes from the method start. (Branch deltas are position-independent, so `base` only
/// affects `tableswitch`/`lookupswitch` padding.)
pub fn assemble_at(insns: &[Insn], base: usize) -> Vec<u8> {
    let offs = insn_offsets_at(insns, base);
    let mut out = Vec::with_capacity(offs[insns.len()].saturating_sub(base));
    for (i, insn) in insns.iter().enumerate() {
        let here = offs[i];
        match insn {
            Insn::Plain { op, operands } => {
                out.push(*op);
                out.extend_from_slice(operands);
            }
            Insn::Branch { op, target } => {
                out.push(*op);
                out.extend_from_slice(
                    &((offs[*target] as isize - here as isize) as i16).to_be_bytes(),
                );
            }
            Insn::BranchW { op, target } => {
                out.push(*op);
                out.extend_from_slice(
                    &((offs[*target] as isize - here as isize) as i32).to_be_bytes(),
                );
            }
            Insn::TableSwitch {
                default,
                low,
                targets,
            } => {
                out.push(0xaa);
                while !(base + out.len()).is_multiple_of(4) {
                    out.push(0);
                }
                out.extend_from_slice(
                    &((offs[*default] as isize - here as isize) as i32).to_be_bytes(),
                );
                out.extend_from_slice(&low.to_be_bytes());
                out.extend_from_slice(&(*low + targets.len() as i32 - 1).to_be_bytes());
                for t in targets {
                    out.extend_from_slice(
                        &((offs[*t] as isize - here as isize) as i32).to_be_bytes(),
                    );
                }
            }
            Insn::LookupSwitch { default, pairs } => {
                out.push(0xab);
                while !(base + out.len()).is_multiple_of(4) {
                    out.push(0);
                }
                out.extend_from_slice(
                    &((offs[*default] as isize - here as isize) as i32).to_be_bytes(),
                );
                out.extend_from_slice(&(pairs.len() as i32).to_be_bytes());
                for (m, t) in pairs {
                    out.extend_from_slice(&m.to_be_bytes());
                    out.extend_from_slice(
                        &((offs[*t] as isize - here as isize) as i32).to_be_bytes(),
                    );
                }
            }
        }
    }
    out
}

/// If `op` is a one-byte-indexed local load/store (`iload`/`astore`/…), its canonical `*_N`-form base
/// opcode (`iload`→`iload_0`), used to pick the compact form when the shifted index is 0..=3.
fn n_form_base(op: u8) -> Option<u8> {
    Some(match op {
        0x15 => 0x1a, // iload
        0x16 => 0x1e, // lload
        0x17 => 0x22, // fload
        0x18 => 0x26, // dload
        0x19 => 0x2a, // aload
        0x36 => 0x3b, // istore
        0x37 => 0x3f, // lstore
        0x38 => 0x43, // fstore
        0x39 => 0x47, // dstore
        0x3a => 0x4b, // astore
        _ => return None,
    })
}

/// Decode a `*_N` load/store opcode to its `(indexed base opcode, local index)` — `iload_2` →
/// `(iload, 2)`. `None` for any other opcode.
fn decode_n_form(op: u8) -> Option<(u8, u16)> {
    let (base, n_base) = match op {
        0x1a..=0x1d => (0x15, 0x1a),
        0x1e..=0x21 => (0x16, 0x1e),
        0x22..=0x25 => (0x17, 0x22),
        0x26..=0x29 => (0x18, 0x26),
        0x2a..=0x2d => (0x19, 0x2a),
        0x3b..=0x3e => (0x36, 0x3b),
        0x3f..=0x42 => (0x37, 0x3f),
        0x43..=0x46 => (0x38, 0x43),
        0x47..=0x4a => (0x39, 0x47),
        0x4b..=0x4e => (0x3a, 0x4b),
        _ => return None,
    };
    Some((base, (op - n_base) as u16))
}

/// Build the most compact load/store instruction for `(indexed base opcode, index)`: a `*_N` form for
/// 0..=3, a one-byte-indexed form for 4..=255, else a `wide` form.
fn local_load_store(base_op: u8, idx: u16) -> Insn {
    if idx <= 3 {
        if let Some(nb) = n_form_base(base_op) {
            return Insn::Plain {
                op: nb + idx as u8,
                operands: vec![],
            };
        }
    }
    if idx <= 0xff {
        Insn::Plain {
            op: base_op,
            operands: vec![idx as u8],
        }
    } else {
        Insn::Plain {
            op: 0xc4,
            operands: vec![base_op, (idx >> 8) as u8, idx as u8],
        }
    }
}

/// Add `base` to every local-variable index in the body (load/store in all forms, `iinc`, `ret`, and
/// their `wide` variants), re-selecting the compact encoding. Relocating an inline body's locals into
/// the caller's frame is a prerequisite for splicing — the body then occupies `base..base+max_locals`.
pub fn shift_locals(insns: &mut [Insn], base: u16) -> Option<()> {
    for insn in insns.iter_mut() {
        let Insn::Plain { op, operands } = insn else {
            continue;
        };
        let op = *op;
        if let Some((base_op, idx)) = decode_n_form(op) {
            *insn = local_load_store(base_op, idx + base);
        } else if matches!(op, 0x15..=0x19 | 0x36..=0x3a) {
            // One-byte-indexed load/store.
            let idx = *operands.first()? as u16 + base;
            *insn = local_load_store(op, idx);
        } else if op == 0xa9 {
            // ret <index>.
            let idx = *operands.first()? as u16 + base;
            *insn = if idx <= 0xff {
                Insn::Plain {
                    op: 0xa9,
                    operands: vec![idx as u8],
                }
            } else {
                Insn::Plain {
                    op: 0xc4,
                    operands: vec![0xa9, (idx >> 8) as u8, idx as u8],
                }
            };
        } else if op == 0x84 {
            // iinc <index> <const>.
            let idx = *operands.first()? as u16 + base;
            let c = operands[1];
            *insn = if idx <= 0xff {
                Insn::Plain {
                    op: 0x84,
                    operands: vec![idx as u8, c],
                }
            } else {
                // wide iinc: 2-byte index + sign-extended 2-byte const.
                let chi = if (c as i8) < 0 { 0xff } else { 0 };
                Insn::Plain {
                    op: 0xc4,
                    operands: vec![0x84, (idx >> 8) as u8, idx as u8, chi, c],
                }
            };
        } else if op == 0xc4 {
            // wide <sub-op> <index:2> [<const:2> for iinc].
            let sub = *operands.first()?;
            let idx = ((operands[1] as u16) << 8 | operands[2] as u16) + base;
            if sub == 0x84 {
                *insn = Insn::Plain {
                    op: 0xc4,
                    operands: vec![0x84, (idx >> 8) as u8, idx as u8, operands[3], operands[4]],
                };
            } else {
                *insn = local_load_store(sub, idx);
            }
        }
    }
    Some(())
}

/// The `(class, method)` a Methodref/InterfaceMethodref in `src_cp` names, for recognizing the
/// reified marker call without relocating it.
fn methodref_target(src_cp: &[C], idx: u16) -> Option<(&str, &str)> {
    let (c, nt) = match src_cp.get(idx as usize)? {
        C::Methodref(c, nt) | C::InterfaceMethodref(c, nt) => (*c, *nt),
        _ => return None,
    };
    let class = class_name(src_cp, c)?;
    let (name, _) = name_and_type(src_cp, nt)?;
    Some((class, name))
}

/// True for the type-bearing ops a `reifiedOperationMarker` precedes: `anewarray`, `checkcast`,
/// `instanceof`, `multianewarray`.
fn is_type_op(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Plain {
            op: 0xbd | 0xc0 | 0xc1 | 0xc5,
            ..
        }
    )
}

/// Substitute Kotlin's `reifiedOperationMarker` pattern in an inline body: the call (preceded by its
/// `iconst <mode>` and `ldc "<typeParam>"` argument pushes) is replaced with `nop`s, and the
/// following type op (`anewarray`/`checkcast`/`instanceof`) is repointed at the concrete reified type
/// from `type_map` (Kotlin type-parameter name → JVM internal name). Returns the `(insn index, target
/// pool index)` repoints to apply *after* relocation (so the type op isn't re-relocated to `Object`).
/// This is how `emptyArray<String>()` inlines to `anewarray java/lang/String`.
pub fn substitute_reified(
    insns: &mut [Insn],
    src_cp: &[C],
    cw: &mut ClassWriter,
    type_map: &std::collections::HashMap<String, String>,
) -> Vec<(usize, u16)> {
    let mut patches = Vec::new();
    for i in 0..insns.len() {
        // The marker is `invokestatic kotlin/jvm/internal/Intrinsics.reifiedOperationMarker`.
        let is_marker = matches!(&insns[i], Insn::Plain { op: 0xb8, operands } if operands.len() == 2
            && methodref_target(src_cp, (operands[0] as u16) << 8 | operands[1] as u16)
                == Some(("kotlin/jvm/internal/Intrinsics", "reifiedOperationMarker")));
        if !is_marker || i < 2 {
            continue;
        }
        // The `ldc "<typeParam>"` immediately before names the reified parameter.
        let name = match &insns[i - 1] {
            Insn::Plain { op: 0x12, operands } if operands.len() == 1 => match src_cp
                .get(operands[0] as usize)
            {
                Some(C::String(u)) => utf8(src_cp, *u).map(|s| s.trim_end_matches('?').to_string()),
                _ => None,
            },
            _ => None,
        };
        // Erase the marker call and its two argument pushes (mode + type-name).
        let nop = Insn::Plain {
            op: 0x00,
            operands: vec![],
        };
        insns[i] = nop.clone();
        insns[i - 1] = nop.clone();
        insns[i - 2] = nop;
        // Repoint the next type op at the concrete type.
        if let Some(name) = name {
            if let Some(concrete) = type_map.get(&name) {
                if let Some(j) = (i + 1..insns.len()).find(|&j| is_type_op(&insns[j])) {
                    patches.push((j, cw.class_ref(concrete)));
                }
            }
        }
    }
    patches
}

/// Overwrite the 2-byte constant-pool operand of a pool-referencing instruction with `idx` (used to
/// apply the reified repoints from [`substitute_reified`] after relocation).
pub fn set_pool_operand(insn: &mut Insn, idx: u16) {
    if let Insn::Plain { op, operands } = insn {
        if let Some((off, 2)) = pool_operand(*op) {
            let o = off - 1;
            if operands.len() > o + 1 {
                operands[o] = (idx >> 8) as u8;
                operands[o + 1] = (idx & 0xff) as u8;
            }
        }
    }
}

/// Relocate every constant-pool reference in a disassembled body into `cw`'s pool (the insn-level
/// counterpart of [`relocate_code`], so relocation composes with the local/return/reified transforms
/// before reassembly). `None` on `invokedynamic` or an `ldc` whose relocated index needs a byte but
/// exceeds it — the assembler would otherwise widen it, which the caller handles by falling back.
pub fn relocate_insns(insns: &mut [Insn], src_cp: &[C], cw: &mut ClassWriter) -> Option<()> {
    for insn in insns.iter_mut() {
        let Insn::Plain { op, operands } = insn else {
            continue;
        };
        let Some((off, width)) = pool_operand(*op) else {
            continue;
        };
        if *op == 0xba {
            // invokedynamic — unrelocatable without bootstrap-method handling. UNREACHABLE for an inline
            // body: kotlinc compiles lambdas inside `inline` functions as anonymous-class singletons
            // (`getstatic …$N.INSTANCE`), never `invokedynamic`, precisely so the inliner can copy them.
            return None;
        }
        // `off` is relative to the opcode; in `operands` (opcode stripped) it is `off - 1`.
        let o = off - 1;
        let src_idx = if width == 1 {
            *operands.get(o)? as u16
        } else {
            (*operands.get(o)? as u16) << 8 | *operands.get(o + 1)? as u16
        };
        let new = relocate_const(src_cp, src_idx, cw)?;
        if width == 1 {
            if new > 0xff {
                return None;
            }
            operands[o] = new as u8;
        } else {
            operands[o] = (new >> 8) as u8;
            operands[o + 1] = (new & 0xff) as u8;
        }
    }
    Some(())
}

/// Redirect every `return`/`?return` in an inline body to the end of the inlined region instead of
/// returning from the *caller*. A value-returning `?return` (`ireturn`/`areturn`/…) leaves its value
/// on the stack — which becomes the call's result — and a plain `return` leaves nothing; replacing
/// each with `goto end` preserves that stack effect while continuing into the caller's code. `end` is
/// index `insns.len()` (one past the last instruction), a valid target the assembler lays out.
pub fn redirect_returns(insns: &mut [Insn]) {
    let end = insns.len();
    for insn in insns.iter_mut() {
        if let Insn::Plain { op, .. } = insn {
            if matches!(*op, 0xac..=0xb1) {
                *insn = Insn::Branch {
                    op: 0xa7,
                    target: end,
                };
            }
        }
    }
}

/// Whether a method body is a **reified `inline`** function — its bytecode calls
/// `Intrinsics.reifiedOperationMarker`, which the compiler must inline away (a direct call to such a
/// method throws `UnsupportedOperationException` at runtime). This recognizes the must-inline case
/// from the body alone, without parsing the `@Metadata` inline flag.
pub fn is_reified_inline(body: &MethodCode) -> bool {
    let Some(insns) = disassemble(&body.code) else {
        return false;
    };
    insns.iter().any(|i| {
        matches!(i, Insn::Plain { op: 0xb8, operands } if operands.len() == 2
        && methodref_target(&body.source_cp, (operands[0] as u16) << 8 | operands[1] as u16)
            == Some(("kotlin/jvm/internal/Intrinsics", "reifiedOperationMarker")))
    })
}

/// Per-parameter `(local slot, store-base opcode)` for a method descriptor, slots starting at `base`
/// (`long`/`double` take two). Used to bind the on-stack call arguments into the inline body's frame.
fn param_store_ops(descriptor: &str, base: u16) -> Option<Vec<(u16, u8)>> {
    let inner = descriptor.strip_prefix('(')?.split(')').next()?;
    let b = inner.as_bytes();
    let mut i = 0;
    let mut slot = base;
    let mut out = Vec::new();
    while i < b.len() {
        let (op, width, advance) = match b[i] {
            b'J' => (0x37, 2, 1), // lstore
            b'D' => (0x39, 2, 1), // dstore
            b'F' => (0x38, 1, 1), // fstore
            b'L' => {
                let mut j = i;
                while *b.get(j)? != b';' {
                    j += 1;
                }
                (0x3a, 1, j - i + 1) // astore
            }
            b'[' => {
                let mut j = i + 1;
                while *b.get(j)? == b'[' {
                    j += 1;
                }
                if *b.get(j)? == b'L' {
                    while *b.get(j)? != b';' {
                        j += 1;
                    }
                }
                (0x3a, 1, j - i + 1) // astore
            }
            _ => (0x36, 1, 1), // istore (I/Z/B/C/S)
        };
        out.push((slot, op));
        slot += width;
        i += advance;
    }
    Some(out)
}

/// Build the spliced instruction sequence for inlining `body` (an inline function with the given
/// `descriptor`) at a call site whose arguments are already on the stack: store each argument into
/// the body's parameter slots (relocated to `base..`), then the body itself — relocated into `cw`'s
/// pool, with locals shifted by `base`, returns redirected to the end, and `reifiedOperationMarker`s
/// resolved against `type_map`. `None` if the body uses a not-yet-relocatable construct
/// (`invokedynamic`, byte-index `ldc` overflow), so the caller emits a real call instead.
/// A JVM verification type (`verification_type_info`), as carried by a `StackMapTable` frame. Pool and
/// bytecode-offset operands stay in the *source* class's terms until relocation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VType {
    Top,
    Int,
    Float,
    Long,
    Double,
    Null,
    UninitThis,
    /// `Object` — a `cpool_index` of a `Class` entry in the source pool.
    Object(u16),
    /// `Uninitialized` — the bytecode `offset` of the `new` that produced the value.
    Uninit(u16),
}

/// One decoded `StackMapTable` frame: the absolute bytecode `offset` it applies to, plus the full
/// (absolute, not delta-encoded) local and operand-stack verification types at that point.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub offset: usize,
    pub locals: Vec<VType>,
    pub stack: Vec<VType>,
}

/// Read one `verification_type_info` at `*i`, advancing the cursor. `None` on truncation.
fn read_vtype(b: &[u8], i: &mut usize) -> Option<VType> {
    let tag = *b.get(*i)?;
    *i += 1;
    Some(match tag {
        0 => VType::Top,
        1 => VType::Int,
        2 => VType::Float,
        3 => VType::Double,
        4 => VType::Long,
        5 => VType::Null,
        6 => VType::UninitThis,
        7 => {
            let idx = (*b.get(*i)? as u16) << 8 | *b.get(*i + 1)? as u16;
            *i += 2;
            VType::Object(idx)
        }
        8 => {
            let off = (*b.get(*i)? as u16) << 8 | *b.get(*i + 1)? as u16;
            *i += 2;
            VType::Uninit(off)
        }
        _ => return None,
    })
}

fn u2_at(b: &[u8], i: &mut usize) -> Option<u16> {
    let v = (*b.get(*i)? as u16) << 8 | *b.get(*i + 1)? as u16;
    *i += 2;
    Some(v)
}

/// Decode a raw `StackMapTable` attribute body into the absolute [`Frame`]s it describes, given the
/// method's implicit frame-0 locals (its parameters, `this` first for an instance method). Resolves
/// the delta-encoded `same`/`same_locals_1_stack`/`chop`/`append`/`full` forms to absolute frames.
/// `None` on a malformed table. Offsets/pool refs stay in the source class's terms (relocated later).
pub fn decode_stackmap(bytes: &[u8], frame0_locals: Vec<VType>) -> Option<Vec<Frame>> {
    let mut i = 0;
    let count = u2_at(bytes, &mut i)?;
    let mut locals = frame0_locals;
    let mut offset: i64 = -1; // first frame's absolute offset = its delta (prev = -1)
    let mut frames = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let tag = *bytes.get(i)?;
        i += 1;
        let (delta, stack) = if tag <= 63 {
            // SAME
            (tag as u16, vec![])
        } else if tag <= 127 {
            // SAME_LOCALS_1_STACK_ITEM
            let s = read_vtype(bytes, &mut i)?;
            ((tag - 64) as u16, vec![s])
        } else if tag == 247 {
            // SAME_LOCALS_1_STACK_ITEM_EXTENDED
            let d = u2_at(bytes, &mut i)?;
            let s = read_vtype(bytes, &mut i)?;
            (d, vec![s])
        } else if (248..=250).contains(&tag) {
            // CHOP: remove (251 - tag) locals from the end
            let d = u2_at(bytes, &mut i)?;
            let n = (251 - tag) as usize;
            for _ in 0..n {
                locals.pop()?;
            }
            (d, vec![])
        } else if tag == 251 {
            // SAME_FRAME_EXTENDED
            (u2_at(bytes, &mut i)?, vec![])
        } else if (252..=254).contains(&tag) {
            // APPEND: add (tag - 251) locals
            let d = u2_at(bytes, &mut i)?;
            let n = (tag - 251) as usize;
            for _ in 0..n {
                locals.push(read_vtype(bytes, &mut i)?);
            }
            (d, vec![])
        } else {
            // FULL_FRAME (255)
            let d = u2_at(bytes, &mut i)?;
            let nl = u2_at(bytes, &mut i)?;
            let mut ls = Vec::with_capacity(nl as usize);
            for _ in 0..nl {
                ls.push(read_vtype(bytes, &mut i)?);
            }
            let ns = u2_at(bytes, &mut i)?;
            let mut st = Vec::with_capacity(ns as usize);
            for _ in 0..ns {
                st.push(read_vtype(bytes, &mut i)?);
            }
            locals = ls;
            (d, st)
        };
        offset += delta as i64 + 1;
        frames.push(Frame {
            offset: offset as usize,
            locals: locals.clone(),
            stack,
        });
    }
    Some(frames)
}

/// Instruction indices of the `FunctionN.invoke(...)` calls in a (disassembled) inline body — the
/// call sites of a lambda *parameter*. Lambda-argument splicing replaces each with the caller's
/// inlined lambda body. The body's only `kotlin/jvm/functions/Function*.invoke` calls are its lambda
/// parameters, so matching the methodref target identifies them without dataflow.
pub fn function_invoke_sites(insns: &[Insn], src_cp: &[C]) -> Vec<usize> {
    insns
        .iter()
        .enumerate()
        .filter_map(|(i, insn)| {
            let Insn::Plain { op: 0xb9, operands } = insn else {
                return None;
            };
            let idx = (*operands.first()? as u16) << 8 | *operands.get(1)? as u16;
            let (cls, name) = methodref_target(src_cp, idx)?;
            (name == "invoke" && cls.starts_with("kotlin/jvm/functions/Function")).then_some(i)
        })
        .collect()
}

/// Whether `insn` is an `aload <slot>` (the compact `aload_0..3` or the indexed `aload`) of `slot` —
/// used to elide the dead load of the lambda-parameter object at a lambda-invoke site.
fn is_aload_of(insn: &Insn, slot: u16) -> bool {
    match insn {
        Insn::Plain { op, operands } => match *op {
            0x2a..=0x2d => (*op as u16 - 0x2a) == slot, // aload_0..aload_3
            0x19 => operands.first().map(|&b| b as u16) == Some(slot), // aload <byte index>
            // wide aload: 0xc4 0x19 <2-byte index> (slot > 255 after the local shift).
            0xc4 => {
                operands.first() == Some(&0x19)
                    && operands.get(1..3).map(|b| (b[0] as u16) << 8 | b[1] as u16) == Some(slot)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Whether a lambda-bearing `inline fun` body can be spliced (gates routing a `let`/`also`/… call to the
/// inliner): branchless, no exception handlers, exactly one `FunctionN.invoke` call, and a single
/// trailing return (after stripping the entry `checkNotNullParameter` null-checks). The front end routes
/// a call to [`splice_unified`] ONLY when this holds — required because an `@InlineOnly` callee has no
/// runtime body to fall back to.
pub fn is_lambda_spliceable(body: &MethodCode) -> bool {
    if !body.handlers.is_empty() {
        return false;
    }
    let Some(mut insns) = disassemble(&body.code) else {
        return false;
    };
    if insns.iter().any(|i| !matches!(i, Insn::Plain { .. })) {
        return false;
    }
    strip_param_null_checks(&mut insns, &body.source_cp);
    if function_invoke_sites(&insns, &body.source_cp).len() != 1 {
        return false;
    }
    let returns = insns
        .iter()
        .filter(|i| matches!(i, Insn::Plain { op, .. } if (0xac..=0xb1).contains(op)))
        .count();
    returns == 1
        && matches!(insns.last(), Some(Insn::Plain { op, .. }) if (0xac..=0xb1).contains(op))
}

/// Remove kotlinc's entry `Intrinsics.checkNotNullParameter`/`checkNotNullExpressionValue` null-checks
/// (the value push + name `ldc` + the call). Shared by the splice and the spliceability check.
fn strip_param_null_checks(insns: &mut Vec<Insn>, src_cp: &[C]) {
    let mut drop = vec![false; insns.len()];
    for (i, insn) in insns.iter().enumerate() {
        if let Insn::Plain { op: 0xb8, operands } = insn {
            let idx = (operands.first().copied().unwrap_or(0) as u16) << 8
                | operands.get(1).copied().unwrap_or(0) as u16;
            if let Some(("kotlin/jvm/internal/Intrinsics", n)) = methodref_target(src_cp, idx) {
                if n == "checkNotNullParameter" || n == "checkNotNullExpressionValue" {
                    drop[i] = true;
                    if i >= 1 {
                        drop[i - 1] = true;
                    }
                    if i >= 2 {
                        drop[i - 2] = true;
                    }
                }
            }
        }
    }
    if drop.iter().any(|&d| d) {
        *insns = std::mem::take(insns)
            .into_iter()
            .zip(drop)
            .filter(|(_, d)| !d)
            .map(|(x, _)| x)
            .collect();
    }
}

/// Byte offset of each instruction in `code` (index → offset), plus a trailing `code.len()`. `None`
/// on malformed bytecode.
fn old_offsets(code: &[u8]) -> Option<Vec<usize>> {
    let mut offs = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        offs.push(pc);
        pc += instruction_len(code, pc)?;
    }
    offs.push(code.len());
    Some(offs)
}

/// Shift every branch/switch target instruction index by `delta` (used after prepending the prologue,
/// whose instructions push every body target forward).
fn shift_targets(insns: &mut [Insn], delta: usize) {
    for insn in insns {
        match insn {
            Insn::Branch { target, .. } | Insn::BranchW { target, .. } => *target += delta,
            Insn::TableSwitch {
                default, targets, ..
            } => {
                *default += delta;
                for t in targets {
                    *t += delta;
                }
            }
            Insn::LookupSwitch { default, pairs, .. } => {
                *default += delta;
                for (_, t) in pairs {
                    *t += delta;
                }
            }
            Insn::Plain { .. } => {}
        }
    }
}

/// The method's return value as a [`VType`] (`None` value ⇒ `void`), relocating a reference return's
/// class into `cw`. The outer `None` is a parse error.
fn ret_vtype(descriptor: &str, cw: &mut ClassWriter) -> Option<Option<VType>> {
    let ret = descriptor.split(')').nth(1)?;
    Some(match *ret.as_bytes().first()? {
        b'V' => None,
        b'I' | b'B' | b'S' | b'C' | b'Z' => Some(VType::Int),
        b'J' => Some(VType::Long),
        b'F' => Some(VType::Float),
        b'D' => Some(VType::Double),
        b'L' => Some(VType::Object(cw.class_ref(&ret[1..ret.len() - 1]))),
        b'[' => Some(VType::Object(cw.class_ref(ret))),
        _ => return None,
    })
}

/// Relocate a frame's verification type into `cw` (an `Object`'s `Class` pool ref). `None` for an
/// `Uninitialized`/`UninitializedThis` type (not modeled).
fn relocate_vtype(v: &VType, src_cp: &[C], cw: &mut ClassWriter) -> Option<VType> {
    Some(match v {
        VType::Object(idx) => VType::Object(relocate_const(src_cp, *idx, cw)?),
        VType::Uninit(_) | VType::UninitThis => return None,
        other => *other,
    })
}

/// The result of splicing a **branchy** body: the spliced bytes (laid out at the `start_offset` passed
/// to [`splice_unified`]) plus the relocated `StackMapTable` frames the caller must add (each: ABSOLUTE
/// byte offset, the *body* locals at that point, and the operand stack). The caller prepends its own
/// locals (slots `0..base`). The **join** is where the body's returns land (empty body locals + the
/// return value on the stack), bound by the caller right after the spliced bytes.
pub struct BranchySplice {
    pub bytes: Vec<u8>,
    /// Frames *inside* the body: (ABSOLUTE byte offset, body locals, stack). The caller prepends its own
    /// locals and binds at the offset directly.
    pub frames: Vec<(usize, Vec<VType>, Vec<VType>)>,
    /// The operand stack at the **join** (where the body's returns land = the continuation right after
    /// `bytes`): the return value, or empty for `void`. The caller binds this frame at the live
    /// post-splice position (not a precomputed end offset, which could fall at `code.len()`).
    pub join_stack: Vec<VType>,
    /// Whether the splice actually needs the relocated frames + a join frame bound (so it requires an
    /// empty operand-stack baseline). `false` for a pure BRANCHLESS body — no branches, the single
    /// trailing return dropped to fall through — which the caller can then append at ANY stack height
    /// (mid-expression), exactly like the former `splice_branchless`.
    pub join_required: bool,
    /// ABSOLUTE byte offset where each lambda argument's spliced body begins (parallel to the `lambdas`
    /// input). The caller relocates that lambda body's OWN `StackMapTable` frames relative to this (a
    /// branchy predicate body has internal branch targets).
    pub lambda_byte_starts: Vec<usize>,
    /// The host's BODY locals live at each lambda's invoke point (slots `base..`, a spliced-away lambda
    /// param → `Top`), parallel to `lambdas`. For a host with a LOOP this is the loop-body frame (the
    /// iterator/accumulator are live there), not just the parameters — the context a branchy lambda
    /// body's own frames need. Empty (the params are the only locals) for a host with no `StackMapTable`.
    pub lambda_host_locals: Vec<Vec<VType>>,
    /// The host OPERAND-STACK prefix sitting *below* each lambda's value when its body runs (parallel to
    /// `lambdas`): what the host pushed before loading the lambda — e.g. the destination collection a
    /// `map`/`filter` keeps under the lambda result, or empty for `forEach`/`fold`/`takeIf`. A branchy
    /// lambda body's own frames are compiled against an empty base, so the caller must prepend this to
    /// each of them. Computed by a typed forward simulation from the nearest host frame to the lambda
    /// load; `None` for any lambda whose prefix couldn't be modeled (the caller then bails that splice).
    pub lambda_stack_prefix: Vec<Option<Vec<VType>>>,
    /// The body's exception table, relocated into the caller: `(start, end, handler, catch_type)` as
    /// ABSOLUTE byte offsets in the spliced output, with `catch_type` re-interned into `cw` (0 =
    /// catch-all/`finally`). The handler frames themselves are already in `frames` (a handler is a
    /// StackMapTable target). Empty for a body with no handlers.
    pub handlers: Vec<(usize, usize, usize, u16)>,
}

/// One lambda argument to splice into a host body at its `FunctionN.invoke` site.
pub struct LambdaSplice {
    /// The host parameter index of this lambda (its position in the descriptor).
    pub param_index: usize,
    /// Pre-built lambda body (relocated into the target pool, locals absolute), leaving the lambda's
    /// result boxed to `Object` on the stack — exactly what the replaced `invoke` produced. Branchless
    /// (no frames) in v1.
    pub body: Vec<Insn>,
}

/// A single field-type descriptor (`I`, `J`, `Lfoo/Bar;`, `[I`, …) → its operand-stack [`VType`].
/// An object type is resolved to a `src_cp` `Class` index if one exists, else `Top` (opaque — fine
/// for a value that is consumed before the prefix is read; a surviving `Top` makes the prefix invalid
/// and the caller bails). `None` for an empty/`V` descriptor.
fn field_desc_vtype(desc: &str, src_cp: &[C]) -> Option<VType> {
    Some(match desc.as_bytes().first()? {
        b'B' | b'C' | b'I' | b'S' | b'Z' => VType::Int,
        b'J' => VType::Long,
        b'F' => VType::Float,
        b'D' => VType::Double,
        b'L' => {
            source_class_index(src_cp, &desc[1..desc.len() - 1]).map_or(VType::Top, VType::Object)
        }
        b'[' => source_class_index(src_cp, desc).map_or(VType::Top, VType::Object),
        _ => return None,
    })
}

/// The number of operand-stack entries the descriptor's parameter list pops (each `long`/`double`
/// counts as ONE entry — matching the frame stack representation, not JVM words) and the return
/// type's [`VType`] (`None` for `void`). `None` if the descriptor is malformed.
fn method_desc_effect(desc: &str, src_cp: &[C]) -> Option<(usize, Option<VType>)> {
    let close = desc.find(')')?;
    let mut args = 0usize;
    let mut b = desc[1..close].bytes().peekable();
    while let Some(c) = b.next() {
        match c {
            b'[' => continue, // array dimension — keep scanning until the element type
            b'L' => {
                while b.next()? != b';' {}
                args += 1;
            }
            b'B' | b'C' | b'I' | b'S' | b'Z' | b'F' | b'D' | b'J' => args += 1,
            _ => return None,
        }
    }
    let ret = &desc[close + 1..];
    let rv = if ret == "V" {
        None
    } else {
        Some(field_desc_vtype(ret, src_cp)?)
    };
    Some((args, rv))
}

/// Pop one "value group" off the operand stack for the `dup2`/`dup_x2`/`dup2_x1`/`dup2_x2` forms: a
/// single category-2 value (`long`/`double`, one frame entry), else the top two category-1 values.
/// Returned bottom-to-top so it can be re-pushed by `extend`.
fn pop_group(stack: &mut Vec<VType>) -> Option<Vec<VType>> {
    let top = stack.pop()?;
    if matches!(top, VType::Long | VType::Double) {
        Some(vec![top])
    } else {
        let lo = stack.pop()?;
        Some(vec![lo, top])
    }
}

/// Set local slot `idx` to `v`, growing the slot table with `Top`; a `long`/`double` also clobbers
/// its second slot to `Top` (it occupies two slots).
fn set_slot(slots: &mut Vec<VType>, idx: usize, v: VType) {
    while slots.len() <= idx + 1 {
        slots.push(VType::Top);
    }
    slots[idx] = v;
    if matches!(v, VType::Long | VType::Double) {
        slots[idx + 1] = VType::Top;
    }
}

/// The host's live state — `(slot-indexed locals, operand stack)` — just before the lambda value is
/// loaded at instruction `load_idx`. The operand stack is the prefix a branchy lambda body's frames
/// must be rebased onto; the locals are the host context (loop iterator/element/accumulator) those
/// frames need (the nearest StackMapTable frame is stale — locals assigned later in the loop body
/// aren't in it). Seeds from the nearest host frame ≤ `load_idx` (or method entry: `frame0` locals,
/// empty stack) and simulates forward over the straight-line region. Returns `None` for any opcode
/// not modeled, or if an opaque `Top` survives onto the operand stack (caller then falls back).
fn host_state_at(
    insns: &[Insn],
    load_idx: usize,
    host_frames: &[(usize, Frame)],
    frame0: &[VType],
    src_cp: &[C],
) -> Option<(Vec<VType>, Vec<VType>)> {
    let (start, locals_collapsed, mut stack) = host_frames
        .iter()
        .filter(|(i, _)| *i <= load_idx)
        .max_by_key(|(i, _)| *i)
        .map(|(i, f)| (*i, f.locals.clone(), f.stack.clone()))
        .unwrap_or((0, frame0.to_vec(), Vec::new()));
    // Expand the collapsed locals to a slot-indexed table (a `long`/`double` occupies two slots).
    let mut slots: Vec<VType> = Vec::new();
    for v in &locals_collapsed {
        slots.push(*v);
        if matches!(v, VType::Long | VType::Double) {
            slots.push(VType::Top);
        }
    }
    let is_cat2 = |v: &VType| matches!(v, VType::Long | VType::Double);
    for insn in &insns[start..load_idx] {
        let (op, operands) = match insn {
            Insn::Plain { op, operands } => (*op, operands.as_slice()),
            // A branch/switch in the region: we follow the fall-through to `load_idx`, so only the
            // operand it pops matters.
            Insn::Branch { op, .. } | Insn::BranchW { op, .. } => (*op, [].as_slice()),
            Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => {
                stack.pop()?; // the switch key
                continue;
            }
        };
        match op {
            0x00 => {}                                                    // nop
            0x01 => stack.push(VType::Null),                              // aconst_null
            0x02..=0x08 | 0x10 | 0x11 => stack.push(VType::Int),          // iconst_*/bipush/sipush
            0x09 | 0x0a => stack.push(VType::Long),                       // lconst_*
            0x0b..=0x0d => stack.push(VType::Float),                      // fconst_*
            0x0e | 0x0f => stack.push(VType::Double),                     // dconst_*
            0x12 | 0x13 => stack.push(ldc_vtype(operands, op, src_cp)?),  // ldc/ldc_w
            0x14 => stack.push(ldc2_vtype(operands, src_cp)?),            // ldc2_w
            0x15 | 0x1a..=0x1d => stack.push(VType::Int),                 // iload(_n)
            0x16 | 0x1e..=0x21 => stack.push(VType::Long),                // lload(_n)
            0x17 | 0x22..=0x25 => stack.push(VType::Float),               // fload(_n)
            0x18 | 0x26..=0x29 => stack.push(VType::Double),              // dload(_n)
            0x19 => stack.push(*slots.get(*operands.first()? as usize)?), // aload <byte>
            0x2a..=0x2d => stack.push(*slots.get((op - 0x2a) as usize)?), // aload_0..3
            0x2e | 0x30 => {
                stack.pop()?;
                stack.pop()?;
                stack.push(if op == 0x2e { VType::Int } else { VType::Float });
            } // iaload/faload
            0x2f => {
                stack.pop()?;
                stack.pop()?;
                stack.push(VType::Long);
            } // laload
            0x31 => {
                stack.pop()?;
                stack.pop()?;
                stack.push(VType::Double);
            } // daload
            0x32 => {
                stack.pop()?;
                stack.pop()?;
                stack.push(VType::Top); // aaload — element type opaque (bails if it reaches the prefix)
            }
            0x33..=0x35 => {
                stack.pop()?;
                stack.pop()?;
                stack.push(VType::Int);
            } // baload/caload/saload
            0x36..=0x3a => {
                let v = stack.pop()?;
                set_slot(&mut slots, *operands.first()? as usize, v);
            } // istore/lstore/fstore/dstore/astore <byte>
            0x3b..=0x4e => {
                let v = stack.pop()?;
                set_slot(&mut slots, ((op - 0x3b) % 4) as usize, v);
            } // i/l/f/d/astore_0..3
            0x4f..=0x56 => {
                stack.pop()?;
                stack.pop()?;
                stack.pop()?;
            } // all array stores — pop array,index,value
            0x57 => {
                stack.pop()?;
            } // pop
            0x58 => {
                let t = stack.pop()?;
                if !is_cat2(&t) {
                    stack.pop()?;
                }
            } // pop2
            0x59 => {
                let t = *stack.last()?;
                stack.push(t);
            } // dup
            0x5a => {
                let a = stack.pop()?;
                let b = stack.pop()?;
                stack.push(a);
                stack.push(b);
                stack.push(a);
            } // dup_x1
            0x5b => {
                let a = stack.pop()?; // top is always category-1 for dup_x2
                let under = pop_group(&mut stack)?;
                stack.push(a);
                stack.extend(under);
                stack.push(a);
            } // dup_x2: [..h, a] -> [.., a, h, a]
            0x5c => {
                let g = pop_group(&mut stack)?;
                stack.extend(g.iter().copied());
                stack.extend(g);
            } // dup2
            0x5d => {
                let g = pop_group(&mut stack)?;
                let u = stack.pop()?; // single category-1 value below the group
                stack.extend(g.iter().copied());
                stack.push(u);
                stack.extend(g);
            } // dup2_x1
            0x5e => {
                let g = pop_group(&mut stack)?;
                let h = pop_group(&mut stack)?;
                stack.extend(g.iter().copied());
                stack.extend(h);
                stack.extend(g);
            } // dup2_x2
            0x5f => {
                let a = stack.pop()?;
                let b = stack.pop()?;
                stack.push(a);
                stack.push(b);
            } // swap
            0x74..=0x77 => {} // ineg/lneg/fneg/dneg — pop1 push1, same type
            0x60..=0x73 => {
                // binary arithmetic: pop 2 operands (each one entry), push 1 of the same type
                let t = *stack.last()?;
                stack.pop()?;
                stack.pop()?;
                stack.push(t);
            }
            0x78..=0x83 => {
                // shifts (int shift amount) and l/i and/or/xor: pop 2 push 1
                let t = *stack.get(stack.len().checked_sub(2)?)?;
                stack.pop()?;
                stack.pop()?;
                stack.push(t);
            }
            0x84 => {} // iinc — no stack effect
            0x85..=0x93 => {
                let f = stack.pop()?;
                stack.push(num_convert(f, op)?);
            } // i2l..i2s
            0x94 => {
                stack.pop()?;
                stack.pop()?;
                stack.push(VType::Int);
            } // lcmp
            0x95..=0x98 => {
                stack.pop()?;
                stack.pop()?;
                stack.push(VType::Int);
            } // fcmp/dcmp
            0x99..=0x9e | 0xc6 | 0xc7 => {
                stack.pop()?;
            } // ifeq..ifle, ifnull/ifnonnull — pop one
            0x9f..=0xa6 => {
                stack.pop()?;
                stack.pop()?;
            } // if_icmp*/if_acmp* — pop two
            0xa7 | 0xc8 => {} // goto(_w)
            0xb2 => stack.push(fieldref_vtype(operands, src_cp)?), // getstatic
            0xb3 => {
                stack.pop()?;
            } // putstatic
            0xb4 => {
                stack.pop()?; // objectref
                stack.push(fieldref_vtype(operands, src_cp)?);
            } // getfield
            0xb5 => {
                stack.pop()?; // value
                stack.pop()?; // objectref
            } // putfield
            0xb6 | 0xb7 | 0xb9 => {
                let idx = (*operands.first()? as u16) << 8 | *operands.get(1)? as u16;
                let (args, ret) = methodref_desc_effect(src_cp, idx)?;
                for _ in 0..args {
                    stack.pop()?;
                }
                stack.pop()?; // receiver
                if let Some(r) = ret {
                    stack.push(r);
                }
            }
            0xb8 => {
                let idx = (*operands.first()? as u16) << 8 | *operands.get(1)? as u16;
                let (args, ret) = methodref_desc_effect(src_cp, idx)?;
                for _ in 0..args {
                    stack.pop()?;
                }
                if let Some(r) = ret {
                    stack.push(r);
                }
            } // invokestatic — no receiver
            0xbb => {
                let idx = (*operands.first()? as u16) << 8 | *operands.get(1)? as u16;
                stack.push(VType::Object(idx)); // new — the Class index is the source pool entry
            }
            0xbc => {
                stack.pop()?;
                stack.push(VType::Top); // newarray — primitive array type opaque
            }
            0xbd => {
                stack.pop()?;
                stack.push(VType::Top); // anewarray
            }
            0xbe => {
                stack.pop()?;
                stack.push(VType::Int);
            } // arraylength
            0xc0 => {
                stack.pop()?;
                let idx = (*operands.first()? as u16) << 8 | *operands.get(1)? as u16;
                stack.push(VType::Object(idx));
            } // checkcast — retype top to the named class
            0xc1 => {
                stack.pop()?;
                stack.push(VType::Int);
            } // instanceof
            0xc2 | 0xc3 => {
                stack.pop()?;
            } // monitorenter/monitorexit
            0xc4 => {
                // wide: a 2-byte-index load/store (or `wide iinc`, no stack effect).
                let sub = *operands.first()?;
                let idx = ((*operands.get(1)? as usize) << 8) | *operands.get(2)? as usize;
                match sub {
                    0x15 => stack.push(VType::Int),       // wide iload
                    0x16 => stack.push(VType::Long),      // wide lload
                    0x17 => stack.push(VType::Float),     // wide fload
                    0x18 => stack.push(VType::Double),    // wide dload
                    0x19 => stack.push(*slots.get(idx)?), // wide aload
                    0x36..=0x3a => {
                        let v = stack.pop()?;
                        set_slot(&mut slots, idx, v);
                    } // wide istore/lstore/fstore/dstore/astore
                    0x84 => {}                            // wide iinc — no stack effect
                    _ => return None,
                }
            }
            0xc5 => {
                let dims = *operands.get(2)? as usize; // index2 + dimensions byte
                for _ in 0..dims {
                    stack.pop()?;
                }
                stack.push(VType::Top); // array type opaque (bails if it reaches the prefix)
            } // multianewarray
            // Residual `None` is a SOUNDNESS BOUNDARY, not an unmodeled-feature gap: `invokedynamic`
            // (0xba) can't be relocated without bootstrap-method handling — the splice bails on it at
            // `relocate_insns` anyway; `athrow`/returns/`jsr`/`ret` are terminal or forbidden in v52+, so
            // they can't appear on a straight-line fall-through path before the lambda load.
            _ => return None,
        }
    }
    // A surviving opaque `Top` can't be expressed as a valid operand-stack frame entry.
    if stack.contains(&VType::Top) {
        return None;
    }
    Some((slots, stack))
}

/// Collapse a slot-indexed local table to StackMapTable frame form (a `long`/`double` is one entry;
/// its second slot — always `Top` in our tables — is dropped). The inverse of `expand_collapsed_locals`.
fn collapse_slots(slots: &[VType]) -> Vec<VType> {
    let mut out = Vec::with_capacity(slots.len());
    let mut skip = false;
    for v in slots {
        if skip {
            skip = false;
            continue;
        }
        out.push(*v);
        if matches!(v, VType::Long | VType::Double) {
            skip = true;
        }
    }
    out
}

fn ldc_vtype(operands: &[u8], op: u8, src_cp: &[C]) -> Option<VType> {
    let idx = if op == 0x12 {
        *operands.first()? as u16
    } else {
        (*operands.first()? as u16) << 8 | *operands.get(1)? as u16
    };
    Some(match src_cp.get(idx as usize)? {
        C::Integer(_) => VType::Int,
        C::Float(_) => VType::Float,
        // The exact object type only matters if it survives onto the prefix (then a `Top` bails); a
        // consumed `ldc` (e.g. the null-check message string) is fine as opaque `Top`.
        C::String(_) => {
            source_class_index(src_cp, "java/lang/String").map_or(VType::Top, VType::Object)
        }
        C::Class(_) => {
            source_class_index(src_cp, "java/lang/Class").map_or(VType::Top, VType::Object)
        }
        _ => return None,
    })
}

fn ldc2_vtype(operands: &[u8], src_cp: &[C]) -> Option<VType> {
    let idx = (*operands.first()? as u16) << 8 | *operands.get(1)? as u16;
    Some(match src_cp.get(idx as usize)? {
        C::Long(_) => VType::Long,
        C::Double(_) => VType::Double,
        _ => return None,
    })
}

fn fieldref_vtype(operands: &[u8], src_cp: &[C]) -> Option<VType> {
    let idx = (*operands.first()? as u16) << 8 | *operands.get(1)? as u16;
    match src_cp.get(idx as usize)? {
        C::Fieldref(_, nt) => {
            let (_, d) = name_and_type(src_cp, *nt)?;
            field_desc_vtype(d, src_cp)
        }
        _ => None,
    }
}

fn methodref_desc_effect(src_cp: &[C], idx: u16) -> Option<(usize, Option<VType>)> {
    match src_cp.get(idx as usize)? {
        C::Methodref(_, nt) | C::InterfaceMethodref(_, nt) => {
            let (_, d) = name_and_type(src_cp, *nt)?;
            method_desc_effect(d, src_cp)
        }
        _ => None,
    }
}

fn num_convert(_from: VType, op: u8) -> Option<VType> {
    Some(match op {
        0x85 | 0x8c | 0x8f => VType::Long,              // i2l, f2l, d2l
        0x86 | 0x89 | 0x90 => VType::Float,             // i2f, l2f, d2f
        0x87 | 0x8a | 0x8d => VType::Double,            // i2d, l2d, f2d
        0x88 | 0x8b | 0x8e | 0x91..=0x93 => VType::Int, // l2i, f2i, d2i, i2b, i2c, i2s
        _ => return None,
    })
}

/// The index of the `Class` constant in `src_cp` whose name is `name`, if any.
fn source_class_index(src_cp: &[C], name: &str) -> Option<u16> {
    src_cp
        .iter()
        .position(|c| matches!(c, C::Class(ni) if matches!(src_cp.get(*ni as usize), Some(C::Utf8(s)) if s == name)))
        .map(|p| p as u16)
}

fn source_class_or_object_index(src_cp: &[C], name: &str) -> Option<u16> {
    source_class_index(src_cp, name).or_else(|| source_class_index(src_cp, "java/lang/Object"))
}

/// Frame-0 locals of a *static* method: one [`VType`] per parameter (`long`/`double` are one entry). A
/// reference/array parameter becomes `Object(<src_cp Class index>)` so its frame type survives (a
/// `takeIf` receiver returned from the body), or `Top` if that class has no `Class` constant in the
/// source pool — then the caller requires it to be a spliced-away lambda (a dead slot), bailing otherwise.
fn param_vtypes_full(descriptor: &str, src_cp: &[C]) -> Option<Vec<VType>> {
    let inner = descriptor.strip_prefix('(')?.split(')').next()?;
    let b = inner.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        match b[i] {
            b'I' | b'B' | b'S' | b'C' | b'Z' => {
                out.push(VType::Int);
                i += 1;
            }
            b'J' => {
                out.push(VType::Long);
                i += 1;
            }
            b'D' => {
                out.push(VType::Double);
                i += 1;
            }
            b'F' => {
                out.push(VType::Float);
                i += 1;
            }
            b'L' => {
                let start = i;
                while *b.get(i)? != b';' {
                    i += 1;
                }
                let name = std::str::from_utf8(&b[start + 1..i]).ok()?;
                out.push(
                    source_class_or_object_index(src_cp, name).map_or(VType::Top, VType::Object),
                );
                i += 1;
            }
            b'[' => {
                let start = i;
                i += 1;
                while *b.get(i)? == b'[' {
                    i += 1;
                }
                if *b.get(i)? == b'L' {
                    while *b.get(i)? != b';' {
                        i += 1;
                    }
                }
                i += 1;
                let name = std::str::from_utf8(&b[start..i]).ok()?;
                out.push(
                    source_class_or_object_index(src_cp, name).map_or(VType::Top, VType::Object),
                );
            }
            _ => return None,
        }
    }
    Some(out)
}

fn param_vtypes_target(descriptor: &str, cw: &mut ClassWriter) -> Option<Vec<VType>> {
    let inner = descriptor.strip_prefix('(')?.split(')').next()?;
    let b = inner.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        match b[i] {
            b'I' | b'B' | b'S' | b'C' | b'Z' => {
                out.push(VType::Int);
                i += 1;
            }
            b'J' => {
                out.push(VType::Long);
                i += 1;
            }
            b'D' => {
                out.push(VType::Double);
                i += 1;
            }
            b'F' => {
                out.push(VType::Float);
                i += 1;
            }
            b'L' => {
                let start = i;
                while *b.get(i)? != b';' {
                    i += 1;
                }
                let name = std::str::from_utf8(&b[start + 1..i]).ok()?;
                out.push(VType::Object(cw.class_ref(name)));
                i += 1;
            }
            b'[' => {
                let start = i;
                i += 1;
                while *b.get(i)? == b'[' {
                    i += 1;
                }
                if *b.get(i)? == b'L' {
                    while *b.get(i)? != b';' {
                        i += 1;
                    }
                }
                i += 1;
                let name = std::str::from_utf8(&b[start..i]).ok()?;
                out.push(VType::Object(cw.class_ref(name)));
            }
            _ => return None,
        }
    }
    Some(out)
}

/// The local slot of each parameter in the original (static) method — `long`/`double` take two. Used to
/// locate a lambda parameter's `aload <slot>` before relocation/shifting.
fn param_offsets(descriptor: &str) -> Option<Vec<u16>> {
    let inner = descriptor.strip_prefix('(')?.split(')').next()?;
    let b = inner.as_bytes();
    let mut i = 0;
    let mut slot = 0u16;
    let mut out = Vec::new();
    while i < b.len() {
        out.push(slot);
        match b[i] {
            b'J' | b'D' => {
                slot += 2;
                i += 1;
            }
            b'L' => {
                while *b.get(i)? != b';' {
                    i += 1;
                }
                i += 1;
                slot += 1;
            }
            b'[' => {
                while *b.get(i)? == b'[' {
                    i += 1;
                }
                if *b.get(i)? == b'L' {
                    while *b.get(i)? != b';' {
                        i += 1;
                    }
                }
                i += 1;
                slot += 1;
            }
            _ => {
                slot += 1;
                i += 1;
            }
        }
    }
    Some(out)
}

/// THE unified inline splice. Relocates a (possibly branchy) host `inline fun` body into the caller,
/// replacing each zero-arg lambda-parameter `Function0.invoke` site with that lambda's pre-built body.
/// Subsumes the special cases: no lambdas + no branches → a single fall-through segment (like
/// [`splice_branchless`]); branches + no lambdas (the former `splice_branchy`); one lambda + no branches
/// (the former `branchless_lambda_segments`). The caller emits the non-lambda arguments first (empty
/// baseline otherwise) and binds the returned frames + the join frame. `None` on an unsupported shape
/// (exception handlers, reified, an unparseable body) ⇒ the caller falls back / skips, never miscompiles.
pub fn splice_unified(
    body: &MethodCode,
    descriptor: &str,
    base: u16,
    lambdas: &[LambdaSplice],
    start_offset: usize,
    cw: &mut ClassWriter,
) -> Option<BranchySplice> {
    // A `reifiedOperationMarker` body needs its reified type substituted (`substitute_reified`), which
    // requires the call's reified type arguments — a separate concern from frame/operand-stack splicing
    // and orthogonal to this path (the IR inliner skips reified fns; reified intrinsics like `emptyArray`
    // go through the synthetics registry). Unreached by the lambda/host splicer (0 corpus occurrences).
    if is_reified_inline(body) {
        return None;
    }
    let ret = ret_vtype(descriptor, cw)?;
    let offsets_of_param = param_offsets(descriptor)?;
    let mut insns = disassemble(&body.code)?;
    // `assert` is a codegen INTRINSIC, not a normal inline: kotlinc guards it on a synthetic per-class
    // `$assertionsDisabled` field (or elides it per `-Xassertions`/`ASSERTIONS_MODE`), and when disabled
    // does NOT even evaluate the argument. Splicing its library body (which reads `kotlin/_Assertions.
    // ENABLED`) reproduces neither — refuse a body that READS such a field (this method, not the whole
    // class pool, which `require`/`check` share with `assert`), leaving the call unresolved (skip).
    if insns.iter().any(|i| {
        let Insn::Plain { op: 0xb2, operands } = i else {
            return false;
        }; // getstatic
        let Some(idx) = operands
            .first()
            .zip(operands.get(1))
            .map(|(a, b)| (*a as u16) << 8 | *b as u16)
        else {
            return false;
        };
        matches!(body.source_cp.get(idx as usize), Some(C::Fieldref(ci, _))
            if class_name(&body.source_cp, *ci).is_some_and(|n| n.contains("_Assertions")))
    }) {
        return None;
    }
    let old_off = old_offsets(&body.code)?;
    // Decode the host frames against the ORIGINAL body, keyed by old instruction index. A reference
    // parameter is `Top` in frame0 (its real type is unmodeled), so EVERY reference parameter must be a
    // spliced-away lambda (a dead slot) — otherwise a frame that keeps it live would be wrong; bail.
    let host_frames: Vec<(usize, Frame)> = match body.stackmap.as_ref() {
        Some(sm) => {
            let frame0 = param_vtypes_full(descriptor, &body.source_cp)?;
            for (pi, v) in frame0.iter().enumerate() {
                if *v == VType::Top && !lambdas.iter().any(|l| l.param_index == pi) {
                    return None; // an unresolved non-lambda reference parameter — can't model its frame
                }
            }
            decode_stackmap(sm, frame0)?
                .into_iter()
                .map(|f| old_off.iter().position(|&o| o == f.offset).map(|i| (i, f)))
                .collect::<Option<Vec<_>>>()?
        }
        None => Vec::new(),
    };

    // Collect edits over the host instruction list (resolved against the SOURCE pool, BEFORE relocation
    // rewrites the indices), sorted by start index, non-overlapping:
    //  • delete each entry `checkNotNullParameter`/`…ExpressionValue` triplet (aload/ldc/invokestatic);
    //  • replace each lambda's `aload <slot>; invokeinterface Function0.invoke` with its body.
    struct Edit {
        at: usize,
        len: usize,
        repl: Vec<Insn>,
    }
    let mut edits: Vec<Edit> = Vec::new();
    for (i, insn) in insns.iter().enumerate() {
        if let Insn::Plain { op: 0xb8, operands } = insn {
            if operands.len() == 2 {
                let idx = (operands[0] as u16) << 8 | operands[1] as u16;
                if let Some(("kotlin/jvm/internal/Intrinsics", n)) =
                    methodref_target(&body.source_cp, idx)
                {
                    if (n == "checkNotNullParameter" || n == "checkNotNullExpressionValue")
                        && i >= 2
                    {
                        edits.push(Edit {
                            at: i - 2,
                            len: 3,
                            repl: Vec::new(),
                        });
                    }
                }
            }
        }
    }
    // The body must contain exactly one `FunctionN.invoke` per lambda argument — otherwise matching each
    // lambda to its invoke site (and which `aload` feeds it) is ambiguous, and a mis-paired splice calls
    // `.invoke` on the wrong object. Conservative: bail unless the counts line up (skips complex stdlib
    // HOFs that call a lambda more than once or alongside other functional values).
    let invoke_count = insns
        .iter()
        .filter(|insn| {
            matches!(insn, Insn::Plain { op: 0xb9, operands } if operands.len() == 4
                && methodref_target(&body.source_cp, (operands[0] as u16) << 8 | operands[1] as u16)
                    .is_some_and(|(cls, n)| n == "invoke" && cls.starts_with("kotlin/jvm/functions/Function")))
        })
        .count();
    if invoke_count != lambdas.len() {
        return None;
    }
    // Indices already consumed by a null-check deletion (its `aload <lambda>` doesn't count as a use).
    let deleted: std::collections::HashSet<usize> =
        edits.iter().flat_map(|e| e.at..e.at + e.len).collect();
    let mut lambda_sites: Vec<usize> = Vec::with_capacity(lambdas.len()); // invoke index per lambda
    let mut lambda_loads: Vec<usize> = Vec::with_capacity(lambdas.len()); // aload index per lambda
    for lam in lambdas {
        let orig_slot = *offsets_of_param.get(lam.param_index)?;
        // The lambda parameter is loaded exactly once (the receiver of its `FunctionN.invoke`); for an
        // N-ary lambda its `aload` is NOT adjacent to the invoke — the lambda's argument expressions sit
        // between (`block.invoke(this)` = `aload block; aload this; invoke`). Locate that single load
        // (ignoring the entry null-check's load, already slated for deletion) and the `FunctionN.invoke`
        // site after it, then DELETE the load (the closure object is gone) and REPLACE the invoke with
        // the lambda body (which consumes the on-stack arguments).
        let load_idx = {
            let mut found = None;
            for (i, insn) in insns.iter().enumerate() {
                if !deleted.contains(&i) && is_aload_of(insn, orig_slot) {
                    if found.is_some() {
                        return None; // loaded more than once — used in a way we don't model
                    }
                    found = Some(i);
                }
            }
            found?
        };
        let site = insns
            .iter()
            .enumerate()
            .skip(load_idx + 1)
            .find_map(|(i, insn)| {
                let Insn::Plain { op: 0xb9, operands } = insn else {
                    return None;
                };
                let idx = (*operands.first()? as u16) << 8 | *operands.get(1)? as u16;
                let (cls, name) = methodref_target(&body.source_cp, idx)?;
                (name == "invoke" && cls.starts_with("kotlin/jvm/functions/Function")).then_some(i)
            })?;
        edits.push(Edit {
            at: load_idx,
            len: 1,
            repl: Vec::new(),
        }); // delete the dead lambda-object load
        edits.push(Edit {
            at: site,
            len: 1,
            repl: lam.body.clone(),
        }); // replace the invoke with the lambda body
        lambda_sites.push(site);
        lambda_loads.push(load_idx);
    }
    // The host's live state (locals + operand-stack prefix) below each lambda, simulated on the ORIGINAL
    // source-pool insns/frames (before relocation rewrites indices/pool refs). `None` per lambda whose
    // state couldn't be modeled.
    let prefix_frame0 = param_vtypes_full(descriptor, &body.source_cp).unwrap_or_default();
    let host_states: Vec<Option<(Vec<VType>, Vec<VType>)>> = lambda_loads
        .iter()
        .map(|&li| host_state_at(&insns, li, &host_frames, &prefix_frame0, &body.source_cp))
        .collect();
    // A BRANCHY lambda body has its own frames, compiled against an empty operand base; they must be
    // rebased onto the host state. If that state couldn't be modeled, bail (a BRANCHLESS body has no
    // frames, so its unmodeled state is irrelevant). The caller then falls back to a real call.
    for (k, lam) in lambdas.iter().enumerate() {
        let branchy = lam.body.iter().any(|i| !matches!(i, Insn::Plain { .. }));
        if branchy && host_states[k].is_none() {
            return None;
        }
    }
    relocate_insns(&mut insns, &body.source_cp, cw)?;
    shift_locals(&mut insns, base)?;
    // Return handling: DROP a trailing return (fall through with the result on the stack), and redirect
    // any earlier return to the join (`goto` past the body). A pure BRANCHLESS body — no branches, a
    // single trailing return dropped — then needs NO frames/join, so the caller may splice it at ANY
    // operand-stack height (mid-expression), exactly like the former `splice_branchless`. A branchy body
    // (`require`'s `ifne`) or a non-trailing return needs the join frame ⇒ an empty baseline.
    let host_has_branches = insns.iter().any(|i| !matches!(i, Insn::Plain { .. }));
    let synthesize_empty_branch_frames = host_has_branches && body.stackmap.is_none();
    let last_idx = insns.len().saturating_sub(1);
    let join_pos = insns.len();
    let mut made_goto = false;
    for (i, insn) in insns.iter_mut().enumerate() {
        if let Insn::Plain { op, .. } = insn {
            if matches!(*op, 0xac..=0xb1) && i != last_idx {
                *insn = Insn::Branch {
                    op: 0xa7,
                    target: join_pos,
                };
                made_goto = true;
            }
        }
    }
    if matches!(insns.last(), Some(Insn::Plain { op, .. }) if matches!(op, 0xac..=0xb1)) {
        edits.push(Edit {
            at: last_idx,
            len: 1,
            repl: Vec::new(),
        }); // drop the trailing return → fall through
    }
    let join_required = host_has_branches || made_goto || !host_frames.is_empty();
    edits.sort_by_key(|e| e.at);
    // Reject overlapping edits (shouldn't happen for the shapes above).
    for w in edits.windows(2) {
        if w[0].at + w[0].len > w[1].at {
            return None;
        }
    }

    // Pass 1: old index → new index (consumed indices collapse to their replacement's start).
    let mut old2new = vec![0usize; insns.len() + 1];
    {
        let mut pos = 0usize;
        let mut i = 0usize;
        let mut e = 0usize;
        while i < insns.len() {
            if e < edits.len() && edits[e].at == i {
                for k in 0..edits[e].len {
                    old2new[i + k] = pos;
                }
                pos += edits[e].repl.len();
                i += edits[e].len;
                e += 1;
            } else {
                old2new[i] = pos;
                pos += 1;
                i += 1;
            }
        }
        old2new[insns.len()] = pos;
    }
    // Pass 2: build the merged list, remapping every host branch target through `old2new`. Replacement
    // (lambda) instructions are branchless, so they carry no targets to remap.
    let mut merged: Vec<Insn> = Vec::new();
    {
        let mut i = 0usize;
        let mut e = 0usize;
        while i < insns.len() {
            if e < edits.len() && edits[e].at == i {
                // A replacement (lambda body) carries its OWN internal branch targets (instruction
                // indices within the lambda body). Shift them by `p0`, the lambda body's start position
                // in `merged`, so a branchy predicate body's branches resolve in the merged stream.
                let p0 = merged.len();
                for mut insn in edits[e].repl.iter().cloned() {
                    match &mut insn {
                        Insn::Branch { target, .. } | Insn::BranchW { target, .. } => *target += p0,
                        Insn::TableSwitch {
                            default, targets, ..
                        } => {
                            *default += p0;
                            for t in targets {
                                *t += p0;
                            }
                        }
                        Insn::LookupSwitch { default, pairs } => {
                            *default += p0;
                            for (_, t) in pairs {
                                *t += p0;
                            }
                        }
                        Insn::Plain { .. } => {}
                    }
                    merged.push(insn);
                }
                i += edits[e].len;
                e += 1;
            } else {
                let mut insn = insns[i].clone();
                match &mut insn {
                    Insn::Branch { target, .. } | Insn::BranchW { target, .. } => {
                        *target = old2new[*target]
                    }
                    Insn::TableSwitch {
                        default, targets, ..
                    } => {
                        *default = old2new[*default];
                        for t in targets {
                            *t = old2new[*t];
                        }
                    }
                    Insn::LookupSwitch { default, pairs } => {
                        *default = old2new[*default];
                        for (_, t) in pairs {
                            *t = old2new[*t];
                        }
                    }
                    Insn::Plain { .. } => {}
                }
                merged.push(insn);
                i += 1;
            }
        }
    }

    // Prologue: store each NON-lambda argument (already on the stack, top = last) into its slot.
    let stores = param_store_ops(descriptor, base)?;
    let lambda_slots: std::collections::HashSet<u16> = lambdas
        .iter()
        .filter_map(|l| offsets_of_param.get(l.param_index).map(|o| base + o))
        .collect();
    let prologue: Vec<Insn> = stores
        .iter()
        .rev()
        .filter(|(slot, _)| !lambda_slots.contains(slot))
        .map(|&(slot, op)| local_load_store(op, slot))
        .collect();
    let p = prologue.len();
    shift_targets(&mut merged, p);
    let mut final_insns = prologue;
    final_insns.extend(merged);
    // Lay out at the body's REAL method offset so a `tableswitch`/`lookupswitch` (e.g. `toList`'s
    // `when (size)`) pads correctly; returned frame/lambda offsets are then absolute.
    let offs = insn_offsets_at(&final_insns, start_offset);

    // Relocate the host frames: remap old index → new (+prologue), drop each spliced-away lambda slot
    // (its local is now dead → `Top`), and relocate the verification types into `cw`.
    let lambda_entry: std::collections::HashSet<usize> =
        lambdas.iter().map(|l| l.param_index).collect();
    let mut frames = Vec::with_capacity(host_frames.len() + 1);
    for (old_idx, f) in &host_frames {
        let new_idx = old2new[*old_idx] + p;
        let locals = f
            .locals
            .iter()
            .enumerate()
            .map(|(k, v)| {
                if lambda_entry.contains(&k) {
                    Some(VType::Top) // the lambda param is spliced away — its slot is dead
                } else {
                    relocate_vtype(v, &body.source_cp, cw)
                }
            })
            .collect::<Option<Vec<_>>>()?;
        // The spliced-away lambda's `aload` is deleted, so its FunctionN value no longer sits on
        // the operand stack at any host frame between the load and the (now replaced) invoke; drop
        // it from the frame stack so the relocated frame matches the post-splice operand stack.
        let stack = f
            .stack
            .iter()
            .filter(|v| {
                !matches!(v, VType::Object(idx)
                    if class_name(&body.source_cp, *idx)
                        .is_some_and(|n| n.starts_with("kotlin/jvm/functions/Function")))
            })
            .map(|v| relocate_vtype(v, &body.source_cp, cw))
            .collect::<Option<Vec<_>>>()?;
        frames.push((offs[new_idx], locals, stack));
    }
    if synthesize_empty_branch_frames {
        let locals = param_vtypes_target(descriptor, cw)?;
        let mut targets = std::collections::BTreeSet::new();
        for insn in &final_insns {
            match insn {
                Insn::Branch { target, .. } | Insn::BranchW { target, .. } => {
                    targets.insert(*target);
                }
                Insn::TableSwitch {
                    default,
                    targets: ts,
                    ..
                } => {
                    targets.insert(*default);
                    targets.extend(ts.iter().copied());
                }
                Insn::LookupSwitch { default, pairs } => {
                    targets.insert(*default);
                    targets.extend(pairs.iter().map(|(_, t)| *t));
                }
                Insn::Plain { .. } => {}
            }
        }
        for target in targets {
            if target < final_insns.len() {
                let off = offs[target];
                if !frames.iter().any(|(existing, _, _)| *existing == off) {
                    frames.push((off, locals.clone(), Vec::new()));
                }
            }
        }
    }
    // A DIVERGING spliced lambda body ends in a `*return`/`athrow` (a non-local return — `repeat { return
    // … }`): the host's post-invoke continuation (e.g. a loop back-edge / exit) is then unreachable, and
    // the verifier can't fall through the return, so it needs a stack-map frame there. Synthesize one from
    // the host state at the invoke plus the (dropped) `FunctionN.invoke` result, so the dead continuation
    // still verifies. (Without this the splice would emit a frameless target → `VerifyError`.)
    for (k, lam) in lambdas.iter().enumerate() {
        let diverges = matches!(
            lam.body.last(),
            Some(Insn::Plain { op, .. }) if matches!(op, 0xac..=0xb1 | 0xbf)
        );
        if !diverges {
            continue;
        }
        let Some((locals, stack)) = host_states[k].clone() else {
            continue;
        };
        let cont_old = lambda_sites[k] + 1;
        if cont_old >= insns.len() {
            continue; // the diverging body is the last instruction — no continuation to frame
        }
        let cont_off = offs[old2new[cont_old] + p];
        if frames.iter().any(|(o, _, _)| *o == cont_off) {
            continue; // already framed (a branch target)
        }
        let mut rl = Vec::with_capacity(locals.len());
        for v in &locals {
            rl.push(relocate_vtype(v, &body.source_cp, cw)?);
        }
        let mut rs = Vec::with_capacity(stack.len() + 1);
        for v in &stack {
            rs.push(relocate_vtype(v, &body.source_cp, cw)?);
        }
        rs.push(VType::Object(cw.class_ref("java/lang/Object"))); // the dropped invoke result
        frames.push((cont_off, rl, rs));
    }
    // Relocate the exception table: each entry's `start`/`end`/`handler` are byte offsets into the
    // ORIGINAL code — map each to its instruction index (`old_off`), through `old2new` (+ prologue `p`)
    // to the spliced instruction, then to its absolute byte offset (`offs`). `catch_type` is re-interned
    // into `cw` (0 = catch-all/`finally`). The handler's own frame is already in `frames` (it is a
    // StackMapTable target). `end` may equal the code length — `old_off` includes that boundary.
    let byte_to_abs = |bp: u16| -> Option<usize> {
        let old_idx = old_off.iter().position(|&o| o == bp as usize)?;
        offs.get(old2new[old_idx] + p).copied()
    };
    let mut handlers = Vec::with_capacity(body.handlers.len());
    for h in &body.handlers {
        let start = byte_to_abs(h.start_pc)?;
        let end = byte_to_abs(h.end_pc)?;
        let handler = byte_to_abs(h.handler_pc)?;
        let catch_type = if h.catch_type == 0 {
            0
        } else {
            relocate_const(&body.source_cp, h.catch_type, cw)?
        };
        handlers.push((start, end, handler, catch_type));
    }
    // Byte offset where each lambda's spliced body begins (= its replaced invoke's new position): the
    // caller binds that lambda body's own (branchy) frames relative to this.
    let lambda_byte_starts: Vec<usize> = lambda_sites
        .iter()
        .map(|&site| offs[p + old2new[site]])
        .collect();
    // The host's live body locals at each lambda's invoke point — the host frame (decoded, before
    // relocation) with the largest old index ≤ the invoke. For a loop host that's the loop-body frame
    // (iterator/accumulator live), the context a branchy lambda body's frames need. Empty if no frame
    // precedes the invoke (the caller then uses the parameters).
    // Fallback context when no host frame precedes the invoke (`takeIf` — the invoke is before the first
    // branch): the method's parameters (`base..`, a lambda param dead → `Top`).
    let param_ctx: Vec<VType> = param_vtypes_full(descriptor, &body.source_cp)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(k, v)| {
            if lambda_entry.contains(&k) {
                VType::Top
            } else {
                v
            }
        })
        .collect();
    // The host's live locals + operand-stack prefix at each lambda's invoke, from the forward simulation
    // (`host_states`): the loop-body context a branchy lambda body's frames need. The locals are collapsed
    // to frame form, the spliced-away lambda slot blanked to `Top`, and both relocated into `cw`. A `None`
    // state only occurs for a BRANCHLESS lambda (its frames/prefix are unused) → the `param_ctx` filler.
    let mut lambda_host_locals: Vec<Vec<VType>> = Vec::with_capacity(host_states.len());
    let mut lambda_stack_prefix: Vec<Option<Vec<VType>>> = Vec::with_capacity(host_states.len());
    for st in &host_states {
        match st {
            Some((slots, stack)) => {
                lambda_host_locals.push(
                    collapse_slots(slots)
                        .iter()
                        .enumerate()
                        .map(|(k, v)| {
                            if lambda_entry.contains(&k) {
                                Some(VType::Top)
                            } else {
                                relocate_vtype(v, &body.source_cp, cw)
                            }
                        })
                        .collect::<Option<Vec<_>>>()?,
                );
                lambda_stack_prefix.push(
                    stack
                        .iter()
                        .map(|v| relocate_vtype(v, &body.source_cp, cw))
                        .collect::<Option<Vec<_>>>(),
                );
            }
            None => {
                lambda_host_locals.push(param_ctx.clone());
                lambda_stack_prefix.push(None);
            }
        }
    }
    Some(BranchySplice {
        bytes: assemble_at(&final_insns, start_offset),
        frames,
        join_stack: ret.into_iter().collect(),
        join_required,
        lambda_byte_starts,
        lambda_host_locals,
        lambda_stack_prefix,
        handlers,
    })
}

pub fn splice(
    body: &MethodCode,
    descriptor: &str,
    base: u16,
    type_map: &HashMap<String, String>,
    cw: &mut ClassWriter,
) -> Option<Vec<Insn>> {
    let mut insns = disassemble(&body.code)?;
    // Reified first (nops the marker region) so its now-dead ldc isn't needlessly relocated.
    let patches = substitute_reified(&mut insns, &body.source_cp, cw, type_map);
    relocate_insns(&mut insns, &body.source_cp, cw)?;
    for (j, idx) in patches {
        set_pool_operand(&mut insns[j], idx);
    }
    shift_locals(&mut insns, base)?;
    redirect_returns(&mut insns);
    // Prologue: pop the arguments (top = last param) into their slots, declaration order reversed.
    let params = param_store_ops(descriptor, base)?;
    let mut out: Vec<Insn> = params
        .iter()
        .rev()
        .map(|&(slot, op)| local_load_store(op, slot))
        .collect();
    out.extend(insns);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_unified_branchless_drops_return_and_stores_args() {
        // Body of `inline fun triple(x: Int): Int = x * 3` — `iload_0; iconst_3; imul; ireturn`.
        let body = MethodCode {
            max_stack: 2,
            max_locals: 1,
            code: vec![0x1a, 0x06, 0x68, 0xac],
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
        };
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let bs = splice_unified(&body, "(I)I", 3, &[], 0, &mut cw).expect("branchless splice");
        // Prologue stores the one arg into slot 3, then the body runs with no trailing return.
        // istore_3 ; iload_3 ; iconst_3 ; imul   (compact slot-3 forms; the `ireturn` is dropped)
        assert_eq!(bs.bytes, vec![0x3e, 0x1d, 0x06, 0x68]);
        // A pure branchless body needs no join frame — appendable at any operand-stack height.
        assert!(!bs.join_required);
    }

    #[test]
    fn finds_function_invoke_sites() {
        // pool: Function1.invoke(Object)Object as an InterfaceMethodref, + an unrelated Methodref.
        let cp = vec![
            C::Other,
            C::Utf8("kotlin/jvm/functions/Function1".into()), // 1
            C::Class(1),                                      // 2
            C::Utf8("invoke".into()),                         // 3
            C::Utf8("(Ljava/lang/Object;)Ljava/lang/Object;".into()), // 4
            C::NameAndType(3, 4),                             // 5
            C::InterfaceMethodref(2, 5),                      // 6
            C::Utf8("java/util/Iterator".into()),             // 7
            C::Class(7),                                      // 8
            C::Utf8("next".into()),                           // 9
            C::Utf8("()Ljava/lang/Object;".into()),           // 10
            C::NameAndType(9, 10),                            // 11
            C::InterfaceMethodref(8, 11),                     // 12
        ];
        // aload_1 ; invokeinterface Iterator.next #12 ; aload_2 ; invokeinterface Function1.invoke #6 ; pop
        let insns = vec![
            Insn::Plain {
                op: 0x2b,
                operands: vec![],
            },
            Insn::Plain {
                op: 0xb9,
                operands: vec![0x00, 0x0c, 0x01, 0x00],
            },
            Insn::Plain {
                op: 0x2c,
                operands: vec![],
            },
            Insn::Plain {
                op: 0xb9,
                operands: vec![0x00, 0x06, 0x02, 0x00],
            },
            Insn::Plain {
                op: 0x57,
                operands: vec![],
            },
        ];
        assert_eq!(function_invoke_sites(&insns, &cp), vec![3]);
    }

    #[test]
    fn method_desc_effect_counts_args_and_return() {
        let cp = vec![C::Other];
        // (Object, int) -> boolean : 2 arg entries, Int return.
        assert_eq!(
            method_desc_effect("(Ljava/lang/Object;I)Z", &cp),
            Some((2, Some(VType::Int)))
        );
        // (long, double) -> void : 2 arg entries (a cat-2 is ONE stack entry), no return.
        assert_eq!(method_desc_effect("(JD)V", &cp), Some((2, None)));
        // ([I, String) -> long : 2 arg entries, Long return.
        assert_eq!(
            method_desc_effect("([ILjava/lang/String;)J", &cp),
            Some((2, Some(VType::Long)))
        );
        assert_eq!(method_desc_effect("()V", &cp), Some((0, None)));
    }

    #[test]
    fn collapse_slots_is_inverse_of_expand() {
        // [Object, <long>, Top(2nd half), Int] collapses to [Object, Long, Int]; a standalone `Top`
        // (a genuinely uninitialized slot, not a cat-2 tail) is preserved.
        let slots = vec![
            VType::Object(5),
            VType::Long,
            VType::Top,
            VType::Int,
            VType::Top,
        ];
        assert_eq!(
            collapse_slots(&slots),
            vec![VType::Object(5), VType::Long, VType::Int, VType::Top]
        );
    }

    #[test]
    fn host_state_at_computes_loop_prefix_and_locals() {
        // A `map`-shaped fragment: a loop-body frame establishes [dest:Collection in slot 4], then the
        // body stores the iterated element to slot 7 and pushes the destination before loading the lambda.
        // At the lambda load the operand-stack prefix is [Collection] and slot 7 is live as Integer.
        let cp = vec![
            C::Other,
            C::Utf8("java/util/Collection".into()), // 1
            C::Class(1),                            // 2
            C::Utf8("java/lang/Integer".into()),    // 3
            C::Class(3),                            // 4
        ];
        // frame at index 0: locals slot4 = Collection (others Top), empty stack.
        let frame = Frame {
            offset: 0,
            locals: vec![
                VType::Top,
                VType::Top,
                VType::Top,
                VType::Top,
                VType::Object(2),
            ],
            stack: vec![],
        };
        // insns: [0] astore_? no — model: aload? We seed at idx0 and walk to load_idx.
        //   0: aconst_null            (stand-in element ref on stack)
        //   1: astore 7               (element → slot 7; here typed Null, but checkcast retypes below)
        //   2: aload 7                (push element)
        //   3: checkcast Integer      (retype top to Integer)
        //   4: astore 7               (element:Integer → slot 7)
        //   5: aload 4                (push dest Collection)  ← prefix entry
        //   6: <lambda load>          (load_idx = 6)
        let insns = vec![
            Insn::Plain {
                op: 0x01,
                operands: vec![],
            }, // aconst_null
            Insn::Plain {
                op: 0x3a,
                operands: vec![7],
            }, // astore 7
            Insn::Plain {
                op: 0x19,
                operands: vec![7],
            }, // aload 7
            Insn::Plain {
                op: 0xc0,
                operands: vec![0x00, 0x04],
            }, // checkcast Integer
            Insn::Plain {
                op: 0x3a,
                operands: vec![7],
            }, // astore 7
            Insn::Plain {
                op: 0x19,
                operands: vec![4],
            }, // aload 4 (dest)
            Insn::Plain {
                op: 0x19,
                operands: vec![9],
            }, // (the lambda load — index 6)
        ];
        let (slots, stack) = host_state_at(&insns, 6, &[(0, frame)], &[], &cp).expect("state");
        assert_eq!(stack, vec![VType::Object(2)]); // prefix = [Collection]
        assert_eq!(slots[7], VType::Object(4)); // element local retyped to Integer
        assert_eq!(slots[4], VType::Object(2)); // dest local still Collection
    }

    #[test]
    fn host_state_at_models_dup_family() {
        // Seed an empty frame; push three distinct cat-1 refs then `dup2_x1`, and stop at the load.
        //   aconst_null(Null) ; ldc-ish via iconst → use checkcast to make refs distinguishable.
        // Simpler: iconst_1(Int=c), aconst_null(Null=b), aconst_null then checkcast Integer (a),
        //   dup2_x1  → [c, a?, ...]. We assert the resulting stack matches the spec form 1.
        let cp = vec![
            C::Other,
            C::Utf8("java/lang/Integer".into()), // 1
            C::Class(1),                         // 2
        ];
        // 0: iconst_1            [Int]
        // 1: aconst_null         [Int, Null]
        // 2: aconst_null         [Int, Null, Null]
        // 3: checkcast Integer   [Int, Null, Object]   (top retyped)
        // 4: dup2_x1             [Int]→ form1: [Null, Object, Int, Null, Object]
        // 5: nop  (load_idx = 5)
        let insns = vec![
            Insn::Plain {
                op: 0x04,
                operands: vec![],
            }, // iconst_1
            Insn::Plain {
                op: 0x01,
                operands: vec![],
            }, // aconst_null
            Insn::Plain {
                op: 0x01,
                operands: vec![],
            }, // aconst_null
            Insn::Plain {
                op: 0xc0,
                operands: vec![0x00, 0x02],
            }, // checkcast Integer
            Insn::Plain {
                op: 0x5d,
                operands: vec![],
            }, // dup2_x1
            Insn::Plain {
                op: 0x00,
                operands: vec![],
            }, // nop
        ];
        // dup2_x1 form1 over [Int, Null, Object]: top group=[Null,Object], under=Int →
        //   [Null, Object, Int, Null, Object]. But a surviving `Top`? none here — all typed.
        let frame = Frame {
            offset: 0,
            locals: vec![],
            stack: vec![],
        };
        let (_, stack) = host_state_at(&insns, 5, &[(0, frame)], &[], &cp).expect("state");
        assert_eq!(
            stack,
            vec![
                VType::Null,
                VType::Object(2),
                VType::Int,
                VType::Null,
                VType::Object(2)
            ]
        );
    }

    #[test]
    fn host_state_at_bails_on_surviving_opaque() {
        // An `aaload` pushes an opaque element type; if it survives onto the prefix the state is unusable.
        let cp = vec![C::Other];
        let insns = vec![
            Insn::Plain {
                op: 0x01,
                operands: vec![],
            }, // aconst_null (array)
            Insn::Plain {
                op: 0x03,
                operands: vec![],
            }, // iconst_0 (index)
            Insn::Plain {
                op: 0x32,
                operands: vec![],
            }, // aaload → Top on stack
            Insn::Plain {
                op: 0x00,
                operands: vec![],
            }, // nop (load_idx = 3, Top still on stack)
        ];
        assert_eq!(host_state_at(&insns, 3, &[], &[], &cp), None);
    }

    #[test]
    fn decode_stackmap_append_and_full() {
        // count=2:
        //   APPEND(252) delta=5, +1 local Integer
        //   FULL(255)  delta=3, locals=[Object #9, Long], stack=[Int]
        let bytes = vec![
            0x00, 0x02, 252, 0x00, 0x05, 0x01, 255, 0x00, 0x03, 0x00, 0x02, 0x07, 0x00, 0x09, 0x04,
            0x00, 0x01, 0x01,
        ];
        let frames = decode_stackmap(&bytes, vec![VType::Int]).unwrap();
        assert_eq!(frames[0].offset, 5); // -1 + 5 + 1
        assert_eq!(frames[0].locals, vec![VType::Int, VType::Int]);
        assert_eq!(frames[0].stack, vec![]);
        assert_eq!(frames[1].offset, 9); // 5 + 3 + 1
        assert_eq!(frames[1].locals, vec![VType::Object(9), VType::Long]);
        assert_eq!(frames[1].stack, vec![VType::Int]);
    }

    #[test]
    fn decode_stackmap_chop_and_same_stack() {
        // count=2:
        //   SAME_LOCALS_1_STACK_ITEM tag=66 (delta=2), stack=Float
        //   CHOP(250) delta=4 → remove 1 local
        let bytes = vec![0x00, 0x02, 66, 0x02, 250, 0x00, 0x04];
        let frames = decode_stackmap(&bytes, vec![VType::Int, VType::Long]).unwrap();
        assert_eq!(frames[0].offset, 2);
        assert_eq!(frames[0].locals, vec![VType::Int, VType::Long]);
        assert_eq!(frames[0].stack, vec![VType::Float]);
        assert_eq!(frames[1].offset, 7); // 2 + 4 + 1
        assert_eq!(frames[1].locals, vec![VType::Int]); // chopped the Long
        assert_eq!(frames[1].stack, vec![]);
    }

    #[test]
    fn splice_unified_synthesizes_branch_frames_without_stackmap() {
        // `if (x != 0) 1 else 0` as a tiny inline body without a source `StackMapTable`. The inliner
        // synthesizes descriptor-based empty-stack frames for branch targets.
        let body = MethodCode {
            max_stack: 1,
            max_locals: 1,
            code: vec![0x1a, 0x99, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03, 0xac],
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
        };
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let out = splice_unified(&body, "(I)I", 1, &[], 0, &mut cw).expect("splice");
        assert!(out.join_required);
        assert!(!out.frames.is_empty());
    }

    #[test]
    fn relocates_pool_entries() {
        // A miniature source pool: Object.hashCode()I as a Methodref, plus a String and an Integer.
        let src_cp = vec![
            C::Other,
            C::Utf8("java/lang/Object".into()), // 1
            C::Class(1),                        // 2
            C::Utf8("hashCode".into()),         // 3
            C::Utf8("()I".into()),              // 4
            C::NameAndType(3, 4),               // 5
            C::Methodref(2, 5),                 // 6
            C::Utf8("hi".into()),               // 7
            C::String(7),                       // 8
            C::Integer(42),                     // 9
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
            C::Utf8("Foo".into()), // 1
            C::Class(1),           // 2
            C::Utf8("bar".into()), // 3
            C::Utf8("()V".into()), // 4
            C::NameAndType(3, 4),  // 5
            C::Methodref(2, 5),    // 6
        ];
        // invokestatic #6 ; return
        let code = [0xb8, 0x00, 0x06, 0xb1];
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let out = relocate_code(&code, &src_cp, &mut cw).expect("relocate");
        assert_eq!(out.len(), code.len(), "instruction lengths preserved");
        let expected = cw.methodref("Foo", "bar", "()V");
        assert_eq!(
            (out[1] as u16) << 8 | out[2] as u16,
            expected,
            "index points at target methodref"
        );
        assert_eq!(out[3], 0xb1, "return opcode unchanged");
    }

    #[test]
    fn shift_locals_zero_is_identity_and_shift_works() {
        // iload_1; istore_2; iinc 1, 1; return  (javac-style compact forms).
        let code = [0x1b, 0x3d, 0x84, 0x01, 0x01, 0xb1];
        let mut insns = disassemble(&code).unwrap();
        shift_locals(&mut insns, 0).unwrap();
        assert_eq!(assemble(&insns), code, "shift by 0 is identity");

        let mut s = disassemble(&code).unwrap();
        shift_locals(&mut s, 4).unwrap();
        // iload_1 → iload 5 (0x15 5), istore_2 → istore 6 (0x36 6), iinc 1→5.
        assert_eq!(
            assemble(&s),
            [0x15, 0x05, 0x36, 0x06, 0x84, 0x05, 0x01, 0xb1]
        );

        // Shifting past 3 must promote _N forms to indexed (size grows; assemble relays out).
        let mut t = disassemble(&[0x1a, 0xb1]).unwrap(); // iload_0; return
        shift_locals(&mut t, 10).unwrap();
        assert_eq!(assemble(&t), [0x15, 0x0a, 0xb1]); // iload 10; return
    }

    #[test]
    fn is_reified_inline_negative() {
        // A plain body (iconst_1; ireturn) with no marker is not reified-inline.
        let body = MethodCode {
            max_stack: 1,
            max_locals: 0,
            code: vec![0x04, 0xac],
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
        };
        assert!(!is_reified_inline(&body));
    }

    #[test]
    fn splice_identity_function() {
        // inline fun id(x: Int): Int = x  →  body: iload_0; ireturn
        let body = MethodCode {
            max_stack: 1,
            max_locals: 1,
            code: vec![0x1a, 0xac],
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
        };
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let tm = HashMap::new();
        let insns = splice(&body, "(I)I", 1, &tm, &mut cw).expect("splice");
        // Prologue stores the arg into slot 1 (istore_1), then the body loads it (iload 1) and the
        // return became a goto to the end (value left on stack).
        let bytes = assemble(&insns);
        // istore_1(0x3c); iload 1 → iload_1(0x1b); goto end.
        assert_eq!(bytes[0], 0x3c, "istore_1 binds the argument");
        assert_eq!(bytes[1], 0x1b, "iload_1 reads it back");
        assert_eq!(bytes[2], 0xa7, "return redirected to goto");
    }

    #[test]
    fn substitute_reified_empty_array() {
        // Source pool for an emptyArray-shaped body.
        let src_cp = vec![
            C::Other,
            C::Utf8("kotlin/jvm/internal/Intrinsics".into()), // 1
            C::Class(1),                                      // 2
            C::Utf8("reifiedOperationMarker".into()),         // 3
            C::Utf8("(ILjava/lang/String;)V".into()),         // 4
            C::NameAndType(3, 4),                             // 5
            C::Methodref(2, 5),                               // 6
            C::Utf8("T?".into()),                             // 7
            C::String(7),                                     // 8
            C::Utf8("java/lang/Object".into()),               // 9
            C::Class(9),                                      // 10
        ];
        // iconst_0(size); iconst_0(mode); ldc "T?"; invokestatic marker; anewarray Object; areturn
        let mut insns = vec![
            Insn::Plain {
                op: 0x03,
                operands: vec![],
            },
            Insn::Plain {
                op: 0x03,
                operands: vec![],
            },
            Insn::Plain {
                op: 0x12,
                operands: vec![8],
            },
            Insn::Plain {
                op: 0xb8,
                operands: vec![0, 6],
            },
            Insn::Plain {
                op: 0xbd,
                operands: vec![0, 10],
            },
            Insn::Plain {
                op: 0xb0,
                operands: vec![],
            },
        ];
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let mut tm = std::collections::HashMap::new();
        tm.insert("T".to_string(), "java/lang/String".to_string());
        let patches = substitute_reified(&mut insns, &src_cp, &mut cw, &tm);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].0, 4, "the anewarray is repointed");
        // Marker call + its two arg pushes became nops; the size push (insn 0) is untouched.
        assert!(matches!(insns[0], Insn::Plain { op: 0x03, .. }));
        for k in 1..=3 {
            assert!(
                matches!(insns[k], Insn::Plain { op: 0x00, .. }),
                "insn {k} nop"
            );
        }
        set_pool_operand(&mut insns[4], patches[0].1);
        if let Insn::Plain { op: 0xbd, operands } = &insns[4] {
            assert_eq!(
                (operands[0] as u16) << 8 | operands[1] as u16,
                patches[0].1,
                "anewarray now uses String"
            );
        } else {
            panic!("expected anewarray");
        }
        assert_eq!(patches[0].1, cw.class_ref("java/lang/String"));
    }

    #[test]
    fn relocate_insns_through_pipeline() {
        let src_cp = vec![
            C::Other,
            C::Utf8("Foo".into()), // 1
            C::Class(1),           // 2
            C::Utf8("bar".into()), // 3
            C::Utf8("()V".into()), // 4
            C::NameAndType(3, 4),  // 5
            C::Methodref(2, 5),    // 6
        ];
        let code = [0xb8, 0x00, 0x06, 0xb1]; // invokestatic #6 ; return
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let mut insns = disassemble(&code).unwrap();
        relocate_insns(&mut insns, &src_cp, &mut cw).expect("relocate");
        let out = assemble(&insns);
        let expected = cw.methodref("Foo", "bar", "()V");
        assert_eq!((out[1] as u16) << 8 | out[2] as u16, expected);
        assert_eq!(out.len(), code.len());
    }

    #[test]
    fn redirect_returns_jumps_to_end() {
        // iload_0; ifeq +6 (→ second return); iconst_1; ireturn; iconst_0; ireturn
        // Two value-returns; both become goto end, value left on stack.
        let code = [0x1a, 0x99, 0x00, 0x06, 0x04, 0xac, 0x03, 0xac];
        let mut insns = disassemble(&code).unwrap();
        let n = insns.len();
        redirect_returns(&mut insns);
        // No return opcodes remain; both replaced by goto.
        assert!(insns
            .iter()
            .all(|i| !matches!(i, Insn::Plain { op, .. } if (0xac..=0xb1).contains(op))));
        let gotos = insns
            .iter()
            .filter(|i| matches!(i, Insn::Branch { op: 0xa7, target } if *target == n))
            .count();
        assert_eq!(gotos, 2, "both returns became goto end");
        // Reassembles to valid bytecode of the right shape (goto is 3 bytes vs ireturn's 1).
        let out = assemble(&insns);
        assert!(out.len() > code.len());
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

    #[test]
    fn instruction_len_wide_load_and_multianewarray_and_invokeinterface() {
        // wide iload (0xc4 0x15 idx-hi idx-lo) is 4 bytes.
        assert_eq!(instruction_len(&[0xc4, 0x15, 0, 1], 0), Some(4));
        // multianewarray: 2-byte index + 1 dim byte = 4.
        assert_eq!(instruction_len(&[0xc5, 0, 1, 2], 0), Some(4));
        // invokeinterface: 2-byte index + 2 trailing = 5.
        assert_eq!(instruction_len(&[0xb9, 0, 1, 1, 0], 0), Some(5));
        // sipush: 2 operand bytes = 3.
        assert_eq!(instruction_len(&[0x11, 0, 5], 0), Some(3));
        // aload <byte>: 1 operand = 2.
        assert_eq!(instruction_len(&[0x19, 2], 0), Some(2));
    }

    #[test]
    fn instruction_len_tableswitch_and_lookupswitch() {
        // tableswitch at pc=0: pad to 4-byte boundary (pc+1=1 → 3 pad bytes), then
        // default(4) low(4) high(4) + (high-low+1) offsets.  low=0 high=1 → 2 offsets.
        let mut code = vec![0xaa, 0, 0, 0]; // op + 3 pad
        code.extend_from_slice(&[0, 0, 0, 0]); // default
        code.extend_from_slice(&[0, 0, 0, 0]); // low = 0
        code.extend_from_slice(&[0, 0, 0, 1]); // high = 1
        code.extend_from_slice(&[0, 0, 0, 0]); // offset[0]
        code.extend_from_slice(&[0, 0, 0, 0]); // offset[1]
        assert_eq!(instruction_len(&code, 0), Some(code.len()));

        // lookupswitch: op + 3 pad, default(4), npairs(4)=1, then 1 * (match,offset) = 8.
        let mut lk = vec![0xab, 0, 0, 0];
        lk.extend_from_slice(&[0, 0, 0, 0]); // default
        lk.extend_from_slice(&[0, 0, 0, 1]); // npairs = 1
        lk.extend_from_slice(&[0, 0, 0, 9]); // match
        lk.extend_from_slice(&[0, 0, 0, 0]); // offset
        assert_eq!(instruction_len(&lk, 0), Some(lk.len()));
    }

    #[test]
    fn instruction_len_none_on_truncation_and_missing_opcode() {
        // A 2-operand opcode with only one trailing byte is truncated.
        assert_eq!(instruction_len(&[0x11, 0], 0), None);
        // pc past the end.
        assert_eq!(instruction_len(&[0x60], 5), None);
    }

    #[test]
    fn pool_operand_widths_and_none() {
        assert_eq!(pool_operand(0x12), Some((1, 1))); // ldc
        assert_eq!(pool_operand(0x13), Some((1, 2))); // ldc_w
        assert_eq!(pool_operand(0x14), Some((1, 2))); // ldc2_w
        assert_eq!(pool_operand(0xb2), Some((1, 2))); // getstatic
        assert_eq!(pool_operand(0xb8), Some((1, 2))); // invokestatic
        assert_eq!(pool_operand(0xb9), Some((1, 2))); // invokeinterface
        assert_eq!(pool_operand(0xba), Some((1, 2))); // invokedynamic
        assert_eq!(pool_operand(0xbb), Some((1, 2))); // new
        assert_eq!(pool_operand(0xc0), Some((1, 2))); // checkcast
        assert_eq!(pool_operand(0xc1), Some((1, 2))); // instanceof
        assert_eq!(pool_operand(0xc5), Some((1, 2))); // multianewarray
                                                      // A non-pool opcode (iadd) has no pool operand.
        assert_eq!(pool_operand(0x60), None);
    }

    #[test]
    fn n_form_base_maps_all_load_stores_and_none() {
        assert_eq!(n_form_base(0x15), Some(0x1a)); // iload
        assert_eq!(n_form_base(0x16), Some(0x1e)); // lload
        assert_eq!(n_form_base(0x17), Some(0x22)); // fload
        assert_eq!(n_form_base(0x18), Some(0x26)); // dload
        assert_eq!(n_form_base(0x19), Some(0x2a)); // aload
        assert_eq!(n_form_base(0x36), Some(0x3b)); // istore
        assert_eq!(n_form_base(0x37), Some(0x3f)); // lstore
        assert_eq!(n_form_base(0x38), Some(0x43)); // fstore
        assert_eq!(n_form_base(0x39), Some(0x47)); // dstore
        assert_eq!(n_form_base(0x3a), Some(0x4b)); // astore
        assert_eq!(n_form_base(0x60), None);
    }

    #[test]
    fn decode_n_form_covers_every_range_and_index() {
        // iload_0..iload_3 → (iload, 0..3).
        assert_eq!(decode_n_form(0x1a), Some((0x15, 0)));
        assert_eq!(decode_n_form(0x1d), Some((0x15, 3)));
        // lload_2, fload_1, dload_3, aload_0.
        assert_eq!(decode_n_form(0x20), Some((0x16, 2)));
        assert_eq!(decode_n_form(0x23), Some((0x17, 1)));
        assert_eq!(decode_n_form(0x29), Some((0x18, 3)));
        assert_eq!(decode_n_form(0x2a), Some((0x19, 0)));
        // istore_1, lstore_0, fstore_3, dstore_2, astore_3.
        assert_eq!(decode_n_form(0x3c), Some((0x36, 1)));
        assert_eq!(decode_n_form(0x3f), Some((0x37, 0)));
        assert_eq!(decode_n_form(0x46), Some((0x38, 3)));
        assert_eq!(decode_n_form(0x49), Some((0x39, 2)));
        assert_eq!(decode_n_form(0x4e), Some((0x3a, 3)));
        // Non-N-form.
        assert_eq!(decode_n_form(0x60), None);
    }

    #[test]
    fn local_load_store_picks_most_compact_encoding() {
        // idx <= 3 → *_N form (iload_2 = 0x1a + 2).
        assert!(matches!(
            local_load_store(0x15, 2),
            Insn::Plain { op: 0x1c, ref operands } if operands.is_empty()
        ));
        // 4..=255 → one-byte-indexed form.
        assert!(matches!(
            local_load_store(0x15, 40),
            Insn::Plain { op: 0x15, ref operands } if operands == &[40]
        ));
        // > 255 → wide form: 0xc4 base idx-hi idx-lo.
        assert!(matches!(
            local_load_store(0x15, 300),
            Insn::Plain { op: 0xc4, ref operands } if operands == &[0x15, 0x01, 0x2c]
        ));
    }

    #[test]
    fn param_offsets_accounts_for_wide_slots() {
        // (I, J, Ljava/lang/String;, [I, D): slots 0,1,3,4,5.
        assert_eq!(
            param_offsets("(IJLjava/lang/String;[ID)V"),
            Some(vec![0, 1, 3, 4, 5])
        );
        assert_eq!(param_offsets("()V"), Some(vec![]));
        assert_eq!(param_offsets("no-paren"), None);
    }

    #[test]
    fn param_store_ops_selects_store_opcodes_and_slots() {
        // (J, D, F, Ljava/lang/String;, [I, I) starting at base slot 1.
        let ops = param_store_ops("(JDFLjava/lang/String;[II)V", 1).expect("parses");
        assert_eq!(
            ops,
            vec![
                (1, 0x37), // lstore @1 (2 slots)
                (3, 0x39), // dstore @3 (2 slots)
                (5, 0x38), // fstore @5
                (6, 0x3a), // astore @6 (String)
                (7, 0x3a), // astore @7 ([I)
                (8, 0x36), // istore @8
            ]
        );
        assert_eq!(param_store_ops("bad", 0), None);
    }

    #[test]
    fn read_vtype_decodes_every_tag() {
        let mk = |bytes: &[u8]| {
            let mut i = 0;
            read_vtype(bytes, &mut i).map(|v| (v, i))
        };
        assert_eq!(mk(&[0]), Some((VType::Top, 1)));
        assert_eq!(mk(&[1]), Some((VType::Int, 1)));
        assert_eq!(mk(&[2]), Some((VType::Float, 1)));
        assert_eq!(mk(&[3]), Some((VType::Double, 1)));
        assert_eq!(mk(&[4]), Some((VType::Long, 1)));
        assert_eq!(mk(&[5]), Some((VType::Null, 1)));
        assert_eq!(mk(&[6]), Some((VType::UninitThis, 1)));
        // Object + Uninit carry a 2-byte operand.
        assert_eq!(mk(&[7, 0x01, 0x02]), Some((VType::Object(0x0102), 3)));
        assert_eq!(mk(&[8, 0x00, 0x09]), Some((VType::Uninit(9), 3)));
        // Unknown tag.
        assert_eq!(mk(&[9]), None);
        // Truncated object operand.
        assert_eq!(mk(&[7, 0]), None);
    }

    #[test]
    fn u2_at_reads_big_endian_and_advances() {
        let mut i = 0;
        assert_eq!(u2_at(&[0x12, 0x34], &mut i), Some(0x1234));
        assert_eq!(i, 2);
        let mut j = 0;
        assert_eq!(u2_at(&[0x00], &mut j), None);
    }

    #[test]
    fn field_desc_vtype_maps_primitives_and_missing_class_to_top() {
        let cp = vec![C::Other];
        assert_eq!(field_desc_vtype("I", &cp), Some(VType::Int));
        assert_eq!(field_desc_vtype("Z", &cp), Some(VType::Int));
        assert_eq!(field_desc_vtype("C", &cp), Some(VType::Int));
        assert_eq!(field_desc_vtype("J", &cp), Some(VType::Long));
        assert_eq!(field_desc_vtype("F", &cp), Some(VType::Float));
        assert_eq!(field_desc_vtype("D", &cp), Some(VType::Double));
        // No matching Class constant → Top.
        assert_eq!(field_desc_vtype("Ldemo/Foo;", &cp), Some(VType::Top));
        assert_eq!(field_desc_vtype("[I", &cp), Some(VType::Top));
        // Malformed.
        assert_eq!(field_desc_vtype("", &cp), None);
        assert_eq!(field_desc_vtype("Q", &cp), None);
    }

    #[test]
    fn field_desc_vtype_resolves_present_class_to_object() {
        // pool: Class #1 → "demo/Foo".
        let cp = vec![C::Utf8("demo/Foo".into()), C::Class(0)];
        assert_eq!(field_desc_vtype("Ldemo/Foo;", &cp), Some(VType::Object(1)));
    }

    #[test]
    fn source_class_index_and_object_fallback() {
        let cp = vec![
            C::Utf8("demo/Foo".into()),         // 0
            C::Class(0),                        // 1
            C::Utf8("java/lang/Object".into()), // 2
            C::Class(2),                        // 3
        ];
        assert_eq!(source_class_index(&cp, "demo/Foo"), Some(1));
        assert_eq!(source_class_index(&cp, "missing"), None);
        // The or-Object fallback resolves a missing name to the Object class index.
        assert_eq!(source_class_or_object_index(&cp, "missing"), Some(3));
        assert_eq!(source_class_or_object_index(&cp, "demo/Foo"), Some(1));
    }

    #[test]
    fn num_convert_maps_conversion_opcodes() {
        // Widen/narrow into each result category (the `from` operand is ignored).
        assert_eq!(num_convert(VType::Int, 0x85), Some(VType::Long)); // i2l
        assert_eq!(num_convert(VType::Float, 0x8c), Some(VType::Long)); // f2l
        assert_eq!(num_convert(VType::Int, 0x86), Some(VType::Float)); // i2f
        assert_eq!(num_convert(VType::Long, 0x89), Some(VType::Float)); // l2f
        assert_eq!(num_convert(VType::Int, 0x87), Some(VType::Double)); // i2d
        assert_eq!(num_convert(VType::Long, 0x8b), Some(VType::Int)); // l2i
        assert_eq!(num_convert(VType::Int, 0x91), Some(VType::Int)); // i2b
        assert_eq!(num_convert(VType::Int, 0x93), Some(VType::Int)); // i2s
                                                                     // Not a conversion opcode.
        assert_eq!(num_convert(VType::Int, 0x60), None);
    }

    #[test]
    fn pop_group_handles_category1_and_category2() {
        // A cat-2 value (long) pops as a single group.
        let mut s = vec![VType::Int, VType::Long];
        assert_eq!(pop_group(&mut s), Some(vec![VType::Long]));
        assert_eq!(s, vec![VType::Int]);
        // Two cat-1 values pop together, returned bottom-to-top.
        let mut s2 = vec![VType::Float, VType::Int, VType::Null];
        assert_eq!(pop_group(&mut s2), Some(vec![VType::Int, VType::Null]));
        assert_eq!(s2, vec![VType::Float]);
        // Empty stack.
        let mut empty: Vec<VType> = vec![];
        assert_eq!(pop_group(&mut empty), None);
    }

    #[test]
    fn set_slot_grows_and_clobbers_second_wide_slot() {
        let mut slots = vec![VType::Int];
        set_slot(&mut slots, 3, VType::Long);
        // Grows to hold slot 3 + its filler slot 4, clobbering slot 4 to Top.
        assert_eq!(
            slots,
            vec![VType::Int, VType::Top, VType::Top, VType::Long, VType::Top]
        );
        // A cat-1 store does not clobber the following slot.
        set_slot(&mut slots, 1, VType::Float);
        assert_eq!(slots[1], VType::Float);
        assert_eq!(slots[2], VType::Top);
    }

    #[test]
    fn param_vtypes_full_maps_each_kind() {
        // pool has demo/Foo so a reference param resolves to Object(index); [I falls back to Top.
        let cp = vec![C::Utf8("demo/Foo".into()), C::Class(0)];
        let got = param_vtypes_full("(IJDFLdemo/Foo;[I)V", &cp).expect("parses");
        assert_eq!(
            got,
            vec![
                VType::Int,
                VType::Long,
                VType::Double,
                VType::Float,
                VType::Object(1),
                VType::Top, // [I has no Class constant, and Object isn't in this pool
            ]
        );
        assert_eq!(param_vtypes_full("no-paren", &cp), None);
    }

    #[test]
    fn is_type_op_only_true_for_array_cast_instanceof() {
        for op in [0xbd, 0xc0, 0xc1, 0xc5] {
            assert!(is_type_op(&Insn::Plain {
                op,
                operands: vec![0, 1]
            }));
        }
        assert!(!is_type_op(&Insn::Plain {
            op: 0xb8,
            operands: vec![0, 1]
        }));
        assert!(!is_type_op(&Insn::Branch {
            op: 0xa7,
            target: 0
        }));
    }

    #[test]
    fn cp_accessor_helpers_resolve_and_reject() {
        let cp = vec![
            C::Other,              // 0
            C::Utf8("Foo".into()), // 1
            C::Class(1),           // 2
            C::Utf8("bar".into()), // 3
            C::Utf8("()V".into()), // 4
            C::NameAndType(3, 4),  // 5
        ];
        assert_eq!(utf8(&cp, 1), Some("Foo"));
        assert_eq!(utf8(&cp, 2), None); // not a Utf8
        assert_eq!(utf8(&cp, 99), None); // out of range
        assert_eq!(class_name(&cp, 2), Some("Foo"));
        assert_eq!(class_name(&cp, 1), None); // Utf8, not Class
        assert_eq!(name_and_type(&cp, 5), Some(("bar", "()V")));
        assert_eq!(name_and_type(&cp, 3), None); // not a NameAndType
    }

    #[test]
    fn disassemble_assemble_roundtrips_branch_body() {
        // iload_0; ifeq +6; iconst_1; ireturn; iconst_0; ireturn
        let code = [0x1a, 0x99, 0x00, 0x06, 0x04, 0xac, 0x03, 0xac];
        let insns = disassemble(&code).unwrap();
        assert!(matches!(
            insns[1],
            Insn::Branch {
                op: 0x99,
                target: 5
            }
        ));
        assert_eq!(assemble(&insns), code);
    }

    #[test]
    fn disassemble_assemble_roundtrips_tableswitch() {
        // tableswitch at pc 0 with 2 offsets all targeting the switch itself.
        let mut code = vec![0xaa, 0, 0, 0]; // op + 3 pad
        code.extend_from_slice(&[0, 0, 0, 0]); // default → pc 0
        code.extend_from_slice(&[0, 0, 0, 0]); // low = 0
        code.extend_from_slice(&[0, 0, 0, 1]); // high = 1
        code.extend_from_slice(&[0, 0, 0, 0]); // offset[0] → pc 0
        code.extend_from_slice(&[0, 0, 0, 0]); // offset[1] → pc 0
        let insns = disassemble(&code).unwrap();
        assert!(matches!(insns[0], Insn::TableSwitch { low: 0, .. }));
        assert_eq!(assemble(&insns), code.as_slice());
    }

    #[test]
    fn disassemble_assemble_roundtrips_lookupswitch_and_gotow() {
        // goto_w +9 (→ the lookupswitch), then lookupswitch (1 pair) both landing on itself.
        let mut code = vec![0xc8, 0, 0, 0, 9]; // goto_w to offset 5+... actually to lookupswitch at 8
                                               // pad goto_w target math: goto_w at 0 len 5 → next at 5; lookupswitch must be 4-aligned at 8.
        code.extend_from_slice(&[0x00, 0x00, 0x00]); // 3 nops to reach offset 8
                                                     // fix goto_w to point at offset 8.
        code[1] = 0;
        code[2] = 0;
        code[3] = 0;
        code[4] = 8;
        // lookupswitch at pc 8: pc+1=9, pad=(4-(9%4))%4=3.
        code.push(0xab);
        code.extend_from_slice(&[0, 0, 0]); // pad
        code.extend_from_slice(&[0, 0, 0, 0]); // default → pc 8
        code.extend_from_slice(&[0, 0, 0, 1]); // npairs = 1
        code.extend_from_slice(&[0, 0, 0, 7]); // match
        code.extend_from_slice(&[0, 0, 0, 0]); // offset → pc 8
        let insns = disassemble(&code).unwrap();
        assert!(matches!(insns[0], Insn::BranchW { op: 0xc8, .. }));
        assert!(matches!(insns.last(), Some(Insn::LookupSwitch { pairs, .. }) if pairs.len() == 1));
        assert_eq!(assemble(&insns), code.as_slice());
    }

    #[test]
    fn insn_offsets_tracks_variable_sizes() {
        // iload_0(1); goto(3); return(1) → offsets [0,1,4,5].
        let insns = vec![
            Insn::Plain {
                op: 0x1a,
                operands: vec![],
            },
            Insn::Branch {
                op: 0xa7,
                target: 2,
            },
            Insn::Plain {
                op: 0xb1,
                operands: vec![],
            },
        ];
        assert_eq!(insn_offsets(&insns), vec![0, 1, 4, 5]);
        // At base 10 the offsets shift accordingly.
        assert_eq!(insn_offsets_at(&insns, 10), vec![10, 11, 14, 15]);
    }

    #[test]
    fn old_offsets_maps_bytes_and_bails_on_truncation() {
        // bipush(2); iadd(1) → offsets [0, 2, 3].
        assert_eq!(old_offsets(&[0x10, 0x05, 0x60]), Some(vec![0, 2, 3]));
        // sipush truncated → None.
        assert_eq!(old_offsets(&[0x11, 0x00]), None);
    }

    #[test]
    fn shift_targets_moves_all_target_kinds() {
        let mut insns = vec![
            Insn::Branch {
                op: 0xa7,
                target: 1,
            },
            Insn::BranchW {
                op: 0xc8,
                target: 2,
            },
            Insn::TableSwitch {
                default: 0,
                low: 0,
                targets: vec![1, 2],
            },
            Insn::LookupSwitch {
                default: 0,
                pairs: vec![(9, 3)],
            },
            Insn::Plain {
                op: 0x00,
                operands: vec![],
            },
        ];
        shift_targets(&mut insns, 3);
        assert!(matches!(insns[0], Insn::Branch { target: 4, .. }));
        assert!(matches!(insns[1], Insn::BranchW { target: 5, .. }));
        assert!(
            matches!(&insns[2], Insn::TableSwitch { default: 3, targets, .. } if targets == &[4, 5])
        );
        assert!(
            matches!(&insns[3], Insn::LookupSwitch { default: 3, pairs } if pairs == &[(9, 6)])
        );
    }

    #[test]
    fn methodref_target_names_class_and_method() {
        let cp = vec![
            C::Other,
            C::Utf8("Foo".into()),       // 1
            C::Class(1),                 // 2
            C::Utf8("bar".into()),       // 3
            C::Utf8("()V".into()),       // 4
            C::NameAndType(3, 4),        // 5
            C::Methodref(2, 5),          // 6
            C::InterfaceMethodref(2, 5), // 7
        ];
        assert_eq!(methodref_target(&cp, 6), Some(("Foo", "bar")));
        assert_eq!(methodref_target(&cp, 7), Some(("Foo", "bar")));
        assert_eq!(methodref_target(&cp, 5), None); // not a method ref
    }

    #[test]
    fn ret_vtype_maps_return_descriptors() {
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        assert_eq!(ret_vtype("()V", &mut cw), Some(None));
        assert_eq!(ret_vtype("()I", &mut cw), Some(Some(VType::Int)));
        assert_eq!(ret_vtype("()Z", &mut cw), Some(Some(VType::Int)));
        assert_eq!(ret_vtype("()J", &mut cw), Some(Some(VType::Long)));
        assert_eq!(ret_vtype("()F", &mut cw), Some(Some(VType::Float)));
        assert_eq!(ret_vtype("()D", &mut cw), Some(Some(VType::Double)));
        let obj = cw.class_ref("java/lang/String");
        assert_eq!(
            ret_vtype("()Ljava/lang/String;", &mut cw),
            Some(Some(VType::Object(obj)))
        );
        let arr = cw.class_ref("[I");
        assert_eq!(ret_vtype("()[I", &mut cw), Some(Some(VType::Object(arr))));
        assert_eq!(ret_vtype("()Q", &mut cw), None);
        assert_eq!(ret_vtype("no-paren", &mut cw), None);
    }

    #[test]
    fn relocate_vtype_relocates_object_and_rejects_uninit() {
        let src_cp = vec![C::Utf8("demo/Foo".into()), C::Class(0)];
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let want = cw.class_ref("demo/Foo");
        assert_eq!(
            relocate_vtype(&VType::Object(1), &src_cp, &mut cw),
            Some(VType::Object(want))
        );
        assert_eq!(
            relocate_vtype(&VType::Int, &src_cp, &mut cw),
            Some(VType::Int)
        );
        assert_eq!(relocate_vtype(&VType::Uninit(3), &src_cp, &mut cw), None);
        assert_eq!(relocate_vtype(&VType::UninitThis, &src_cp, &mut cw), None);
    }

    #[test]
    fn ldc_and_ldc2_vtype_classify_constants() {
        let cp = vec![
            C::Integer(1),                      // 0
            C::Float(1.0f32.to_bits()),         // 1
            C::Utf8("hi".into()),               // 2
            C::String(2),                       // 3
            C::Long(9),                         // 4
            C::Double(2.0f64.to_bits()),        // 5
            C::Utf8("java/lang/String".into()), // 6
            C::Class(6),                        // 7
        ];
        // ldc (1-byte index)
        assert_eq!(ldc_vtype(&[0], 0x12, &cp), Some(VType::Int));
        assert_eq!(ldc_vtype(&[1], 0x12, &cp), Some(VType::Float));
        // String resolves to the pool's String class Object.
        assert_eq!(ldc_vtype(&[3], 0x12, &cp), Some(VType::Object(7)));
        // ldc_w (2-byte index) pointing at Integer.
        assert_eq!(ldc_vtype(&[0, 0], 0x13, &cp), Some(VType::Int));
        // A Long is not a valid ldc constant.
        assert_eq!(ldc_vtype(&[4], 0x12, &cp), None);
        // ldc2_w
        assert_eq!(ldc2_vtype(&[0, 4], &cp), Some(VType::Long));
        assert_eq!(ldc2_vtype(&[0, 5], &cp), Some(VType::Double));
        assert_eq!(ldc2_vtype(&[0, 0], &cp), None); // Integer is not ldc2
    }

    #[test]
    fn fieldref_and_methodref_desc_effects() {
        let cp = vec![
            C::Other,                // 0
            C::Utf8("Foo".into()),   // 1
            C::Class(1),             // 2
            C::Utf8("count".into()), // 3
            C::Utf8("I".into()),     // 4
            C::NameAndType(3, 4),    // 5
            C::Fieldref(2, 5),       // 6
            C::Utf8("m".into()),     // 7
            C::Utf8("(II)J".into()), // 8
            C::NameAndType(7, 8),    // 9
            C::Methodref(2, 9),      // 10
        ];
        assert_eq!(fieldref_vtype(&[0, 6], &cp), Some(VType::Int));
        assert_eq!(fieldref_vtype(&[0, 5], &cp), None); // not a fieldref
        assert_eq!(methodref_desc_effect(&cp, 10), Some((2, Some(VType::Long))));
        assert_eq!(methodref_desc_effect(&cp, 5), None); // not a methodref
    }

    #[test]
    fn param_vtypes_target_creates_class_refs() {
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let got = param_vtypes_target("(IJDFLjava/lang/String;[I)V", &mut cw).expect("parses");
        assert_eq!(got[0], VType::Int);
        assert_eq!(got[1], VType::Long);
        assert_eq!(got[2], VType::Double);
        assert_eq!(got[3], VType::Float);
        assert_eq!(got[4], VType::Object(cw.class_ref("java/lang/String")));
        assert_eq!(got[5], VType::Object(cw.class_ref("[I")));
        assert_eq!(param_vtypes_target("no-paren", &mut cw), None);
    }

    #[test]
    fn strip_param_null_checks_removes_check_triple() {
        // aload_1 ; ldc "x" ; invokestatic Intrinsics.checkNotNullParameter ; return
        let cp = vec![
            C::Other,
            C::Utf8("kotlin/jvm/internal/Intrinsics".into()), // 1
            C::Class(1),                                      // 2
            C::Utf8("checkNotNullParameter".into()),          // 3
            C::Utf8("(Ljava/lang/Object;Ljava/lang/String;)V".into()), // 4
            C::NameAndType(3, 4),                             // 5
            C::Methodref(2, 5),                               // 6
            C::Utf8("x".into()),                              // 7
            C::String(7),                                     // 8
        ];
        let mut insns = vec![
            Insn::Plain {
                op: 0x2b,
                operands: vec![],
            }, // aload_1
            Insn::Plain {
                op: 0x12,
                operands: vec![8],
            }, // ldc "x"
            Insn::Plain {
                op: 0xb8,
                operands: vec![0x00, 0x06],
            }, // invokestatic check
            Insn::Plain {
                op: 0xb1,
                operands: vec![],
            }, // return
        ];
        strip_param_null_checks(&mut insns, &cp);
        assert_eq!(insns.len(), 1);
        assert!(matches!(insns[0], Insn::Plain { op: 0xb1, .. }));
    }

    #[test]
    fn is_lambda_spliceable_positive_and_negatives() {
        let cp = vec![
            C::Other,
            C::Utf8("kotlin/jvm/functions/Function1".into()), // 1
            C::Class(1),                                      // 2
            C::Utf8("invoke".into()),                         // 3
            C::Utf8("(Ljava/lang/Object;)Ljava/lang/Object;".into()), // 4
            C::NameAndType(3, 4),                             // 5
            C::InterfaceMethodref(2, 5),                      // 6
        ];
        // aload_1 ; invokeinterface Function1.invoke #6 ; areturn
        let good = MethodCode {
            max_stack: 2,
            max_locals: 2,
            code: vec![0x2b, 0xb9, 0x00, 0x06, 0x01, 0x00, 0xb0],
            source_cp: cp.clone(),
            stackmap: None,
            handlers: vec![],
        };
        assert!(is_lambda_spliceable(&good));

        // Handlers present → not spliceable.
        let mut with_handler = good.clone();
        with_handler.handlers = vec![crate::jvm::classreader::ExcEntry {
            start_pc: 0,
            end_pc: 1,
            handler_pc: 1,
            catch_type: 0,
        }];
        assert!(!is_lambda_spliceable(&with_handler));

        // A branch instruction disqualifies it (not all-Plain).
        let branchy = MethodCode {
            max_stack: 1,
            max_locals: 1,
            code: vec![0xa7, 0x00, 0x03, 0xb1],
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
        };
        assert!(!is_lambda_spliceable(&branchy));

        // No invoke site → not spliceable.
        let no_invoke = MethodCode {
            max_stack: 1,
            max_locals: 1,
            code: vec![0x03, 0xac],
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
        };
        assert!(!is_lambda_spliceable(&no_invoke));
    }

    #[test]
    fn set_pool_operand_patches_only_pool_ops() {
        // invokestatic has a 2-byte pool operand: patch it.
        let mut good = Insn::Plain {
            op: 0xb8,
            operands: vec![0x00, 0x01],
        };
        set_pool_operand(&mut good, 0x0203);
        assert!(
            matches!(good, Insn::Plain { op: 0xb8, ref operands } if operands == &[0x02, 0x03])
        );
        // A non-pool op (iadd) is left untouched.
        let mut plain = Insn::Plain {
            op: 0x60,
            operands: vec![],
        };
        set_pool_operand(&mut plain, 0x0203);
        assert!(matches!(plain, Insn::Plain { op: 0x60, ref operands } if operands.is_empty()));
    }

    #[test]
    fn is_aload_of_matches_all_aload_forms() {
        // aload_0..aload_3 compact forms.
        assert!(is_aload_of(
            &Insn::Plain {
                op: 0x2c,
                operands: vec![]
            },
            2
        ));
        assert!(!is_aload_of(
            &Insn::Plain {
                op: 0x2c,
                operands: vec![]
            },
            1
        ));
        // aload <byte index>.
        assert!(is_aload_of(
            &Insn::Plain {
                op: 0x19,
                operands: vec![40]
            },
            40
        ));
        // wide aload: 0xc4 0x19 idx-hi idx-lo.
        assert!(is_aload_of(
            &Insn::Plain {
                op: 0xc4,
                operands: vec![0x19, 0x01, 0x2c]
            },
            300
        ));
        // A non-aload opcode / non-Plain insn.
        assert!(!is_aload_of(
            &Insn::Plain {
                op: 0x1a,
                operands: vec![]
            },
            0
        ));
        assert!(!is_aload_of(
            &Insn::Branch {
                op: 0xa7,
                target: 0
            },
            0
        ));
    }
}
