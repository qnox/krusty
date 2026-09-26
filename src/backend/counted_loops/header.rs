//! kotlinc's `ProgressionHeaderInfo` and the handlers that build one from a checked progression:
//! - a range literal (`RangeToHandler`, `DownToHandler`, `UntilHandler`, `RangeUntilHandler`) has
//!   a unit step and a known direction; `until`/`..<` have an exclusive last bound, and on a target
//!   preferring Java-like counter loops `..`/`downTo` become exclusive when the last bound is a
//!   constant that can move one step outward without overflowing;
//! - a progression value (`DefaultProgressionHandler`) is read through the `first`, `last` and
//!   `step` members selected from its class; a `*Range` has step 1 and increases, any other progression's direction is known only
//!   from the sign of its step at run time;
//! - `step` (`StepHandler`) checks its argument, negates it to follow the nested progression's
//!   direction, and recomputes `last` with `getProgressionLastElement`;
//! - `reversed` (`ReversedHandler`) swaps first and last and negates the step.

use crate::fir::FirRangeOperation;
use crate::ir::{
    ExprId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrFile, IrProgressionSource,
    IrRuntimeFunction, IrShortCircuitKind,
};
use crate::types::Ty;

use super::{
    constant_bound, constant_value, is_unsigned, step_constant, CounterLoopStyle, Operand, Realizer,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Direction {
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

/// kotlinc's `ProgressionHeaderInfo`, over IR operands.
#[derive(Clone)]
pub(super) struct ProgressionHeader {
    pub(super) ty: Ty,
    pub(super) step_ty: Ty,
    pub(super) direction: Direction,
    pub(super) first: Operand,
    pub(super) last: Operand,
    pub(super) last_is_inclusive: bool,
    pub(super) step: Operand,
    pub(super) is_reversed: bool,
    /// Set by a handler that knows the answer; otherwise computed from the constants.
    can_overflow: Option<bool>,
    /// The inclusive bound an exclusive `last` was derived from (`originalLastInclusive`).
    original_last: Option<ExprId>,
    /// Statements that must run before the loop's own variables (`additionalStatements`).
    pub(super) prelude: Vec<ExprId>,
}

/// The loop's variables once declared: where `last` and `step` are read from.
pub(super) struct LoopVariables {
    pub(super) induction: u32,
    pub(super) last: ExprId,
    pub(super) step: ExprId,
}

impl ProgressionHeader {
    /// `canOverflow`: an inclusive bound cannot overflow only when the step and last bound are
    /// constants and `last` is at least one step inside the type's range.
    pub(super) fn can_overflow(&self, ir: &IrFile) -> bool {
        if let Some(can_overflow) = self.can_overflow {
            return can_overflow;
        }
        // kotlinc reads constants through `constLongValue`, which sees no unsigned constant.
        if is_unsigned(self.ty) {
            return true;
        }
        let (Some(step), Some(last)) = (
            constant_value(ir, self.step.value),
            constant_value(ir, self.last.value),
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

    /// `revertToLastInclusive`: the inclusive form of a bound made exclusive by a handler. Checked
    /// FIR applies `step` and `reversed` only to a progression with an inclusive last bound, so an
    /// exclusive one here was always derived from an inclusive original.
    fn revert_to_last_inclusive(&self) -> Self {
        if self.last_is_inclusive {
            return self.clone();
        }
        let original = self
            .original_last
            .expect("checked FIR steps or reverses only a progression with an inclusive last");
        Self {
            last: Operand {
                value: original,
                can_change: self.last.can_change,
            },
            last_is_inclusive: true,
            original_last: None,
            ..self.clone()
        }
    }

    /// `inductionVar < last` (`<=` when inclusive), `last < inductionVar` when decreasing, and both
    /// guarded by the step's sign when the direction is unknown.
    pub(super) fn condition(
        &self,
        realizer: &mut Realizer<'_>,
        variables: &LoopVariables,
    ) -> ExprId {
        match self.direction {
            Direction::Increasing => self.bound_check(realizer, variables, Direction::Increasing),
            Direction::Decreasing => self.bound_check(realizer, variables, Direction::Decreasing),
            Direction::Unknown => {
                let positive =
                    realizer.compare_with_zero(variables.step, self.step_ty, IrBinOp::Gt);
                let increasing = self.bound_check(realizer, variables, Direction::Increasing);
                let increasing =
                    realizer.short_circuit(IrShortCircuitKind::And, positive, increasing);
                let negative =
                    realizer.compare_with_zero(variables.step, self.step_ty, IrBinOp::Lt);
                let decreasing = self.bound_check(realizer, variables, Direction::Decreasing);
                let decreasing =
                    realizer.short_circuit(IrShortCircuitKind::And, negative, decreasing);
                realizer.short_circuit(IrShortCircuitKind::Or, increasing, decreasing)
            }
        }
    }

    fn bound_check(
        &self,
        realizer: &mut Realizer<'_>,
        variables: &LoopVariables,
        direction: Direction,
    ) -> ExprId {
        let induction = realizer.add(IrExpr::GetValue(variables.induction));
        let op = if self.last_is_inclusive {
            IrBinOp::Le
        } else {
            IrBinOp::Lt
        };
        let (lhs, rhs) = match direction {
            Direction::Decreasing => (variables.last, induction),
            Direction::Increasing | Direction::Unknown => (induction, variables.last),
        };
        let Some(compare) = realizer.unsigned_compare.clone() else {
            return realizer.add(IrExpr::PrimitiveBinOp { op, lhs, rhs });
        };
        // `lhs.compareTo(rhs) < 0` (`<= 0`) through the selected unsigned comparison.
        let compared = realizer.runtime_call(&compare, vec![lhs, rhs]);
        let zero = realizer.add(IrExpr::Const(IrConst::Int(0)));
        realizer.add(IrExpr::PrimitiveBinOp {
            op,
            lhs: compared,
            rhs: zero,
        })
    }

    /// The entry condition evaluated between two constant bounds.
    pub(super) fn holds_between(&self, first: i64, last: i64) -> Option<bool> {
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

/// The type a progression of `ty` steps by: `Long` for a `Long` or `ULong` progression, `Int`
/// otherwise.
fn step_type(ty: Ty) -> Ty {
    if matches!(ty, Ty::Long | Ty::ULong) {
        Ty::Long
    } else {
        Ty::Int
    }
}

fn exclusive_bound(ir: &mut IrFile, last: ExprId, direction: Direction, ty: Ty) -> Option<ExprId> {
    let delta: i64 = match direction {
        Direction::Increasing => 1,
        Direction::Decreasing => -1,
        Direction::Unknown => return None,
    };
    let moved = match constant_bound(ir, last)? {
        IrConst::Byte(value) => IrConst::Byte(value.checked_add(i8::try_from(delta).ok()?)?),
        IrConst::Short(value) => IrConst::Short(value.checked_add(i16::try_from(delta).ok()?)?),
        IrConst::Int(value) => IrConst::Int(value.checked_add(i32::try_from(delta).ok()?)?),
        IrConst::Long(value) => IrConst::Long(value.checked_add(delta)?),
        IrConst::Char(value) => IrConst::Char(u16::try_from(i64::from(value) + delta).ok()?),
        _ => return None,
    };
    let moved = ir.add_expr(IrExpr::Const(moved));
    Some(ir.add_expr(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: moved,
        type_operand: ty,
    }))
}

impl Realizer<'_> {
    pub(super) fn progression_header(
        &mut self,
        source: &IrProgressionSource,
        ty: Ty,
    ) -> ProgressionHeader {
        match source {
            IrProgressionSource::Literal {
                operation,
                start,
                end,
            } => self.range_literal_header(ty, *operation, *start, *end),
            IrProgressionSource::Value {
                setup,
                first,
                last,
                step,
            } => self.progression_value_header(ty, *setup, *first, *last, *step),
            IrProgressionSource::Step {
                nested,
                step,
                last_element,
            } => {
                let nested = self.progression_header(nested, ty);
                self.stepped_header(nested, *step, last_element)
            }
            IrProgressionSource::Reversed(nested) => {
                let nested = self.progression_header(nested, ty);
                self.reversed_header(nested)
            }
        }
    }

    /// `RangeToHandler`, `DownToHandler`, `UntilHandler` and `RangeUntilHandler`: an inclusive
    /// constant bound that is not the extreme value in the direction of travel is rewritten to the
    /// exclusive bound one step further (`0..10` iterates while `i < 11`).
    fn range_literal_header(
        &mut self,
        ty: Ty,
        operation: FirRangeOperation,
        start: ExprId,
        end: ExprId,
    ) -> ProgressionHeader {
        let first = self.bound_operand(start, ty);
        let last = self.bound_operand(end, ty);
        let direction = match operation {
            FirRangeOperation::DownTo => Direction::Decreasing,
            _ => Direction::Increasing,
        };
        let inclusive = matches!(
            operation,
            FirRangeOperation::Through | FirRangeOperation::DownTo
        );
        let exclusive = (inclusive && self.style == CounterLoopStyle::JavaLike && !is_unsigned(ty))
            .then(|| exclusive_bound(self.ir, last.value, direction, ty))
            .flatten();
        let step_ty = step_type(ty);
        let unit = if direction == Direction::Decreasing {
            -1
        } else {
            1
        };
        let step = self.add(IrExpr::Const(step_constant(step_ty, unit)));
        ProgressionHeader {
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
        }
    }

    /// `DefaultProgressionHandler`: the loop takes its `first`, `last` and `step` from the
    /// progression's selected members, after the value was stored if it needed a temporary.
    fn progression_value_header(
        &mut self,
        ty: Ty,
        setup: Option<ExprId>,
        first: ExprId,
        last: ExprId,
        step: Option<ExprId>,
    ) -> ProgressionHeader {
        let read = |value| Operand {
            value,
            can_change: true,
        };
        let step_ty = step_type(ty);
        let (step, direction) = match step {
            Some(step) => (read(step), Direction::Unknown),
            None => {
                let one = self.add(IrExpr::Const(step_constant(step_ty, 1)));
                (Operand::stable(one), Direction::Increasing)
            }
        };
        ProgressionHeader {
            ty,
            step_ty,
            direction,
            first: read(first),
            last: read(last),
            last_is_inclusive: true,
            step,
            is_reversed: false,
            can_overflow: None,
            original_last: None,
            prelude: setup.into_iter().collect(),
        }
    }

    /// `asReversed`: first and last swap and the step is negated, from the inclusive form.
    fn reversed_header(&mut self, header: ProgressionHeader) -> ProgressionHeader {
        let header = header.revert_to_last_inclusive();
        ProgressionHeader {
            first: header.last,
            last: header.first,
            step: self.negated(header.step, header.step_ty),
            is_reversed: !header.is_reversed,
            direction: header.direction.reversed(),
            can_overflow: None,
            original_last: None,
            ..header
        }
    }

    /// `StepHandler`: the step argument is checked to be positive, negated to follow the nested
    /// progression's direction (tested at run time when that is unknown), and `last` is moved to
    /// the last element the stepped progression reaches.
    fn stepped_header(
        &mut self,
        nested: ProgressionHeader,
        step: ExprId,
        last_element: &IrRuntimeFunction,
    ) -> ProgressionHeader {
        let nested = nested.revert_to_last_inclusive();
        let step_ty = nested.step_ty;
        let step_argument = self.bound_operand(step, step_ty);
        let step_argument_constant = constant_value(self.ir, step_argument.value);
        // A constant step is folded to the step type, so stepping by it is an `iinc`.
        let step_argument = match step_argument_constant {
            Some(value) => Operand::stable(self.add(IrExpr::Const(step_constant(step_ty, value)))),
            None => step_argument,
        };
        // A constant step equal to the nested one changes nothing.
        if let (Some(argument), Some(nested_step)) = (
            step_argument_constant,
            constant_value(self.ir, nested.step.value),
        ) {
            if nested_step.checked_abs() == Some(argument) {
                return nested;
            }
        }
        let mut step_statements = Vec::new();
        let (step_argument_slot, step_argument) =
            self.loop_temporary(step_argument, step_ty, &mut step_statements);
        match step_argument_constant {
            None => {
                let not_positive = self.compare_with_zero(step_argument, step_ty, IrBinOp::Le);
                let failure = self.illegal_step(step_argument);
                let check = self.add(IrExpr::When {
                    branches: vec![(Some(not_positive), failure)],
                });
                step_statements.push(check);
            }
            Some(value) if value <= 0 => {
                let failure = self.illegal_step(step_argument);
                step_statements.push(failure);
            }
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
                    Operand::stable(
                        self.loop_temporary(negated, step_ty, &mut step_statements)
                            .1,
                    )
                }
                Some(slot) => {
                    let negated = self.negated_read(slot, step_ty);
                    let negate = self.add(IrExpr::SetValue {
                        var: slot,
                        value: negated,
                    });
                    step_statements.push(negate);
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
                        let chosen = self.add(IrExpr::When {
                            branches: vec![
                                (Some(not_positive), negated.value),
                                (None, step_argument),
                            ],
                        });
                        let chosen = Operand {
                            value: chosen,
                            can_change: true,
                        };
                        Operand::stable(
                            self.loop_temporary(chosen, step_ty, &mut step_statements).1,
                        )
                    }
                    Some(slot) => {
                        let negated = self.negated_read(slot, step_ty);
                        let negate = self.add(IrExpr::SetValue {
                            var: slot,
                            value: negated,
                        });
                        let check = self.add(IrExpr::When {
                            branches: vec![(Some(not_positive), negate)],
                        });
                        step_statements.push(check);
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
        let unit_step =
            constant_value(self.ir, final_step.value).is_some_and(|step| step.abs() == 1);
        let last = if unit_step {
            Operand::stable(last)
        } else {
            // The selected overload's element type (`Int` for a `Char` progression).
            let element_ty = last_element.result;
            let first = self.as_step_type(first, nested.ty, element_ty);
            let last = self.as_step_type(last, nested.ty, element_ty);
            let element = self.runtime_call(last_element, vec![first, last, final_step.value]);
            Operand {
                value: self.as_step_type(element, element_ty, nested.ty),
                can_change: true,
            }
        };
        // The induction variable is a fresh copy of the (possibly stored) first element.
        let first = self.reread(first);
        ProgressionHeader {
            first: Operand::stable(first),
            last,
            last_is_inclusive: true,
            step: final_step,
            can_overflow: None,
            original_last: None,
            prelude,
            ..nested
        }
    }

    fn illegal_step(&mut self, step: ExprId) -> ExprId {
        let step = self.reread(step);
        self.add(IrExpr::Checked(
            IrCheckedOperation::IllegalProgressionStep { step },
        ))
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
        match constant_value(self.ir, step.value) {
            Some(value) => Operand::stable(
                self.add(IrExpr::Const(step_constant(step_ty, value.wrapping_neg()))),
            ),
            None => Operand {
                value: self.add(IrExpr::PrimitiveNeg {
                    operand: step.value,
                    ty: step_ty,
                }),
                can_change: true,
            },
        }
    }

    fn negated_read(&mut self, slot: u32, step_ty: Ty) -> ExprId {
        let current = self.add(IrExpr::GetValue(slot));
        self.add(IrExpr::PrimitiveNeg {
            operand: current,
            ty: step_ty,
        })
    }

    /// `value > 0`, `value < 0` or `value <= 0` over an `Int` or `Long` step.
    fn compare_with_zero(&mut self, value: ExprId, step_ty: Ty, op: IrBinOp) -> ExprId {
        let value = self.reread(value);
        let zero = self.add(IrExpr::Const(step_constant(step_ty, 0)));
        self.add(IrExpr::PrimitiveBinOp {
            op,
            lhs: value,
            rhs: zero,
        })
    }
}
