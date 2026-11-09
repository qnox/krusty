//! What common lowering records about a `when` (and every `if` it lowers to one), by common-IR
//! identity, for a backend to emit it without re-deciding.

use super::expression_copy::InheritedFacts;
use super::ExprId;
use crate::fx_hash::{HashMap, HashSet};
use crate::types::Ty;

#[derive(Clone, Debug, Default)]
pub struct IrWhenFacts {
    /// Checked result types, as fir2ir types them (`Unit` when not exhaustive down an `else if`
    /// chain). Backends use this to preserve value flow and emit the mandatory no-match failure path
    /// without re-running exhaustiveness analysis.
    pub exhaustive: HashMap<ExprId, Ty>,
    /// The `when` expressions the source wrote (fir2ir's `IrStatementOrigin.WHEN`), with the line of
    /// their `when` keyword. An `if`, and a `when` lowering builds, is not one. kotlinc's JVM codegen
    /// marks that line and a `nop` before the branches of such a `when` unless it becomes a switch.
    pub source_lines: HashMap<ExprId, u32>,
    /// The `when` each elvis (`a ?: b`) lowers to, as fir2ir builds it, with the elvis's source line
    /// (`0` when it has none): the first branch tests the left value's temporary against `null` and
    /// yields the right side, the `else` yields the left value. A backend whose platform compiler
    /// rebuilds an elvis's branches at the elvis's own position uses this provenance rather than
    /// recognizing the shape.
    pub elvises: HashMap<ExprId, u32>,
    /// The elvis `when`s that are a later link of a chain `a ?: b ?: c`. kotlinc folds a chain into
    /// one block (`JvmSafeCallChainFoldingLowering`), so a later link opens no elvis block of its
    /// own and its code takes the position the chain starts at.
    pub elvis_chain_links: HashSet<ExprId>,
}

impl InheritedFacts for IrWhenFacts {
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        let Self {
            exhaustive,
            source_lines,
            elvises,
            elvis_chain_links,
        } = self;
        exhaustive.inherit(source, target);
        source_lines.inherit(source, target);
        elvises.inherit(source, target);
        elvis_chain_links.inherit(source, target);
    }
}
