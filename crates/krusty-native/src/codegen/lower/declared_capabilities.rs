//! Runtime capabilities made unsafe by declarations in the current file.
//!
//! These summaries are computed once from checked override identities and classifier facts. Body
//! lowering then asks the summary instead of rediscovering declaration relationships at each call.

use krusty::ir::IrFile;
use krusty::types::Ty;

use super::classifier_shapes;

/// Dependency types this file puts a class of its own behind, by resolved identity.
///
/// A class another unit `laid_out` is not one: it is an ordinary class of the program, whose
/// members a call reaches through its own table whichever subclass the receiver is. That holds
/// only while every instance of the class is the program's: the runtime also makes instances of
/// an integral range's iterator (an array's or a range's walk), and a call keyed on such a
/// receiver must still tell the file's own subclasses from those.
pub(super) fn implemented_dependencies(
    ir: &IrFile,
    laid_out: impl Fn(krusty::types::TypeName) -> bool,
) -> krusty::fx_hash::HashSet<krusty::types::TypeName> {
    let mut owners = krusty::fx_hash::HashSet::default();
    for edge in ir.function_overrides.values().flatten() {
        if matches!(
            edge.overridden,
            krusty::fir::ResolvedFunctionOverrideTarget::External(_)
        ) {
            owners.insert(edge.overridden_owner);
        }
    }
    for edge in ir.property_overrides.values().flatten() {
        if matches!(
            edge.overridden,
            krusty::fir::ResolvedPropertyOverrideTarget::External(_)
        ) {
            owners.insert(edge.overridden_owner);
        }
    }
    owners
        .retain(|&owner| !laid_out(owner) || classifier_shapes::is_integral_range_iterator(owner));
    owners
}

/// Collection representation shapes this file puts a class of its own behind.
pub(super) fn implemented_collections(
    ir: &IrFile,
    classifiers: &dyn krusty::backend::BackendClassifierSource,
) -> krusty::fx_hash::HashSet<super::super::super::intrinsics::CollectionShape> {
    let mut shapes = krusty::fx_hash::HashSet::default();
    for edge in ir.function_overrides.values().flatten() {
        if matches!(
            edge.overridden,
            krusty::fir::ResolvedFunctionOverrideTarget::External(_)
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
            krusty::fir::ResolvedPropertyOverrideTarget::External(_)
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
            krusty::fir::ResolvedPropertyOverrideTarget::External(_)
        ) && super::super::super::intrinsics::throwable_field(edge.overridden_owner, &edge.name)
            .is_some()
    })
}

/// Whether an object declared here could stand behind a `Comparable<T>` receiver.
pub(super) fn declares_its_own_comparable(ir: &IrFile) -> bool {
    let named = |class: &krusty::ir::IrClass| {
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
        // The runtime orders an enum's constants by their ordinals, its own storage.
        !(super::super::super::classes::is_enum(class) || class.is_enum_entry) && named(class)
    }) || ir.function_overrides.values().flatten().any(|edge| {
        matches!(
            edge.overridden,
            krusty::fir::ResolvedFunctionOverrideTarget::External(_)
        ) && edge.overridden_semantic_role
            == Some(krusty::types::SemanticCallRole::KotlinComparableCompareTo)
    })
}
