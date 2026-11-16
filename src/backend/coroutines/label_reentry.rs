//! The state machine of a suspend body on common IR, as structured re-entry: the body runs from
//! its start on every resumption, and the label of the suspension it resumes at selects which
//! statements run.
//!
//! Each suspension statement (a suspension point as a statement, a local's initializer, an
//! assignment's value or a returned value, as the shared normalizations leave it) takes a label,
//! numbered from 1 in source order. A statement owns the contiguous range of the labels inside it.
//! Label 0 runs the body forward; label `k` skips to the statement after suspension `k`:
//!
//! * a run of statements with no suspension runs only on label 0;
//! * suspension `k` on label 0 stores `k`, makes its call and returns the marker if the call
//!   suspended; on label `k` it stores 0 and takes the resumed value instead. Either way the
//!   statement then completes with that value;
//! * a `when` branch is taken on label 0 by its condition, or by holding the label: no condition
//!   is evaluated again on resumption;
//! * a loop holding the label is entered without testing its condition once.
//!
//! The pass assumes every value the body reads after a suspension is kept where a resumption finds
//! it again (the continuation's fields); it does not choose that storage, nor how a call passes the
//! continuation, nor where the label and the resumed value live. [`MachineStorage`] answers those.
//! A suspension anywhere else, such as inside a `try` or a value-position block, declines the body.

use crate::fx_hash::HashSet;
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::{wk, Ty};

use super::bottom_completion::unwrap_suspend_cast;
use super::suspension_points::{expr_calls_suspend, is_suspension_point};

/// Where a target keeps the state of one suspend body's machine.
pub trait MachineStorage {
    /// The label of the suspension being resumed, `0` on the first run (an `Int`).
    fn read_label(&self, ir: &mut IrFile) -> ExprId;
    /// Store `label` (an `Int` expression).
    fn write_label(&self, ir: &mut IrFile, label: ExprId) -> ExprId;
    /// The value the machine was resumed with, as `Any?`, a failure rethrown.
    fn resumed_value(&self, ir: &mut IrFile) -> ExprId;
    /// The `COROUTINE_SUSPENDED` marker.
    fn suspended_marker(&self, ir: &mut IrFile) -> ExprId;
    /// `call`, a suspension point, made with the machine's continuation. Its result is `Any?`: the
    /// value it completed with, or the marker.
    fn call_with_continuation(
        &self,
        ir: &mut IrFile,
        call: ExprId,
    ) -> Result<ExprId, ReentryDecline>;
}

/// Why a body cannot take this machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReentryDecline {
    /// A suspension in a position no statement form above holds.
    UnsupportedSuspension,
    /// The target has no checked call shape for a suspension node.
    UnsupportedCallShape,
}

/// Rewrite the statements of `body` (a `Block` without a value) into the machine. `next_value` is
/// the first value index the body does not use; the machine's own locals take indices from it.
/// Returns the number of suspensions, the labels `1..=n`.
pub fn split_at_suspensions(
    ir: &mut IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    storage: &dyn MachineStorage,
    next_value: u32,
) -> Result<u32, ReentryDecline> {
    let IrExpr::Block { stmts, value: None } = ir.exprs[body as usize].clone() else {
        return Err(ReentryDecline::UnsupportedSuspension);
    };
    let mut split = Split {
        suspend_set,
        storage,
        next_value,
        next_label: 1,
    };
    for &stmt in &stmts {
        split.count(ir, stmt)?;
    }
    let stmts = split.list(ir, &stmts)?;
    ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
    Ok(split.next_label - 1)
}

struct Split<'a> {
    suspend_set: &'a HashSet<u32>,
    storage: &'a dyn MachineStorage,
    next_value: u32,
    next_label: u32,
}

/// One suspension statement: the statement and the suspension point it completes with.
struct Leaf {
    stmt: ExprId,
    call: ExprId,
}

