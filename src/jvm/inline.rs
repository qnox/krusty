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
            return Insn::Plain { op: nb + idx as u8, operands: vec![] };
        }
    }
    if idx <= 0xff {
        Insn::Plain { op: base_op, operands: vec![idx as u8] }
    } else {
        Insn::Plain { op: 0xc4, operands: vec![base_op, (idx >> 8) as u8, idx as u8] }
    }
}

/// Add `base` to every local-variable index in the body (load/store in all forms, `iinc`, `ret`, and
/// their `wide` variants), re-selecting the compact encoding. Relocating an inline body's locals into
/// the caller's frame is a prerequisite for splicing — the body then occupies `base..base+max_locals`.
pub fn shift_locals(insns: &mut [Insn], base: u16) -> Option<()> {
    for insn in insns.iter_mut() {
        let Insn::Plain { op, operands } = insn else { continue };
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
                Insn::Plain { op: 0xa9, operands: vec![idx as u8] }
            } else {
                Insn::Plain { op: 0xc4, operands: vec![0xa9, (idx >> 8) as u8, idx as u8] }
            };
        } else if op == 0x84 {
            // iinc <index> <const>.
            let idx = *operands.first()? as u16 + base;
            let c = operands[1];
            *insn = if idx <= 0xff {
                Insn::Plain { op: 0x84, operands: vec![idx as u8, c] }
            } else {
                // wide iinc: 2-byte index + sign-extended 2-byte const.
                let chi = if (c as i8) < 0 { 0xff } else { 0 };
                Insn::Plain { op: 0xc4, operands: vec![0x84, (idx >> 8) as u8, idx as u8, chi, c] }
            };
        } else if op == 0xc4 {
            // wide <sub-op> <index:2> [<const:2> for iinc].
            let sub = *operands.first()?;
            let idx = ((operands[1] as u16) << 8 | operands[2] as u16) + base;
            if sub == 0x84 {
                *insn = Insn::Plain { op: 0xc4, operands: vec![0x84, (idx >> 8) as u8, idx as u8, operands[3], operands[4]] };
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
    matches!(insn, Insn::Plain { op: 0xbd | 0xc0 | 0xc1 | 0xc5, .. })
}

/// Substitute Kotlin's `reifiedOperationMarker` pattern in an inline body: the call (preceded by its
/// `iconst <mode>` and `ldc "<typeParam>"` argument pushes) is replaced with `nop`s, and the
/// following type op (`anewarray`/`checkcast`/`instanceof`) is repointed at the concrete reified type
/// from `type_map` (Kotlin type-parameter name → JVM internal name). Returns the `(insn index, target
/// pool index)` repoints to apply *after* relocation (so the type op isn't re-relocated to `Object`).
/// This is how `emptyArray<String>()` inlines to `anewarray java/lang/String`.
pub fn substitute_reified(insns: &mut [Insn], src_cp: &[C], cw: &mut ClassWriter, type_map: &std::collections::HashMap<String, String>) -> Vec<(usize, u16)> {
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
            Insn::Plain { op: 0x12, operands } if operands.len() == 1 => match src_cp.get(operands[0] as usize) {
                Some(C::String(u)) => utf8(src_cp, *u).map(|s| s.trim_end_matches('?').to_string()),
                _ => None,
            },
            _ => None,
        };
        // Erase the marker call and its two argument pushes (mode + type-name).
        let nop = Insn::Plain { op: 0x00, operands: vec![] };
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
        let Insn::Plain { op, operands } = insn else { continue };
        let Some((off, width)) = pool_operand(*op) else { continue };
        if *op == 0xba {
            return None; // invokedynamic
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
                *insn = Insn::Branch { op: 0xa7, target: end };
            }
        }
    }
}

