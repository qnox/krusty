//! The checked form of a `for` loop that destructures `withIndex()` in its header, which kotlinc's
//! `ForLoopsLowering` iterates without the `IndexedValue`s (`WithIndexHandler`,
//! `WithIndexLoopHeader`).

use super::FirLoopHeader;
use crate::fir::{FirStatementId, LocalValueId};

/// `for ((i, v) in x.withIndex())`: the loop over `x` itself, an index counted beside it, and the
/// destructuring declaration whose entries read the index or the element instead of an
/// `IndexedValue`.
#[derive(Clone, Debug, PartialEq)]
pub struct FirWithIndexLoop {
    /// The loop over the receiver of `withIndex()`, as a loop over that receiver would be
    /// checked. Its variable is the element, which no source declaration names.
    pub nested: FirLoopHeader,
    /// The index the loop counts unless the nested loop's own counter counts the same way.
    pub index: LocalValueId,
    /// The destructuring the loop body opened with. The checked body no longer contains it: each
    /// iteration binds its entries, in its entries' order, before the body runs.
    pub destructure: FirStatementId,
    /// Which `IndexedValue` component each destructuring entry reads, parallel to its entries;
    /// `None` for a positional `_`, which reads nothing.
    pub components: Box<[Option<FirIndexedValueComponent>]>,
}

impl FirWithIndexLoop {
    /// Whether any entry reads the element.
    pub fn reads_value(&self) -> bool {
        self.components
            .contains(&Some(FirIndexedValueComponent::Value))
    }
}

/// A component of `kotlin.collections.IndexedValue`, selected by position (`component1`,
/// `component2`) or by property (`index`, `value`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirIndexedValueComponent {
    Index,
    Value,
}
