//! Atomic accesses to an instance field or an array element: one cranelift atomic instruction
//! per operation.
//!
//! The receiver and every operand are evaluated first, in order, before the instruction touches
//! the word. `CompareAndSet` answers whether the stored word was the expected one. `Exchange`,
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
        Ok(self.atomic_access(operation, clif, address, &values))
    }

    /// The atomic access `operation` through the property reference `reference` to the storage of a
    /// top-level property, which the reference names but this call never materializes: the
    /// operands are evaluated in order and the instruction touches the property's own word.
    pub(super) fn atomic_property_access(
        &mut self,
        operation: AtomicOperation,
        reference: u32,
        operands: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        if operands.len() != operation.operands() {
            return Err("an atomic property access without its operation's operands".into());
        }
        let IrExpr::Checked(checked) = self.file.ir.expr(reference) else {
            return Err(
                "an atomic access through another receiver than a property reference".into(),
            );
        };
        let IrCheckedOperation::PropertyReference {
            target:
                krusty::fir::FirPropertyReferenceTarget::Module(property)
                | krusty::fir::FirPropertyReferenceTarget::SpecializedModule { property, .. },
            dispatch_receiver: None,
            extension_receiver: None,
            ..
        } = &**checked
        else {
            return Err(
                "an atomic access through a property reference that does not name top-level storage"
                    .into(),
            );
        };
        let property = *property;
        let super::statics::TopLevel::Slot(index) = self.top_level(&property, true)? else {
            return Err("an atomic access to a property that is not a plain storage slot".into());
        };
        let ty = self.file.ir.statics[index as usize].ty;
        let clif = match self.carrier(ty) {
            Carrier::Scalar(clif, _) if clif.is_int() => clif,
            Carrier::Ref => types::I64,
            Carrier::Scalar(..) | Carrier::Void => {
                return Err(
                    "an atomic access to a property that is not integral or a reference".into(),
                )
            }
        };
        if operation == AtomicOperation::FetchAndAdd && matches!(self.carrier(ty), Carrier::Ref) {
            return Err("an atomic addition to a reference property".into());
        }
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
        self.initialize_static_owner(index)?;
        let slot = self.file.statics[index as usize];
        let address = self.data_address(slot);
        Ok(self.atomic_access(operation, clif, address, &values))
    }

    /// The atomic access `operation` to the word of width `clif` at `address`, with `values` its
    /// operation's operands: what it answers, or nothing for a store.
    pub(super) fn atomic_access(
        &mut self,
        operation: AtomicOperation,
        clif: types::Type,
        address: Value,
        values: &[Value],
    ) -> Option<Value> {
        let flags = super::trusted();
        Some(match operation {
            AtomicOperation::Load => self.builder.ins().atomic_load(clif, flags, address),
            AtomicOperation::Store => {
                self.builder.ins().atomic_store(flags, values[0], address);
                return None;
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
                self.builder.ins().icmp(IntCC::Equal, found, values[0])
            }
            AtomicOperation::CompareAndExchange => self
                .builder
                .ins()
                .atomic_cas(flags, address, values[0], values[1]),
        })
    }
}

impl BodyLowering<'_, '_, '_> {
    /// The atomic access `operation` to the element `index` of `array`, after proving the index
    /// is inside the array. A reference array's element is compared by identity.
    pub(super) fn atomic_array_element(
        &mut self,
        operation: AtomicOperation,
        array: u32,
        index: u32,
        operands: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        if operands.len() != operation.operands() {
            return Err("an atomic array element access without its operation's operands".into());
        }
        let (object, shape) = self.array_of(array)?;
        let clif = match self.carrier(shape.stored) {
            Carrier::Scalar(clif, _) if clif.is_int() => clif,
            Carrier::Ref if operation != AtomicOperation::FetchAndAdd => types::I64,
            Carrier::Ref => return Err("an atomic addition to a reference array element".into()),
            Carrier::Scalar(..) | Carrier::Void => {
                return Err(
                    "an atomic access to an array element that is not integral or a reference"
                        .into(),
                )
            }
        };
        let index = self.coerce(index, Ty::Int)?;
        let mut values = Vec::with_capacity(operands.len());
        for &operand in operands {
            let Some(value) = self.coerce(operand, shape.stored)? else {
                return Ok(None);
            };
            values.push(value);
        }
        if self.terminated {
            return Ok(None);
        }
        let Some(index) = index else {
            return Err("an array index of no value".into());
        };
        let address = self.element_address(object, index, shape.stride)?;
        Ok(self.atomic_access(operation, clif, address, &values))
    }
}
