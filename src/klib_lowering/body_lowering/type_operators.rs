//! Type operators: `is`, `!is`, `as` and the implicit casts a KLIB states explicitly.
//!
//! Each lowers to the type operation checked FIR lowering produces for the source form, where
//! that is one operation over the lowered operand, or to the shared shape of an `is` of a nullable
//! type or of an `as?`. A form checked FIR lowering settles without a test (`is Nothing`), or
//! derives from facts a KLIB does not carry (a platform type's implicit non-null assertion names
//! the call that produced the value) declines by its name.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr, IrTypeOp};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrType, KlibIrTypeId, KlibIrTypeOperator};
use crate::metadata::klib_ir::KlibIrSymbolKind;
use crate::types::{Ty, TyKind};

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
        self.install_operand_interface(operand)?;
        let target = self.semantic(operand)?;
        let op = match operator {
            KlibIrTypeOperator::SamConversion => return self.sam_conversion(operand, argument, ty),
            KlibIrTypeOperator::ImplicitCast => return self.implicit_cast(target, argument, ty),
            KlibIrTypeOperator::SafeCast => return self.safe_cast(target, argument, ty),
            KlibIrTypeOperator::InstanceOf | KlibIrTypeOperator::NotInstanceOf => {
                if ty != Ty::Boolean {
                    return Err(mismatch("an `is` check is not typed `Boolean`"));
                }
                if target.is_nullable() {
                    return self.nullable_type_test(
                        operator == KlibIrTypeOperator::NotInstanceOf,
                        target,
                        argument,
                    );
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
                IrTypeOp::cast_to(target)
            }
            other => return unsupported(type_operator_name(other)),
        };
        let arg = self.expression(argument)?;
        if self.lowered_type(arg) == Ty::Unit {
            // Checked FIR lowering first gives a `Unit` operand a value of its own.
            return unsupported("a type operator on a `Unit` value");
        }
        let lowered = self.ir.add_expr(
            IrExpr::type_op(op, arg, target).with_erased(
                (op != IrTypeOp::InstanceOf && op != IrTypeOp::NotInstanceOf)
                    .then(|| target.cast_erasure())
                    .flatten(),
            ),
        );
        if operator == KlibIrTypeOperator::Cast {
            self.ir.written_casts.insert(lowered);
        }
        Ok(self.typed(lowered, ty))
    }

    /// Install the non-public interface `operand` names, if it names one, so the test or cast of
    /// it has a class to name and its supertypes are known to the narrowing it permits.
    fn install_operand_interface(&mut self, operand: KlibIrTypeId) -> Lowered<()> {
        let KlibIrType::Simple { classifier, .. } = self.arena.ty(operand) else {
            return Ok(());
        };
        if classifier.kind != KlibIrSymbolKind::Class
            || self.linker.local_class(&classifier.signature)?.is_none()
            || !self.linker.is_local_interface(&classifier.signature)
        {
            return Ok(());
        }
        self.linker
            .link_local_interface(self.ir, &classifier.signature)
    }

    /// `argument is target` (or `!is`) for a nullable `target`, through the test checked FIR
    /// lowering chooses for it ([`crate::ir::IrFile::nullable_target_type_test`]). This lowering owns only
    /// its own declines: a nullable type-parameter target names a reified parameter only in a
    /// declaration this lowering does not prepare, and a `Unit` operand has no value of its own.
    fn nullable_type_test(
        &mut self,
        negated: bool,
        target: Ty,
        argument: KlibIrExprId,
    ) -> Lowered<ExprId> {
        if target.non_null().is_ty_param() {
            return unsupported("an `is` check of a nullable type parameter");
        }
        let arg = self.expression(argument)?;
        let actual = self.lowered_type(arg);
        if actual == Ty::Unit {
            // Checked FIR lowering first gives a `Unit` operand a value of its own.
            return unsupported("a type operator on a `Unit` value");
        }
        let locals = &mut self.locals;
        let test = self
            .ir
            .nullable_target_type_test(negated, arg, actual, target, || locals.anonymous_slot());
        Ok(self.typed(test, Ty::Boolean))
    }

    /// `argument as? target`, typed `ty` (`target?`), through the guarded shape checked FIR
    /// lowering builds for the source form ([`crate::ir::IrFile::safe_cast`]).
    fn safe_cast(&mut self, target: Ty, argument: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if ty != Ty::nullable(target) {
            return Err(mismatch(
                "a safe cast is typed apart from its nullable target",
            ));
        }
        let arg = self.expression(argument)?;
        let actual = self.lowered_type(arg);
        let locals = &mut self.locals;
        let cast = self
            .ir
            .safe_cast(arg, actual, target, target.cast_erasure(), || {
                locals.anonymous_slot()
            });
        Ok(self.typed(cast, ty))
    }

    /// An implicit cast to `target`. One to the value's own type converts nothing and adds no
    /// node, and neither does one of `null` to a nullable type. One to the value's nullable type
    /// is a widening checked FIR lowering marks as a coercion, and one to another supertype (by
    /// the calling compilation's subtyping) is the value itself, as in the source. An implicit
    /// cast of a reference to a subtype, or to another parameterization of its classifier, is a
    /// smart cast the declaring compilation proved, which checked FIR lowering lowers as a cast
    /// to a non-null reference type and as a coercion to a nullable or primitive one: a counted
    /// loop's inlined `Any?` element read back as `Int` is such a coercion. One of a primitive
    /// value from its nullable type is a coercion. One of a value to another application of a
    /// supertype classifier of it is the upcast to the classifier. Any other implicit cast
    /// declines.
    fn implicit_cast(&mut self, target: Ty, argument: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if target != ty {
            return Err(mismatch("an implicit cast is typed apart from its target"));
        }
        if let Some(proven) = self.null_proven_receiver(argument, target)? {
            return Ok(proven);
        }
        let arg = self.expression(argument)?;
        self.implicit_cast_of(arg, target, ty)
    }

    /// The lowered `arg` implicitly cast to `target`, typed `ty`; see [`Self::implicit_cast`].
    pub(super) fn implicit_cast_of(&mut self, arg: ExprId, target: Ty, ty: Ty) -> Lowered<ExprId> {
        let actual = self.lowered_type(arg);
        if super::is_value_of(actual, target) {
            return Ok(arg);
        }
        if let Some(widened) = self.nullable_widening(arg, actual, target) {
            return Ok(widened);
        }
        if let Some(upcast) = self.upcast(arg, actual, target) {
            return Ok(upcast);
        }
        let to_non_null = !target.is_nullable() && Ty::nullable(target) == actual;
        let op = if to_non_null {
            if target.is_reference() && actual.is_reference() {
                IrTypeOp::Cast
            } else {
                IrTypeOp::ImplicitCoercion
            }
        } else if actual.is_reference() && self.narrows(actual, target) {
            if target.is_reference() && !target.is_nullable() {
                IrTypeOp::Cast
            } else {
                IrTypeOp::ImplicitCoercion
            }
        } else {
            return self
                .retyped_supertype_arguments(arg, actual, target)
                .ok_or(KlibBodyDeclineReason::ImplicitConversion);
        };
        let cast = self.ir.add_expr(IrExpr::type_op(op, arg, target));
        Ok(self.typed(cast, ty))
    }
}

