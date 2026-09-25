//! Counted `for` loops over a range literal or a progression value, lowered in the shape of
//! kotlinc's `ForLoopsLowering` (`ProgressionHeaderInfo` and `ProgressionLoopHeader.buildLoop` with
//! the JVM's preference for a Java-like counter loop).
//!
//! A header is built from the loop's iterable, as kotlinc's handlers build one:
//! - a range literal (`RangeToHandler`, `DownToHandler`, `UntilHandler`, `RangeUntilHandler`) has
//!   a unit step and a known direction; `until`/`..<` have an exclusive last bound, and on a target
//!   preferring Java-like counter loops `..`/`downTo` become exclusive when the last bound is a
//!   constant that can move one step outward without overflowing;
//! - a progression value (`DefaultProgressionHandler`) is read through its `first`, `last` and
//!   `step`; a `*Range` has step 1 and increases, any other progression's direction is known only
//!   from the sign of its step at run time.
//!
//! The header then decides whether the induction variable can overflow (an inclusive bound that is
//! not provably below the type's limit can), and which of `last` and `step` need a temporary: only
//! a value that can change while the loop runs, which is anything but a constant or a read of an
//! immutable local. The loop shapes this produces are
//!
//! ```text
//! // exclusive last on a target preferring Java-like counter loops (the JVM)
//! while (inductionVar < last) { body; inductionVar += step }
//!
//! // exclusive last elsewhere, or inclusive last that cannot overflow
//! if (inductionVar < last) do { body; inductionVar += step } while (inductionVar < last)
//!
//! // the induction variable may overflow
//! if (inductionVar <= last) do { body; if (inductionVar == last) break; inductionVar += step } while (true)
//! ```
//!
//! with the comparison written `last < inductionVar` (`last <= inductionVar`) for a decreasing
//! progression, and `(step > 0 && inductionVar <= last) || (step < 0 && last <= inductionVar)`
//! when the direction is unknown.

use crate::fir::{
    ControlTargetId, FirExprId, FirProgressionClass, FirRangeCounterKind, FirRangeOperation,
    LocalValueId,
};
use crate::ir::{
    ExprId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrProgressionMember, IrTypeOp,
};
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

/// The checked pieces of one counted loop over a progression value.
pub(super) struct ProgressionLoop {
    pub(super) target: ControlTargetId,
    pub(super) variable: LocalValueId,
    pub(super) progression: FirProgressionClass,
    pub(super) iterable: FirExprId,
    pub(super) body: FirExprId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Increasing,
    Decreasing,
    Unknown,
}

/// A lowered header operand and whether its value can change while the loop runs
/// (`canChangeValueDuringExecution`), which decides whether it is copied to a temporary.
#[derive(Clone, Copy)]
struct Operand {
    value: ExprId,
    can_change: bool,
}

/// kotlinc's `ProgressionHeaderInfo`, with its operands already lowered.
struct ProgressionHeader {
    ty: Ty,
    step_ty: Ty,
    direction: Direction,
    first: ExprId,
    last: Operand,
    last_is_inclusive: bool,
    step: Operand,
    /// Statements that must run before the loop's own variables (`additionalStatements`).
    prelude: Vec<ExprId>,
}

/// The loop's variables once declared: where `last` and `step` are read from.
struct LoopVariables {
    induction: u32,
    last: ExprId,
    step: ExprId,
}

