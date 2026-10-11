//! kotlinc's `RangeContainsLowering` over a range literal: `x in a..b` is `a <= x && x <= b`.
//!
//! The comparisons run in the widest numeric type of the bounds and the value. A bound or value
//! that may have side effects keeps its place in the evaluation order:
//! - when the value may, it is stored after whichever bounds may (`a`, `b`, then `x`);
//! - otherwise, when both bounds may, the first-written one is stored and the other bound's
//!   comparison goes first, so `&&` cannot skip it;
//! - otherwise nothing is stored.
//!
//! On a target preferring Java-like counter loops a constant inclusive upper bound becomes the
//! exclusive one a step above it (`x < 11` for `x in 1..10`), as kotlinc's `RangeToHandler` builds
//! the header both loops and membership read. The conjunction is a `when` that yields `false` when
//! the first comparison fails; `!in` negates it.

use crate::fir::FirRangeOperation;
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrTypeOp};
use crate::types::Ty;

use super::header::{moved_bound, Direction};
use super::{constant_bound, integral_value, CounterLoopStyle, Realizer};

/// A checked membership test in a range literal, compared in `comparison`.
pub(super) struct Membership {
    pub(super) operation: FirRangeOperation,
    pub(super) value: ExprId,
    pub(super) start: ExprId,
    pub(super) end: ExprId,
    pub(super) comparison: Ty,
}

/// The bounds `x` is compared with: `lower <= x` and `x <= upper` (`x < upper` when exclusive).
struct Bounds {
    lower: ExprId,
    upper: ExprId,
    upper_inclusive: bool,
    /// `shouldUpperComeFirst`: the upper bound is written, and evaluated, first.
    upper_first: bool,
}

