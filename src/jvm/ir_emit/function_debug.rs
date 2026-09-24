//! JVM debug tables for declared and explicitly published generated functions.

use super::{jvm_declared_ty, jvm_function_params, method_descriptor, slot_words};
use crate::ir::IrFile;
use crate::jvm::classfile::ClassWriter;

/// Attach kotlinc's `LineNumberTable` + `LocalVariableTable` to a declared method. Source methods
/// take their line/name identities from lowering; generated methods use their producer-owned
/// publication contract. Both paths reject incomplete parameter identities.
pub(super) fn attach_declared_function_debug(
    ir: &IrFile,
    fid: u32,
    owner: &str,
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
    let parameter_identities = ir.function_parameter_identities(fid);
    if !param_tys.is_empty() {
        let parameter_identities = parameter_identities
            .expect("a debug-published function carries exact parameter identities");
        assert_eq!(
            parameter_identities.len(),
            param_tys.len(),
            "debug parameter identities exactly match physical arity for function {fid} ({})",
            function.name,
        );
    }
    let descriptor = method_descriptor(&param_tys, jvm_declared_ty(&function.ret));
    let assertion_names = crate::jvm::parameter_names::function_assertions(ir, fid, &param_tys);
    let mut locals = Vec::new();
    let mut slot = 0u16;
    if !function.is_static {
        locals.push(("this".to_string(), format!("L{owner};"), 0));
        slot = 1;
    }
    let mut body_pc = 0u16;
    let local_names = crate::jvm::parameter_names::function_locals(ir, fid, &param_tys);
    for (index, ty) in param_tys.iter().enumerate() {
        let name = local_names
            .as_ref()
            .and_then(|names| names.get(index))
            .cloned()
            .expect("a debug-published parameter needs its canonical source identity");
        if function
            .param_checks
            .get(index)
            .is_some_and(Option::is_some)
        {
            let guarded = assertion_names
                .as_ref()
                .and_then(|names| names.get(index))
                .and_then(Clone::clone)
                .expect("a checked parameter carries an assertion identity");
            body_pc += aload_len(slot) + cw.string_ldc_len(&guarded).unwrap_or(2) + 3;
        }
        if let Some(name) = name {
            locals.push((name, crate::jvm::names::type_descriptor(*ty), slot));
        }
        slot += slot_words(*ty);
    }
    cw.set_method_debug(
        &function.name,
        &descriptor,
        line.map(|line| (body_pc, line)),
        &locals,
    );
    // Only the generating producer may request a closing-line entry, and only the declared-method
    // emitter can identify the implicit fallthrough return's exact bytecode position. A diverging
    // body has no recorded return and therefore retains the declaration-line-only table.
    let Some(start) = line else { return };
    let Some(fallthrough_line) =
        generated.and_then(|publication| publication.debug.fallthrough_line())
    else {
        return;
    };
    let Some(return_pc) = cw.method_implicit_void_return_pc(&function.name, &descriptor) else {
        return;
    };
    if return_pc <= body_pc {
        return;
    }
    cw.set_method_lines(
        &function.name,
        &descriptor,
        &[(body_pc, start), (return_pc, fallthrough_line)],
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
