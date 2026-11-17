//! Selects the `CharSequence` members a target's `StringBuilder` reads an appended sequence
//! through ([`crate::fir::CharSequenceProtocol`]).
//!
//! Kotlin/Wasm WASI appends a `CharSequence` to a builder by reading its `length` and each indexed
//! `Char` (`StringBuilder.append(CharSequence?)` and `appendRange`), so a custom sequence's own
//! `length` and `get` run, and its `toString` does not. A target realizing the builder itself calls
//! those members whether or not the source names them, so resolution selects them once per
//! compilation, as it selects the coroutine runtime, and the handoff freezes the selection.

use crate::fir::CharSequenceProtocol;
use crate::libraries::{FnKind, PropKind};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// The one `length` property and the one `get(Int): Char` function `CharSequence` declares, or
/// `None` when the classpath declares no single one of either.
pub(super) fn selected(source: &dyn SymbolSource) -> Option<CharSequenceProtocol> {
    let sequence = Ty::obj_name(char_sequence());
    let lengths = crate::symbol_resolver::declared_member_callables(source, sequence, "length");
    let mut length = lengths.properties().iter().filter(|property| {
        property.kind == PropKind::Member && property.ty.canonical_semantic() == Ty::Int
    });
    let (Some(length), None) = (length.next(), length.next()) else {
        return None;
    };
    let gets = crate::symbol_resolver::declared_member_callables(source, sequence, "get");
    let mut get = gets.functions().iter().filter(|function| {
        function.kind == FnKind::Member
            && function
                .callable
                .params
                .iter()
                .map(|param| param.canonical_semantic())
                .eq([Ty::Int])
            && function.callable.ret.canonical_semantic() == Ty::Char
    });
    let (Some(get), None) = (get.next(), get.next()) else {
        return None;
    };
    Some(CharSequenceProtocol {
        length: length.getter.external_property_identity?,
        get: get.callable.external_identity?,
    })
}

fn char_sequence() -> TypeName {
    crate::types::wk::char_sequence()
}

#[cfg(test)]
mod tests;
