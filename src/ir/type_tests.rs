//! The runtime shapes Kotlin's `is` expands into, built once for every common-IR producer.
//!
//! Checked FIR lowering and KLIB body lowering both lower `is`. Each decides which test applies
//! from the facts it owns. The IR they produce for that test is built here, so the two cannot
//! drift apart.

use super::{ExprId, IrBinOp, IrBindingStability, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// The language-defined answer for an instance check that does not depend on the operand.
pub(crate) fn settled_instance_answer(target: Ty) -> Option<bool> {
    (target.non_null().canonical_semantic() == Ty::Nothing).then_some(false)
}

/// kotlin IR's `IrType.isNullable()`: a nullable or flexible occurrence, or a type parameter whose
/// bound admits `null`. A definitely-non-null `T & Any` does not.
pub(crate) fn admits_null_at_runtime(ty: Ty) -> bool {
    ty.admits_null() || (ty.is_ty_param() && ty.upper_bound_admits_null())
}

/// Where a tested operand is read from twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestedOperand {
    /// The operand is a stable read of this slot, tested in place.
    Stable(u32),
    /// The operand is evaluated once into this fresh temporary slot.
    Temporary(u32),
}

impl IrFile {
    /// The slot a lowered `operand` reads, when it is a read of a stable binding.
    pub(crate) fn stable_value_read(&self, operand: ExprId) -> Option<u32> {
        match self.expr(operand) {
            IrExpr::GetValue(slot)
                if self.binding_read_stability.get(&operand)
                    == Some(&IrBindingStability::Stable) =>
            {
                Some(*slot)
            }
            _ => None,
        }
    }

    /// `operand is target` (or `!is`), with the one case the language settles folded: `Nothing`
    /// has no instances. The operand is still evaluated.
    pub(crate) fn instance_check(&mut self, negated: bool, operand: ExprId, target: Ty) -> ExprId {
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

    /// kotlinc's `irLetS(operand) { tmp -> tmp == null || tmp is target }`, for a nullable
    /// operand tested against a nullable target. `target` is the non-null classifier tested.
    pub(crate) fn null_or_instance(
        &mut self,
        operand: ExprId,
        target: Ty,
        tested: TestedOperand,
    ) -> ExprId {
        let (declaration, slot) = match tested {
            TestedOperand::Stable(slot) => (None, slot),
            TestedOperand::Temporary(slot) => {
                let declaration = self.add_expr(IrExpr::Variable {
                    index: slot,
                    ty: Ty::nullable(Ty::obj("kotlin/Any")),
                    init: Some(operand),
                    named: false,
                });
                self.safe_cast_temporaries.insert(declaration);
                (Some(declaration), slot)
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
        let instance_test = self.instance_check(false, instance_read, target);
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
