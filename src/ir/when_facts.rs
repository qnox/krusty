//! What common lowering records about a `when` (and every `if` it lowers to one), by common-IR
//! identity, for a backend to emit it without re-deciding.

use super::expression_copy::InheritedFacts;
use super::{ExprId, IrExpr};
use crate::fx_hash::HashMap;
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

impl super::IrFile {
    /// A `when` built after checking that yields `result`, `Unit` for a statement. Its producer
    /// names the result, as the checker does for a source `when`, so no backend joins its
    /// branches. A `when` with an `else` records it; one without is a `Unit` statement.
    pub fn add_when(&mut self, branches: Vec<(Option<ExprId>, ExprId)>, result: Ty) -> ExprId {
        let has_else = branches.iter().any(|(condition, _)| condition.is_none());
        debug_assert!(
            has_else || result == Ty::Unit,
            "a `when` without an `else` is a `Unit` statement, not a {result:?}"
        );
        let when = self.add_expr(IrExpr::When { branches });
        if has_else {
            self.whens.exhaustive.insert(when, result);
        }
        when
    }

    /// The type a value `when` yields: the result common lowering recorded for it (as fir2ir types
    /// it), or the checked type of the node when its producer recorded one there. A backend reads
    /// it and chooses only the carrier; it never joins the branches' values.
    pub fn when_result_type(&self, when: ExprId) -> Option<Ty> {
        self.whens
            .exhaustive
            .get(&when)
            .copied()
            .or_else(|| self.physical_type(when))
            .or_else(|| self.logical_type(when))
    }

    /// The first `when` with an `else` whose result type no producer recorded, if any.
    pub fn unrecorded_when_result(&self) -> Option<ExprId> {
        self.exprs.iter().enumerate().find_map(|(id, expression)| {
            let id = ExprId::try_from(id).expect("too many expressions");
            let IrExpr::When { branches } = expression else {
                return None;
            };
            (branches.iter().any(|(condition, _)| condition.is_none())
                && self.when_result_type(id).is_none())
            .then_some(id)
        })
    }
}
