//! Counted `for` loops over a range literal, lowered in the shape of kotlinc's `ForLoopsLowering`
//! (`ProgressionHeaderInfo` and `ProgressionLoopHeader.buildLoop` with the JVM's preference for a
//! Java-like counter loop).
//!
//! The header decides three things from the checked range operation and its bounds:
//! - whether the last bound is exclusive: `until`/`..<` always are, and on a target preferring Java-like
//!   counter loops `..`/`downTo` become exclusive when the last bound is a constant that can move one
//!   step outward without overflowing;
//! - whether the induction variable can overflow: only an inclusive bound can, and then the loop
//!   exits by comparing the loop variable with `last` before stepping;
//! - whether `last` needs a temporary: only when its value can change while the loop runs, which is
//!   anything but a constant or a read of an immutable local.
//!
//! The loop shapes this produces are
//!
//! ```text
//! // exclusive last on a target preferring Java-like counter loops (the JVM)
//! while (inductionVar < last) { body; inductionVar += step }
//!
//! // exclusive last elsewhere
//! if (inductionVar < last) do { body; inductionVar += step } while (inductionVar < last)
//!
//! // inclusive last: the induction variable may overflow
//! if (inductionVar <= last) do { body; if (inductionVar == last) break; inductionVar += step } while (true)
//! ```
//!
//! with the comparison written `last < inductionVar` (`last <= inductionVar`) for a decreasing
//! progression.

use crate::fir::{
    ControlTargetId, FirExprId, FirRangeCounterKind, FirRangeOperation, LocalValueId,
};
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrTypeOp};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

/// The checked pieces of one counted loop over a range literal.
pub(super) struct CountedLoop {
    pub(super) target: ControlTargetId,
    pub(super) variable: LocalValueId,
    pub(super) counter: FirRangeCounterKind,
    pub(super) operation: FirRangeOperation,
    pub(super) start: FirExprId,
    pub(super) end: FirExprId,
    pub(super) body: FirExprId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Increasing,
    Decreasing,
}

/// kotlinc's `ProgressionHeaderInfo` for a progression with a constant unit step.
struct ProgressionHeader {
    direction: Direction,
    last: ExprId,
    last_is_inclusive: bool,
}

impl ProgressionHeader {
    /// `RangeToHandler`, `DownToHandler`, `UntilHandler` and `RangeUntilHandler`: an inclusive
    /// constant bound that is not the extreme value in the direction of travel is rewritten to the
    /// exclusive bound one step further (`0..10` iterates while `i < 11`).
    fn new(
        operation: FirRangeOperation,
        last: ExprId,
        lowering: &mut BodyLowering<'_>,
        ty: Ty,
    ) -> Self {
        let direction = match operation {
            FirRangeOperation::DownTo => Direction::Decreasing,
            _ => Direction::Increasing,
        };
        let inclusive = matches!(
            operation,
            FirRangeOperation::Through | FirRangeOperation::DownTo
        );
        if inclusive && lowering.options.prefer_java_like_counter_loop {
            if let Some(exclusive) = exclusive_bound(lowering, last, direction, ty) {
                return Self {
                    direction,
                    last: exclusive,
                    last_is_inclusive: false,
                };
            }
        }
        Self {
            direction,
            last,
            last_is_inclusive: inclusive,
        }
    }

    /// `inductionVar < last` (`<=` when inclusive), or `last < inductionVar` when decreasing.
    fn condition(&self, lowering: &mut BodyLowering<'_>, induction: u32, last: ExprId) -> ExprId {
        let induction = lowering.ir.add_expr(IrExpr::GetValue(induction));
        let op = if self.last_is_inclusive {
            IrBinOp::Le
        } else {
            IrBinOp::Lt
        };
        let (lhs, rhs) = match self.direction {
            Direction::Increasing => (induction, last),
            Direction::Decreasing => (last, induction),
        };
        lowering
            .ir
            .add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs })
    }
}

