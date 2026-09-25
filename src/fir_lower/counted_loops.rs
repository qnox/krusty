//! Counted `for` loops over a progression, lowered in the shape of kotlinc's `ForLoopsLowering`
//! (`ProgressionHeaderInfo` and `ProgressionLoopHeader.buildLoop` with the JVM's preference for a
//! Java-like counter loop).
//!
//! A header is built from the loop's iterable, as kotlinc's handlers build one:
//! - a range literal (`RangeToHandler`, `DownToHandler`, `UntilHandler`, `RangeUntilHandler`) has
//!   a unit step and a known direction; `until`/`..<` have an exclusive last bound, and on a target
//!   preferring Java-like counter loops `..`/`downTo` become exclusive when the last bound is a
//!   constant that can move one step outward without overflowing;
//! - a progression value (`DefaultProgressionHandler`) is read through its `first`, `last` and
//!   `step`; a `*Range` has step 1 and increases, any other progression's direction is known only
//!   from the sign of its step at run time;
//! - `step` (`StepHandler`) checks its argument, negates it to follow the nested progression's
//!   direction, and recomputes `last` with `getProgressionLastElement`;
//! - `reversed` (`ReversedHandler`) swaps first and last and negates the step.
//!
//! The header then decides whether the induction variable can overflow (an inclusive bound that is
//! not provably below the type's limit can), and which operands need a temporary: only a value
//! that can change while the loop runs, which is anything but a constant or a read of an immutable
//! local. The loop shapes this produces are
//!
//! ```text
//! // the induction variable may overflow
//! if (inductionVar <= last) do { body; if (inductionVar == last) break; inductionVar += step } while (true)
//!
//! // exclusive last on a target preferring Java-like counter loops (the JVM)
//! while (inductionVar < last) { body; inductionVar += step }
//!
//! // any other bound that cannot overflow
//! if (inductionVar <= last) do { val i = inductionVar; inductionVar += step; body } while (inductionVar <= last)
//! ```
//!
//! with the comparison written `last < inductionVar` (`last <= inductionVar`) for a decreasing
//! progression, and `(step > 0 && inductionVar <= last) || (step < 0 && last <= inductionVar)`
//! when the direction is unknown.

use crate::fir::{
    ControlTargetId, FirExprId, FirProgressionClass, FirProgressionSource, FirRangeCounterKind,
    FirRangeOperation, LocalValueId,
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

/// The checked pieces of one counted loop over a built or stored progression.
pub(super) struct ProgressionLoop<'a> {
    pub(super) target: ControlTargetId,
    pub(super) variable: LocalValueId,
    pub(super) counter: FirRangeCounterKind,
    pub(super) source: &'a FirProgressionSource,
    pub(super) body: FirExprId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Increasing,
    Decreasing,
    Unknown,
}

impl Direction {
    fn reversed(self) -> Self {
        match self {
            Self::Increasing => Self::Decreasing,
            Self::Decreasing => Self::Increasing,
            Self::Unknown => Self::Unknown,
        }
    }
}

/// A lowered header operand and whether its value can change while the loop runs
/// (`canChangeValueDuringExecution`), which decides whether it is copied to a temporary.
#[derive(Clone, Copy)]
struct Operand {
    value: ExprId,
    can_change: bool,
}

impl Operand {
    fn stable(value: ExprId) -> Self {
        Self {
            value,
            can_change: false,
        }
    }
}

