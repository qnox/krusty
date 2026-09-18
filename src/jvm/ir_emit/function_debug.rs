//! JVM debug tables for declared and explicitly published generated functions.

use super::{jvm_declared_ty, jvm_function_params, method_descriptor, slot_words};
use crate::ir::IrFile;
use crate::jvm::classfile::ClassWriter;
use crate::types::Ty;

/// Attach kotlinc's `LineNumberTable` + `LocalVariableTable` to a declared method. Source methods
/// take their line/name identities from lowering; generated methods use their producer-owned
/// publication contract. Both paths reject incomplete parameter identities.
pub(super) fn attach_declared_function_debug(
    ir: &IrFile,
    fid: u32,
    owner: &str,
    // The line the owning class declaration CLOSES on, or 0 when there is none. A generated
    // member's trailing `return` maps there, the same rule a generated `<clinit>` already follows.
    class_end_line: u32,
    cw: &mut ClassWriter,
) {
    let Some(function) = ir.functions.get(fid as usize) else {
        return;
    };
    let generated = ir.generated_function_publication(fid);
    let line = match generated {
        Some(publication) if publication.debug.records_locals() => publication.debug.line(),
        Some(_) => return,
        None => {
            let Some(&line) = ir.fn_decl_lines.get(&fid) else {
                return;
            };
            Some(line)
        }
    };
    if function.body.is_none() {
        return;
    }
    let param_tys = jvm_function_params(ir, fid);
    let parameter_names = generated
        .map(|publication| publication.parameter_names.as_slice())
        .or_else(|| ir.param_names(fid));
    if !param_tys.is_empty() {
        let parameter_names =
            parameter_names.expect("a debug-published function carries exact parameter identities");
        assert_eq!(
            parameter_names.len(),
            param_tys.len(),
            "debug parameter identities exactly match physical arity for function {fid} ({})",
            function.name,
        );
    }
    let descriptor = method_descriptor(&param_tys, jvm_declared_ty(&function.ret));
    let mut locals = Vec::new();
    let mut slot = 0u16;
    if !function.is_static {
        locals.push(("this".to_string(), format!("L{owner};"), 0));
        slot = 1;
    }
    let mut body_pc = 0u16;
    for (index, ty) in param_tys.iter().enumerate() {
        let name = parameter_names
            .and_then(|names| names.get(index))
            .cloned()
            .expect("a debug-published parameter needs its canonical source identity");
        if let Some(Some(guarded)) = function.param_checks.get(index) {
            body_pc += aload_len(slot) + cw.string_ldc_len(guarded).unwrap_or(2) + 3;
        }
        locals.push((name, crate::jvm::names::type_descriptor(*ty), slot));
        slot += slot_words(*ty);
    }
    cw.set_method_debug(
        &function.name,
        &descriptor,
        line.map(|line| (body_pc, line)),
        &locals,
    );
    // A GENERATED member has no per-statement source to map: its body belongs to the declaration it
    // was generated for, and kotlinc closes it on that declaration's own closing line — the same two
    // entries it gives a generated `<clinit>`. Only a member that ENDS in a plain `return` has a
    // final instruction to map there; one that returns a value maps its return to the value.
    let Some(start) = line else { return };
    if generated.is_none() || class_end_line == 0 || jvm_declared_ty(&function.ret) != Ty::Unit {
        return;
    }
    let Some(code_len) = cw.method_code_len(&function.name, &descriptor) else {
        return;
    };
    let Some(return_pc) = code_len.checked_sub(1) else {
        return;
    };
    if return_pc <= body_pc {
        return;
    }
    cw.set_method_lines(
        &function.name,
        &descriptor,
        &[(body_pc, start), (return_pc, class_end_line)],
    );
}

/// Byte width of `aload <slot>` (`aload_0..3`, `aload u1`, or `wide aload u2`).
fn aload_len(slot: u16) -> u16 {
    if slot <= 3 {
        1
    } else if slot <= 255 {
        2
    } else {
        4
    }
}
