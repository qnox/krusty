//! JVM names and construction guards for anonymous classes copied at a reified inline call.
//!
//! Common IR records the copy and its expansion. The physical `{owner}${caller}$$inlined${callee}$N`
//! spelling waits until the caller's placement is final. The declaration class is recorded when
//! its members use a reified parameter, so constructing it calls `needClassReification`; a fully
//! specialized copy is not in that set.

use crate::jvm::classfile::{ClassWriter, CodeBuilder};
use crate::types::TypeName;

pub(crate) fn rename(
    ir: &mut crate::ir::IrFile,
    facade: &str,
    modes: crate::jvm::ir_emit::LambdaModes,
) {
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

/// `Intrinsics.needClassReification` immediately before `new` of a declaration class whose members
/// use a reified parameter. A specialized copy is absent from that set, so its construction stays
/// unmarked.
pub(crate) fn guard_reified_construction(
    ir: &crate::ir::IrFile,
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

fn needs_class_reification(ir: &crate::ir::IrFile, internal: TypeName) -> bool {
    ir.class_id_by_name(internal)
        .is_some_and(|class| ir.reified_anonymous_declarations.contains(&class))
}
