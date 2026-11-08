//! Boxing and unboxing a primitive where a use carries it as a reference (see
//! `crate::wasm::boxes`).

use super::*;

impl Body<'_> {
    /// Box the `ty` value on the stack.
    pub(super) fn box_value(&mut self, ty: Ty) -> Result<(), Unsupported> {
        let form = self
            .runtime
            .box_form(self.module, ty)
            .ok_or_else(|| format!("a `{}` value carried as a reference", describe(ty)))?;
        let scratch = match form.needs_scratch() {
            true => Some(self.local(carrier(ty)?.expect("a boxed primitive has a value"))),
            false => None,
        };
        form.emit_box(&mut self.code, scratch);
        Ok(())
    }

    /// A library unsigned constructor (`kotlin.UInt` and the other three) is the value it is
    /// given. `None` when `internal` is not one of those classes.
    pub(super) fn unsigned_value_construction(
        &mut self,
        site: u32,
        internal: crate::types::TypeName,
        args: &[u32],
    ) -> Option<Result<(), Unsupported>> {
        let unsigned = Ty::obj_name(internal).canonical_semantic();
        if !unsigned.is_unsigned() {
            return None;
        }
        let [argument] = args else {
            return Some(Err(format!(
                "construction of `{}` from {} arguments",
                describe(unsigned),
                args.len()
            )));
        };
        let Some(underlying) = unsigned.scalar_value_repr() else {
            return Some(Err(format!(
                "construction of `{}` with no underlying value",
                describe(unsigned)
            )));
        };
        Some((|| {
            self.value(*argument, underlying)?;
            let result = self.type_of(site)?.unwrap_or(unsigned);
            if matches!(carrier(result)?, Some(ValType::Ref { .. })) {
                self.box_value(unsigned)?;
            }
            Ok(())
        })())
    }

    /// Read the `ty` value out of the box the frontend checked the reference on the stack is.
    pub(super) fn unbox(&mut self, ty: Ty) -> Result<(), Unsupported> {
        let form = self
            .runtime
            .box_form(self.module, ty)
            .ok_or_else(|| format!("a reference read as a `{}` value", describe(ty)))?;
        form.emit_unbox(&mut self.code);
        Ok(())
    }
}
