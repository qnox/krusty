//! What a loop that reads its elements by index (kotlinc's `IndexedGetIterationHandler`) indexes,
//! and through which declarations.

use super::{FirCallTarget, FirPropertyTarget};

/// The iterable of an indexed loop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirBuiltinIterableKind {
    Array,
    String,
    /// Any other `CharSequence` (`CharSequenceIterationHandler`). Its length may change while the
    /// loop runs, so the loop reads it again before every iteration instead of caching it.
    CharSequence(Box<FirCharSequenceIndexing>),
}

/// The `length` and `get(Int)` of `kotlin.CharSequence` an indexed loop over a `CharSequence`
/// reads, selected by resolution on `CharSequence` itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirCharSequenceIndexing {
    pub length: FirPropertyTarget,
    pub get: FirCallTarget,
}
