//! The runtime shapes Kotlin's `is` expands into, built once for every common-IR producer.
//!
//! Checked FIR lowering and KLIB body lowering both lower `is`. The test a nullable target takes,
//! and the IR every test produces, are chosen and built here, so the two cannot drift apart. Each
//! caller keeps only the facts it owns, such as reified targets or an erased upper bound.

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
enum TestedOperand {
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

    /// `value as? target`, for a `value` of the checked type `operand_type`: one evaluation followed
    /// by an `is target` guard, a checked cast on the successful branch, and `null` otherwise.
    /// `IrTypeOp::SafeCast` cannot be a backend no-op: the guarded shape is language semantics, so
    /// it is built here for every common-IR producer. A stable read is tested and cast in place;
    /// any other operand is evaluated once into the temporary slot `temporary` allocates, which is
    /// asked for only then.
    pub(crate) fn safe_cast(
        &mut self,
        mut value: ExprId,
        operand_type: Ty,
        target: Ty,
        temporary: impl FnOnce() -> u32,
    ) -> ExprId {
        // A safe cast to either `Nothing` or `Nothing?` can never produce a non-null value. Fold
        // the whole cast here, while the semantic target is still available: retaining the generic
        // guarded `when` would leave an unreachable cast branch whose JVM stack type (`Object`)
        // still has to verify against the null-only result branch. The operand remains a statement,
        // so its effects and divergence are preserved exactly once.
        if settled_instance_answer(target) == Some(false) {
            let null = self.add_expr(IrExpr::Const(IrConst::Null));
            return self.add_expr(IrExpr::Block {
                stmts: vec![value],
                value: Some(null),
            });
        }
        let storage_type = if operand_type == Ty::Unit {
            let unit = self.add_expr(IrExpr::UnitInstance);
            value = self.add_expr(IrExpr::Block {
                stmts: vec![value],
                value: Some(unit),
            });
            Ty::obj("kotlin/Unit")
        } else if operand_type.scalar_value_repr().is_some() {
            let any = Ty::obj("kotlin/Any");
            value = self.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: any,
            });
            any
        } else {
            crate::types::stored_value_ty(operand_type)
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
                let temporary = temporary();
                let declaration = self.add_expr(IrExpr::Variable {
                    index: temporary,
                    ty: storage_type,
                    init: Some(value),
                    named: false,
                });
                self.safe_cast_temporaries.insert(declaration);
                (Some(declaration), temporary)
            }
        };
        let read = self.add_expr(IrExpr::GetValue(operand_slot));
        let matches = self.instance_check(false, read, runtime_target);
        let read = self.add_expr(IrExpr::GetValue(operand_slot));
        // `as? T` yields `T?`. A concrete primitive already names its boxed wrapper (`Int?`).
        // A type parameter does not: an inline call later substitutes `T` with `Int`, and a
        // success cast recorded as bare `T` unboxes that match and then unboxes the `null`
        // mismatch. Recording `T?` specializes to `Int?`, which stays a boxed reference.
        let cast_target = if runtime_target.is_ty_param() {
            Ty::nullable(runtime_target)
        } else {
            runtime_target
        };
        let cast = self.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: read,
            type_operand: cast_target,
        });
        let null = self.add_expr(IrExpr::Const(IrConst::Null));
        let result = self.add_expr(IrExpr::When {
            branches: vec![(Some(matches), cast), (None, null)],
        });
        self.set_logical_type(result, Ty::nullable(cast_target));
        match declaration {
            Some(declaration) => self.add_expr(IrExpr::Block {
                stmts: vec![declaration],
                value: Some(result),
            }),
            None => result,
        }
    }

    /// `operand is target` (or `!is`) for a target that admits `null` and is not a reified type
    /// parameter, as kotlinc's `TypeOperatorLowering.lowerInstanceOf` shapes it from the operand's
    /// type: an operand that admits `null` is bound once (`irLetS`) and becomes
    /// `tmp == null || tmp is T`, negated for `!is`; any other operand tests the non-null target,
    /// so `x is String?` for a non-null `x` is `x is String`.
    ///
    /// A stable read is tested in place. Any other operand is evaluated once into the temporary
    /// slot `temporary` allocates, which is asked for only then.
    pub(crate) fn nullable_target_type_test(
        &mut self,
        negated: bool,
        operand: ExprId,
        operand_type: Ty,
        target: Ty,
        temporary: impl FnOnce() -> u32,
    ) -> ExprId {
        let tested = target.non_null();
        if !admits_null_at_runtime(operand_type) {
            return self.instance_check(negated, operand, tested);
        }
        let read = match self.stable_value_read(operand) {
            Some(slot) => TestedOperand::Stable(slot),
            None => TestedOperand::Temporary(temporary()),
        };
        let test = self.null_or_instance(operand, tested, read);
        if negated {
            self.add_negation(test)
        } else {
            test
        }
    }

    /// kotlinc's `irLetS(operand) { tmp -> tmp == null || tmp is target }`, for a nullable
    /// operand tested against a nullable target. `target` is the non-null classifier tested.
    fn null_or_instance(&mut self, operand: ExprId, target: Ty, tested: TestedOperand) -> ExprId {
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