impl ProgressionHeader {
    /// `canOverflow`: an exclusive bound never overflows; an inclusive one cannot only when the
    /// step and last bound are constants and `last` is at least one step inside the type's range.
    fn can_overflow(&self, lowering: &BodyLowering<'_>) -> bool {
        if !self.last_is_inclusive {
            return false;
        }
        let (Some(step), Some(last)) = (
            constant_bound(lowering, self.step.value)
                .as_ref()
                .and_then(integral_value),
            constant_bound(lowering, self.last.value)
                .as_ref()
                .and_then(integral_value),
        ) else {
            return true;
        };
        let (min, max) = match self.ty {
            Ty::Long => (i64::MIN, i64::MAX),
            Ty::Char => (0, i64::from(u16::MAX)),
            _ => (i64::from(i32::MIN), i64::from(i32::MAX)),
        };
        match self.direction {
            Direction::Increasing => max.checked_sub(step).is_none_or(|limit| last > limit),
            Direction::Decreasing => min.checked_sub(step).is_none_or(|limit| last < limit),
            Direction::Unknown => true,
        }
    }

    /// `inductionVar < last` (`<=` when inclusive), `last < inductionVar` when decreasing, and both
    /// guarded by the step's sign when the direction is unknown.
    fn condition(&self, lowering: &mut BodyLowering<'_>, variables: &LoopVariables) -> ExprId {
        match self.direction {
            Direction::Increasing => self.bound_check(lowering, variables, Direction::Increasing),
            Direction::Decreasing => self.bound_check(lowering, variables, Direction::Decreasing),
            Direction::Unknown => {
                let positive = self.step_sign(lowering, variables, IrBinOp::Gt);
                let increasing = self.bound_check(lowering, variables, Direction::Increasing);
                let increasing = lowering.short_circuit_and(positive, increasing);
                let negative = self.step_sign(lowering, variables, IrBinOp::Lt);
                let decreasing = self.bound_check(lowering, variables, Direction::Decreasing);
                let decreasing = lowering.short_circuit_and(negative, decreasing);
                lowering.short_circuit_or(increasing, decreasing)
            }
        }
    }

