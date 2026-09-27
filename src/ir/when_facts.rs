//! What common lowering records about a `when` (and every `if` it lowers to one), by common-IR
//! identity, for a backend to emit it without re-deciding.

use super::ExprId;
use crate::types::Ty;
use std::collections::HashMap;

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
}
