//! Source-owned class-member scheduling for JVM emission.

use super::secondary_constructor::defer_serialization_constructor;
use crate::ir::{IrClass, IrFile, IrProperty, IrSecondaryCtor};

pub(super) enum SourceOrderedMember<'a> {
    Property(&'a IrProperty),
    Function(u32),
    SecondaryConstructor(usize, &'a IrSecondaryCtor),
}

/// Interleave source declarations through one stable ordering key. Generated constructors are
/// excluded only when their producer recorded an exact later placement; JVM access flags never
/// participate in this semantic schedule.
pub(super) fn source_ordered_members<'a>(
    ir: &IrFile,
    class: &'a IrClass,
    deferred_serialization_constructor: Option<u32>,
) -> Vec<SourceOrderedMember<'a>> {
    let mut ordered = Vec::with_capacity(
        class.properties.len() + class.methods.len() + class.secondary_ctors.len(),
    );
    ordered.extend(class.properties.iter().map(SourceOrderedMember::Property));
    ordered.extend(
        class
            .methods
            .iter()
            .copied()
            .filter(|function| !ir.serialization_cache_methods.contains(function))
            .map(SourceOrderedMember::Function),
    );
    ordered.extend(
        class
            .secondary_ctors
            .iter()
            .enumerate()
            .filter(|(ordinal, _)| {
                !defer_serialization_constructor(*ordinal, deferred_serialization_constructor)
            })
            .map(|(ordinal, constructor)| {
                SourceOrderedMember::SecondaryConstructor(ordinal, constructor)
            }),
    );
    ordered.sort_by_key(|member| match member {
        SourceOrderedMember::Property(property) => property.source_order,
        SourceOrderedMember::Function(function) => ir
            .fn_source_order
            .get(function)
            .copied()
            .unwrap_or(u32::MAX),
        SourceOrderedMember::SecondaryConstructor(_, constructor) => constructor.source_order,
    });
    ordered
}