/// kotlinc's `ProgressionHeaderInfo`, with its operands already lowered.
#[derive(Clone)]
struct ProgressionHeader {
    ty: Ty,
    step_ty: Ty,
    direction: Direction,
    first: Operand,
    last: Operand,
    last_is_inclusive: bool,
    step: Operand,
    is_reversed: bool,
    /// Set by a handler that knows the answer; otherwise computed from the constants.
    can_overflow: Option<bool>,
    /// The inclusive bound an exclusive `last` was derived from (`originalLastInclusive`).
    original_last: Option<ExprId>,
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
    /// `canOverflow`: an inclusive bound cannot overflow only when the step and last bound are
    /// constants and `last` is at least one step inside the type's range.
    fn can_overflow(&self, lowering: &BodyLowering<'_>) -> bool {
        if let Some(can_overflow) = self.can_overflow {
            return can_overflow;
        }
        let (Some(step), Some(last)) = (
            constant_value(lowering, self.step.value),
            constant_value(lowering, self.last.value),
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

    /// `revertToLastInclusive`: the inclusive form of a bound made exclusive by a handler.
    fn revert_to_last_inclusive(&self) -> Result<Self, FirLoweringFailure> {
        if self.last_is_inclusive {
            return Ok(self.clone());
        }
        let original = self
            .original_last
            .ok_or(FirLoweringFailure::IrreversibleProgression)?;
        Ok(Self {
            last: Operand {
                value: original,
                can_change: self.last.can_change,
            },
            last_is_inclusive: true,
            original_last: None,
            ..self.clone()
        })
    }

    /// `asReversed`: first and last swap and the step is negated, from the inclusive form.
    fn reversed(&self, lowering: &mut BodyLowering<'_>) -> Result<Self, FirLoweringFailure> {
        let header = self.revert_to_last_inclusive()?;
        Ok(Self {
            first: header.last,
            last: header.first,
            step: lowering.negated(header.step, header.step_ty),
            is_reversed: !header.is_reversed,
            direction: header.direction.reversed(),
            can_overflow: None,
            original_last: None,
            ..header
        })
    }

    /// `inductionVar < last` (`<=` when inclusive), `last < inductionVar` when decreasing, and both
    /// guarded by the step's sign when the direction is unknown.
    fn condition(&self, lowering: &mut BodyLowering<'_>, variables: &LoopVariables) -> ExprId {
        match self.direction {
            Direction::Increasing => self.bound_check(lowering, variables, Direction::Increasing),
            Direction::Decreasing => self.bound_check(lowering, variables, Direction::Decreasing),
            Direction::Unknown => {
                let positive = lowering.compare_with_zero(variables.step, self.step_ty, IrBinOp::Gt);
                let increasing = self.bound_check(lowering, variables, Direction::Increasing);
                let increasing = lowering.short_circuit_and(positive, increasing);
                let negative = lowering.compare_with_zero(variables.step, self.step_ty, IrBinOp::Lt);
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

    /// The entry condition evaluated between two constant bounds.
    fn holds_between(&self, first: i64, last: i64) -> Option<bool> {
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

/// `constLongValue`.
fn constant_value(lowering: &BodyLowering<'_>, expression: ExprId) -> Option<i64> {
    constant_bound(lowering, expression)
        .as_ref()
        .and_then(integral_value)
}

fn step_constant(step_ty: Ty, value: i64) -> IrConst {
    if step_ty == Ty::Long {
        IrConst::Long(value)
    } else {
        IrConst::Int(value as i32)
    }
}

/// The type a progression of `ty` steps by: `Long` for a `Long` progression, `Int` otherwise.
fn step_type(ty: Ty) -> Ty {
    if ty == Ty::Long { Ty::Long } else { Ty::Int }
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
        let header = self.range_literal_header(lp.counter.ty(), lp.operation, lp.start, lp.end)?;
        let body = self.expression(lp.body)?;
        Ok(self.progression_loop_expression(header, lp.variable, body, label))
    }

    pub(super) fn progression_loop(
        &mut self,
        lp: ProgressionLoop<'_>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let label = self.control_label(0, lp.target)?;
        let header = self.progression_header(lp.source, lp.counter.ty())?;
        let body = self.expression(lp.body)?;
        Ok(self.progression_loop_expression(header, lp.variable, body, label))
    }

    fn progression_header(
        &mut self,
        source: &FirProgressionSource,
        ty: Ty,
    ) -> Result<ProgressionHeader, FirLoweringFailure> {
        match source {
            FirProgressionSource::Literal {
                operation,
                start,
                end,
            } => self.range_literal_header(ty, *operation, *start, *end),
            FirProgressionSource::Value {
                progression,
                iterable,
            } => self.progression_value_header(*progression, *iterable),
            FirProgressionSource::Step { nested, step } => {
                let nested = self.progression_header(nested, ty)?;
                self.stepped_header(nested, *step)
            }
            FirProgressionSource::Reversed(nested) => {
                self.progression_header(nested, ty)?.reversed(self)
            }
        }
    }

    /// A header operand read from a checked bound: a constant or a read of an immutable local of
    /// the operand's own type cannot change (a widened bound such as `0L..n` is a conversion).
    fn bound_operand(&mut self, bound: FirExprId, ty: Ty) -> Result<Operand, FirLoweringFailure> {
        let lowered = self.expression(bound)?;
        let stable = constant_bound(self, lowered).is_some()
            || (self.fir_type(bound) == Some(ty) && self.reads_immutable_local(bound, lowered));
        Ok(Operand {
            value: self.range_bound(lowered, ty),
            can_change: !stable,
        })
    }

    /// `RangeToHandler`, `DownToHandler`, `UntilHandler` and `RangeUntilHandler`: an inclusive
    /// constant bound that is not the extreme value in the direction of travel is rewritten to the
    /// exclusive bound one step further (`0..10` iterates while `i < 11`).
    fn range_literal_header(
        &mut self,
        ty: Ty,
        operation: FirRangeOperation,
        start: FirExprId,
        end: FirExprId,
    ) -> Result<ProgressionHeader, FirLoweringFailure> {
        let first = self.bound_operand(start, ty)?;
        let last = self.bound_operand(end, ty)?;
        let direction = match operation {
            FirRangeOperation::DownTo => Direction::Decreasing,
            _ => Direction::Increasing,
        };
        let inclusive = matches!(
            operation,
            FirRangeOperation::Through | FirRangeOperation::DownTo
        );
        let exclusive = (inclusive && self.options.prefer_java_like_counter_loop)
            .then(|| exclusive_bound(self, last.value, direction, ty))
            .flatten();
        let step_ty = step_type(ty);
        let unit = if direction == Direction::Decreasing { -1 } else { 1 };
        let step = self.ir.add_expr(IrExpr::Const(step_constant(step_ty, unit)));
        Ok(ProgressionHeader {
            ty,
            step_ty,
            direction,
            first,
            last: Operand {
                value: exclusive.unwrap_or(last.value),
                can_change: last.can_change,
            },
            last_is_inclusive: inclusive && exclusive.is_none(),
            step: Operand::stable(step),
            is_reversed: false,
            can_overflow: (!inclusive || exclusive.is_some()).then_some(false),
            original_last: exclusive.map(|_| last.value),
            prelude: Vec::new(),
        })
    }

    /// `DefaultProgressionHandler`: the progression is read once, through a temporary unless it is
    /// a constant or a local read, and the loop takes its `first`, `last` and `step` from it.
    fn progression_value_header(
        &mut self,
        progression: FirProgressionClass,
        iterable: FirExprId,
    ) -> Result<ProgressionHeader, FirLoweringFailure> {
        let ty = progression.counter.ty();
        let class = progression.ty;
        let value = self.expression(iterable)?;
        // `irCastIfNeeded` to the progression class the header was built from.
        let value = if self.fir_type(iterable) == Some(class) {
            value
        } else {
            self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: value,
                type_operand: class,
            })
        };
        let mut prelude = Vec::new();
        let value = if matches!(self.ir.expr(value), IrExpr::GetValue(_) | IrExpr::Const(_)) {
            value
        } else {
            self.loop_temporary(
                Operand {
                    value,
                    can_change: true,
                },
                class,
                &mut prelude,
            )
            .1
        };
        let member = |lowering: &mut Self, member| {
            let progression = lowering.ir.add_expr(lowering.ir.expr(value).clone());
            Operand {
                value: lowering
                    .ir
                    .add_expr(IrExpr::Checked(IrCheckedOperation::ProgressionMember {
                        progression,
                        class,
                        member,
                    })),
                can_change: true,
            }
        };
        let first = member(self, IrProgressionMember::First);
        let last = member(self, IrProgressionMember::Last);
        let step_ty = step_type(ty);
        let (step, direction) = if progression.unit_step {
            let one = self.ir.add_expr(IrExpr::Const(step_constant(step_ty, 1)));
            (Operand::stable(one), Direction::Increasing)
        } else {
            (member(self, IrProgressionMember::Step), Direction::Unknown)
        };
        Ok(ProgressionHeader {
            ty,
            step_ty,
            direction,
            first,
            last,
            last_is_inclusive: true,
            step,
            is_reversed: false,
            can_overflow: None,
            original_last: None,
            prelude,
        })
    }

    /// `StepHandler`: the step argument is checked to be positive, negated to follow the nested
    /// progression's direction (tested at run time when that is unknown), and `last` is moved to
    /// the last element the stepped progression reaches.
    fn stepped_header(
        &mut self,
        nested: ProgressionHeader,
        step: FirExprId,
    ) -> Result<ProgressionHeader, FirLoweringFailure> {
        let nested = nested.revert_to_last_inclusive()?;
        let step_ty = nested.step_ty;
        let step_argument = self.bound_operand(step, step_ty)?;
        let step_argument_constant = constant_value(self, step_argument.value);
        // A constant step is folded to the step type, so stepping by it is an `iinc`.
        let step_argument = match step_argument_constant {
            Some(value) => Operand::stable(
                self.ir
                    .add_expr(IrExpr::Const(step_constant(step_ty, value))),
            ),
            None => step_argument,
        };
        // A constant step equal to the nested one changes nothing.
        if let (Some(argument), Some(nested_step)) =
            (step_argument_constant, constant_value(self, nested.step.value))
        {
            if nested_step.checked_abs() == Some(argument) {
                return Ok(nested);
            }
        }
        let mut step_statements = Vec::new();
        let (step_argument_slot, step_argument) =
            self.loop_temporary(step_argument, step_ty, &mut step_statements);
        match step_argument_constant {
            None => {
                let not_positive = self.compare_with_zero(step_argument, step_ty, IrBinOp::Le);
                let failure = self.illegal_step(step_argument);
                step_statements.push(self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(not_positive), failure)],
                }));
            }
            Some(value) if value <= 0 => step_statements.push(self.illegal_step(step_argument)),
            Some(_) => {}
        }
        let mut nested_step_statements = Vec::new();
        // A step negated in place lives in a variable the header reassigns, so the loop copies it
        // again (`canChangeValueDuringExecution` holds for a mutable variable).
        let negated_in_place = Operand {
            value: step_argument,
            can_change: true,
        };
        let final_step = match nested.direction {
            Direction::Increasing => Operand::stable(step_argument),
            Direction::Decreasing => match step_argument_slot {
                None => {
                    let negated = self.negated(Operand::stable(step_argument), step_ty);
                    Operand::stable(self.loop_temporary(negated, step_ty, &mut step_statements).1)
                }
                Some(slot) => {
                    let negated = self.negated_read(slot, step_ty);
                    step_statements.push(self.ir.add_expr(IrExpr::SetValue {
                        var: slot,
                        value: negated,
                    }));
                    negated_in_place
                }
            },
            Direction::Unknown => {
                let (_, nested_step) =
                    self.loop_temporary(nested.step, step_ty, &mut nested_step_statements);
                let not_positive = self.compare_with_zero(nested_step, step_ty, IrBinOp::Le);
                match step_argument_slot {
                    None => {
                        let negated = self.negated(Operand::stable(step_argument), step_ty);
                        let chosen = self.ir.add_expr(IrExpr::When {
                            branches: vec![
                                (Some(not_positive), negated.value),
                                (None, step_argument),
                            ],
                        });
                        let chosen = Operand {
                            value: chosen,
                            can_change: true,
                        };
                        Operand::stable(self.loop_temporary(chosen, step_ty, &mut step_statements).1)
                    }
                    Some(slot) => {
                        let negated = self.negated_read(slot, step_ty);
                        let negate = self.ir.add_expr(IrExpr::SetValue {
                            var: slot,
                            value: negated,
                        });
                        step_statements.push(self.ir.add_expr(IrExpr::When {
                            branches: vec![(Some(not_positive), negate)],
                        }));
                        negated_in_place
                    }
                }
            }
        };
        let mut prelude = nested.prelude.clone();
        let mut first_statements = Vec::new();
        let (_, first) = self.loop_temporary(nested.first, nested.ty, &mut first_statements);
        let mut last_statements = Vec::new();
        let (_, last) = self.loop_temporary(nested.last, nested.ty, &mut last_statements);
        if nested.is_reversed {
            prelude.extend(last_statements);
            prelude.extend(first_statements);
        } else {
            prelude.extend(first_statements);
            prelude.extend(last_statements);
        }
        prelude.extend(nested_step_statements);
        prelude.extend(step_statements);
        let unit_step = constant_value(self, final_step.value).is_some_and(|step| step.abs() == 1);
        let last = if unit_step {
            Operand::stable(last)
        } else {
            let first = self.as_step_type(first, nested.ty, step_ty);
            let last = self.as_step_type(last, nested.ty, step_ty);
            let element = self.ir.add_expr(IrExpr::Checked(
                IrCheckedOperation::ProgressionLastElement {
                    first,
                    last,
                    step: final_step.value,
                    ty: step_ty,
                },
            ));
            Operand {
                value: self.as_step_type(element, step_ty, nested.ty),
                can_change: true,
            }
        };
        // The induction variable is a fresh copy of the (possibly stored) first element.
        let first = self.ir.add_expr(self.ir.expr(first).clone());
        Ok(ProgressionHeader {
            first: Operand::stable(first),
            last,
            last_is_inclusive: true,
            step: final_step,
            can_overflow: None,
            original_last: None,
            prelude,
            ..nested
        })
    }

