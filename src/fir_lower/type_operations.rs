//! Lower checked type operations whose semantics require control flow.

use crate::fir::FirExprId;
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrTypeOp};
use crate::types::{stored_value_ty, Ty};

use super::{BodyLowering, FirLoweringFailure};

/// The language-defined answer for an instance check which does not depend on the operand.
fn settled_instance_answer(target: Ty) -> Option<bool> {
    (target.non_null().canonical_semantic() == Ty::Nothing).then_some(false)
}

/// kotlin IR's `IrType.isNullable()`: a nullable or flexible occurrence, or a type parameter whose
/// bound admits `null`. A definitely-non-null `T & Any` does not.
fn admits_null_at_runtime(ty: Ty) -> bool {
    ty.admits_null() || (ty.is_ty_param() && ty.upper_bound_admits_null())
}

/// kotlinc's `erasedUpperBound`: the classifier a type stands for once erased, through type
/// parameter bounds and nullability. A function type stands for its `FunctionN` classifier.
fn erased_upper_bound(ty: Ty) -> Option<Ty> {
    let mut current = ty.non_null();
    let mut seen = std::collections::HashSet::new();
    loop {
        match current {
            Ty::TyParam(name, bound) if seen.insert(name) => current = bound.non_null(),
            Ty::DefinitelyNotNull(inner) => current = inner.non_null(),
            Ty::Obj(name, _) => return Some(Ty::obj_name(name).canonical_semantic()),
            Ty::Fun(signature) => {
                return Some(Ty::obj_name(
                    crate::libraries::function_classifiers::function_classifier_identity(
                        signature.params.len(),
                        signature.suspend,
                    ),
                ))
            }
            Ty::TyParam(..)
            | Ty::Nullable(_)
            | Ty::PlatformNullable(_)
            | Ty::Null
            | Ty::Nothing
            | Ty::Error
            | Ty::Pending
            | Ty::InProjection(_)
            | Ty::OutProjection(_)
            | Ty::StarProjection(_)
            | Ty::Intersection(_) => return None,
            builtin => return Some(builtin),
        }
    }
}

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
        if let Some(answer) = settled_instance_answer(target) {
            let answer = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(if negated {
                !answer
            } else {
                answer
            })));
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
    ) -> ExprId {
        if self.is_reified_type_parameter(target) {
            return self.instance_check(negated, operand, target);
        }
        if admits_null_at_runtime(operand_type) && target.admits_null() {
            let test = self.null_or_instance(operand, target.non_null());
            return if negated {
                self.ir.add_negation(test)
            } else {
                test
            };
        }
        if admits_null_at_runtime(operand_type)
            && erased_upper_bound(operand_type)
                .is_some_and(|bound| Some(bound) == erased_upper_bound(target))
        {
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

    /// `irLetS(operand) { tmp -> tmp == null || tmp is target }`: a stable read is tested in place,
    /// any other operand is evaluated once into an `Any?` temporary.
    fn null_or_instance(&mut self, operand: ExprId, target: Ty) -> ExprId {
        let (declaration, slot) = match self.stable_value_read(operand) {
            Some(slot) => (None, slot),
            None => {
                let temporary = self.allocate_temporary();
                let declaration = self.ir.add_expr(IrExpr::Variable {
                    index: temporary,
                    ty: Ty::nullable(Ty::obj("kotlin/Any")),
                    init: Some(operand),
                    named: false,
                });
                (Some(declaration), temporary)
            }
        };
        let nullable_read = self.ir.add_expr(IrExpr::GetValue(slot));
        let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let null_test = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::RefEq,
            lhs: nullable_read,
            rhs: null,
        });
        let instance_read = self.ir.add_expr(IrExpr::GetValue(slot));
        let instance_test = self.instance_check(false, instance_read, target);
        // `JvmBackendContext.oror` is a plain `when`, not a source `||`: its true branch is a
        // constant and its else branch the instance test's value.
        let true_value = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let test = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(null_test), true_value), (None, instance_test)],
        });
        match declaration {
            Some(declaration) => self.ir.add_expr(IrExpr::Block {
                stmts: vec![declaration],
                value: Some(test),
            }),
            None => test,
        }
    }

    /// Whether `target` (or `target?`) is a reified type parameter, whose test an inline call site
    /// specializes and therefore keeps as written.
    fn is_reified_type_parameter(&self, target: Ty) -> bool {
        let Ty::TyParam(identity, _) = target.non_null() else {
            return false;
        };
        self.index
            .type_parameter_by_semantic_name(identity)
            .and_then(|parameter| self.index.type_parameter_header(parameter))
            .is_some_and(|header| header.flags.is_reified())
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
        // A safe cast to either `Nothing` or `Nothing?` can never produce a non-null value. Fold
        // the whole cast here, while the semantic target is still available: retaining the generic
        // guarded `when` would leave an unreachable cast branch whose JVM stack type (`Object`)
        // still has to verify against the null-only result branch. The operand remains a statement,
        // so its effects and divergence are preserved exactly once.
        if settled_instance_answer(target) == Some(false) {
            let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
            return Ok(self.ir.add_expr(IrExpr::Block {
                stmts: vec![value],
                value: Some(null),
            }));
        }
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
        // kotlinc's `irLetS`: a read of an immutable binding is tested and cast in place; any other
        // operand is evaluated once into a temporary.
        let (declaration, operand_slot) = match self.stable_value_read(value) {
            Some(slot) => (None, slot),
            None => {
                let temporary = self.allocate_temporary();
                let declaration = self.ir.add_expr(IrExpr::Variable {
                    index: temporary,
                    ty: storage_type,
                    init: Some(value),
                    named: false,
                });
                (Some(declaration), temporary)
            }
        };
        let read = self.ir.add_expr(IrExpr::GetValue(operand_slot));
        let matches = self.instance_check(false, read, runtime_target);
        let read = self.ir.add_expr(IrExpr::GetValue(operand_slot));
        // `as? T` yields `T?`. A concrete primitive already names its boxed wrapper (`Int?`).
        // A type parameter does not: an inline call later substitutes `T` with `Int`, and a
        // success cast recorded as bare `T` unboxes that match and then unboxes the `null`
        // mismatch. Recording `T?` specializes to `Int?`, which stays a boxed reference.
        let cast_target = if runtime_target.is_ty_param() {
            Ty::nullable(runtime_target)
        } else {
            runtime_target
        };
        let cast = self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: read,
            type_operand: cast_target,
        });
        let null = self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let result = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(matches), cast), (None, null)],
        });
        Ok(match declaration {
            Some(declaration) => self.ir.add_expr(IrExpr::Block {
                stmts: vec![declaration],
                value: Some(result),
            }),
            None => result,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::fir::{
        BodyOwnerId, FirBody, FirExpr, FirExprKind, FirStatement, FirStatementKind,
        FirTypeOperation, OriginId, ResolvedModuleIndex, ResolvedTy,
    };
    use crate::ir::{IrConst, IrExpr, IrFile, IrTypeOp};
    use crate::types::Ty;

    use super::super::lower_body;
    use super::settled_instance_answer;

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
