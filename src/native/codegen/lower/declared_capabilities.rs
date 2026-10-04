//! Runtime capabilities made unsafe by declarations in the current file.
//!
//! These summaries are computed once from checked override identities and classifier facts. Body
//! lowering then asks the summary instead of rediscovering declaration relationships at each call.

use crate::ir::IrFile;
use crate::types::Ty;

use super::classifier_shapes;

/// Dependency types this file puts a class of its own behind, by resolved identity.
pub(super) fn implemented_dependencies(
    ir: &IrFile,
) -> std::collections::HashSet<crate::types::TypeName> {
    let mut owners = std::collections::HashSet::new();
    for edge in ir.function_overrides.values().flatten() {
        if matches!(
            edge.overridden,
            crate::fir::ResolvedFunctionOverrideTarget::External(_)
        ) {
            owners.insert(edge.overridden_owner);
        }
    }
    for edge in ir.property_overrides.values().flatten() {
        if matches!(
            edge.overridden,
            crate::fir::ResolvedPropertyOverrideTarget::External(_)
        ) {
            owners.insert(edge.overridden_owner);
        }
    }
    owners
}

/// Collection representation shapes this file puts a class of its own behind.
pub(super) fn implemented_collections(
    ir: &IrFile,
    classifiers: &dyn crate::backend::BackendClassifierSource,
) -> std::collections::HashSet<super::super::super::intrinsics::CollectionShape> {
    let mut shapes = std::collections::HashSet::new();
    for edge in ir.function_overrides.values().flatten() {
        if matches!(
            edge.overridden,
            crate::fir::ResolvedFunctionOverrideTarget::External(_)
        ) {
            shapes.extend(classifier_shapes::collection_shape(
                classifiers,
                edge.overridden_owner,
            ));
        }
    }
    for edge in ir.property_overrides.values().flatten() {
        if matches!(
            edge.overridden,
            crate::fir::ResolvedPropertyOverrideTarget::External(_)
        ) {
            shapes.extend(classifier_shapes::collection_shape(
                classifiers,
                edge.overridden_owner,
            ));
        }
    }
    shapes
}

/// Whether this file overrides either runtime-owned `Throwable` accessor.
pub(super) fn overrides_a_throwable_accessor(ir: &IrFile) -> bool {
    ir.property_overrides.values().flatten().any(|edge| {
        matches!(
            edge.overridden,
            crate::fir::ResolvedPropertyOverrideTarget::External(_)
        ) && super::super::super::intrinsics::throwable_field(edge.overridden_owner, &edge.name)
            .is_some()
    })
}

/// Whether an object declared here could stand behind a `Comparable<T>` receiver.
pub(super) fn declares_its_own_comparable(ir: &IrFile) -> bool {
    let named = |class: &crate::ir::IrClass| {
        std::iter::once(class.superclass)
            .chain(class.interfaces.iter())
            .chain(
                class
                    .supertypes
                    .iter()
                    .copied()
                    .filter_map(Ty::obj_internal),
            )
            .any(super::super::super::intrinsics::is_comparable_supertype)
    };
    ir.classes.iter().any(|class| {
        !class.enum_entries.is_empty() || class.enum_entry_of.is_some() || named(class)
    }) || ir.function_overrides.values().flatten().any(|edge| {
        matches!(
            edge.overridden,
            crate::fir::ResolvedFunctionOverrideTarget::External(_)
        ) && edge.overridden_semantic_role
            == Some(crate::types::SemanticCallRole::KotlinComparableCompareTo)
    })
}
