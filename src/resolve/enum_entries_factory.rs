//! Selects the stdlib function an enum's `entries` list is built by
//! ([`crate::fir::EnumEntriesFactory`]).
//!
//! kotlinc-js/wasm build an enum's `entries` by calling `kotlin.enums.enumEntries(Array<E>)` with
//! the enum's `values()`; its library body makes the stdlib's `EnumEntriesList`. The shared enum
//! realization builds that call after checking for every enum of a target that follows them, so
//! resolution selects the declaration once per compilation, as it selects the coroutine runtime,
//! and the realization and the dependency prewalk both read the selected identity.

use crate::fir::EnumEntriesFactory;
use crate::libraries::FnKind;
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::type_name;

/// The one top-level `kotlin.enums.enumEntries` taking a single `Array` of its one type parameter,
/// or `None` when the platform declares no single one.
pub(super) fn selected(source: &dyn SymbolSource) -> Option<EnumEntriesFactory> {
    let symbols = source.symbols(
        SymbolNamespace::Package(type_name("kotlin/enums")),
        "enumEntries",
    );
    let array = type_name("kotlin/Array");
    let mut factories = symbols.callables.functions().iter().filter(|function| {
        function.kind == FnKind::TopLevel
            && function.semantic_receiver().is_none()
            && function.context_count == 0
            && matches!(
                &*function.semantic_params(),
                [param] if param.non_null().obj_internal() == Some(array)
            )
    });
    let (Some(factory), None) = (factories.next(), factories.next()) else {
        return None;
    };
    Some(EnumEntriesFactory {
        function: factory.callable.external_identity?,
    })
}

#[cfg(test)]
mod tests;
