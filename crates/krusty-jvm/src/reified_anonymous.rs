//! JVM names and construction guards for anonymous classes copied at a reified inline call.
//!
//! Common IR records the copy and its expansion. The physical `{owner}${caller}$$inlined${callee}$N`
//! spelling waits until the caller's placement is final. The declaration class is recorded when
//! its members use a reified parameter, so constructing it calls `needClassReification`; a fully
//! specialized copy is not in that set.

use krusty::jvm::classfile::{ClassWriter, CodeBuilder};
use krusty::types::TypeName;

/// The coroutine facts that name a copy's continuation classes by their placeholder spelling.
pub(crate) struct Continuations<'a> {
    pub(crate) machines: &'a mut crate::suspend::EmitTimeMachines,
    pub(crate) metadata: &'a mut crate::suspend::ContinuationMetadataMap,
}

pub(crate) fn rename(
    ir: &mut krusty::ir::IrFile,
    facade: &str,
    modes: crate::ir_emit::LambdaModes,
    continuations: Continuations<'_>,
) {
    let mut pending = ir
        .specialized_anonymous_classes
        .keys()
        .copied()
        .collect::<Vec<_>>();
    pending.sort_unstable();
    let mut names = krusty::fx_hash::HashMap::default();
    let mut renamed_continuations = Vec::new();
    for class in pending {
        let from = ir.classes[class as usize].fq_name;
        // kotlinc keeps the declaration's captured parameters in @Metadata while the JVM
        // Signature names the applied superclass/interfaces of this call-site class.
        if let Some(mut signature) = ir.class_signature_name(from).cloned() {
            let copy = &ir.classes[class as usize];
            signature.supers = copy.supertypes.clone();
            if !signature
                .supers
                .iter()
                .any(|parent| parent.obj_internal() == Some(copy.superclass))
            {
                signature
                    .supers
                    .insert(0, krusty::types::Ty::obj_name(copy.superclass));
            }
            ir.insert_lowered_class_signature(from, signature);
        }
        let name =
            crate::ir_emit::lambda_class_names::anonymous_class_name(ir, class, facade, modes)
                .expect("a specialized anonymous object retains complete caller provenance");
        // Coroutine lowering named each continuation of the copy's methods under the copy's
        // placeholder. kotlinc numbers the classes a regenerated object declares under the copy:
        // its continuations take `{copy}$N`, in the order of the methods they re-enter.
        for (ordinal, (continuation, function)) in
            copy_continuations(ir, class).into_iter().enumerate()
        {
            let placeholder = ir.classes[continuation as usize].fq_name;
            let to = krusty::types::type_name(&format!("{name}${}", ordinal + 1));
            names.insert(placeholder, to);
            renamed_continuations.push((
                function,
                placeholder,
                to,
                krusty::types::type_name(&name),
            ));
        }
        names.insert(from, krusty::types::type_name(&name));
    }
    ir.remap_classifier_identities(&names);
    for (function, from, to, host) in renamed_continuations {
        continuations.machines.rename_continuation(function, to);
        if let Some(mut metadata) = continuations.metadata.remove(&from) {
            // The classfile and `@DebugMetadata` spell the host; the map keeps its identity.
            let host = host.render();
            metadata.c = host.replace('/', ".");
            metadata.enclosing_class = host;
            continuations.metadata.insert(to, metadata);
        }
    }
}

/// The continuation classes of `copy`'s methods, in the order of the methods they re-enter.
fn copy_continuations(
    ir: &krusty::ir::IrFile,
    copy: krusty::ir::ClassId,
) -> Vec<(krusty::ir::ClassId, krusty::ir::FunId)> {
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
    ir: &krusty::ir::IrFile,
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

fn needs_class_reification(ir: &krusty::ir::IrFile, internal: TypeName) -> bool {
    ir.class_id_by_name(internal).is_some_and(|class| {
        ir.reified_anonymous_declarations.contains(&class)
            || ir.classes[class as usize]
                .lambda
                .as_ref()
                .is_some_and(|lambda| lambda.requires_reification)
    })
}
