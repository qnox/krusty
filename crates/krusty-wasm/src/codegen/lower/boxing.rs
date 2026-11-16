//! Boxing and unboxing a scalar where a use carries it as a reference (see
//! `crate::boxes`).

use super::objects::any;
use super::*;

impl Body<'_> {
    /// Box the `ty` value on the stack.
    pub(super) fn box_value(&mut self, ty: Ty) -> Result<(), Unsupported> {
        let value = scalar_carrier(ty)?
            .ok_or_else(|| format!("a `{}` value carried as a reference", describe(ty)))?;
        let form = self
            .runtime
            .box_form(self.module, ty, value)?
            .ok_or_else(|| format!("a `{}` value carried as a reference", describe(ty)))?;
        let scratch = match form.needs_scratch() {
            true => Some(self.local(value)),
            false => None,
        };
        form.emit_box(&mut self.code, scratch);
        Ok(())
    }

    /// The box of the scalar `ty`'s values, declared on first use; `None` for a type carried as a
    /// reference.
    pub(super) fn boxed_form(
        &mut self,
        ty: Ty,
    ) -> Result<Option<crate::boxes::BoxForm>, Unsupported> {
        Ok(match scalar_carrier(ty)? {
            Some(value) => self.runtime.box_form(self.module, ty, value)?,
            None => None,
        })
    }

    /// Read the `ty` value out of the box the frontend checked the reference on the stack is.
    pub(super) fn unbox(&mut self, ty: Ty) -> Result<(), Unsupported> {
        let form = self
            .boxed_form(ty)?
            .ok_or_else(|| format!("a reference read as a `{}` value", describe(ty)))?;
        form.emit_unbox(&mut self.code);
        Ok(())
    }

    /// `x as Int` or `x as UInt?` on a reference: Kotlin checks the cast, so another type's box
    /// (and, for a non-null target, a null) throws the runtime `ClassCastException` through the
    /// Kotlin exception tag, as kotlinc-wasm's cast does, instead of trapping in the unchecked read
    /// [`Self::unbox`] performs for a reference the frontend proved is the box. A non-null target
    /// then reads the value out of its box; a nullable one keeps the checked reference.
    pub(super) fn checked_primitive_cast(
        &mut self,
        arg: u32,
        target: Ty,
    ) -> Result<(), Unsupported> {
        let nullable = matches!(target.kind(), TyKind::Nullable(_));
        let scalar = target.non_null();
        let form = self
            .boxed_form(scalar)?
            .ok_or_else(|| format!("a reference cast to a `{}` value", describe(target)))?;
        let failure = self.runtime.exception_class(
            self.module,
            self.classifiers,
            krusty::libraries::RuntimeExceptionKind::ClassCast.classifier(),
        )?;
        self.value(arg, any())?;
        let value = self.local(REFERENCE);
        self.code
            .local_tee(value)
            .ref_test(nullable, HeapType::Concrete(form.structure))
            .op(EQZ32)
            .if_(BlockType::Empty);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            failure,
            None,
            self.runtime.string,
        )?;
        self.code
            .throw(self.runtime.exceptions.tag)
            .end()
            .local_get(value);
        if !nullable {
            form.emit_unbox(&mut self.code);
        }
        Ok(())
    }
}

/// The scalar carrier of `ty`, the one a box of `ty` stores; `None` for a type carried as a
/// reference or with no value.
fn scalar_carrier(ty: Ty) -> Result<Option<ValType>, Unsupported> {
    Ok(carrier(ty)?.filter(|value| !matches!(value, ValType::Ref { .. })))
}
