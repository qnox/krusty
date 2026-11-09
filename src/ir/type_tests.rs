//! Runtime type tests (`is`, `!is`) in the shape kotlinc's `TypeOperatorLowering.lowerInstanceOf`
//! gives them.
//!
//! Checked FIR lowering and KLIB dependency body lowering both produce these nodes, so the
//! expansion of a test against a type that admits `null` (`tmp == null || tmp is T`) and the
//! language-settled answer for `Nothing` are built here once. Each producer decides only which
//! test applies from the checked operand and target types; the node shape is common.

use super::{ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// The language-defined answer for an instance check which does not depend on the operand.
pub(crate) fn settled_instance_answer(target: Ty) -> Option<bool> {
    (target.non_null().canonical_semantic() == Ty::Nothing).then_some(false)
}

/// kotlin IR's `IrType.isNullable()`: a nullable or flexible occurrence, or a type parameter whose
/// bound admits `null`. A definitely-non-null `T & Any` does not.
pub(crate) fn admits_null_at_runtime(ty: Ty) -> bool {
    ty.admits_null() || (ty.is_ty_param() && ty.upper_bound_admits_null())
}

impl IrFile {
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
    /// `x is Nothing?` reaches this through [`Self::add_nullable_target_type_test`], so it becomes
    /// `x == null || false` — which is what `Nothing?`, the type of `null` and of nothing else,
    /// means. Safe casts consume the same settled answer before constructing their generic guard
    /// and directly yield `null`, which is the same fact seen from the other side.
    pub fn add_instance_check(&mut self, negated: bool, operand: ExprId, target: Ty) -> ExprId {
        if let Some(answer) = settled_instance_answer(target) {
            let answer = self.add_expr(IrExpr::Const(IrConst::Boolean(answer != negated)));
            let block = self.add_expr(IrExpr::Block {
                stmts: vec![operand],
                value: Some(answer),
            });
            // This synthesized value-bearing block has no source node whose finalization can
            // attach the result of the settled semantic check.
            self.set_logical_type(block, Ty::Boolean);
            return block;
        }
        let op = if negated {
            IrTypeOp::NotInstanceOf
        } else {
            IrTypeOp::InstanceOf
        };
        self.add_expr(IrExpr::TypeOp {
            op,
            arg: operand,
            type_operand: target,
        })
    }

    /// `operand is target` (or `!is`) for a `target` that admits `null`, where `operand` has the
    /// checked type `operand_type`:
    ///
    /// - an operand that admits `null` is bound once (`irLetS`) and becomes
    ///   `tmp == null || tmp is T`, negated as a whole for `!is`;
    /// - any other operand tests the non-null target, so `x is String?` for a non-null `x` is
    ///   `x is String`.
    ///
    /// `temporary` supplies the slot of the `Any?` temporary an operand that is not already a
    /// stable read is evaluated into; it is called only then.
    pub fn add_nullable_target_type_test(
        &mut self,
        negated: bool,
        operand: ExprId,
        operand_type: Ty,
        target: Ty,
        temporary: impl FnOnce() -> u32,
    ) -> ExprId {
        if !admits_null_at_runtime(operand_type) {
            return self.add_instance_check(negated, operand, target.non_null());
        }
        let test = self.add_null_or_instance(operand, target.non_null(), temporary);
        if negated {
            self.add_negation(test)
        } else {
            test
        }
    }

    /// `irLetS(operand) { tmp -> tmp == null || tmp is target }`: a stable read is tested in place,
    /// any other operand is evaluated once into an `Any?` temporary.
    fn add_null_or_instance(
        &mut self,
        operand: ExprId,
        target: Ty,
        temporary: impl FnOnce() -> u32,
    ) -> ExprId {
        let (declaration, slot) = match self.stable_value_read(operand) {
            Some(slot) => (None, slot),
            None => {
                let temporary = temporary();
                let declaration = self.add_expr(IrExpr::Variable {
                    index: temporary,
                    ty: Ty::nullable(Ty::obj("kotlin/Any")),
                    init: Some(operand),
                    named: false,
                });
                self.safe_cast_temporaries.insert(declaration);
                (Some(declaration), temporary)
            }
        };
        let nullable_read = self.add_expr(IrExpr::GetValue(slot));
        let null = self.add_expr(IrExpr::Const(IrConst::Null));
        let null_test = self.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::RefEq,
            lhs: nullable_read,
            rhs: null,
        });
        let instance_read = self.add_expr(IrExpr::GetValue(slot));
        let instance_test = self.add_instance_check(false, instance_read, target);
        // `JvmBackendContext.oror` is a plain `when`, not a source `||`: its true branch is a
        // constant and its else branch the instance test's value.
        let true_value = self.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let test = self.add_expr(IrExpr::When {
            branches: vec![(Some(null_test), true_value), (None, instance_test)],
        });
        self.set_logical_type(test, Ty::Boolean);
        match declaration {
            Some(declaration) => {
                let block = self.add_expr(IrExpr::Block {
                    stmts: vec![declaration],
                    value: Some(test),
                });
                self.set_logical_type(block, Ty::Boolean);
                block
            }
            None => test,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::settled_instance_answer;
    use crate::types::Ty;

    #[test]
    fn the_settled_answer_normalizes_both_nothing_representations() {
        assert_eq!(settled_instance_answer(Ty::Nothing), Some(false));
        assert_eq!(
            settled_instance_answer(Ty::obj("kotlin/Nothing")),
            Some(false)
        );
    }
}