impl ProgressionHeader {
    /// The entry condition evaluated between two constant bounds, when both are integral.
    fn holds_between(&self, first: &IrConst, last: &IrConst) -> Option<bool> {
        let (first, last) = (integral_value(first)?, integral_value(last)?);
        let (lower, upper) = match self.direction {
            Direction::Increasing => (first, last),
            Direction::Decreasing => (last, first),
        };
        Some(if self.last_is_inclusive {
            lower <= upper
        } else {
            lower < upper
        })
    }
}

fn integral_value(constant: &IrConst) -> Option<i64> {
    match *constant {
        IrConst::Byte(value) => Some(i64::from(value)),
        IrConst::Short(value) => Some(i64::from(value)),
        IrConst::Int(value) => Some(i64::from(value)),
        IrConst::Long(value) => Some(value),
        IrConst::Char(value) => Some(i64::from(value)),
        _ => None,
    }
}

/// The constant under an implicit numeric coercion, which is how a lowered literal bound arrives.
fn constant_bound(lowering: &BodyLowering<'_>, expression: ExprId) -> Option<IrConst> {
    match lowering.ir.expr(expression) {
        IrExpr::Const(constant) => Some(constant.clone()),
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => constant_bound(lowering, *arg),
        _ => None,
    }
}

fn exclusive_bound(
    lowering: &mut BodyLowering<'_>,
    last: ExprId,
    direction: Direction,
    ty: Ty,
) -> Option<ExprId> {
    let delta: i64 = match direction {
        Direction::Increasing => 1,
        Direction::Decreasing => -1,
    };
    let moved = match constant_bound(lowering, last)? {
        IrConst::Byte(value) => IrConst::Byte(value.checked_add(i8::try_from(delta).ok()?)?),
        IrConst::Short(value) => IrConst::Short(value.checked_add(i16::try_from(delta).ok()?)?),
        IrConst::Int(value) => IrConst::Int(value.checked_add(i32::try_from(delta).ok()?)?),
        IrConst::Long(value) => IrConst::Long(value.checked_add(delta)?),
        IrConst::Char(value) => IrConst::Char(u16::try_from(i64::from(value) + delta).ok()?),
        _ => return None,
    };
    let moved = lowering.ir.add_expr(IrExpr::Const(moved));
    Some(lowering.ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: moved,
        type_operand: ty,
    }))
}

