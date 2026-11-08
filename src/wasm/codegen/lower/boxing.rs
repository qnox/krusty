//! Boxing and unboxing a scalar where a use carries it as a reference (see
//! `crate::wasm::boxes`).

use super::*;

impl Body<'_> {
    /// Box the `ty` value on the stack.
    pub(super) fn box_value(&mut self, ty: Ty) -> Result<(), Unsupported> {
        let value = scalar_carrier(ty)?
            .ok_or_else(|| format!("a `{}` value carried as a reference", describe(ty)))?;
        let form = self
            .runtime
            .box_form(self.module, ty, value)
            .ok_or_else(|| format!("a `{}` value carried as a reference", describe(ty)))?;
        let scratch = match form.needs_scratch() {
            true => Some(self.local(value)),
            false => None,
        };
        form.emit_box(&mut self.code, scratch);
        Ok(())
    }

    /// Read the `ty` value out of the box the frontend checked the reference on the stack is.
    pub(super) fn unbox(&mut self, ty: Ty) -> Result<(), Unsupported> {
        let form = scalar_carrier(ty)?
            .and_then(|value| self.runtime.box_form(self.module, ty, value))
            .ok_or_else(|| format!("a reference read as a `{}` value", describe(ty)))?;
        form.emit_unbox(&mut self.code);
        Ok(())
    }
}

/// The scalar carrier of `ty`, the one a box of `ty` stores; `None` for a type carried as a
/// reference or with no value.
fn scalar_carrier(ty: Ty) -> Result<Option<ValType>, Unsupported> {
    Ok(carrier(ty)?.filter(|value| !matches!(value, ValType::Ref { .. })))
}
