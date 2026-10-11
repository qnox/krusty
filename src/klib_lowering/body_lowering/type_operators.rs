//! Type operators: `is`, `!is`, `as` and the implicit casts a KLIB states explicitly.
//!
//! Each lowers to the type operation checked FIR lowering produces for the source form, where
//! that is one operation over the lowered operand. A form checked FIR lowering expands into
//! control flow of its own (`as?`, `is` of a nullable type), settles without a test (`is
//! Nothing`), or derives from facts a KLIB does not carry (a platform type's implicit non-null
//! assertion names the call that produced the value) declines by its name.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr, IrTypeOp};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrTypeId, KlibIrTypeOperator};
use crate::types::Ty;

use super::super::decline::KlibBodyDeclineReason;
use super::super::klib_types::semantic_type;

impl BodyLowering<'_, '_, '_> {
    /// The type operator `operator` with the type operand `operand` applied to `argument`, typed
    /// `ty`. Checked types are recorded here, since an implicit cast that converts nothing adds no
    /// node of its own.
    pub(super) fn type_operator(
        &mut self,
        operator: KlibIrTypeOperator,
        operand: KlibIrTypeId,
        argument: KlibIrExprId,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let target = semantic_type(self.arena, operand)?;
        let op = match operator {
            KlibIrTypeOperator::ImplicitCast => return self.implicit_cast(target, argument, ty),
            KlibIrTypeOperator::InstanceOf | KlibIrTypeOperator::NotInstanceOf => {
                if ty != Ty::Boolean {
                    return Err(mismatch("an `is` check is not typed `Boolean`"));
                }
                if target.is_nullable() {
                    return unsupported("an `is` check of a nullable type");
                }
                if target.canonical_semantic() == Ty::Nothing {
                    return unsupported("an `is` check of `Nothing`");
                }
                if operator == KlibIrTypeOperator::InstanceOf {
                    IrTypeOp::InstanceOf
                } else {
                    IrTypeOp::NotInstanceOf
                }
            }
            KlibIrTypeOperator::Cast => {
                if ty != target {
                    return Err(mismatch("a cast is typed apart from its target"));
                }
                // A cast to a non-null type also rejects `null`.
                if target.is_nullable() {
                    IrTypeOp::Cast
                } else {
                    IrTypeOp::CastNonNull
                }
            }
            other => return unsupported(type_operator_name(other)),
        };
        let arg = self.expression(argument)?;
        if self.lowered_type(arg) == Ty::Unit {
            // Checked FIR lowering first gives a `Unit` operand a value of its own.
            return unsupported("a type operator on a `Unit` value");
        }
        let lowered = self.ir.add_expr(IrExpr::TypeOp {
            op,
            arg,
            type_operand: target,
        });
        if operator == KlibIrTypeOperator::Cast {
            self.ir.written_casts.insert(lowered);
        }
        Ok(self.typed(lowered, ty))
    }

    /// An implicit cast to `target`. One to the value's own type converts nothing and adds no
    /// node. One from a nullable type to its non-null type is the smart cast the source relies
    /// on, which checked FIR lowering lowers as a cast of a reference and as a coercion of a
    /// primitive value. Any other implicit cast is a narrowing or a widening this lowering cannot
    /// tell apart without the classes' supertypes, and declines.
    fn implicit_cast(&mut self, target: Ty, argument: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if target != ty {
            return Err(mismatch("an implicit cast is typed apart from its target"));
        }
        let arg = self.expression(argument)?;
        let actual = self.lowered_type(arg);
        if actual == target {
            return Ok(arg);
        }
        if target.is_nullable() || Ty::nullable(target) != actual {
            return Err(KlibBodyDeclineReason::ImplicitConversion);
        }
        let op = if target.is_reference() {
            IrTypeOp::Cast
        } else {
            IrTypeOp::ImplicitCoercion
        };
        let cast = self.ir.add_expr(IrExpr::TypeOp {
            op,
            arg,
            type_operand: target,
        });
        Ok(self.typed(cast, ty))
    }
}

fn type_operator_name(operator: KlibIrTypeOperator) -> &'static str {
    match operator {
        KlibIrTypeOperator::Cast => "a cast",
        KlibIrTypeOperator::ImplicitCast => "an implicit cast",
        KlibIrTypeOperator::ImplicitNotNull => "an implicit non-null assertion",
        KlibIrTypeOperator::ImplicitCoercionToUnit => "an implicit coercion to `Unit`",
        KlibIrTypeOperator::ImplicitIntegerCoercion => "an implicit integer coercion",
        KlibIrTypeOperator::SafeCast => "a safe cast",
        KlibIrTypeOperator::InstanceOf => "an `is` check",
        KlibIrTypeOperator::NotInstanceOf => "an `!is` check",
        KlibIrTypeOperator::SamConversion => "a SAM conversion",
        KlibIrTypeOperator::ImplicitDynamicCast => "an implicit dynamic cast",
        KlibIrTypeOperator::ReinterpretCast => "a reinterpreting cast",
    }
}
