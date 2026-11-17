//! Selects the collection members a target's runtime `ArrayList` reads another collection through
//! ([`crate::fir::CollectionProtocol`]).
//!
//! The stdlib's `ArrayList.addAll` and `containsAll` walk their argument with its `iterator()`,
//! and `removeAll` and `retainAll` ask it `contains`, so a custom collection's own members run. A
//! target realizing the list itself calls those members whether or not the source names them, so
//! the handoff selects them from the compilation's provider and freezes the selection.

use crate::fir::{CollectionProtocol, ExternalCallableId};
use crate::libraries::FnKind;
use crate::symbol_source::SymbolSource;
use crate::types::{wk, Ty, TypeName};

/// The one `iterator()` `Iterable` declares, the one `hasNext(): Boolean` and `next()` `Iterator`
/// declares, and the one `contains(E)` `Collection` declares; `None` when the classpath declares
/// no single one of any.
pub(crate) fn selected(source: &dyn SymbolSource) -> Option<CollectionProtocol> {
    Some(CollectionProtocol {
        iterator: member(source, wk::iterable(), "iterator", 0, None)?,
        has_next: member(source, wk::iterator(), "hasNext", 0, Some(Ty::Boolean))?,
        next: member(source, wk::iterator(), "next", 0, None)?,
        contains: member(source, wk::collection(), "contains", 1, Some(Ty::Boolean))?,
    })
}

/// The one member function `owner` declares as `name` with `arity` parameters, answering
/// `result` when it is given.
fn member(
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &str,
    arity: usize,
    result: Option<Ty>,
) -> Option<ExternalCallableId> {
    let declared =
        crate::symbol_resolver::declared_member_callables(source, Ty::obj_name(owner), name);
    let mut candidates = declared.functions().iter().filter(|function| {
        function.kind == FnKind::Member
            && function.callable.params.len() == arity
            && result.is_none_or(|result| function.callable.ret.canonical_semantic() == result)
    });
    let (Some(selected), None) = (candidates.next(), candidates.next()) else {
        return None;
    };
    selected.callable.external_identity
}

#[cfg(test)]
mod tests;
