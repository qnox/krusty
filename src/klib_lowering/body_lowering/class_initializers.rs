//! Ordered field stores and anonymous initializer effects in their real constructor frame.

use super::super::class_initialization::{ClassInitialization, InitializationStep};
use super::*;
use crate::ir::ClassId;

#[derive(Clone, Copy)]
pub(super) struct InitializationFrame<'a> {
    class: ClassId,
    plan: &'a ClassInitialization<'a, 'a>,
}

impl<'a> BodyLowering<'a> {
    pub(in super::super) fn initialize_class(
        mut self,
        class: ClassId,
        plan: &'a ClassInitialization<'a, 'a>,
    ) -> Lowered<ExprId> {
        self.initialization = Some(InitializationFrame { class, plan });
        let mut stmts = Vec::new();
        for step in &plan.steps {
            let statement = match step {
                InitializationStep::Field { index, value } => {
                    let field = &plan.storage.fields[*index as usize];
                    let receiver = self.read(
                        &plan
                            .header
                            .class
                            .this_receiver
                            .as_ref()
                            .expect("the plan authenticated its instance receiver")
                            .base
                            .symbol,
                    )?;
                    let receiver =
                        self.typed(receiver, Ty::obj_name(plan.header.contract.classifier));
                    let value = self.operand(*value, field.ty)?;
                    let store = self.ir.add_expr(IrExpr::SetField {
                        receiver,
                        class,
                        index: *index,
                        value,
                    });
                    self.typed(store, Ty::Unit)
                }
                InitializationStep::Block(statements) => self.block(statements)?,
            };
            stmts.push(statement);
        }
        let body = self.ir.add_expr(IrExpr::Block { stmts, value: None });
        self.ir.callable_scopes.insert(body);
        Ok(body)
    }

    pub(super) fn initialized_field(
        &mut self,
        symbol: &KlibIrSymbol,
        receiver: Option<KlibIrExprId>,
        value: Option<KlibIrExprId>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if let (None, Some((class, field)), None) = (self.initialization, self.value_member, value)
        {
            return self.value_field(class, field, symbol, receiver, ty);
        }
        let Some(frame) = self.initialization else {
            return unsupported(if value.is_some() {
                "a field write"
            } else {
                "a field read"
            });
        };
        let Some(index) = frame
            .plan
            .field_declarations
            .iter()
            .position(|field| &field.base.symbol == symbol)
        else {
            return Err(mismatch(
                "an initializer accesses a field outside its exact declaring class",
            ));
        };
        let field = &frame.plan.storage.fields[index];
        let receiver =
            receiver.ok_or_else(|| mismatch("an instance field access has no receiver"))?;
        let receiver = self.operand(
            receiver,
            Ty::obj_name(frame.plan.header.contract.classifier),
        )?;
        let index = u32::try_from(index).expect("a field list fits u32");
        let lowered = match value {
            Some(value) => {
                if ty != Ty::Unit {
                    return Err(mismatch("a field store is not typed Unit"));
                }
                let value = self.operand(value, field.ty)?;
                self.ir.add_expr(IrExpr::SetField {
                    receiver,
                    class: frame.class,
                    index,
                    value,
                })
            }
            None => {
                if ty != field.ty {
                    return Err(mismatch(
                        "a field read disagrees with its declared storage type",
                    ));
                }
                self.ir.add_expr(IrExpr::GetField {
                    receiver,
                    class: frame.class,
                    index,
                })
            }
        };
        Ok(self.typed(lowered, ty))
    }

    /// A value class member's read of its class's one field: the value the receiver holds.
    fn value_field(
        &mut self,
        class: ClassId,
        field: &crate::metadata::klib_ir::tree::KlibIrField,
        symbol: &KlibIrSymbol,
        receiver: Option<KlibIrExprId>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if &field.base.symbol != symbol {
            return Err(mismatch(
                "a value class member reads a field outside its declaring class",
            ));
        }
        let declared = self.ir.classes[class as usize].fields[0].ty;
        if ty != declared {
            return Err(mismatch(
                "a field read disagrees with its declared storage type",
            ));
        }
        let receiver_ty = self
            .header
            .value_receiver()
            .ok_or_else(|| mismatch("a value class member has no receiver"))?;
        let receiver =
            receiver.ok_or_else(|| mismatch("an instance field access has no receiver"))?;
        let receiver = self.operand(receiver, receiver_ty)?;
        let lowered = self.ir.add_expr(IrExpr::GetField {
            receiver,
            class,
            index: 0,
        });
        Ok(self.typed(lowered, ty))
    }
}