impl BodyLowering<'_> {
    pub(super) fn counted_loop(&mut self, lp: CountedLoop) -> Result<ExprId, FirLoweringFailure> {
        let ty = lp.counter.ty();
        let label = self.control_label(0, lp.target)?;
        let start = self.expression(lp.start)?;
        let first = constant_bound(self, start);
        let start = self.range_bound(start, ty);
        let (induction, induction_declaration) =
            self.loop_variable_declaration(lp.variable.raw(), ty, start);
        let end = self.expression(lp.end)?;
        // A widened bound (`0L..n` with an `Int` `n`) is a conversion, not a read.
        let unchanging_end = constant_bound(self, end).is_some()
            || (self.fir_type(lp.end) == Some(ty) && self.reads_immutable_local(lp.end, end));
        let end = self.range_bound(end, ty);
        let header = ProgressionHeader::new(lp.operation, end, self, ty);
        // `createLoopTemporaryVariableIfNecessary`: a bound that cannot change while the loop runs is
        // re-read where it is used.
        let (last_declaration, last) = if unchanging_end {
            (None, header.last)
        } else {
            let slot = self.allocate_temporary();
            let declaration = self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(header.last),
                named: false,
            });
            (Some(declaration), self.ir.add_expr(IrExpr::GetValue(slot)))
        };
        let body = self.expression(lp.body)?;
        let step = self.step_induction_variable(induction, header.direction, ty);
        let loop_expression = if header.last_is_inclusive {
            // The induction variable can overflow past an inclusive bound, so the loop leaves by
            // comparing it with `last` before stepping, and the entry test guards the whole loop.
            let current = self.ir.add_expr(IrExpr::GetValue(induction));
            let at_last = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: current,
                rhs: last,
            });
            let leave = self.ir.add_expr(IrExpr::Break {
                label: Some(label.clone()),
            });
            let guard = self.ir.add_expr(IrExpr::When {
                branches: vec![(Some(at_last), leave)],
            });
            let update = self.ir.add_expr(IrExpr::Block {
                stmts: vec![guard, step],
                value: None,
            });
            let always = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            let repeat = self.ir.add_expr(IrExpr::While {
                cond: always,
                body,
                update: Some(update),
                post_test: true,
                label: Some(label),
            });
            // kotlinc folds an `Int`-sized comparison between two constants before emission, so the
            // guard disappears; a `Long` comparison (`lcmp`) is not folded.
            let last_constant = constant_bound(self, last).filter(|_| ty != Ty::Long);
            if let (Some(first), Some(last_constant)) = (&first, &last_constant) {
                if header.holds_between(first, last_constant) == Some(true) {
                    repeat
                } else {
                    self.guarded(&header, induction, last, repeat)
                }
            } else {
                self.guarded(&header, induction, last, repeat)
            }
        } else if self.options.prefer_java_like_counter_loop {
            let condition = header.condition(self, induction, last);
            self.ir.add_expr(IrExpr::While {
                cond: condition,
                body,
                update: Some(step),
                post_test: false,
                label: Some(label),
            })
        } else {
            // An exclusive bound cannot overflow: the guarded loop re-tests the bound after stepping.
            let condition = header.condition(self, induction, last);
            let repeat = self.ir.add_expr(IrExpr::While {
                cond: condition,
                body,
                update: Some(step),
                post_test: true,
                label: Some(label),
            });
            self.guarded(&header, induction, last, repeat)
        };
        let mut statements = vec![induction_declaration];
        statements.extend(last_declaration);
        statements.push(loop_expression);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        }))
    }

    /// `if (<entry condition>) <loop>`.
    fn guarded(
        &mut self,
        header: &ProgressionHeader,
        induction: u32,
        last: ExprId,
        repeat: ExprId,
    ) -> ExprId {
        let entry = header.condition(self, induction, last);
        self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(entry), repeat)],
        })
    }

    fn fir_type(&self, expression: FirExprId) -> Option<Ty> {
        self.body
            .expr(expression)
            .map(|expression| expression.ty.get())
    }

    /// `inductionVar += step` with the progression's unit step, `-1` when decreasing.
    fn step_induction_variable(&mut self, induction: u32, direction: Direction, ty: Ty) -> ExprId {
        let current = self.ir.add_expr(IrExpr::GetValue(induction));
        let step = match direction {
            Direction::Increasing => 1,
            Direction::Decreasing => -1,
        };
        let step = self.ir.add_expr(IrExpr::Const(if ty == Ty::Long {
            IrConst::Long(step)
        } else {
            IrConst::Int(step as i32)
        }));
        let stepped = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: current,
            rhs: step,
        });
        let stepped = if ty == Ty::Char {
            self.range_bound(stepped, ty)
        } else {
            stepped
        };
        self.ir.add_expr(IrExpr::SetValue {
            var: induction,
            value: stepped,
        })
    }

    /// Whether a lowered bound reads a local whose value cannot change (`canChangeValueDuringExecution`
    /// is false for an immutable `IrGetValue`).
    fn reads_immutable_local(&self, source: FirExprId, lowered: ExprId) -> bool {
        let Some(crate::fir::FirExprKind::ValueRead(value)) =
            self.body.expr(source).map(|expression| &expression.kind)
        else {
            return false;
        };
        !self.local_value_is_mutable(*value)
            && matches!(self.ir.expr(lowered), IrExpr::GetValue(slot) if *slot == self.value_slot(*value))
    }

    fn range_bound(&mut self, expression: ExprId, target: Ty) -> ExprId {
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: expression,
            type_operand: target,
        })
    }
}