    fn illegal_step(&mut self, step: ExprId) -> ExprId {
        let step = self.ir.add_expr(self.ir.expr(step).clone());
        self.ir
            .add_expr(IrExpr::Checked(IrCheckedOperation::IllegalProgressionStep { step }))
    }

    /// `asStepType`/`asElementType`: the coercion between a `Char` element and its `Int` step.
    fn as_step_type(&mut self, value: ExprId, from: Ty, to: Ty) -> ExprId {
        if from == to {
            value
        } else {
            self.range_bound(value, to)
        }
    }

    /// `IrExpression.negate()`: a constant step is folded, anything else is negated at run time.
    fn negated(&mut self, step: Operand, step_ty: Ty) -> Operand {
        match constant_value(self, step.value) {
            Some(value) => Operand::stable(
                self.ir
                    .add_expr(IrExpr::Const(step_constant(step_ty, value.wrapping_neg()))),
            ),
            None => Operand {
                value: self.ir.add_expr(IrExpr::PrimitiveNeg {
                    operand: step.value,
                    ty: step_ty,
                }),
                can_change: true,
            },
        }
    }

    fn negated_read(&mut self, slot: u32, step_ty: Ty) -> ExprId {
        let current = self.ir.add_expr(IrExpr::GetValue(slot));
        self.ir.add_expr(IrExpr::PrimitiveNeg {
            operand: current,
            ty: step_ty,
        })
    }

