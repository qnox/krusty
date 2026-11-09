//! Native atomicfu fields: one cranelift atomic instruction per operation.
//!
//! The plugin has already evaluated the receiver and every argument, so the operands here are
//! locals. `compareAndSet` returns whether the stored word was the expected one. An add or
//! subtract returns the word that was there before the update; the caller adds the delta when the
//! Kotlin operation returns the new value.

use cranelift_codegen::ir::AtomicRmwOp;
use cranelift_codegen::ir::InstBuilder;

use super::*;

impl BodyLowering<'_, '_, '_> {
    pub(super) fn atomic_field_operation(
        &mut self,
        kind: &str,
        exprs: &[u32],
        data: &[TypeName],
        types: &[Ty],
    ) -> Result<Option<Value>, Unsupported> {
        let (class, index, ty) = self.atomic_field(exprs, data, types)?;
        let Some(object) = self.receiver(exprs[0])? else {
            return Ok(None);
        };
        let Some(clif) = self.carrier(ty).clif() else {
            return Err("an atomic field of a non-integer type".into());
        };
        let offset = self.file.model.layout(class).fields[index as usize].offset as i32;
        let address = self.builder.ins().iadd_imm_s(object, i64::from(offset));
        match kind {
            "compareAndSet" => {
                let Some(expected) = self.atomic_operand(exprs, 2, ty)? else {
                    return Ok(None);
                };
                let Some(update) = self.atomic_operand(exprs, 3, ty)? else {
                    return Ok(None);
                };
                let previous =
                    self.builder
                        .ins()
                        .atomic_cas(super::trusted(), address, expected, update);
                Ok(Some(self.builder.ins().icmp(
                    IntCC::Equal,
                    previous,
                    expected,
                )))
            }
            "getAndAdd" => self.atomic_rmw(AtomicRmwOp::Add, exprs, address, clif, ty),
            "getAndSub" => self.atomic_rmw(AtomicRmwOp::Sub, exprs, address, clif, ty),
            "exchange" => self.atomic_rmw(AtomicRmwOp::Xchg, exprs, address, clif, ty),
            _ => Err(format!("the atomic operation `{kind}`").into()),
        }
    }

    fn atomic_field(
        &self,
        exprs: &[u32],
        data: &[TypeName],
        types: &[Ty],
    ) -> Result<(ClassId, u32, Ty), Unsupported> {
        let (Some(&index_expr), Some(&class_name), Some(&ty)) =
            (exprs.get(1), data.first(), types.first())
        else {
            return Err("an atomic field operation".into());
        };
        let Some(class) = self.file.ir.class_id_by_name(class_name) else {
            return Err("an atomic field of another module".into());
        };
        let IrExpr::Const(IrConst::Int(index)) = self.file.ir.expr(index_expr) else {
            return Err("an atomic field index".into());
        };
        let index =
            u32::try_from(*index).map_err(|_| Unsupported::from("an atomic field index"))?;
        Ok((class, index, ty))
    }

    fn atomic_operand(
        &mut self,
        exprs: &[u32],
        position: usize,
        ty: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(&operand) = exprs.get(position) else {
            return Err("an atomic field operation".into());
        };
        self.coerce(operand, ty)
    }

    fn atomic_rmw(
        &mut self,
        op: AtomicRmwOp,
        exprs: &[u32],
        address: Value,
        clif: Type,
        ty: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(argument) = self.atomic_operand(exprs, 2, ty)? else {
            return Ok(None);
        };
        Ok(Some(self.builder.ins().atomic_rmw(
            clif,
            super::trusted(),
            op,
            address,
            argument,
        )))
    }
}
