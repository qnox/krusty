//! Lower checked type operations whose semantics require control flow.

use crate::fir::FirExprId;
use crate::ir::{ExprId, IrConst, IrExpr, IrTypeOp};
use crate::types::{stored_value_ty, Ty};

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    /// `operand is target` (or `!is`), with the one case the LANGUAGE settles folded.
    ///
    /// `Nothing` has no instances, so no value is one and the answer does not depend on the
    /// operand. There is also no class to test against — `kotlin.Nothing` is uninstantiable by
    /// construction — so a backend asking the question has nothing to ask about, and the JVM
    /// emitter answered it by erasing the target to `java/lang/Object`, which every non-null value
    /// passes. The rule is Kotlin's rather than a target's, so it is settled here, once.
    ///
    /// The operand is still EVALUATED. A settled check folds the answer, not the expression, and an
    /// operand may have effects.
    ///
    /// The target is normalized first. `Nothing` reaches here both as `Ty::Nothing` and as
    /// `Obj("kotlin/Nothing")` depending on where it was written, and matching one spelling would
    /// answer the rule for some programs and not others — so `canonical_semantic` decides it once
    /// rather than this becoming another site that lists both.
    ///
    /// `x is Nothing?` reaches this through the null-or-instance expansion in `expression`, so it
    /// becomes `x == null || false` — which is what `Nothing?`, the type of `null` and of nothing
    /// else, means. `x as? Nothing` reaches it through the guard below and yields `null`, which is
    /// the same fact seen from the other side.
    pub(super) fn instance_check(
        &mut self,
        negated: bool,
        operand: crate::ir::ExprId,
        target: crate::types::Ty,
    ) -> crate::ir::ExprId {
        if target.non_null().canonical_semantic() == crate::types::Ty::Nothing {
            let answer = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(negated)));
            return self.ir.add_expr(IrExpr::Block {
                stmts: vec![operand],
                value: Some(answer),
            });
        }
        let op = if negated {
            IrTypeOp::NotInstanceOf
        } else {
            IrTypeOp::InstanceOf
        };
        self.ir.add_expr(IrExpr::TypeOp {
            op,
            arg: operand,
            type_operand: target,
        })
    }

    /// Lower `value as? T` to one evaluation followed by an `is T` guard, a checked cast on the
    /// successful branch, and `null` otherwise. `IrTypeOp::SafeCast` cannot be a backend no-op: the
    /// guarded shape is language semantics and therefore belongs in common lowering.
    pub(super) fn safe_cast_expression(
        &mut self,
        operand: FirExprId,
        target: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let operand_type = self
            .body
            .expr(operand)
            .ok_or(FirLoweringFailure::MissingExpression(operand))?
            .ty
            .get();
        let mut value = self.expression(operand)?;
        let storage_type = if operand_type == Ty::Unit {
            let unit = self.ir.add_expr(IrExpr::UnitInstance);
            value = self.ir.add_expr(IrExpr::Block {
                stmts: vec![value],
                value: Some(unit),
            });
            Ty::obj("kotlin/Unit")
        } else if operand_type.scalar_value_repr().is_some() {
            let any = Ty::obj("kotlin/Any");
            value = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: any,
            });
            any
        } else {
            stored_value_ty(operand_type)
        };

        // A safe cast to a primitive tests and retains its nullable wrapper. Reference targets and
        // erased type parameters already have a reference representation of their own.
        let target = target.non_null();
        let runtime_target = if target == Ty::Unit {
            Ty::obj("kotlin/Unit")
        } else {
            target.nullable_boxed().unwrap_or(target)
        };
        let temporary = self.allocate_temporary();
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: temporary,
            ty: storage_type,
            init: Some(value),
            named: false,
        });
        let read = self.ir.add_expr(IrExpr::GetValue(temporary));
        let matches = self.instance_check(false, read, runtime_target);
        let read = self.ir.add_expr(IrExpr::GetValue(temporary));
        let cast = self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: read,
            type_operand: runtime_target,
        });
        let null = self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let result = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(matches), cast), (None, null)],
        });
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: vec![declaration],
            value: Some(result),
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::fir::{
        BodyOwnerId, FirBody, FirExpr, FirExprKind, FirStatement, FirStatementKind,
        FirTypeOperation, OriginId, ResolvedModuleIndex, ResolvedTy,
    };
    use crate::ir::{IrConst, IrExpr, IrFile};
    use crate::types::Ty;

    use super::super::lower_body;

    fn resolved(ty: Ty) -> ResolvedTy {
        ResolvedTy::new(ty).unwrap()
    }

    /// Lower `<a literal> is/!is Nothing` as the only root of a body, and answer the `IrExpr` the
    /// root produced together with the file it was built in.
    fn lowered_nothing_check(operation: FirTypeOperation) -> (IrFile, IrExpr) {
        let origin = OriginId::from_raw(0);
        let mut body = FirBody::new(BodyOwnerId::from_raw(1));
        let operand = body.add_expr(FirExpr {
            origin,
            ty: resolved(Ty::Int),
            kind: FirExprKind::Constant(crate::fir::FirConstant::Int(7)),
        });
        let check = body.add_expr(FirExpr {
            origin,
            ty: resolved(Ty::Boolean),
            kind: FirExprKind::TypeOperation {
                operation,
                operand,
                target: resolved(Ty::Nothing),
            },
        });
        let statement = body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(check),
        });
        body.push_root(statement);

        let mut ir = IrFile::default();
        let root = lower_body(body, &ResolvedModuleIndex::default(), &mut ir)
            .expect("a settled check lowers");
        let produced = ir.expr(*root.roots.last().expect("one root")).clone();
        (ir, produced)
    }

    /// A safe cast to `Nothing` guards on the SAME settled answer, which is what makes it yield
    /// `null` for every value. Its guard is a separate construction from the `is` above — it had
    /// its own `InstanceOf` until this change — so the `is` tests alone would not have held it.
    #[test]
    fn a_safe_cast_to_nothing_guards_on_the_settled_false() {
        let origin = OriginId::from_raw(0);
        let mut body = FirBody::new(BodyOwnerId::from_raw(1));
        let operand = body.add_expr(FirExpr {
            origin,
            ty: resolved(Ty::obj("kotlin/Any")),
            kind: FirExprKind::Constant(crate::fir::FirConstant::Int(7)),
        });
        let cast = body.add_expr(FirExpr {
            origin,
            ty: resolved(Ty::nullable(Ty::Nothing)),
            kind: FirExprKind::TypeOperation {
                operation: FirTypeOperation::SafeCast,
                operand,
                target: resolved(Ty::Nothing),
            },
        });
        let statement = body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(cast),
        });
        body.push_root(statement);

        let mut ir = IrFile::default();
        let root = lower_body(body, &ResolvedModuleIndex::default(), &mut ir)
            .expect("a safe cast to Nothing lowers");
        let produced = ir.expr(*root.roots.last().expect("one root")).clone();

        // `Block { the temporary, When { [(guard, cast), (_, null)] } }` — reach the guard.
        let IrExpr::Block { value, .. } = produced else {
            panic!("a safe cast lowers to a block, got {produced:?}");
        };
        let value = value.expect("a safe cast produces a value");
        let IrExpr::When { branches } = ir.expr(value) else {
            panic!("a safe cast lowers to a `when`, got {:?}", ir.expr(value));
        };
        let guard = branches
            .first()
            .and_then(|(guard, _)| *guard)
            .expect("the first branch is guarded");
        let IrExpr::Block { value: answer, .. } = ir.expr(guard) else {
            panic!("the guard is the settled block, got {:?}", ir.expr(guard));
        };
        let answer = answer.expect("the settled guard produces a value");
        assert!(
            matches!(ir.expr(answer), IrExpr::Const(IrConst::Boolean(false))),
            "the guard is a settled `false`, got {:?}",
            ir.expr(answer)
        );
    }

    /// The shape, not just the answer: a settled check folds the ANSWER and keeps the operand as an
    /// evaluated statement. Asserting only the `false` would pass for a lowering that dropped the
    /// operand, and an operand with effects would then stop running.
    #[test]
    fn a_settled_nothing_check_keeps_its_operand_and_answers_a_constant() {
        for (operation, expected) in [
            (FirTypeOperation::Is, false),
            (FirTypeOperation::NotIs, true),
        ] {
            let (ir, produced) = lowered_nothing_check(operation);
            let IrExpr::Block { stmts, value } = produced else {
                panic!("{operation:?}: a settled check lowers to a block, got {produced:?}");
            };
            assert_eq!(
                stmts.len(),
                1,
                "{operation:?}: the operand is the statement"
            );
            assert!(
                matches!(ir.expr(stmts[0]), IrExpr::Const(IrConst::Int(7))),
                "{operation:?}: the operand is still evaluated, got {:?}",
                ir.expr(stmts[0])
            );
            let value = value.expect("a settled check produces a value");
            assert!(
                matches!(ir.expr(value), IrExpr::Const(IrConst::Boolean(answer)) if *answer == expected),
                "{operation:?}: the answer is the exact Boolean, got {:?}",
                ir.expr(value)
            );
        }
    }
}
