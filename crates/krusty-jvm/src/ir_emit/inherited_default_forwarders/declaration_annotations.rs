//! The annotations of the exact declaration an inherited default forwarder republishes.
//!
//! A declaration this module compiles is read from the file that owns it. Another file of the
//! same module does not carry that function, and the sibling class is not a classpath dependency,
//! so a missing local function means the declaration states nothing this file can copy. A
//! dependency declaration is read back from the class file that publishes it, and only when that
//! file records annotation classes: an unannotated dependency has nothing to copy.
//!
//! A class that already has the method on a superclass copies that method's annotations instead.
//! The bridge still calls the selected interface body, and kotlinc keeps the superclass method's
//! annotations on the override.

use super::*;
use krusty::fir::{
    InheritedMemberName, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget,
};
use krusty::ir::{DeclarationAnnotations, IrLocalPropertyLayout, RetainedAnnotation};

pub(super) fn selected(
    ir: &IrFile,
    default: &krusty::fir::ResolvedInheritedDefault,
    _forwarder: &InheritedForwarder,
    env: &EmitEnv<'_>,
) -> Option<DeclarationAnnotations> {
    match (default.function, default.accessor) {
        (Some(ResolvedFunctionOverrideTarget::Module(callable)), None) => Some(
            ir.checked_callable_functions
                .get(&callable)
                .and_then(|function| ir.function_annotations.get(function))
                .cloned()
                .unwrap_or_default(),
        ),
        (None, Some(ResolvedPropertyOverrideTarget::Module(property))) => {
            Some(module_property_annotations(ir, default, property))
        }
        (Some(ResolvedFunctionOverrideTarget::External(callable)), None)
        | (None, Some(ResolvedPropertyOverrideTarget::External(callable))) => {
            dependency_annotations(callable, env)
        }
        _ => Some(DeclarationAnnotations::default()),
    }
}

/// Annotations of the superclass method this forwarder overrides, when that method is already in
/// a class file. Nullability stays on the forwarder's own result and parameters.
pub(super) fn superclass_method(
    class: &krusty::ir::IrClass,
    forwarder: &InheritedForwarder,
    env: &EmitEnv<'_>,
) -> Option<DeclarationAnnotations> {
    if !class.has_non_top_superclass() {
        return None;
    }
    let mut owner = class.superclass();
    for _ in 0..8 {
        if owner.is_empty() || owner == "java/lang/Object" {
            return None;
        }
        let bytes = env.bodies.class_file(&owner)?;
        if let Some(annotations) = krusty::jvm::classreader::read_method_annotation_applications(
            &bytes,
            &forwarder.name,
            &forwarder.descriptor(),
        ) {
            return Some(without_jvm_nullability(annotations));
        }
        owner = krusty::jvm::classreader::parse_class(&bytes)
            .ok()?
            .super_class()?;
    }
    None
}

fn without_jvm_nullability(
    annotations: Vec<(
        krusty::types::AnnotationRetention,
        krusty::types::ResolvedAnnotation,
    )>,
) -> DeclarationAnnotations {
    DeclarationAnnotations::new(
        annotations
            .into_iter()
            .filter(|(_, annotation)| {
                !annotation
                    .annotation
                    .matches("org/jetbrains/annotations/NotNull")
                    && !annotation
                        .annotation
                        .matches("org/jetbrains/annotations/Nullable")
            })
            .map(|(retention, annotation)| RetainedAnnotation {
                retention,
                annotation: krusty::ir::AppliedAnnotation::from(&annotation),
                facts: annotation.facts,
            })
            .collect(),
    )
}

fn module_property_annotations(
    ir: &IrFile,
    default: &krusty::fir::ResolvedInheritedDefault,
    property: krusty::fir::PropertyId,
) -> DeclarationAnnotations {
    let setter = matches!(default.name, InheritedMemberName::PropertySetter(_));
    let function = ir.local_property_layouts.get(&property).and_then(|layout| {
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
    });
    function
        .and_then(|function| ir.function_annotations.get(&function).cloned())
        .unwrap_or_default()
}

fn dependency_annotations(
    identity: krusty::fir::ExternalCallableId,
    env: &EmitEnv<'_>,
) -> Option<DeclarationAnnotations> {
    let fact = env
        .dependency_callables
        .callable(identity)
        .expect("an inherited dependency declaration has frozen callable facts");
    if fact.annotations.is_empty() {
        return Some(DeclarationAnnotations::default());
    }
    let owner = fact.physical_owner.render();
    let name = fact.physical_name();
    let descriptor = fact.descriptor.as_str();
    let annotations = env.bodies.class_file(&owner).and_then(|bytes| {
        krusty::jvm::classreader::read_method_annotation_applications(&bytes, name, descriptor)
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
