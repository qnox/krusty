//! The resume protocol of a suspend function that returns a value class as its reference carrier.
//!
//! The function returns the carrier when it completes without suspending, but its continuation's
//! `invokeSuspend` boxes the carrier it re-enters with, so a caller resumed by that continuation reads
//! the box back from `result`. kotlinc's caller therefore unboxes the resumed value. This machine
//! passes a synchronous result through `result` as well, so it stores the box there too and every
//! read of `result` sees the one representation.

use super::emission_facts::ForwardedSuspendResult;
use crate::ir::{
    Callee, ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp, IrValueClassSuspendResult,
};
use crate::libraries::InlineKind;
use crate::types::{Ty, TypeName};

/// A value class returned from a suspend function as its reference carrier.
#[derive(Clone, Copy)]
pub(super) struct ReferenceCarrier {
    classifier: TypeName,
    carrier: Ty,
    nullable: bool,
}

impl ReferenceCarrier {
    pub(super) fn of(result: Option<IrValueClassSuspendResult>) -> Option<Self> {
        let Some(IrValueClassSuspendResult::Carrier {
            classifier,
            carrier,
        }) = result
        else {
            return None;
        };
        // A nullable value class over a primitive keeps its box as the carrier.
        let boxed = carrier.non_null() == Ty::obj_name(classifier);
        (carrier.is_reference() && !boxed).then_some(Self {
            classifier,
            carrier: carrier.non_null(),
            nullable: carrier.is_nullable(),
        })
    }

    /// The adaptation of the continuation's re-entry result.
    pub(super) fn reentry(self) -> ForwardedSuspendResult {
        ForwardedSuspendResult::ValueClassBox {
            classifier: self.classifier,
            carrier: self.carrier,
            nullable: self.nullable,
        }
    }

    /// `box-impl(value)`, null-safe when the value class is nullable; `slot` holds the operand.
    pub(super) fn boxed(self, ir: &mut IrFile, value: ExprId, slot: u32) -> ExprId {
        let owner = self.classifier;
        let descriptor = format!(
            "({})L{};",
            crate::jvm::names::type_descriptor(self.carrier),
            owner.render()
        );
        self.guarded(ir, value, self.carrier, slot, |ir, operand| {
            ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner,
                    name: "box-impl".to_string(),
                    descriptor,
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![operand],
            })
        })
    }

    /// The carrier of the box `value` resumed with, null-safe when the value class is nullable;
    /// `slot` holds the box.
    pub(super) fn unboxed(self, ir: &mut IrFile, value: ExprId, slot: u32) -> ExprId {
        let owner = self.classifier;
        let descriptor = format!("(){}", crate::jvm::names::type_descriptor(self.carrier));
        let boxed = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: value,
            type_operand: Ty::obj_name(owner),
        });
        self.guarded(ir, boxed, Ty::obj_name(owner), slot, |ir, operand| {
            ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner,
                    name: "unbox-impl".to_string(),
                    descriptor,
                    params: None,
                    interface: false,
                    module_target: None,
                },
                dispatch_receiver: Some(operand),
                args: Vec::new(),
            })
        })
    }

    /// `convert(value)`, or `{ slot = value; if (slot == null) null else convert(slot) }` as a null
    /// guard when the value class is nullable.
    fn guarded(
        self,
        ir: &mut IrFile,
        value: ExprId,
        operand_ty: Ty,
        slot: u32,
        convert: impl FnOnce(&mut IrFile, ExprId) -> ExprId,
    ) -> ExprId {
        if !self.nullable {
            return convert(ir, value);
        }
        let variable = ir.add_expr(IrExpr::Variable {
            index: slot,
            ty: Ty::nullable(operand_ty),
            init: Some(value),
            named: false,
        });
        let tested = ir.add_expr(IrExpr::GetValue(slot));
        let null = ir.add_expr(IrExpr::Const(IrConst::Null));
        let is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: tested,
            rhs: null,
        });
        let null_result = ir.add_expr(IrExpr::Const(IrConst::Null));
        let operand = ir.add_expr(IrExpr::GetValue(slot));
        let converted = convert(ir, operand);
        let guard = ir.add_expr(IrExpr::When {
            branches: vec![(Some(is_null), null_result), (None, converted)],
        });
        ir.null_guards.insert(guard);
        ir.add_expr(IrExpr::Block {
            stmts: vec![variable],
            value: Some(guard),
        })
    }
}

impl super::Flat<'_> {
    /// What a synchronous completion of the suspension `point` leaves in `result`: the box when the
    /// callee returns a reference carrier, as its continuation would resume with.
    pub(super) fn resumable_result(&mut self, point: ExprId, value: ExprId) -> ExprId {
        let result = super::value_class_suspension_result(self.ir, point, self.suspend);
        match ReferenceCarrier::of(result) {
            Some(carrier) => {
                let slot = self.fresh();
                carrier.boxed(self.ir, value, slot)
            }
            None => value,
        }
    }

    /// The carrier a reference-carrier callee's box resumes `point` with, if it has one.
    pub(super) fn resumed_carrier(&mut self, point: ExprId, value: ExprId) -> Option<ExprId> {
        let result = super::value_class_suspension_result(self.ir, point, self.suspend);
        let carrier = ReferenceCarrier::of(result)?;
        let slot = self.fresh();
        Some(carrier.unboxed(self.ir, value, slot))
    }
}
