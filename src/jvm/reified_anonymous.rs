//! JVM names and construction guards for anonymous classes copied at a reified inline call.
//!
//! Common IR records the copy and its expansion. The physical `{owner}${caller}$$inlined${callee}$N`
//! spelling waits until the caller's placement is final. The declaration class is recorded when
//! its members use a reified parameter, so constructing it calls `needClassReification`; a fully
//! specialized copy is not in that set.

use crate::jvm::classfile::{ClassWriter, CodeBuilder};
use crate::types::TypeName;

/// The coroutine facts that name a copy's continuation classes by their placeholder spelling.
pub(crate) struct Continuations<'a> {
    pub(crate) machines: &'a mut crate::jvm::suspend::EmitTimeMachines,
    pub(crate) metadata: &'a mut crate::jvm::suspend::ContinuationMetadataMap,
}

pub(crate) fn rename(
    ir: &mut crate::ir::IrFile,
    facade: &str,
    modes: crate::jvm::ir_emit::LambdaModes,
    continuations: Continuations<'_>,
) {
    let mut pending = ir
        .specialized_anonymous_classes
        .keys()
        .copied()
        .collect::<Vec<_>>();
    pending.sort_unstable();
    let mut names = crate::fx_hash::HashMap::default();
    let mut renamed_continuations = Vec::new();
    for class in pending {
        let from = ir.classes[class as usize].fq_name;
        let name =
            crate::jvm::ir_emit::lambda_class_names::anonymous_class_name(ir, class, facade, modes)
                .expect("a specialized anonymous object retains complete caller provenance");
        // Coroutine lowering named each continuation of the copy's methods under the copy's
        // placeholder. kotlinc numbers the classes a regenerated object declares under the copy:
        // its continuations take `{copy}$N`, in the order of the methods they re-enter.
        for (ordinal, (continuation, function)) in
            copy_continuations(ir, class).into_iter().enumerate()
        {
            let placeholder = ir.classes[continuation as usize].fq_name;
            let to = crate::types::type_name(&format!("{name}${}", ordinal + 1));
            names.insert(placeholder, to);
            renamed_continuations.push((function, placeholder, to, crate::types::type_name(&name)));
        }
        names.insert(from, crate::types::type_name(&name));
    }
    ir.remap_classifier_identities(&names);
    for (function, from, to, host) in renamed_continuations {
        continuations.machines.rename_continuation(function, to);
        if let Some(mut metadata) = continuations.metadata.remove(&from.render()) {
            let host = host.render();
            metadata.c = host.replace('/', ".");
            metadata.enclosing_class = host;
            continuations.metadata.insert(to.render(), metadata);
        }
    }
}

/// The continuation classes of `copy`'s methods, in the order of the methods they re-enter.
fn copy_continuations(
    ir: &crate::ir::IrFile,
    copy: crate::ir::ClassId,
) -> Vec<(crate::ir::ClassId, crate::ir::FunId)> {
    let methods = &ir.classes[copy as usize].methods;
    let mut continuations = ir
        .jvm_continuation_functions
        .iter()
        .filter_map(|(&continuation, &function)| {
            let position = methods.iter().position(|&method| method == function)?;
            Some((position, continuation, function))
        })
        .collect::<Vec<_>>();
    continuations.sort_unstable();
    continuations
        .into_iter()
        .map(|(_, continuation, function)| (continuation, function))
        .collect()
}

/// `Intrinsics.needClassReification` immediately before `new` of a declaration class, or the load
/// of its instance, when its members or its own function type use a reified parameter. A
/// specialized copy is in neither set, so its construction stays unmarked.
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
    ir.class_id_by_name(internal).is_some_and(|class| {
        ir.reified_anonymous_declarations.contains(&class)
            || ir.classes[class as usize]
                .lambda
                .as_ref()
                .is_some_and(|lambda| lambda.requires_reification)
    })
}