impl BodyLowering<'_> {
    /// Whether a smart cast of a value of `actual` to `target` narrows it: `target` is a subtype
    /// of `actual`, of `actual`'s classifier at any type arguments (`Collection<*>` after
    /// `this is Collection<*>` on an `Iterable<T>`), or names the same classifier at other type
    /// arguments. A value of a type parameter narrows to any type: its argument can be any
    /// subtype of its bounds. So does a value of a function type, an interface any class can
    /// implement besides its own supertypes (`this is BaseContinuationImpl` on a
    /// `suspend () -> T`). A value of an anonymous or local class this body lowered narrows as a
    /// value of one of its supertypes does: a prepared copy types the object an inlined call
    /// expanded with the erasure of the inline function's type parameters
    /// (`Sequence<Any?>` for `Sequence { }`), which the call's result then casts back to its
    /// declared type (`Sequence<T>`).
    fn narrows(&self, actual: Ty, target: Ty) -> bool {
        if matches!(
            actual.non_null().kind(),
            TyKind::TyParam(..) | TyKind::Fun(..)
        ) {
            return true;
        }
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
            || self.local_class(actual).is_some_and(|class| {
                class
                    .supertypes
                    .iter()
                    .any(|supertype| self.narrows(*supertype, target))
            })
    }

    /// `arg`, of the type `actual`, as a value of `target`, a type of one of its supertype
    /// classifiers that applies other type arguments than `actual`'s hierarchy does: the cast
    /// the declaring compilation proved (`Int` as `Comparable<Any?>`, a use of the contravariant
    /// `Comparable` through a wider argument). Type arguments are erased, so it is as the upcast
    /// to the classifier is: the value itself, or the coercion that boxes or unboxes it.
    fn retyped_supertype_arguments(
        &mut self,
        arg: ExprId,
        actual: Ty,
        target: Ty,
    ) -> Option<ExprId> {
        let supertype = Self::star_projected(target);
        if !self.is_supertype_of(supertype, actual) {
            return None;
        }
        Some(
            if actual.is_nullable() == target.is_nullable()
                && actual.is_reference() == target.is_reference()
            {
                arg
            } else {
                let coerced = self.coercion(arg, target);
                self.typed(coerced, target)
            },
        )
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
    /// checked FIR lowering places for the target to represent, typed `target`. `None` for any other pair.
    pub(super) fn nullable_widening(
        &mut self,
        value: ExprId,
        actual: Ty,
        target: Ty,
    ) -> Option<ExprId> {
        if actual.is_nullable() || target != Ty::nullable(actual) {
            return None;
        }
        let widened = self.coercion(value, target);
        Some(self.typed(widened, target))
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
