//! Realization of the `expect` declarations a common source binds.
//!
//! In a refined fragment hierarchy a common source binds a top-level `expect` function or property
//! by its own declaration and signature, as kotlinc does. Every target realizes the `actual` that
//! replaces it, so a selection of the `expect` is lowered as that `actual`: the published
//! actualization pair decides it, never a lookup.

use crate::fir::{CallableId, PropertyId, ResolvedModuleIndex};

/// The callable a selection of `callable` is realized as: the `actual` replacing it when
/// `callable` is an `expect` a common source bound, otherwise `callable` itself.
pub(super) fn realized_callable(index: &ResolvedModuleIndex, callable: CallableId) -> CallableId {
    index
        .callable(callable)
        .and_then(|header| {
            index
                .source_fragments()
                .replacing_actual(header.declaration)
        })
        .and_then(|actual| index.callable_for_declaration(actual))
        .map_or(callable, |actual| actual.id)
}

/// The property a selection of `property` is realized as: the `actual` replacing it when
/// `property` is an `expect` a common source bound, otherwise `property` itself.
pub(super) fn realized_property(index: &ResolvedModuleIndex, property: PropertyId) -> PropertyId {
    index
        .property(property)
        .and_then(|header| {
            index
                .source_fragments()
                .replacing_actual(header.declaration)
        })
        .and_then(|actual| index.property_for_declaration(actual))
        .unwrap_or(property)
}