/// Whether a method body is a **reified `inline`** function — its bytecode calls
/// `Intrinsics.reifiedOperationMarker`, which the compiler must inline away (a direct call to such a
/// method throws `UnsupportedOperationException` at runtime). This recognizes the must-inline case
/// from the body alone, without parsing the `@Metadata` inline flag.
pub fn is_reified_inline(body: &MethodCode) -> bool {
    let Some(insns) = disassemble(&body.code) else { return false };
    insns.iter().any(|i| matches!(i, Insn::Plain { op: 0xb8, operands } if operands.len() == 2
        && methodref_target(&body.source_cp, (operands[0] as u16) << 8 | operands[1] as u16)
            == Some(("kotlin/jvm/internal/Intrinsics", "reifiedOperationMarker"))))
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
pub fn splice(body: &MethodCode, descriptor: &str, base: u16, type_map: &HashMap<String, String>, cw: &mut ClassWriter) -> Option<Vec<Insn>> {
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
    let mut out: Vec<Insn> = params.iter().rev().map(|&(slot, op)| local_load_store(op, slot)).collect();
    out.extend(insns);
    Some(out)
}

/// Splice a **branchless, single-exit** inline body at a call site whose arguments are already on the
/// stack. Unlike [`splice`], it *drops* the trailing return (leaving the computed value on the stack
/// to fall through) instead of rewriting it to a `goto` — so the spliced region contains no branch
/// target and needs no StackMapTable frame, which is what makes it safely emittable today. `None`
/// (caller emits a normal `invokestatic`) if the body has any branch/switch, isn't single-exit, or
/// uses a pool entry `relocate_insns` can't relocate (`invokedynamic`).
pub fn splice_branchless(body: &MethodCode, descriptor: &str, base: u16, cw: &mut ClassWriter) -> Option<Vec<Insn>> {
    // A body with exception handlers needs handler-table relocation (not supported) — bail.
    if body.has_handlers {
        return None;
    }
    let mut insns = disassemble(&body.code)?;
    // Branchless: every instruction is `Plain` (no `goto`/conditional/`switch` target).
    if insns.iter().any(|i| !matches!(i, Insn::Plain { .. })) {
        return None;
    }
    // Single exit: exactly one return opcode, and it is the last instruction.
    let returns: Vec<usize> = insns.iter().enumerate()
        .filter(|(_, i)| matches!(i, Insn::Plain { op, .. } if (0xac..=0xb1).contains(op)))
        .map(|(j, _)| j)
        .collect();
    if returns.len() != 1 || returns[0] != insns.len() - 1 {
        return None;
    }
    relocate_insns(&mut insns, &body.source_cp, cw)?;
    shift_locals(&mut insns, base)?;
    insns.pop(); // drop the trailing return: fall through with the result on the stack
    // Prologue: pop the arguments (top = last param) into the body's parameter slots (`base..`).
    let params = param_store_ops(descriptor, base)?;
    let mut out: Vec<Insn> = params.iter().rev().map(|&(slot, op)| local_load_store(op, slot)).collect();
    out.extend(insns);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_branchless_drops_return_and_stores_args() {
        // Body of `inline fun triple(x: Int): Int = x * 3` — `iload_0; iconst_3; imul; ireturn`.
        let body = MethodCode { max_stack: 2, max_locals: 1, code: vec![0x1a, 0x06, 0x68, 0xac], source_cp: vec![C::Other], stackmap: None, has_handlers: false };
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let insns = splice_branchless(&body, "(I)I", 3, &mut cw).expect("branchless splice");
        // Prologue stores the one arg into slot 3, then the body runs with no trailing return.
        // istore_3 ; iload_3 ; iconst_3 ; imul   (compact slot-3 forms; the `ireturn` is dropped)
        assert_eq!(assemble(&insns), vec![0x3e, 0x1d, 0x06, 0x68]);
    }

    #[test]
    fn splice_branchless_bails_on_branch() {
        // `iload_0; ifeq +4; iconst_1; ireturn` — has a branch ⇒ not branchless.
        let body = MethodCode { max_stack: 1, max_locals: 1, code: vec![0x1a, 0x99, 0x00, 0x04, 0x04, 0xac], source_cp: vec![C::Other], stackmap: None, has_handlers: false };
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        assert!(splice_branchless(&body, "(I)I", 1, &mut cw).is_none());
    }

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
    fn shift_locals_zero_is_identity_and_shift_works() {
        // iload_1; istore_2; iinc 1, 1; return  (javac-style compact forms).
        let code = [0x1b, 0x3d, 0x84, 0x01, 0x01, 0xb1];
        let mut insns = disassemble(&code).unwrap();
        shift_locals(&mut insns, 0).unwrap();
        assert_eq!(assemble(&insns), code, "shift by 0 is identity");

        let mut s = disassemble(&code).unwrap();
        shift_locals(&mut s, 4).unwrap();
        // iload_1 → iload 5 (0x15 5), istore_2 → istore 6 (0x36 6), iinc 1→5.
        assert_eq!(assemble(&s), [0x15, 0x05, 0x36, 0x06, 0x84, 0x05, 0x01, 0xb1]);

        // Shifting past 3 must promote _N forms to indexed (size grows; assemble relays out).
        let mut t = disassemble(&[0x1a, 0xb1]).unwrap(); // iload_0; return
        shift_locals(&mut t, 10).unwrap();
        assert_eq!(assemble(&t), [0x15, 0x0a, 0xb1]); // iload 10; return
    }

    #[test]
    fn is_reified_inline_negative() {
        // A plain body (iconst_1; ireturn) with no marker is not reified-inline.
        let body = MethodCode { max_stack: 1, max_locals: 0, code: vec![0x04, 0xac], source_cp: vec![C::Other], stackmap: None, has_handlers: false };
        assert!(!is_reified_inline(&body));
    }

    #[test]
    fn splice_identity_function() {
        // inline fun id(x: Int): Int = x  →  body: iload_0; ireturn
        let body = MethodCode { max_stack: 1, max_locals: 1, code: vec![0x1a, 0xac], source_cp: vec![C::Other], stackmap: None, has_handlers: false };
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
            C::Class(1),                                       // 2
            C::Utf8("reifiedOperationMarker".into()),          // 3
            C::Utf8("(ILjava/lang/String;)V".into()),          // 4
            C::NameAndType(3, 4),                              // 5
            C::Methodref(2, 5),                                // 6
            C::Utf8("T?".into()),                              // 7
            C::String(7),                                      // 8
            C::Utf8("java/lang/Object".into()),                // 9
            C::Class(9),                                       // 10
        ];
        // iconst_0(size); iconst_0(mode); ldc "T?"; invokestatic marker; anewarray Object; areturn
        let mut insns = vec![
            Insn::Plain { op: 0x03, operands: vec![] },
            Insn::Plain { op: 0x03, operands: vec![] },
            Insn::Plain { op: 0x12, operands: vec![8] },
            Insn::Plain { op: 0xb8, operands: vec![0, 6] },
            Insn::Plain { op: 0xbd, operands: vec![0, 10] },
            Insn::Plain { op: 0xb0, operands: vec![] },
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
            assert!(matches!(insns[k], Insn::Plain { op: 0x00, .. }), "insn {k} nop");
        }
        set_pool_operand(&mut insns[4], patches[0].1);
        if let Insn::Plain { op: 0xbd, operands } = &insns[4] {
            assert_eq!((operands[0] as u16) << 8 | operands[1] as u16, patches[0].1, "anewarray now uses String");
        } else {
            panic!("expected anewarray");
        }
        assert_eq!(patches[0].1, cw.class_ref("java/lang/String"));
    }

    #[test]
    fn relocate_insns_through_pipeline() {
        let src_cp = vec![
            C::Other,
            C::Utf8("Foo".into()),  // 1
            C::Class(1),            // 2
            C::Utf8("bar".into()),  // 3
            C::Utf8("()V".into()),  // 4
            C::NameAndType(3, 4),   // 5
            C::Methodref(2, 5),     // 6
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
        assert!(insns.iter().all(|i| !matches!(i, Insn::Plain { op, .. } if (0xac..=0xb1).contains(op))));
        let gotos = insns.iter().filter(|i| matches!(i, Insn::Branch { op: 0xa7, target } if *target == n)).count();
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
}