    /// `value > 0`, `value < 0` or `value <= 0` over an `Int` or `Long` step.
    fn compare_with_zero(&mut self, value: ExprId, step_ty: Ty, op: IrBinOp) -> ExprId {
        let value = self.ir.add_expr(self.ir.expr(value).clone());
        let zero = self.ir.add_expr(IrExpr::Const(step_constant(step_ty, 0)));
        self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op,
            lhs: value,
            rhs: zero,
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
        let first_constant = constant_value(self, header.first.value);
        let can_overflow = header.can_overflow(self);
        let java_like = self.options.prefer_java_like_counter_loop && !header.last_is_inclusive;
        // Only the guarded do-while keeps the loop variable apart from the induction variable; the
        // other shapes step the induction variable after the body, so it is the loop variable.
        let separate_loop_variable = !can_overflow && !java_like;
        let mut statements = header.prelude.clone();
        let (induction, induction_declaration) = if separate_loop_variable {
            let slot = self.allocate_temporary();
            let declaration = self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(header.first.value),
                named: false,
            });
            (slot, declaration)
        } else {
            self.loop_variable_declaration(variable.raw(), ty, header.first.value)
        };
        let mut last_statements = Vec::new();
        let (_, last) = self.loop_temporary(header.last, ty, &mut last_statements);
        if header.is_reversed {
            statements.extend(last_statements);
            statements.push(induction_declaration);
        } else {
            statements.push(induction_declaration);
            statements.extend(last_statements);
        }
        let (_, step) = self.loop_temporary(header.step, header.step_ty, &mut statements);
        let variables = LoopVariables {
            induction,
            last,
            step,
        };
        let increment = self.increment_induction_variable(&variables, ty);
        let loop_expression = if can_overflow {
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
            self.guarded_unless_entered(&header, &variables, first_constant, repeat)
        } else if java_like {
            let condition = header.condition(self, &variables);
            self.ir.add_expr(IrExpr::While {
                cond: condition,
                body,
                update: Some(increment),
                post_test: false,
                label: Some(label),
            })
        } else {
            // `val loopVariable = inductionVar; inductionVar += step; body` while the bound holds.
            let current = self.ir.add_expr(IrExpr::GetValue(induction));
            let (_, loop_variable) = self.loop_variable_declaration(variable.raw(), ty, current);
            let body = self.ir.add_expr(IrExpr::Block {
                stmts: vec![loop_variable, increment, body],
                value: None,
            });
            let condition = header.condition(self, &variables);
            let repeat = self.ir.add_expr(IrExpr::While {
                cond: condition,
                body,
                update: None,
                post_test: true,
                label: Some(label),
            });
            self.guarded_unless_entered(&header, &variables, first_constant, repeat)
        };
        statements.push(loop_expression);
        self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        })
    }

    /// `if (<entry condition>) <loop>`. kotlinc folds an `Int`-sized comparison between two
    /// constants before emission, so a guard that always holds disappears; a `Long` comparison
    /// (`lcmp`) is not folded.
    fn guarded_unless_entered(
        &mut self,
        header: &ProgressionHeader,
        variables: &LoopVariables,
        first_constant: Option<i64>,
        repeat: ExprId,
    ) -> ExprId {
        let last_constant = constant_value(self, variables.last).filter(|_| header.ty != Ty::Long);
        let entered = first_constant
            .zip(last_constant)
            .and_then(|(first, last)| header.holds_between(first, last));
        if entered == Some(true) {
            return repeat;
        }
        let entry = header.condition(self, variables);
        self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(entry), repeat)],
        })
    }

    /// `createLoopTemporaryVariableIfNecessary`: an operand that cannot change while the loop runs
    /// is re-read where it is used; anything else is copied to a temporary first.
    fn loop_temporary(
        &mut self,
        operand: Operand,
        ty: Ty,
        statements: &mut Vec<ExprId>,
    ) -> (Option<u32>, ExprId) {
        if !operand.can_change {
            return (None, operand.value);
        }
        let slot = self.allocate_temporary();
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: slot,
            ty,
            init: Some(operand.value),
            named: false,
        }));
        (Some(slot), self.ir.add_expr(IrExpr::GetValue(slot)))
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
