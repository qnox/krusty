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

    /// `holder.element = value` (the write `id`). The write leaves the stored value on the stack
    /// only when its own type carries one: a source assignment is checked as `Unit` and leaves
    /// nothing, while an unchecked write evaluates to what it stored (see `expression_type`).
    pub(super) fn shared_cell_write(
        &mut self,
        id: u32,
        holder: u32,
        elem: Ty,
        value: u32,
    ) -> Result<(), Unsupported> {
        let cell = self.cell_holder(holder, elem)?;
        let carried = carrier(elem)?.ok_or_else(|| "a shared cell of `Unit`".to_string())?;
        match self.carrier_of(id)? {
            None => {
                self.value(value, elem)?;
                self.code.struct_set(cell, 0);
            }
            Some(result) if result == carried => {
                let stored = self.local(carried);
                self.value(value, elem)?;
                self.code.local_tee(stored);
                self.code.struct_set(cell, 0);
                self.code.local_get(stored);
            }
            Some(_) => {
                return Err("a shared-cell write answering another type than its element".into());
            }
        }
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

#[cfg(test)]
mod tests {
    use crate::ir::{IrConst, IrExpr, IrFile, IrFunction};
    use crate::types::Ty;
    use crate::wasm::target::WasmTarget;

    /// The encoded module of one function whose body declares the cell `0` holding an `Int`,
    /// writes `1` into it, and either ends there or answers the write: a source assignment
    /// statement is checked as `Unit`, while an unchecked write evaluates to what it stored.
    fn module_of_one_write(checked: Option<Ty>, ret: Ty) -> Vec<u8> {
        let mut ir = IrFile::default();
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let cell = ir.add_expr(IrExpr::RefNew {
            elem: Ty::Int,
            init: Some(zero),
        });
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::Int,
            init: Some(cell),
            named: true,
        });
        let holder = ir.add_expr(IrExpr::GetValue(0));
        let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let write = ir.add_expr(IrExpr::RefSet {
            holder,
            elem: Ty::Int,
            value: one,
        });
        if let Some(checked) = checked {
            ir.logical_types.insert(write, checked);
        }
        let (stmts, value) = if ret == Ty::Unit {
            (vec![declaration, write], None)
        } else {
            (vec![declaration], Some(write))
        };
        let body = ir.add_expr(IrExpr::Block { stmts, value });
        ir.add_fun(IrFunction {
            name: "write".to_string(),
            params: Vec::new(),
            ret,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut started = super::super::super::Started::new(WasmTarget::Wasi);
        let properties = crate::backend::local_properties::realize(&mut ir)
            .expect("the body declares no local delegated property");
        super::super::lower_alone(&ir, &properties, &mut started).expect("the cell body lowers");
        started.module.encode()
    }

    /// Whether `bytes` holds `struct.set <any type> 0` directly followed by `tail`.
    fn has_struct_set_followed_by(bytes: &[u8], tail: impl Fn(&[u8]) -> bool) -> bool {
        (0..bytes.len().saturating_sub(4)).any(|at| {
            bytes[at..].starts_with(&[0xfb, 0x05])
                && bytes[at + 3] == 0x00
                && tail(&bytes[at + 4..])
        })
    }

    #[test]
    fn a_write_checked_as_unit_leaves_nothing_on_the_stack() {
        let bytes = module_of_one_write(Some(Ty::Unit), Ty::Unit);
        assert!(has_struct_set_followed_by(&bytes, |rest| rest.first() == Some(&0x0b)));
        assert!(!has_struct_set_followed_by(&bytes, |rest| rest.first() == Some(&0x20)));
    }

    #[test]
    fn an_unchecked_write_answers_the_value_it_stored() {
        let bytes = module_of_one_write(None, Ty::Int);
        assert!(has_struct_set_followed_by(&bytes, |rest| {
            rest.first() == Some(&0x20) && rest.get(2) == Some(&0x0b)
        }));
    }
}
