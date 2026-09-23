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

/// Split the schedule around the primary `<init>`. kotlinc emits a value class's declared members
/// and its `Any` overrides (`toString-impl` … `equals`) first, then the private primary `<init>`,
/// then the generated representation members in a fixed order: `constructor-impl`, `box-impl`,
/// `unbox-impl`, `equals-impl0`. Every other class emits `<init>` first.
pub(super) fn split_around_primary_constructor<'a>(
    ir: &IrFile,
    class: &IrClass,
    ordered: Vec<SourceOrderedMember<'a>>,
) -> (Vec<SourceOrderedMember<'a>>, Vec<SourceOrderedMember<'a>>) {
    if !class.is_value || !class.has_primary_ctor {
        return (Vec::new(), ordered);
    }
    let representation_rank = |member: &SourceOrderedMember<'_>| match member {
        SourceOrderedMember::Function(function) => ir
            .jvm_value_class_representation_order
            .get(function)
            .copied(),
        _ => None,
    };
    let (mut after, before): (Vec<_>, Vec<_>) = ordered
        .into_iter()
        .partition(|member| representation_rank(member).is_some());
    after.sort_by_key(|member| representation_rank(member));
    (before, after)
}
