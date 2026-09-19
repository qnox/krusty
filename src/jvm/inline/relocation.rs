//! Moving a compiled body's constant-pool references into ANOTHER class, and deciding whether it
//! may move at all.
//!
//! A body read from the classpath names everything through its DEFINING class's constant pool, and
//! an `invokedynamic` names a `BootstrapMethods` entry of that class by INDEX rather than through
//! the pool at all. Splicing the body into a caller means re-interning every one of those in the
//! host — and first proving the host is allowed to reference what they reach.
//!
//! The two halves live together because they answer one question between them. Relocation is
//! mechanical: it either re-interns an entry or reports that it cannot. Eligibility is the
//! judgement, and it is the half with no second chance — the verifier does not look inside a
//! bootstrap entry, so a member the host may not reference throws `BootstrapMethodError` when the
//! relocated instruction first executes, long after anything could have refused it.
//!
//! The writer boundary is deliberately narrow: nothing here decides WHAT to emit. A `ClassWriter`
//! is taken only to intern, and every function reports failure rather than emitting something
//! approximate.
use super::{class_name, instruction_len, name_and_type, pool_operand, utf8, utf8_value, Insn};
use crate::jvm::classfile::ClassWriter;
use crate::jvm::classreader::C;

/// Re-intern the source constant-pool entry at `idx` (from the inline body's defining class, `src_cp`)
/// into the target class's pool (`cw`), returning the new pool index. Resolving each entry to its
/// semantic form (class/method/field names, descriptors, constant values) and re-interning is what
/// lets a body compiled against one class run inside another. `None` for an entry kind not yet
/// relocatable (`invokedynamic`/method handles — those need bootstrap-method relocation too).
pub fn relocate_const(src_cp: &[C], idx: u16, cw: &mut ClassWriter) -> Option<u16> {
    match src_cp.get(idx as usize)? {
        C::Class(n) => Some(cw.class_ref(utf8(src_cp, *n)?)),
        // A value-bearing string uses the class reader's single code-unit accessor. Names and
        // descriptors use `utf8` above; duplicating the variant conversion here risks making
        // classpath constant inlining disagree with `ConstantValue` field reads.
        C::String(u) => Some(cw.const_string_kt(&utf8_value(src_cp, *u)?)),
        C::Integer(v) => Some(cw.const_int(*v)),
        C::Float(b) => Some(cw.const_float(f32::from_bits(*b))),
        C::Long(v) => Some(cw.const_long(*v)),
        C::Double(b) => Some(cw.const_double(f64::from_bits(*b))),
        C::Methodref(c, nt) => {
            // Owned before the writer is touched: `cw` is borrowed mutably and these all borrow
            // `src_cp`, which the borrow checker will not let overlap.
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            let (n, d) = (n.to_string(), d.to_string());
            Some(cw.methodref(&cn, &n, &d))
        }
        C::Fieldref(c, nt) => {
            // Owned before the writer is touched: `cw` is borrowed mutably and these all borrow
            // `src_cp`, which the borrow checker will not let overlap.
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            let (n, d) = (n.to_string(), d.to_string());
            Some(cw.fieldref(&cn, &n, &d))
        }
        C::InterfaceMethodref(c, nt) => {
            // Owned before the writer is touched: `cw` is borrowed mutably and these all borrow
            // `src_cp`, which the borrow checker will not let overlap.
            let cn = class_name(src_cp, *c)?.to_string();
            let (n, d) = name_and_type(src_cp, *nt)?;
            let (n, d) = (n.to_string(), d.to_string());
            Some(cw.interface_methodref(&cn, &n, &d))
        }
        // A bootstrap method is named by a handle onto a member, and its static arguments are
        // ordinary constants plus, commonly, a `MethodType`. Both relocate through the member/utf8
        // they wrap.
        C::MethodHandle(kind, member) => {
            let (kind, member) = (*kind, *member);
            let relocated = relocate_const(src_cp, member, cw)?;
            Some(cw.method_handle_ref(kind, relocated))
        }
        C::MethodType(descriptor) => {
            let descriptor = utf8(src_cp, *descriptor)?.to_string();
            Some(cw.method_type_ref(&descriptor))
        }
        _ => None,
    }
}

