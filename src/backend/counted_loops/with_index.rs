//! `WithIndexLoopHeader` over a counted loop: the index the loop counts beside its counter, or the
//! counter itself when that starts at constant `0` and steps by `1`.

use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrLoopIndex};
use crate::types::Ty;

use super::header::ProgressionHeader;
use super::{constant_value, Realizer};

/// The index of one counted `withIndex()` loop, once its counter is known.
pub(super) struct LoopIndex {
    /// The index's value slot.
    pub(super) slot: u32,
    /// The counter counts the index, so the loop declares and steps no index of its own.
    pub(super) is_counter: bool,
    /// `var index = 0`, declared after the loop's own variables when the loop counts its own.
    pub(super) declaration: Option<ExprId>,
    bindings: Vec<ExprId>,
    element_bound: bool,
    element_copies: Vec<ExprId>,
}

impl Realizer<'_> {
    /// Whether the counter of the loop `header` builds is the index: an `Int` counter whose first
    /// value is constant `0` and whose step is constant `1`.
    pub(super) fn loop_index(
        &mut self,
        with_index: &IrLoopIndex,
        header: &ProgressionHeader,
        ty: Ty,
        first_constant: Option<i64>,
    ) -> LoopIndex {
        let IrExpr::Variable { index: slot, .. } = *self.ir.expr(with_index.declaration) else {
            unreachable!("common lowering declares a counted loop's index as a variable");
        };
        let is_counter = ty == Ty::Int
            && first_constant == Some(0)
            && constant_value(self.ir, header.step.value) == Some(1);
        LoopIndex {
            slot,
            is_counter,
            declaration: (!is_counter).then_some(with_index.declaration),
            bindings: self.block_statements(with_index.bindings),
            element_bound: with_index.element_bound,
            element_copies: self.block_statements(with_index.element_copies),
        }
    }

    fn block_statements(&self, block: ExprId) -> Vec<ExprId> {
        match self.ir.expr(block) {
            IrExpr::Block { stmts, value: None } => stmts.clone(),
            _ => vec![block],
        }
    }

    /// `initializeIteration`: the index bindings, the index's own step, then the element read
    /// from the counter. An element no entry binds is read only when the loop compares it with
    /// `last` to leave, into a temporary (`loopVariable`). Returns the loop variable and the
    /// statements.
    pub(super) fn with_index_iteration(
        &mut self,
        index: LoopIndex,
        induction: u32,
        (variable, name): (u32, Option<&str>),
        ty: Ty,
        can_overflow: bool,
    ) -> (u32, Vec<ExprId>) {
        let mut statements = index.bindings;
        if !index.is_counter {
            let current = self.add(IrExpr::GetValue(index.slot));
            let one = self.add(IrExpr::Const(IrConst::Int(1)));
            let next = self.add(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Add,
                lhs: current,
                rhs: one,
            });
            let step = self.add(IrExpr::SetValue {
                var: index.slot,
                value: next,
            });
            self.ir.plain_updates.insert(step);
            statements.push(step);
        }
        let current = self.add(IrExpr::GetValue(induction));
        if index.element_bound {
            statements.push(self.loop_variable_declaration(variable, name, ty, current));
            statements.extend(index.element_copies);
            (variable, statements)
        } else if can_overflow {
            let slot = self.allocate_temporary();
            statements.push(self.add(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(current),
                named: false,
            }));
            (slot, statements)
        } else {
            (induction, statements)
        }
    }
}