impl Realizer<'_> {
    /// The node testing `membership` (before an `!in` negates it).
    pub(super) fn range_contains(&mut self, membership: &Membership) -> IrExpr {
        let comparison = membership.comparison;
        let bounds = self.membership_bounds(membership);
        let mut statements = Vec::new();
        let value = self.cast(membership.value, comparison);
        let (value_stored, value) = self.temporary_if_necessary(value);
        let (lower, upper, lower_first) = if let Some(value_declaration) = value_stored {
            let (lower_declaration, lower) = self.temporary_if_necessary(bounds.lower);
            let (upper_declaration, upper) = self.temporary_if_necessary(bounds.upper);
            if bounds.upper_first {
                statements.extend(upper_declaration);
                statements.extend(lower_declaration);
            } else {
                statements.extend(lower_declaration);
                statements.extend(upper_declaration);
            }
            statements.push(value_declaration);
            (lower, upper, true)
        } else if self.has_side_effects(bounds.lower) && self.has_side_effects(bounds.upper) {
            if bounds.upper_first {
                let (declaration, upper) = self.temporary_if_necessary(bounds.upper);
                statements.extend(declaration);
                (bounds.lower, upper, true)
            } else {
                let (declaration, lower) = self.temporary_if_necessary(bounds.lower);
                statements.extend(declaration);
                (lower, bounds.upper, false)
            }
        } else {
            (bounds.lower, bounds.upper, true)
        };
        let lower = self.cast(lower, comparison);
        let upper = self.cast(upper, comparison);
        let value_for_lower = self.reread(value);
        let lower_clause = self.compare(IrBinOp::Le, comparison, lower, value_for_lower);
        let value_for_upper = self.reread(value);
        let upper_operation = if bounds.upper_inclusive {
            IrBinOp::Le
        } else {
            IrBinOp::Lt
        };
        let upper_clause = self.compare(upper_operation, comparison, value_for_upper, upper);
        let (first, second) = if lower_first {
            (lower_clause, upper_clause)
        } else {
            (upper_clause, lower_clause)
        };
        let false_value = self.add_checked(IrExpr::Const(IrConst::Boolean(false)), Ty::Boolean);
        let conjunction = IrExpr::When {
            branches: vec![(Some(first), second), (None, false_value)],
        };
        if statements.is_empty() {
            conjunction
        } else {
            let conjunction = self.add_checked(conjunction, Ty::Boolean);
            IrExpr::Block {
                stmts: statements,
                value: Some(conjunction),
            }
        }
    }

    /// The `RangeToHandler`, `UntilHandler` and `RangeUntilHandler` headers (and a floating-point
    /// `rangeTo`'s, which has no exclusive form) as membership reads them.
    fn membership_bounds(&mut self, membership: &Membership) -> Bounds {
        let Membership {
            operation,
            start,
            end,
            comparison,
            ..
        } = *membership;
        match operation {
            FirRangeOperation::Through => {
                let exclusive = (self.style == CounterLoopStyle::JavaLike
                    && !matches!(comparison, Ty::Float | Ty::Double))
                .then(|| moved_bound(self.ir, end, Direction::Increasing))
                .flatten();
                Bounds {
                    lower: start,
                    upper: exclusive.unwrap_or(end),
                    upper_inclusive: exclusive.is_none(),
                    upper_first: false,
                }
            }
            FirRangeOperation::Until | FirRangeOperation::OpenEnd => Bounds {
                lower: start,
                upper: end,
                upper_inclusive: false,
                upper_first: false,
            },
            // Checked FIR never selects `downTo` for membership: it builds a progression, which
            // is not a closed range. Its bounds are still the reverse of the written order.
            FirRangeOperation::DownTo => Bounds {
                lower: end,
                upper: start,
                upper_inclusive: true,
                upper_first: true,
            },
        }
    }

    /// `canHaveSideEffects`: anything but a constant or a read of a value.
    fn has_side_effects(&self, expression: ExprId) -> bool {
        !matches!(
            self.ir.expr(expression),
            IrExpr::Const(_) | IrExpr::GetValue(_)
        )
    }

    /// `createTemporaryVariableIfNecessary`: an operand that may have side effects is stored in a
    /// temporary of its own type and read from it.
    fn temporary_if_necessary(&mut self, operand: ExprId) -> (Option<ExprId>, ExprId) {
        if !self.has_side_effects(operand) {
            return (None, operand);
        }
        let ty = self
            .ir
            .checked_type(operand)
            .expect("a range operand carries its checked type");
        let slot = self.allocate_temporary();
        let declaration = self.add(IrExpr::Variable {
            index: slot,
            ty,
            init: Some(operand),
            named: false,
        });
        let read = self.add_checked(IrExpr::GetValue(slot), ty);
        (Some(declaration), read)
    }

    /// `castIfNecessary`: a constant is folded to the comparison type and any other operand of
    /// another type is converted.
    fn cast(&mut self, operand: ExprId, target: Ty) -> ExprId {
        let ty = self.ir.checked_type(operand);
        if ty == Some(target) || ty == Some(Ty::Nothing) {
            return operand;
        }
        if let Some(constant) = constant_bound(self.ir, operand) {
            if let Some(converted) = converted_constant(&constant, target) {
                return self.add_checked(IrExpr::Const(converted), target);
            }
        }
        self.add_checked(
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: operand,
                type_operand: target,
            },
            target,
        )
    }

    fn compare(&mut self, operation: IrBinOp, ty: Ty, lhs: ExprId, rhs: ExprId) -> ExprId {
        // The operands are compared in the selected type; targets choose only its representation.
        self.ir.logical_types.insert(lhs, ty);
        self.ir.logical_types.insert(rhs, ty);
        self.add_checked(
            IrExpr::PrimitiveBinOp {
                op: operation,
                lhs,
                rhs,
            },
            Ty::Boolean,
        )
    }
}

/// A numeric constant as the same value of `target`.
fn converted_constant(constant: &IrConst, target: Ty) -> Option<IrConst> {
    let floating = match *constant {
        IrConst::Float(value) => Some(f64::from(value)),
        IrConst::Double(value) => Some(value),
        _ => None,
    };
    let integral = integral_value(constant);
    Some(match target {
        Ty::Int => IrConst::Int(integral? as i32),
        Ty::Long => IrConst::Long(integral?),
        Ty::Char => IrConst::Char(integral? as u16),
        Ty::Float => IrConst::Float(match floating {
            Some(value) => value as f32,
            None => integral? as f32,
        }),
        Ty::Double => IrConst::Double(floating.or_else(|| integral.map(|v| v as f64))?),
        _ => return None,
    })
}
