//! The `CharSequence` members a target's `StringBuilder` reads an appended sequence through.
//!
//! Kotlin/Wasm WASI appends a `CharSequence` to a builder by reading its `length` and each indexed
//! `Char`, so a custom sequence's own members run. Resolution selects the two members once per
//! compilation (`crate::resolve::PassTwoSymbols::char_sequence_protocol`), and a target whose
//! builder reads them (both Wasm stdlibs check `appendRange` against `length`) receives them frozen
//! with each unit's other dependency declarations
//! (`crate::backend::CheckedBackendCallables::char_sequence_protocol`).

use super::{ExternalCallableId, ExternalPropertyId};

/// `CharSequence.length` and `CharSequence.get(Int)`, as their provider declares them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharSequenceProtocol {
    pub length: ExternalPropertyId,
    pub get: ExternalCallableId,
}
