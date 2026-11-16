//! Atomic accesses to an instance field: one cranelift atomic instruction per operation.
//!
//! The receiver and every operand are evaluated first, in order, before the instruction touches
//! the field. `CompareAndSet` answers whether the stored word was the expected one. `Exchange`,
//! `CompareAndExchange` and `FetchAndAdd` answer the word the instruction found; an operation that
//! returns the updated value adds the delta itself. A reference field is compared by identity.

use cranelift_codegen::ir::AtomicRmwOp;
use cranelift_codegen::ir::InstBuilder;
use krusty::libraries::AtomicOperation;

use super::*;

impl BodyLowering<'_, '_, '_> {
    pub(super) fn atomic_field(
        &mut self,
        operation: AtomicOperation,
        receiver: u32,
        class: ClassId,
        index: u32,
        operands: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        if operands.len() != operation.operands() {
            return Err("an atomic field access without its operation's operands".into());
        }
        let ty = model::field_storage_ty(self.file.values, self.file.ir, class, index)?;
        let clif = match self.carrier(ty) {
            Carrier::Scalar(clif, _) if clif.is_int() => clif,
            Carrier::Ref => types::I64,
            Carrier::Scalar(..) | Carrier::Void => {
                return Err(
                    "an atomic access to a field that is not integral or a reference".into(),
                )
            }
        };
        if operation == AtomicOperation::FetchAndAdd && matches!(self.carrier(ty), Carrier::Ref) {
            return Err("an atomic addition to a reference field".into());
        }
        let Some(object) = self.receiver(receiver)? else {
            return Ok(None);
        };
        let mut values = Vec::with_capacity(operands.len());
        for &operand in operands {
            let Some(value) = self.coerce(operand, ty)? else {
                return Ok(None);
            };
            values.push(value);
        }
        if self.terminated {
            return Ok(None);
        }
        let offset = self.file.model.layout(class).fields[index as usize].offset as i32;
        let address = self.builder.ins().iadd_imm_s(object, i64::from(offset));
        let flags = super::trusted();
        let found = match operation {
            AtomicOperation::Load => self.builder.ins().atomic_load(clif, flags, address),
            AtomicOperation::Store => {
                self.builder.ins().atomic_store(flags, values[0], address);
                return Ok(None);
            }
            AtomicOperation::Exchange => {
                self.builder
                    .ins()
                    .atomic_rmw(clif, flags, AtomicRmwOp::Xchg, address, values[0])
            }
            AtomicOperation::FetchAndAdd => {
                self.builder
                    .ins()
                    .atomic_rmw(clif, flags, AtomicRmwOp::Add, address, values[0])
            }
            AtomicOperation::CompareAndSet => {
                let found = self
                    .builder
                    .ins()
                    .atomic_cas(flags, address, values[0], values[1]);
                return Ok(Some(self.builder.ins().icmp(
                    IntCC::Equal,
                    found,
                    values[0],
                )));
            }
            AtomicOperation::CompareAndExchange => self
                .builder
                .ins()
                .atomic_cas(flags, address, values[0], values[1]),
        };
        Ok(Some(found))
    }
}