/// Every member one `BootstrapMethods` entry reaches, as `(owner, name, descriptor)`: the handle
/// naming the factory, and each static argument that names a member in turn.
///
/// This is the complete dependency graph an `invokedynamic` carries. Relocating the instruction
/// into another class re-interns every one of these in the HOST's pool, so the property that
/// decides whether the entry may move is structural, not a question of which factory it names: the
/// host must be allowed to reference each member, and each entry must be a constant kind
/// [`relocate_const`] can re-intern. A `StringConcatFactory` entry reaches only its own public
/// factory, a recipe string and constants; a `LambdaMetafactory` one also names an implementation
/// handle in the DEFINING class, which is usually private and synthetic — but a concat entry may
/// equally carry a handle to an inaccessible member, which is why the answer cannot come from the
/// factory's spelling.
///
/// `None` when any reachable entry is a kind relocation cannot carry (a `CONSTANT_Dynamic`, a
/// handle onto something that is not a member, an index past the pool). Fail closed: the caller
/// cannot vouch for what it cannot read, and declining only means a real call is emitted.
pub fn bootstrap_members<'a>(
    src_cp: &'a [C],
    handle: u16,
    arguments: &[u16],
) -> Option<Vec<(&'a str, &'a str, &'a str)>> {
    // JVMS 4.7.23: a bootstrap method is named by a `CONSTANT_MethodHandle` and nothing else. A
    // slot holding anything else is a table this walk cannot read, not an entry with no members.
    if !matches!(src_cp.get(handle as usize), Some(C::MethodHandle(..))) {
        return None;
    }
    let mut members = Vec::new();
    let mut pending: Vec<u16> = Vec::with_capacity(1 + arguments.len());
    pending.push(handle);
    pending.extend_from_slice(arguments);
    while let Some(index) = pending.pop() {
        match src_cp.get(index as usize)? {
            // A handle is the only entry that names a member the host would have to reference.
            C::MethodHandle(_, member) => {
                let (class, signature) = match src_cp.get(*member as usize)? {
                    C::Methodref(c, nt) | C::InterfaceMethodref(c, nt) | C::Fieldref(c, nt) => {
                        (*c, *nt)
                    }
                    _ => return None,
                };
                let owner = class_name(src_cp, class)?;
                let (name, descriptor) = name_and_type(src_cp, signature)?;
                members.push((owner, name, descriptor));
            }
            // A `MethodType` and the value constants re-intern as themselves, reaching nothing. A
            // `Class` argument names a type, whose own accessibility the verifier decides at the
            // use site exactly as it does for a `checkcast` the body already carries.
            C::MethodType(_)
            | C::Class(_)
            | C::String(_)
            | C::Integer(_)
            | C::Float(_)
            | C::Long(_)
            | C::Double(_) => {}
            _ => return None,
        }
    }
    Some(members)
}

/// Whether `code` references (through `src_cp`) a method/field `is_private` flags as `ACC_PRIVATE`.
/// Such a body runs legally only inside its DEFINING class: spliced into a caller, the reference is
/// an `IllegalAccessError` (kotlinc rewrites it to a synthetic `access$…` bridge — unmodelled
/// here), so the splicer must decline. A malformed body reports `true` — the caller cannot verify
/// what it cannot walk, and declining is always safe (a real call instead).
///
/// Both ways a body reaches a member are walked, and they are held to DIFFERENT standards.
///
/// An instruction's constant-pool operand need only not be `ACC_PRIVATE`: the verifier checks
/// everything else at the use site, and an `IllegalAccessError` is raised before the instruction
/// runs.
///
/// A `BootstrapMethods` entry must PROVE reachability through `is_publicly_reachable`. Its handle
/// and static arguments appear in no instruction operand, nothing verifies them, and an entry that
/// cannot be reached throws `BootstrapMethodError` the first time the relocated instruction
/// executes — after verification, so no earlier check catches it. Anything unproven declines.
///
/// `bootstraps` is the DEFINING class's table, indexed as `CONSTANT_InvokeDynamic` indexes it.
pub fn references_private_member(
    code: &[u8],
    src_cp: &[C],
    bootstraps: &[(u16, Vec<u16>)],
    is_private: &mut dyn FnMut(&str, &str, &str) -> bool,
    is_publicly_reachable: &mut dyn FnMut(&str, &str, &str) -> bool,
) -> bool {
    let mut pc = 0;
    while pc < code.len() {
        let Some(len) = instruction_len(code, pc) else {
            return true;
        };
        if let Some((off, width)) = pool_operand(code[pc]) {
            let idx = if width == 1 {
                u16::from(code[pc + off])
            } else {
                u16::from_be_bytes([code[pc + off], code[pc + off + 1]])
            };
            let member = match src_cp.get(idx as usize) {
                Some(C::Methodref(c, nt) | C::InterfaceMethodref(c, nt) | C::Fieldref(c, nt)) => {
                    class_name(src_cp, *c).zip(name_and_type(src_cp, *nt))
                }
                // An `invokedynamic` names a bootstrap entry rather than a member. Its whole graph
                // is walked; an unreadable one reports `true` for the same reason a malformed
                // instruction does.
                Some(C::InvokeDynamic(entry, _)) => {
                    let Some((handle, arguments)) = bootstraps.get(*entry as usize) else {
                        return true;
                    };
                    let Some(members) = bootstrap_members(src_cp, *handle, arguments) else {
                        return true;
                    };
                    // A bootstrap entry must PROVE reachability, where an instruction operand
                    // need only not be private. The two differ for a package-private or protected
                    // member, for a public member of a package-private class, and for a member
                    // this compilation cannot see at all — and an entry that fails any of those
                    // throws at LINKAGE, so no verifier check exists to fall back on.
                    if !members.into_iter().all(|(owner, name, descriptor)| {
                        is_publicly_reachable(owner, name, descriptor)
                    }) {
                        return true;
                    }
                    None
                }
                _ => None,
            };
            if let Some((owner, (name, descriptor))) = member {
                if is_private(owner, name, descriptor) {
                    return true;
                }
            }
        }
        pc += len;
    }
    false
}

