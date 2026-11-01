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
use crate::types::{Ty, TyKind};

use super::super::decline::KlibBodyDeclineReason;
use super::super::klib_types::semantic_type;

impl BodyLowering<'_> {
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
        let target = semantic_type(self.arena, self.header.type_parameters(), operand)?;
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
    /// node, and neither does one of `null` to a nullable type. One to the value's nullable type
    /// is a widening checked FIR lowering marks as a coercion, and one to another supertype (by
    /// the calling compilation's subtyping) is the value itself, as in the source. An implicit
    /// cast of a reference to a subtype, or to another parameterization of its classifier, is a
    /// smart cast the declaring compilation proved, which checked FIR lowering lowers as a cast.
    /// One of a primitive value from its nullable type is a coercion. Any other implicit cast
    /// declines.
    fn implicit_cast(&mut self, target: Ty, argument: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if target != ty {
            return Err(mismatch("an implicit cast is typed apart from its target"));
        }
        if let Some(proven) = self.null_proven_receiver(argument, target)? {
            return Ok(proven);
        }
        let arg = self.expression(argument)?;
        let actual = self.lowered_type(arg);
        if super::is_value_of(actual, target) {
            return Ok(arg);
        }
        if let Some(widened) = self.nullable_widening(arg, actual, target) {
            return Ok(self.typed(widened, ty));
        }
        if let Some(upcast) = self.upcast(arg, actual, target) {
            return Ok(upcast);
        }
        let to_non_null = !target.is_nullable() && Ty::nullable(target) == actual;
        let op = if target.is_reference()
            && actual.is_reference()
            && (to_non_null || self.narrows(actual, target))
        {
            IrTypeOp::Cast
        } else if to_non_null {
            IrTypeOp::ImplicitCoercion
        } else {
            return Err(KlibBodyDeclineReason::ImplicitConversion);
        };
        let cast = self.ir.add_expr(IrExpr::TypeOp {
            op,
            arg,
            type_operand: target,
        });
        Ok(self.typed(cast, ty))
    }
}

impl BodyLowering<'_> {
    /// Whether a smart cast of a value of `actual` to `target` narrows it: `target` is a subtype
    /// of `actual`, of `actual`'s classifier at any type arguments (`Collection<*>` after
    /// `this is Collection<*>` on an `Iterable<T>`), or names the same classifier at other type
    /// arguments.
    fn narrows(&self, actual: Ty, target: Ty) -> bool {
        let classifier = |ty: Ty| match ty.non_null().kind() {
            TyKind::Obj(name, _) => Some(name),
            _ => None,
        };
        let parameters = self.header.type_parameters();
        self.linker.is_subtype(parameters, target, actual)
            || classifier(actual).is_some_and(|name| {
                classifier(target) == Some(name)
                    || self
                        .linker
                        .is_subtype(parameters, target, Self::star_projected(actual))
            })
    }

    /// `ty`'s classifier with every type argument a star projection.
    fn star_projected(ty: Ty) -> Ty {
        let TyKind::Obj(name, arguments) = ty.non_null().kind() else {
            return ty;
        };
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let stars = arguments
            .iter()
            .map(|_| Ty::star_projection(any))
            .collect::<Vec<_>>();
        Ty::obj_args_name(name, &stars)
    }

    /// `value`, of the non-null type `actual`, widened to its nullable type `target`: a coercion
    /// checked FIR lowering places for the target to represent. `None` for any other pair.
    pub(super) fn nullable_widening(
        &mut self,
        value: ExprId,
        actual: Ty,
        target: Ty,
    ) -> Option<ExprId> {
        if actual.is_nullable() || target != Ty::nullable(actual) {
            return None;
        }
        Some(self.coercion(value, target))
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
