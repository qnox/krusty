//! Selects the `CharSequence` members a target's `StringBuilder` reads an appended sequence
//! through ([`crate::backend::Backend::appends_char_sequences_by_index`]).
//!
//! Kotlin/Wasm WASI appends a `CharSequence` to a builder by reading its `length` and each indexed
//! `Char` (`StringBuilder.append(CharSequence?)` and `appendRange`), so a custom sequence's own
//! `length` and `get` run, and its `toString` does not. A backend that realizes the builder itself
//! calls those members whether or not the source names them; a lowering that runs after checking
//! cannot select a dependency declaration, so the handoff selects both here, as it does the
//! coroutine runtime.

use crate::backend::{CharSequenceProtocol, CheckedBackendCallables, DependencyFactError};
use crate::libraries::{FnKind, PropKind};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

/// Freeze the `CharSequence` members into `callables`. A compilation whose classpath declares no
/// single `length` property and no single `get(Int): Char` on `CharSequence` freezes nothing, and
/// the target declines an append that needs them.
pub(super) fn freeze(
    callables: &mut CheckedBackendCallables,
    source: &dyn SymbolSource,
) -> Result<(), DependencyFactError> {
    match select(source) {
        Some(protocol) => callables.freeze_char_sequence_protocol(protocol, source),
        None => Ok(()),
    }
}

fn select(source: &dyn SymbolSource) -> Option<CharSequenceProtocol> {
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