    fn bound_check(
        &self,
        lowering: &mut BodyLowering<'_>,
        variables: &LoopVariables,
        direction: Direction,
    ) -> ExprId {
        let induction = lowering.ir.add_expr(IrExpr::GetValue(variables.induction));
        let op = if self.last_is_inclusive {
            IrBinOp::Le
        } else {
            IrBinOp::Lt
        };
        let (lhs, rhs) = match direction {
            Direction::Decreasing => (variables.last, induction),
            Direction::Increasing | Direction::Unknown => (induction, variables.last),
        };
        lowering
            .ir
            .add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs })
    }

    /// `step > 0` or `step < 0`.
    fn step_sign(
        &self,
        lowering: &mut BodyLowering<'_>,
        variables: &LoopVariables,
        op: IrBinOp,
    ) -> ExprId {
        let zero = lowering
            .ir
            .add_expr(IrExpr::Const(if self.step_ty == Ty::Long {
                IrConst::Long(0)
            } else {
                IrConst::Int(0)
            }));
        lowering.ir.add_expr(IrExpr::PrimitiveBinOp {
            op,
            lhs: variables.step,
            rhs: zero,
        })
    }

    /// The entry condition evaluated between two constant bounds, when both are integral.
    fn holds_between(&self, first: &IrConst, last: &IrConst) -> Option<bool> {
        let (first, last) = (integral_value(first)?, integral_value(last)?);
        let (lower, upper) = match self.direction {
            Direction::Increasing => (first, last),
            Direction::Decreasing => (last, first),
            Direction::Unknown => return None,
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
        Direction::Unknown => return None,
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
        let label = self.control_label(0, lp.target)?;
        let header = self.range_literal_header(&lp)?;
        let body = self.expression(lp.body)?;
        Ok(self.progression_loop_expression(header, lp.variable, body, label))
    }

    pub(super) fn progression_loop(
        &mut self,
        lp: ProgressionLoop,
    ) -> Result<ExprId, FirLoweringFailure> {
        let label = self.control_label(0, lp.target)?;
        let header = self.progression_value_header(&lp)?;
        let body = self.expression(lp.body)?;
        Ok(self.progression_loop_expression(header, lp.variable, body, label))
    }

    /// `RangeToHandler`, `DownToHandler`, `UntilHandler` and `RangeUntilHandler`: an inclusive
    /// constant bound that is not the extreme value in the direction of travel is rewritten to the
    /// exclusive bound one step further (`0..10` iterates while `i < 11`).
    fn range_literal_header(
        &mut self,
        lp: &CountedLoop,
    ) -> Result<ProgressionHeader, FirLoweringFailure> {
        let ty = lp.counter.ty();
        let first = self.expression(lp.start)?;
        let first = self.range_bound(first, ty);
        let end = self.expression(lp.end)?;
        // A widened bound (`0L..n` with an `Int` `n`) is a conversion, not a read.
        let end_is_stable = constant_bound(self, end).is_some()
            || (self.fir_type(lp.end) == Some(ty) && self.reads_immutable_local(lp.end, end));
        let last = self.range_bound(end, ty);
        let direction = match lp.operation {
            FirRangeOperation::DownTo => Direction::Decreasing,
            _ => Direction::Increasing,
        };
        let inclusive = matches!(
            lp.operation,
            FirRangeOperation::Through | FirRangeOperation::DownTo
        );
        let exclusive = (inclusive && self.options.prefer_java_like_counter_loop)
            .then(|| exclusive_bound(self, last, direction, ty))
            .flatten();
        let step_ty = if ty == Ty::Long { Ty::Long } else { Ty::Int };
        let step = self.ir.add_expr(IrExpr::Const(match (step_ty, direction) {
            (Ty::Long, Direction::Decreasing) => IrConst::Long(-1),
            (Ty::Long, _) => IrConst::Long(1),
            (_, Direction::Decreasing) => IrConst::Int(-1),
            _ => IrConst::Int(1),
        }));
        Ok(ProgressionHeader {
            ty,
            step_ty,
            direction,
            first,
            last: Operand {
                value: exclusive.unwrap_or(last),
                can_change: !end_is_stable,
            },
            last_is_inclusive: inclusive && exclusive.is_none(),
            step: Operand {
                value: step,
                can_change: false,
            },
            prelude: Vec::new(),
        })
    }

    /// `DefaultProgressionHandler`: the progression is read once, through a temporary unless it is
    /// a constant or a local read, and the loop takes its `first`, `last` and `step` from it.
    fn progression_value_header(
        &mut self,
        lp: &ProgressionLoop,
    ) -> Result<ProgressionHeader, FirLoweringFailure> {
        let ty = lp.progression.counter.ty();
        let class = lp.progression.ty;
        let iterable = self.expression(lp.iterable)?;
        // `irCastIfNeeded` to the progression class the header was built from.
        let iterable = if self.fir_type(lp.iterable) == Some(class) {
            iterable
        } else {
            self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: iterable,
                type_operand: class,
            })
        };
        let mut prelude = Vec::new();
        let progression = if matches!(
            self.ir.expr(iterable),
            IrExpr::GetValue(_) | IrExpr::Const(_)
        ) {
            iterable
        } else {
            let slot = self.allocate_temporary();
            prelude.push(self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty: class,
                init: Some(iterable),
                named: false,
            }));
            self.ir.add_expr(IrExpr::GetValue(slot))
        };
        let member = |lowering: &mut Self, member| {
            let progression = lowering.ir.add_expr(lowering.ir.expr(progression).clone());
            lowering
                .ir
                .add_expr(IrExpr::Checked(IrCheckedOperation::ProgressionMember {
                    progression,
                    class,
                    member,
                }))
        };
        let first = member(self, IrProgressionMember::First);
        let last = member(self, IrProgressionMember::Last);
        let step_ty = if ty == Ty::Long { Ty::Long } else { Ty::Int };
        let (step, direction) = if lp.progression.unit_step {
            let one = self.ir.add_expr(IrExpr::Const(if step_ty == Ty::Long {
                IrConst::Long(1)
            } else {
                IrConst::Int(1)
            }));
            (
                Operand {
                    value: one,
                    can_change: false,
                },
                Direction::Increasing,
            )
        } else {
            let step = member(self, IrProgressionMember::Step);
            (
                Operand {
                    value: step,
                    can_change: true,
                },
                Direction::Unknown,
            )
        };
        Ok(ProgressionHeader {
            ty,
            step_ty,
            direction,
            first,
            last: Operand {
                value: last,
                can_change: true,
            },
            last_is_inclusive: true,
            step,
            prelude,
        })
    }

    /// `ProgressionLoopHeader`: declare the induction variable, `last` and `step` in kotlinc's
    /// order, then build the loop shape the header calls for.
    fn progression_loop_expression(
        &mut self,
        header: ProgressionHeader,
        variable: LocalValueId,
        body: ExprId,
        label: String,
    ) -> ExprId {
        let ty = header.ty;
        let first_constant = constant_bound(self, header.first);
        let mut statements = header.prelude.clone();
        let (induction, induction_declaration) =
            self.loop_variable_declaration(variable.raw(), ty, header.first);
        statements.push(induction_declaration);
        let last = self.loop_temporary(header.last, ty, &mut statements);
        let step = self.loop_temporary(header.step, header.step_ty, &mut statements);
        let variables = LoopVariables {
            induction,
            last,
            step,
        };
        let increment = self.increment_induction_variable(&variables, ty);
        let loop_expression = if header.can_overflow(self) {
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
                stmts: vec![guard, increment],
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
            let entered = first_constant
                .zip(last_constant)
                .and_then(|(first, last)| header.holds_between(&first, &last));
            if entered == Some(true) {
                repeat
            } else {
                self.guarded(&header, &variables, repeat)
            }
        } else if self.options.prefer_java_like_counter_loop && !header.last_is_inclusive {
            let condition = header.condition(self, &variables);
            self.ir.add_expr(IrExpr::While {
                cond: condition,
                body,
                update: Some(increment),
                post_test: false,
                label: Some(label),
            })
        } else {
            // A bound that cannot overflow: the guarded loop re-tests the bound after stepping.
            let condition = header.condition(self, &variables);
            let repeat = self.ir.add_expr(IrExpr::While {
                cond: condition,
                body,
                update: Some(increment),
                post_test: true,
                label: Some(label),
            });
            self.guarded(&header, &variables, repeat)
        };
        statements.push(loop_expression);
        self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        })
    }

    /// `createLoopTemporaryVariableIfNecessary`: an operand that cannot change while the loop runs
    /// is re-read where it is used.
    fn loop_temporary(&mut self, operand: Operand, ty: Ty, statements: &mut Vec<ExprId>) -> ExprId {
        if !operand.can_change {
            return operand.value;
        }
        let slot = self.allocate_temporary();
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: slot,
            ty,
            init: Some(operand.value),
            named: false,
        }));
        self.ir.add_expr(IrExpr::GetValue(slot))
    }

    /// `if (<entry condition>) <loop>`.
    fn guarded(
        &mut self,
        header: &ProgressionHeader,
        variables: &LoopVariables,
        repeat: ExprId,
    ) -> ExprId {
        let entry = header.condition(self, variables);
        self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(entry), repeat)],
        })
    }

    fn fir_type(&self, expression: FirExprId) -> Option<Ty> {
        self.body
            .expr(expression)
            .map(|expression| expression.ty.get())
    }

    /// `inductionVar += step`; a `Char` induction variable is narrowed back after the `Int` add.
    fn increment_induction_variable(&mut self, variables: &LoopVariables, ty: Ty) -> ExprId {
        let current = self.ir.add_expr(IrExpr::GetValue(variables.induction));
        let stepped = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: current,
            rhs: variables.step,
        });
        let stepped = if ty == Ty::Char {
            self.range_bound(stepped, ty)
        } else {
            stepped
        };
        self.ir.add_expr(IrExpr::SetValue {
            var: variables.induction,
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
