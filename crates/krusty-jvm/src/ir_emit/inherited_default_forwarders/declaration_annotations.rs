//! The annotations of the exact declaration an inherited default forwarder republishes.

use super::*;
use krusty::fir::{
    InheritedMemberName, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget,
};
use krusty::ir::{DeclarationAnnotations, IrLocalPropertyLayout, RetainedAnnotation};

pub(super) fn selected(
    ir: &IrFile,
    default: &krusty::fir::ResolvedInheritedDefault,
    forwarder: &InheritedForwarder,
    env: &EmitEnv<'_>,
) -> Option<DeclarationAnnotations> {
    let module_function = match (default.function, default.accessor) {
        (Some(ResolvedFunctionOverrideTarget::Module(callable)), None) => {
            ir.checked_callable_functions.get(&callable).copied()
        }
        (None, Some(ResolvedPropertyOverrideTarget::Module(property))) => {
            let setter = matches!(default.name, InheritedMemberName::PropertySetter(_));
            ir.local_property_layouts.get(&property).and_then(|layout| {
                let (getter, write) = match layout {
                    IrLocalPropertyLayout::TopLevelStorage { getter, setter, .. }
                    | IrLocalPropertyLayout::Member { getter, setter, .. } => (*getter, *setter),
                    IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. }
                    | IrLocalPropertyLayout::MemberExtension { getter, setter, .. } => {
                        (Some(*getter), *setter)
                    }
                };
                if setter {
                    write
                } else {
                    getter
                }
            })
        }
        _ => None,
    };
    if let Some(function) = module_function {
        return Some(
            ir.function_annotations
                .get(&function)
                .cloned()
                .unwrap_or_default(),
        );
    }
    let external = match (default.function, default.accessor) {
        (Some(ResolvedFunctionOverrideTarget::External(callable)), None)
        | (None, Some(ResolvedPropertyOverrideTarget::External(callable))) => Some(callable),
        _ => None,
    };
    let (owner, name, descriptor) = match external {
        Some(identity) => {
            let fact = env
                .dependency_callables
                .callable(identity)
                .expect("an inherited dependency declaration has frozen callable facts");
            if fact.annotations.is_empty() {
                return Some(DeclarationAnnotations::default());
            }
            (
                fact.physical_owner.render(),
                fact.physical_name().to_owned(),
                fact.descriptor.clone(),
            )
        }
        None => (
            default.declaring_interface.render(),
            forwarder.target_name.clone(),
            forwarder.target_member_descriptor(),
        ),
    };
    let annotations = env.bodies.class_file(&owner).and_then(|bytes| {
        krusty::jvm::classreader::read_method_annotation_applications(&bytes, &name, &descriptor)
    });
    let Some(annotations) = annotations else {
        env.run.set_emit_error(format!(
            "inherited default declaration has no readable annotation attributes: {owner}.{name}{descriptor}"
        ));
        return None;
    };
    Some(DeclarationAnnotations::new(
        annotations
            .into_iter()
            .map(|(retention, annotation)| RetainedAnnotation {
                retention,
                annotation: krusty::ir::AppliedAnnotation::from(&annotation),
                facts: annotation.facts,
            })
            .collect(),
    ))
}