/// Relocate every constant-pool reference in a disassembled body into `cw`'s pool (the insn-level
/// counterpart of [`relocate_code`], so relocation composes with the local/return/reified transforms
/// before reassembly). `None` on `invokedynamic` or an unsupported one-byte pool operand. An `ldc`
/// whose relocated index exceeds a byte is widened to the identical-semantics `ldc_w` form.
pub fn relocate_insns(
    insns: &mut [Insn],
    src_cp: &[C],
    bootstraps: &[(u16, Vec<u16>)],
    cw: &mut ClassWriter,
) -> Option<()> {
    for insn in insns.iter_mut() {
        let Insn::Plain { op, operands } = insn else {
            continue;
        };
        let Some((off, width)) = pool_operand(*op) else {
            continue;
        };
        if *op == 0xba {
            // invokedynamic names a `BootstrapMethods` entry of its DEFINING class by index, not a
            // constant pool entry, so relocating it means re-interning that entry here: the handle,
            // its static arguments, and the name/type. `add_bootstrap` dedupes on the host side.
            //
            // Kotlin reaches this through string concatenation, which compiles to
            // `invokedynamic makeConcatWithConstants` from JVM target 9 — a lambda inside an
            // `inline` function does not, kotlinc compiling those as anonymous-class singletons
            // precisely so an inliner can copy them.
            let o = off - 1;
            let src_idx = (*operands.get(o)? as u16) << 8 | *operands.get(o + 1)? as u16;
            let C::InvokeDynamic(bootstrap_index, name_and_type_index) =
                *src_cp.get(src_idx as usize)?
            else {
                return None;
            };
            let (handle, arguments) = bootstraps.get(bootstrap_index as usize)?;
            // The entry moves only if its COMPLETE dependency graph can move with it. Walking it
            // here rejects an unsupported constant kind before anything is interned; whether the
            // host may reference the members it reaches is decided by the splice-eligibility check
            // that owns the accessibility predicate, alongside the instruction-level one.
            bootstrap_members(src_cp, *handle, arguments)?;
            let handle = relocate_const(src_cp, *handle, cw)?;
            let arguments = arguments
                .iter()
                .map(|argument| relocate_const(src_cp, *argument, cw))
                .collect::<Option<Vec<u16>>>()?;
            let bootstrap = cw.add_bootstrap(handle, arguments);
            let (name, descriptor) = name_and_type(src_cp, name_and_type_index)?;
            let (name, descriptor) = (name.to_string(), descriptor.to_string());
            let new = cw.invoke_dynamic_ref(bootstrap, &name, &descriptor);
            *operands = vec![(new >> 8) as u8, (new & 0xff) as u8, 0, 0];
            continue;
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
                // `ldc` (0x12) is the only 1-byte-pool-index op; its relocated index overflowed a byte
                // (the host class's pool is large — common when splicing a stdlib body like `require`'s
                // into a big file). Widen to `ldc_w` (0x13), the identical-semantics 2-byte form. The
                // assembler derives instruction length from the opcode, so the size change is handled
                // downstream (see `old_offsets`). A non-`ldc` 1-byte op has no wide form → bail.
                if *op != 0x12 {
                    return None;
                }
                *op = 0x13;
                *operands = vec![(new >> 8) as u8, (new & 0xff) as u8];
                continue;
            }
            operands[o] = new as u8;
        } else {
            operands[o] = (new >> 8) as u8;
            operands[o + 1] = (new & 0xff) as u8;
        }
    }
    Some(())
}
