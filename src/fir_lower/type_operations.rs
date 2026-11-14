//! Lower checked type operations whose semantics require control flow.

use crate::fir::FirExprId;
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr};
use crate::types::{Ty, TyKind};

use super::{BodyLowering, FirLoweringFailure};

use crate::ir::admits_null_at_runtime;

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
    /// `x is Nothing?` reaches this through the null-or-instance expansion in [`Self::type_test`],
    /// so it becomes `x == null || false` — which is what `Nothing?`, the type of `null` and of
    /// nothing else, means. Safe casts consume the same settled answer before constructing their generic
    /// guard and directly yield `null`, which is the same fact seen from the other side.
    pub(super) fn instance_check(
        &mut self,
        negated: bool,
        operand: crate::ir::ExprId,
        target: crate::types::Ty,
    ) -> crate::ir::ExprId {
        self.ir.instance_check(negated, operand, target)
    }

    /// `operand is target` (or `!is`) as kotlinc's `TypeOperatorLowering.lowerInstanceOf` shapes
    /// it, from the operand's checked type and the target:
    ///
    /// - a reified type-parameter target is tested as written, nullability included, so its
    ///   inline-site marker names `T?`;
    /// - a nullable operand tested against a nullable target is bound once (`irLetS`) and becomes
    ///   `tmp == null || tmp is T`;
    /// - a nullable operand tested against a non-null target with the same erased upper bound
    ///   (`x is T` for `x: T?`, `s is String` for `s: String?`) is the null check `x != null`;
    /// - anything else tests the non-null target, so `x is String?` for a non-null `x` is
    ///   `x is String`.
    ///
    /// `!is` is the negation of the same test: `!(tmp == null || tmp is T)`, `x == null`, or
    /// `x !is T`.
    pub(super) fn type_test(
        &mut self,
        negated: bool,
        operand: ExprId,
        operand_type: Ty,
        target: Ty,
        same_erased_upper_bound: bool,
    ) -> ExprId {
        if self.is_reified_type_parameter(target) {
            return self.instance_check(negated, operand, target);
        }
        if admits_null_at_runtime(target) {
            let next_temporary = &mut self.next_temporary;
            return self.ir.nullable_target_type_test(
                negated,
                operand,
                operand_type,
                target,
                || {
                    let temporary = *next_temporary;
                    *next_temporary = next_temporary
                        .checked_add(1)
                        .expect("too many FIR lowering temporaries");
                    temporary
                },
            );
        }
        if admits_null_at_runtime(operand_type) && same_erased_upper_bound {
            let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
            return self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: if negated {
                    IrBinOp::RefEq
                } else {
                    IrBinOp::RefNe
                },
                lhs: operand,
                rhs: null,
            });
        }
        self.instance_check(negated, operand, target.non_null())
    }

    /// Whether `target` (or `target?`) is a reified type parameter, whose test an inline call site
    /// specializes and therefore keeps as written.
    fn is_reified_type_parameter(&self, target: Ty) -> bool {
        let TyKind::TyParam(identity, _) = target.non_null().kind() else {
            return false;
        };
        self.index
            .type_parameter_by_semantic_name(identity.as_str())
            .and_then(|parameter| self.index.type_parameter_header(parameter))
            .is_some_and(|header| header.flags.is_reified())
    }

    /// Lower `value as? T` through the shape every common-IR producer shares
    /// ([`crate::ir::IrFile::safe_cast`]).
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
        let value = self.expression(operand)?;
        let next_temporary = &mut self.next_temporary;
        Ok(self.ir.safe_cast(value, operand_type, target, || {
            let temporary = *next_temporary;
            *next_temporary = temporary
                .checked_add(1)
                .expect("too many FIR lowering temporaries");
            temporary
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::fir::{
        BodyOwnerId, FirBody, FirExpr, FirExprKind, FirStatement, FirStatementKind,
        FirTypeOperation, OriginId, ResolvedModuleIndex, ResolvedTy,
    };
    use crate::ir::{IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
    use crate::types::Ty;

    use super::super::lower_body;
    use crate::ir::settled_instance_answer;

    fn resolved(ty: Ty) -> ResolvedTy {
        ResolvedTy::new(ty).unwrap()
    }

    #[test]
    fn the_settled_answer_normalizes_both_nothing_representations() {
        assert_eq!(settled_instance_answer(Ty::Nothing), Some(false));
        assert_eq!(
            settled_instance_answer(Ty::obj("kotlin/Nothing")),
            Some(false)
        );
    }

    /// Lower `<a literal> is/!is Nothing` as the only root of a body, and answer the `IrExpr` the
    /// root produced together with the file it was built in.
    fn collect_type_ops(ir: &IrFile, id: crate::ir::ExprId, operations: &mut Vec<(IrTypeOp, Ty)>) {
        if let IrExpr::TypeOp {
            op, type_operand, ..
        } = ir.expr(id)
        {
            operations.push((*op, *type_operand));
        }
        crate::ir::for_each_child(&ir.exprs, id, &mut |child| {
            collect_type_ops(ir, child, operations);
        });
    }

    fn lowered_nothing_check(operation: FirTypeOperation) -> (IrFile, crate::ir::ExprId) {
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
        let produced = *root.roots.last().expect("one root");
        (ir, produced)
    }

    /// A safe cast to `Nothing` evaluates its operand and directly produces `null`. Retaining the
    /// generic guarded shape would also retain an unreachable checked-cast branch; backends still
    /// have to verify unreachable branch types, so the language-settled cast must disappear here.
    #[test]
    fn a_safe_cast_to_nothing_keeps_its_operand_and_drops_the_unreachable_cast() {
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

        let IrExpr::Block { stmts, value } = produced else {
            panic!("a safe cast lowers to a block, got {produced:?}");
        };
        assert_eq!(stmts.len(), 1, "the operand is evaluated exactly once");
        assert!(
            matches!(ir.expr(stmts[0]), IrExpr::Const(IrConst::Int(7))),
            "the operand remains the statement, got {:?}",
            ir.expr(stmts[0])
        );
        let value = value.expect("a safe cast produces a value");
        assert!(
            matches!(ir.expr(value), IrExpr::Const(IrConst::Null)),
            "the result is the exact null value, got {:?}",
            ir.expr(value)
        );
    }

    /// `as? T` produces `T?`. The success cast must name that nullable type parameter so a later
    /// inline specialization of `T` to a primitive stays on the boxed wrapper.
    #[test]
    fn a_safe_cast_to_a_type_parameter_casts_the_nullable_parameter() {
        let origin = OriginId::from_raw(0);
        let parameter = Ty::ty_param("T@safecast", Ty::obj("kotlin/Any"));
        let mut body = FirBody::new(BodyOwnerId::from_raw(1));
        let operand = body.add_expr(FirExpr {
            origin,
            ty: resolved(Ty::obj("kotlin/Any")),
            kind: FirExprKind::Constant(crate::fir::FirConstant::Int(7)),
        });
        let cast = body.add_expr(FirExpr {
            origin,
            ty: resolved(Ty::nullable(parameter)),
            kind: FirExprKind::TypeOperation {
                operation: FirTypeOperation::SafeCast,
                operand,
                target: resolved(parameter),
            },
        });
        let statement = body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(cast),
        });
        body.push_root(statement);

        let mut ir = IrFile::default();
        let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir)
            .expect("a safe cast to a type parameter lowers");
        let mut operations = Vec::new();
        for &root in &lowered.roots {
            collect_type_ops(&ir, root, &mut operations);
        }

        assert!(
            operations
                .iter()
                .any(|(op, ty)| { *op == IrTypeOp::InstanceOf && *ty == parameter }),
            "the guard tests the non-null parameter, got {operations:?}"
        );
        assert!(
            operations
                .iter()
                .any(|(op, ty)| { *op == IrTypeOp::Cast && *ty == Ty::nullable(parameter) }),
            "the success cast names T?, got {operations:?}"
        );
    }

    #[test]
    fn a_negated_nullable_test_types_each_synthesized_boolean_value() {
        let origin = OriginId::from_raw(0);
        let mut body = FirBody::new(BodyOwnerId::from_raw(1));
        let operand = body.add_expr(FirExpr {
            origin,
            ty: resolved(Ty::nullable(Ty::obj("kotlin/Any"))),
            kind: FirExprKind::Constant(crate::fir::FirConstant::Null),
        });
        let check = body.add_expr(FirExpr {
            origin,
            ty: resolved(Ty::Boolean),
            kind: FirExprKind::TypeOperation {
                operation: FirTypeOperation::NotIs(crate::fir::FirTypeTestPlan {
                    same_erased_upper_bound: false,
                }),
                operand,
                target: resolved(Ty::nullable(Ty::obj("sample/Item"))),
            },
        });
        let statement = body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(check),
        });
        body.push_root(statement);

        let mut ir = IrFile::default();
        let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir)
            .expect("a negated nullable test lowers");
        let negation = *lowered.roots.last().expect("one root");
        let IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: block,
            ..
        } = ir.expr(negation)
        else {
            panic!("the source !is is a negation, got {:?}", ir.expr(negation));
        };
        let IrExpr::Block {
            value: Some(when), ..
        } = ir.expr(*block)
        else {
            panic!(
                "the unstable operand is bound once, got {:?}",
                ir.expr(*block)
            );
        };

        assert_eq!(ir.checked_type(*when), Some(Ty::Boolean));
        assert_eq!(ir.checked_type(*block), Some(Ty::Boolean));
        assert_eq!(ir.checked_type(negation), Some(Ty::Boolean));
    }

    /// The shape, not just the answer: a settled check folds the ANSWER and keeps the operand as an
    /// evaluated statement. Asserting only the `false` would pass for a lowering that dropped the
    /// operand, and an operand with effects would then stop running.
    #[test]
    fn a_settled_nothing_check_keeps_its_operand_and_answers_a_constant() {
        for (operation, expected) in [
            (
                FirTypeOperation::Is(crate::fir::FirTypeTestPlan {
                    same_erased_upper_bound: false,
                }),
                false,
            ),
            (
                FirTypeOperation::NotIs(crate::fir::FirTypeTestPlan {
                    same_erased_upper_bound: false,
                }),
                true,
            ),
        ] {
            let (ir, produced_id) = lowered_nothing_check(operation);
            assert_eq!(ir.checked_type(produced_id), Some(Ty::Boolean));
            let produced = ir.expr(produced_id).clone();
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
