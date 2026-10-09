//! The last normalization before a body is split: a suspension still sitting in an operand of a
//! statement the state machine does not split by shape gets bound to a preceding temp.
//!
//! The statement-shaped passes before this one leave every suspension the JVM's flattener handles
//! in place: a captured-variable write, a property write, a checked coercion of a suspend call.
//! The shared state machine splits only the plain positions (a statement, a local's initializer, an
//! assignment's value, a `return`'s value), so each other statement goes through the ordinary
//! operand hoister, which keeps Kotlin's left-to-right evaluation order.

use std::collections::{HashMap, HashSet};

use crate::ir::{ExprId, IrExpr, IrFile};
use crate::types::Ty;

use super::bottom_completion::unwrap_suspend_cast;
use super::hoisting::{hoist_call_operands_in_order, hoist_expr};
use super::suspension_points::{expr_calls_suspend, is_suspension_point};
use super::SuspensionTyping;

/// Hoist every residual operand suspension in the statement regions of `body`.
pub(crate) fn hoist_operand_suspensions(
    ir: &mut IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    typing: &SuspensionTyping<'_>,
    value_types: &mut HashMap<u32, Ty>,
) {
    let mut pass = Pass {
        suspend_set,
        typing,
        value_types,
    };
    pass.region(ir, body);
}

struct Pass<'a, 'b> {
    suspend_set: &'a HashSet<u32>,
    typing: &'a SuspensionTyping<'b>,
    value_types: &'a mut HashMap<u32, Ty>,
}

impl Pass<'_, '_> {
    fn is_point(&self, ir: &IrFile, expression: ExprId) -> bool {
        let point = unwrap_suspend_cast(ir, expression, self.suspend_set, false).point;
        is_suspension_point(ir, point, self.suspend_set)
    }

    /// A statement region: a block's statements, or one statement made a block of its own.
    fn region(&mut self, ir: &mut IrFile, region: ExprId) {
        if !expr_calls_suspend(ir, region, self.suspend_set) || self.is_point(ir, region) {
            return;
        }
        let (stmts, value) = match ir.exprs[region as usize].clone() {
            IrExpr::Block { stmts, value } => (stmts, value),
            _ => {
                // Give the statement a block to receive its prelude. The node keeps its id, so a
                // recorded suspension point keeps its identity.
                let moved = ir.add_expr(ir.exprs[region as usize].clone());
                if let Some(ty) = ir.logical_types.get(&region).copied() {
                    ir.logical_types.insert(moved, ty);
                }
                (vec![moved], None)
            }
        };
        let mut out = Vec::with_capacity(stmts.len());
        for statement in stmts {
            self.statement(ir, statement, &mut out);
        }
        ir.exprs[region as usize] = IrExpr::Block { stmts: out, value };
    }

    /// Bind the suspensions among the operands of the suspension point `expression` to temps
    /// ahead of it: they evaluate before it does.
    fn point_operands(&mut self, ir: &mut IrFile, expression: ExprId, out: &mut Vec<ExprId>) {
        let point = unwrap_suspend_cast(ir, expression, self.suspend_set, false).point;
        hoist_call_operands_in_order(
            ir,
            point,
            self.suspend_set,
            self.typing,
            self.value_types,
            out,
        );
    }

    fn statement(&mut self, ir: &mut IrFile, statement: ExprId, out: &mut Vec<ExprId>) {
        if !expr_calls_suspend(ir, statement, self.suspend_set) {
            out.push(statement);
            return;
        }
        if self.is_point(ir, statement) {
            self.point_operands(ir, statement, out);
            out.push(statement);
            return;
        }
        match ir.exprs[statement as usize].clone() {
            IrExpr::Variable {
                init: Some(value), ..
            }
            | IrExpr::SetValue { value, .. }
            | IrExpr::Return(Some(value))
                if self.is_point(ir, value) =>
            {
                self.point_operands(ir, value, out);
            }
            IrExpr::Block { .. } => self.region(ir, statement),
            IrExpr::When { branches } => {
                if branches.iter().any(|(condition, _)| {
                    condition.is_some_and(|condition| {
                        expr_calls_suspend(ir, condition, self.suspend_set)
                    })
                }) {
                    // Conditions were hoisted before; one that still suspends is the machine's
                    // to decline.
                    out.push(statement);
                    return;
                }
                for (_, branch) in branches {
                    self.region(ir, branch);
                }
            }
            IrExpr::While { body, .. } => self.region(ir, body),
            IrExpr::Try { body, catches, .. } => {
                self.region(ir, body);
                for catch in catches {
                    self.region(ir, catch.body);
                }
            }
            _ => {
                let mut prelude = Vec::new();
                let rewritten = hoist_expr(
                    ir,
                    statement,
                    self.suspend_set,
                    self.typing,
                    self.value_types,
                    &mut prelude,
                );
                out.extend(prelude);
                out.push(rewritten);
                return;
            }
        }
        out.push(statement);
    }
}
