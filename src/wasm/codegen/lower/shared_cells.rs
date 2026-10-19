//! Wasm realization of shared mutable-local cells.
//!
//! Common lowering moves a `var` that a lambda writes into a cell: its declaration initializes the
//! local with an [`IrExpr::RefNew`], and every use is an [`IrExpr::RefGet`] or [`IrExpr::RefSet`]
//! through that local. The IR keeps the element's Kotlin type and leaves the cell's shape to the
//! backend. Here a cell is a struct with one mutable field of the element's carrier, one struct
//! type per carrier, and the local holds a reference to it.
//!
//! The local is only ever a cell's holder. Reading it as a value, assigning it, or reaching a cell
//! through any other holder declines: nothing in this generator yet hands a cell to another body.

use super::*;
use crate::wasm::encode::{CompositeType, FieldType, StorageType};

impl Body<'_> {
    /// Declare value `index` as the cell `init` (an [`IrExpr::RefNew`]) creates.
    pub(super) fn shared_cell_variable(
        &mut self,
        index: u32,
        init: u32,
    ) -> Result<(), Unsupported> {
        let IrExpr::RefNew { elem, init } = self.ir.expr(init).clone() else {
            return Err("a shared cell declared without its cell".to_string());
        };
        let cell = self.cell_type(elem)?;
        let local = self.local(ValType::nullable(cell));
        match init {
            Some(value) => {
                self.value(value, elem)?;
                self.code.struct_new(cell);
            }
            None => {
                self.code.struct_new_default(cell);
            }
        }
        self.code.local_set(local);
        self.values.insert(index, (local, elem));
        self.cells.insert(index, cell);
        Ok(())
    }

    /// `holder.element`.
    pub(super) fn shared_cell_read(&mut self, holder: u32, elem: Ty) -> Result<(), Unsupported> {
        let cell = self.cell_holder(holder, elem)?;
        self.code.struct_get(cell, 0);
        Ok(())
    }

    /// `holder.element = value`, leaving `value` on the stack.
    pub(super) fn shared_cell_write(
        &mut self,
        holder: u32,
        elem: Ty,
        value: u32,
    ) -> Result<(), Unsupported> {
        let cell = self.cell_holder(holder, elem)?;
        let carried = carrier(elem)?.ok_or_else(|| "a shared cell of `Unit`".to_string())?;
        let stored = self.local(carried);
        self.value(value, elem)?;
        self.code.local_tee(stored);
        self.code.struct_set(cell, 0);
        self.code.local_get(stored);
        Ok(())
    }

    /// Push the cell `holder` reads, returning its struct type.
    fn cell_holder(&mut self, holder: u32, elem: Ty) -> Result<u32, Unsupported> {
        let IrExpr::GetValue(slot) = self.ir.expr(holder) else {
            return Err("a shared cell reached through another holder".to_string());
        };
        let cell =
            self.cells.get(slot).copied().ok_or_else(|| {
                format!("a shared-cell access of value {slot}, which holds no cell")
            })?;
        if cell != self.cell_type(elem)? {
            return Err(format!(
                "a shared-cell access of value {slot} at another element type"
            ));
        }
        let (local, _) = self.values[slot];
        self.code.local_get(local);
        Ok(cell)
    }

    /// The struct type of a cell holding `elem`.
    fn cell_type(&mut self, elem: Ty) -> Result<u32, Unsupported> {
        let carried = carrier(elem)?.ok_or_else(|| "a shared cell of `Unit`".to_string())?;
        Ok(self.module.ty(CompositeType::Struct(vec![FieldType {
            storage: StorageType::Val(carried),
            mutable: true,
        }])))
    }
}