impl Split<'_> {
    fn suspends(&self, ir: &IrFile, expression: ExprId) -> bool {
        expr_calls_suspend(ir, expression, self.suspend_set)
    }

    /// The suspension point `operand` is, seen through the casts its result takes.
    fn point(&self, ir: &IrFile, operand: ExprId) -> Option<ExprId> {
        let call = unwrap_suspend_cast(ir, operand, self.suspend_set, false).point;
        is_suspension_point(ir, call, self.suspend_set).then_some(call)
    }

    fn leaf(&self, ir: &IrFile, stmt: ExprId) -> Option<Leaf> {
        let operand = match ir.exprs[stmt as usize] {
            IrExpr::Variable {
                init: Some(init), ..
            } => init,
            IrExpr::SetValue { value, .. } => value,
            IrExpr::Return(Some(value)) => value,
            _ => stmt,
        };
        let call = self.point(ir, operand)?;
        // The call's own operands were hoisted ahead of it; a suspension left among them is
        // a second suspension in one statement.
        let mut operands_suspend = false;
        crate::ir::for_each_child(&ir.exprs, call, &mut |child| {
            operands_suspend |= expr_calls_suspend(ir, child, self.suspend_set);
        });
        (!operands_suspend).then_some(Leaf { stmt, call })
    }

    /// How many suspensions `stmt` holds, or why it cannot take a label.
    fn count(&self, ir: &IrFile, stmt: ExprId) -> Result<u32, ReentryDecline> {
        if !self.suspends(ir, stmt) {
            return Ok(0);
        }
        if self.leaf(ir, stmt).is_some() {
            return Ok(1);
        }
        match &ir.exprs[stmt as usize] {
            IrExpr::Block { stmts, value: None } => stmts
                .iter()
                .try_fold(0, |total, &child| Ok(total + self.count(ir, child)?)),
            IrExpr::When { branches } => {
                let mut total = 0;
                for &(condition, body) in branches {
                    if condition.is_some_and(|condition| self.suspends(ir, condition)) {
                        return Err(ReentryDecline::UnsupportedSuspension);
                    }
                    total += self.count(ir, body)?;
                }
                Ok(total)
            }
            IrExpr::While {
                cond, body, update, ..
            } => {
                if self.suspends(ir, *cond) || update.is_some_and(|u| self.suspends(ir, u)) {
                    return Err(ReentryDecline::UnsupportedSuspension);
                }
                self.count(ir, *body)
            }
            _ => Err(ReentryDecline::UnsupportedSuspension),
        }
    }

    fn list(&mut self, ir: &mut IrFile, stmts: &[ExprId]) -> Result<Vec<ExprId>, ReentryDecline> {
        let mut out = Vec::with_capacity(stmts.len());
        let mut plain = Vec::new();
        for &stmt in stmts {
            if self.suspends(ir, stmt) {
                self.flush_plain(ir, &mut plain, &mut out);
                out.push(self.stmt(ir, stmt)?);
            } else {
                plain.push(stmt);
            }
        }
        self.flush_plain(ir, &mut plain, &mut out);
        Ok(out)
    }

    /// A run of statements with no suspension, which runs forward only.
    fn flush_plain(&self, ir: &mut IrFile, plain: &mut Vec<ExprId>, out: &mut Vec<ExprId>) {
        if plain.is_empty() {
            return;
        }
        let stmts = std::mem::take(plain);
        let run = ir.add_expr(IrExpr::Block { stmts, value: None });
        let forward = self.label_is(ir, 0);
        out.push(when(ir, vec![(Some(forward), run)]));
    }

    fn stmt(&mut self, ir: &mut IrFile, stmt: ExprId) -> Result<ExprId, ReentryDecline> {
        if let Some(leaf) = self.leaf(ir, stmt) {
            return self.suspension(ir, leaf);
        }
        match ir.exprs[stmt as usize].clone() {
            IrExpr::Block { stmts, value: None } => {
                let stmts = self.list(ir, &stmts)?;
                Ok(ir.add_expr(IrExpr::Block { stmts, value: None }))
            }
            IrExpr::When { branches } => {
                let mut rewritten = Vec::with_capacity(branches.len());
                for (condition, body) in branches {
                    let first = self.next_label;
                    let count = self.count(ir, body)?;
                    let forward = self.label_is(ir, 0);
                    let taken = match condition {
                        // Resuming tests the label alone: the condition ran when the branch was
                        // first taken.
                        Some(condition) => and_then(ir, forward, condition),
                        None => forward,
                    };
                    let taken = match count {
                        0 => taken,
                        _ => {
                            let held = self.label_in(ir, first, first + count - 1);
                            or_else(ir, taken, held)
                        }
                    };
                    let body = self.stmt_or_plain(ir, body)?;
                    rewritten.push((Some(taken), body));
                }
                Ok(when(ir, rewritten))
            }
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => {
                let first = self.next_label;
                let count = self.count(ir, body)?;
                let resuming_index = self.fresh_value();
                let held = self.label_in(ir, first, first + count - 1);
                let resuming = ir.add_expr(IrExpr::Variable {
                    index: resuming_index,
                    ty: Ty::Boolean,
                    init: Some(held),
                    named: false,
                });
                let read_resuming = ir.add_expr(IrExpr::GetValue(resuming_index));
                let forward = self.label_is(ir, 0);
                let tested = and_then(ir, forward, cond);
                let cond = or_else(ir, read_resuming, tested);
                let resumed = bool_const(ir, false);
                let clear = ir.add_expr(IrExpr::SetValue {
                    var: resuming_index,
                    value: resumed,
                });
                let inner = self.stmt(ir, body)?;
                let body = ir.add_expr(IrExpr::Block {
                    stmts: vec![clear, inner],
                    value: None,
                });
                let looped = ir.add_expr(IrExpr::While {
                    cond,
                    body,
                    update,
                    post_test,
                    label,
                });
                Ok(ir.add_expr(IrExpr::Block {
                    stmts: vec![resuming, looped],
                    value: None,
                }))
            }
            _ => Err(ReentryDecline::UnsupportedSuspension),
        }
    }

    /// A branch body: rewritten when it suspends, else run as it is (its guard already tested the
    /// label).
    fn stmt_or_plain(&mut self, ir: &mut IrFile, body: ExprId) -> Result<ExprId, ReentryDecline> {
        match self.suspends(ir, body) {
            true => self.stmt(ir, body),
            false => Ok(body),
        }
    }

    /// Suspension `k`: run the call on label 0, take the resumed value on label `k`, then complete
    /// the statement with the value either way, on label 0.
    fn suspension(&mut self, ir: &mut IrFile, leaf: Leaf) -> Result<ExprId, ReentryDecline> {
        let label = self.next_label;
        self.next_label += 1;
        let any = Ty::nullable(Ty::obj_name(wk::any()));
        let result = self.fresh_value();
        let declared = ir.add_expr(IrExpr::Variable {
            index: result,
            ty: any,
            init: None,
            named: false,
        });

        let logical = ir.checked_type(leaf.call).unwrap_or(any);
        let call = self.storage.call_with_continuation(ir, leaf.call)?;
        let k = int_const(ir, label as i32);
        let store_label = self.storage.write_label(ir, k);
        let store_result = ir.add_expr(IrExpr::SetValue {
            var: result,
            value: call,
        });
        let completed = ir.add_expr(IrExpr::GetValue(result));
        let marker = self.storage.suspended_marker(ir);
        let suspended = binary(ir, IrBinOp::RefEq, completed, marker);
        let returned = ir.add_expr(IrExpr::GetValue(result));
        let leave = ir.add_expr(IrExpr::Return(Some(returned)));
        let leave_if_suspended = when(ir, vec![(Some(suspended), leave)]);
        // A call that completed without suspending goes on forward, as a resumed one does.
        let zero = int_const(ir, 0);
        let completed_forward = self.storage.write_label(ir, zero);
        let run = ir.add_expr(IrExpr::Block {
            stmts: vec![
                store_label,
                store_result,
                leave_if_suspended,
                completed_forward,
            ],
            value: None,
        });

        let zero = int_const(ir, 0);
        let clear_label = self.storage.write_label(ir, zero);
        let resumed = self.storage.resumed_value(ir);
        let take_resumed = ir.add_expr(IrExpr::SetValue {
            var: result,
            value: resumed,
        });
        let resume = ir.add_expr(IrExpr::Block {
            stmts: vec![clear_label, take_resumed],
            value: None,
        });

        let forward = self.label_is(ir, 0);
        let at_label = self.label_is(ir, label);
        let step = when(ir, vec![(Some(forward), run), (Some(at_label), resume)]);

        // The statement completes with the value where it held the call.
        let value = ir.add_expr(IrExpr::GetValue(result));
        ir.exprs[leaf.call as usize] = IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: value,
            type_operand: logical,
        };
        let completes = self.label_is(ir, 0);
        let complete = when(ir, vec![(Some(completes), leaf.stmt)]);
        Ok(ir.add_expr(IrExpr::Block {
            stmts: vec![declared, step, complete],
            value: None,
        }))
    }

    fn fresh_value(&mut self) -> u32 {
        let index = self.next_value;
        self.next_value += 1;
        index
    }

    fn label_is(&self, ir: &mut IrFile, label: u32) -> ExprId {
        let read = self.storage.read_label(ir);
        let value = int_const(ir, label as i32);
        binary(ir, IrBinOp::Eq, read, value)
    }

    fn label_in(&self, ir: &mut IrFile, first: u32, last: u32) -> ExprId {
        if first == last {
            return self.label_is(ir, first);
        }
        let read = self.storage.read_label(ir);
        let low = int_const(ir, first as i32);
        let above = binary(ir, IrBinOp::Ge, read, low);
        let read = self.storage.read_label(ir);
        let high = int_const(ir, last as i32);
        let below = binary(ir, IrBinOp::Le, read, high);
        binary(ir, IrBinOp::And, above, below)
    }
}

