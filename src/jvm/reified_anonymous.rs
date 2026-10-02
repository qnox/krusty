//! JVM names and construction guards for anonymous classes copied at a reified inline call.
//!
//! Common IR records the copy and its expansion. The physical `{owner}${caller}$$inlined${callee}$N`
//! spelling waits until the caller's placement is final. The declaration class still names a
//! reified parameter, so constructing it calls `needClassReification`; a fully specialized copy
//! does not.

use crate::ir::{IrExpr, IrFile};
use crate::jvm::classfile::{ClassWriter, CodeBuilder};
use crate::types::TypeName;

pub(crate) fn rename(ir: &mut IrFile, facade: &str, modes: crate::jvm::ir_emit::LambdaModes) {
    let pending = ir
        .specialized_anonymous_classes
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let mut names = std::collections::HashMap::new();
    for class in pending {
        let from = ir.classes[class as usize].fq_name;
        let name =
            crate::jvm::ir_emit::lambda_class_names::anonymous_class_name(ir, class, facade, modes)
                .expect("a specialized anonymous object retains complete caller provenance");
        names.insert(from, crate::types::type_name(&name));
    }
    ir.remap_classifier_identities(&names);
}

/// `Intrinsics.needClassReification` immediately before `new` of a class whose members still use
/// a reified parameter. A specialized copy has none, so its construction stays unmarked.
pub(crate) fn guard_reified_construction(
    ir: &IrFile,
    writer: &mut ClassWriter,
    code: &mut CodeBuilder,
    internal: TypeName,
) {
    if !needs_class_reification(ir, internal) {
        return;
    }
    let marker = writer.methodref(
        "kotlin/jvm/internal/Intrinsics",
        "needClassReification",
        "()V",
    );
    code.invokestatic(marker, 0, 0);
}

fn needs_class_reification(ir: &IrFile, internal: TypeName) -> bool {
    let Some(class) = ir.class_id_by_name(internal) else {
        return false;
    };
    let Some(class) = ir.classes.get(class as usize) else {
        return false;
    };
    if !class.is_anonymous_object {
        return false;
    }
    class.methods.iter().any(|&method| {
        ir.functions
            .get(method as usize)
            .and_then(|function| function.body)
            .is_some_and(|body| body_has_reified_markers(ir, body))
    })
}

fn body_has_reified_markers(ir: &IrFile, expression: crate::ir::ExprId) -> bool {
    if matches!(
        ir.expr(expression),
        IrExpr::ReifiedClassMarker { .. } | IrExpr::ReifiedTypeOp { .. }
    ) {
        return true;
    }
    let mut found = false;
    crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
        found |= body_has_reified_markers(ir, child);
    });
    found
}
