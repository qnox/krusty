//! What common lowering records about a `when` (and every `if` it lowers to one), by common-IR
//! identity, for a backend to emit it without re-deciding.

use super::expression_copy::InheritedFacts;
use super::ExprId;
use crate::fx_hash::HashMap;
use crate::types::Ty;

#[derive(Clone, Debug, Default)]
pub(crate) struct IrWhenFacts {
    /// Checked result types, as fir2ir types them (`Unit` when not exhaustive down an `else if`
    /// chain). Backends use this to preserve value flow and emit the mandatory no-match failure path
    /// without re-running exhaustiveness analysis.
    pub(crate) exhaustive: HashMap<ExprId, Ty>,
    /// The `when` expressions the source wrote (fir2ir's `IrStatementOrigin.WHEN`), with the line of
    /// their `when` keyword. An `if`, and a `when` lowering builds, is not one. kotlinc's JVM codegen
    /// marks that line and a `nop` before the branches of such a `when` unless it becomes a switch.
    pub(crate) source_lines: HashMap<ExprId, u32>,
    /// The `when` each elvis (`a ?: b`) lowers to, as fir2ir builds it, with the elvis's source line
    /// (`0` when it has none): the first branch tests the left value's temporary against `null` and
    /// yields the right side, the `else` yields the left value. A backend whose platform compiler
    /// rebuilds an elvis's branches at the elvis's own position uses this provenance rather than
    /// recognizing the shape.
    pub(crate) elvises: HashMap<ExprId, u32>,
}

impl InheritedFacts for IrWhenFacts {
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        let Self {
            exhaustive,
            source_lines,
            elvises,
        } = self;
        exhaustive.inherit(source, target);
        source_lines.inherit(source, target);
        elvises.inherit(source, target);
    }
}