fn int_const(ir: &mut IrFile, value: i32) -> ExprId {
    ir.add_expr(IrExpr::Const(IrConst::Int(value)))
}

fn bool_const(ir: &mut IrFile, value: bool) -> ExprId {
    ir.add_expr(IrExpr::Const(IrConst::Boolean(value)))
}

fn binary(ir: &mut IrFile, op: IrBinOp, lhs: ExprId, rhs: ExprId) -> ExprId {
    ir.add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs })
}

/// `lhs && rhs`, evaluating `rhs` only when `lhs` holds.
fn and_then(ir: &mut IrFile, lhs: ExprId, rhs: ExprId) -> ExprId {
    let otherwise = bool_const(ir, false);
    let test = when(ir, vec![(Some(lhs), rhs), (None, otherwise)]);
    ir.set_logical_type(test, Ty::Boolean);
    test
}

/// `lhs || rhs`, evaluating `rhs` only when `lhs` does not hold.
fn or_else(ir: &mut IrFile, lhs: ExprId, rhs: ExprId) -> ExprId {
    let held = bool_const(ir, true);
    let test = when(ir, vec![(Some(lhs), held), (None, rhs)]);
    ir.set_logical_type(test, Ty::Boolean);
    test
}

fn when(ir: &mut IrFile, branches: Vec<(Option<ExprId>, ExprId)>) -> ExprId {
    ir.add_expr(IrExpr::When { branches })
}

#[cfg(test)]
mod tests;
