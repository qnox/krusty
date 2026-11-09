//! Type operators: `is`, `!is`, `as` and the implicit casts a KLIB states explicitly.
//!
//! Each lowers to the type operation checked FIR lowering produces for the source form. An `is`
//! test is built by the common type-test operations checked FIR lowering uses, so a test of a
//! type that admits `null` expands to the same null-or-instance shape and a test of `Nothing`
//! settles to the same constant. A form checked FIR lowering expands into control flow of its
//! own that a KLIB does not share (`as?`), or that derives from facts a KLIB does not carry (a
//! platform type's implicit non-null assertion names the call that produced the value) declines
//! by its name.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{admits_null_at_runtime, ExprId, IrExpr, IrTypeOp};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrTypeId, KlibIrTypeOperator};
use crate::types::{value_conversion, Ty, TyKind, ValueConversion};

use super::super::decline::KlibBodyDeclineReason;

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
        let target = self.semantic(operand)?;
        let op = match operator {
            KlibIrTypeOperator::ImplicitCast => return self.implicit_cast(target, argument, ty),
            KlibIrTypeOperator::InstanceOf | KlibIrTypeOperator::NotInstanceOf => {
                if ty != Ty::Boolean {
                    return Err(mismatch("an `is` check is not typed `Boolean`"));
                }
                return self.type_test(
                    operator == KlibIrTypeOperator::NotInstanceOf,
                    target,
                    argument,
                );
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
        let arg = self.type_operator_operand(argument)?;
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

    /// `argument is target`, or `!is` when `negated`, built by the type-test operations checked
    /// FIR lowering uses from the nullability of the lowered operand's type and of the target.
    ///
    /// A reified type parameter is tested as written, nullability included. Its bound may admit
    /// `null` (`T : Any?`) while a call instantiates it with a non-null type, and `null is String`
    /// is false: expanding that parameter here into `tmp == null || tmp is T` would accept the
    /// `null`. The call-site substitution names the real target. A prepared inline template records
    /// those parameters on its header; an ordinary body declares none, because a reified
    /// declaration declines before its body is lowered.
    ///
    /// Any other non-null target is tested as written too: a KLIB carries no erased-upper-bound
    /// plan, so `s is String` for `s: String?` is an instance test, which answers what checked
    /// FIR's `s != null` does.
    fn type_test(&mut self, negated: bool, target: Ty, argument: KlibIrExprId) -> Lowered<ExprId> {
        let arg = self.type_operator_operand(argument)?;
        let test = if self.reified_type_parameter_target(target) || !admits_null_at_runtime(target)
        {
            self.ir.add_instance_check(negated, arg, target)
        } else {
            let operand_type = self.lowered_type(arg);
            let locals = &mut self.locals;
            self.ir
                .add_nullable_target_type_test(negated, arg, operand_type, target, || {
                    locals.anonymous_slot()
                })
        };
        Ok(self.typed(test, Ty::Boolean))
    }

    /// Whether `target` (or `target?`) names a reified type parameter this body keeps unsubstituted.
    fn reified_type_parameter_target(&self, target: Ty) -> bool {
        let TyKind::TyParam(identity, _) = target.non_null().kind() else {
            return false;
        };
        let name = identity.as_str();
        self.reified_parameters
            .iter()
            .any(|parameter| parameter == name)
    }

    /// The lowered operand `argument` of a type operator.
    fn type_operator_operand(&mut self, argument: KlibIrExprId) -> Lowered<ExprId> {
        let arg = self.expression(argument)?;
        if self.lowered_type(arg) == Ty::Unit {
            // Checked FIR lowering first gives a `Unit` operand a value of its own.
            return unsupported("a type operator on a `Unit` value");
        }
        Ok(arg)
    }

    /// An implicit cast to `target`. One to the value's own type converts nothing and adds no
    /// node, and neither does one of `null` to a nullable type. One to the value's nullable type
    /// is a widening checked FIR lowering marks as a coercion, and one to another supertype (by
    /// the calling compilation's subtyping) is the value itself, as in the source. An implicit
    /// cast to its non-null type, to a subtype, or to another parameterization of its classifier
    /// is a smart cast the declaring compilation proved. It converts as the checker converts the
    /// same pair of types ([`value_conversion`]): a narrowing to a non-null reference type is a
    /// cast, and one to a nullable or scalar type a coercion (`element: T` used as
    /// `CharSequence?` or `Char`). Any other implicit cast declines.
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
        if !to_non_null && !self.narrows(actual, target) {
            return Err(KlibBodyDeclineReason::ImplicitConversion);
        }
        // The checker's decision for the same pair of types: a use as a nullable type is a
        // widening coercion, one as a scalar type a coercion, and a reference narrowing a cast.
        let op = match value_conversion(actual, target) {
            Some(ValueConversion::NullabilityWidening) => IrTypeOp::ImplicitCoercion,
            Some(ValueConversion::SmartCast | ValueConversion::ConstituentView) | None => {
                IrTypeOp::smart_cast_to(target)
            }
            Some(ValueConversion::CoerceToUnit | ValueConversion::NumericWidening) => {
                return Err(KlibBodyDeclineReason::ImplicitConversion);
            }
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
    /// arguments. A value of a type parameter is narrowed by whatever narrows its upper bound
    /// (`element: T` after `element is CharSequence?` or `element is Char`).
    fn narrows(&self, actual: Ty, target: Ty) -> bool {
        let classifier = |ty: Ty| match ty.non_null().kind() {
            TyKind::Obj(name, _) => Some(name),
            _ => None,
        };
        let parameters = self.header.type_parameters();
        self.linker.is_subtype(parameters, target, actual)
            || actual
                .non_null()
                .ty_param_bound()
                .is_some_and(|bound| self.narrows(bound, target))
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
