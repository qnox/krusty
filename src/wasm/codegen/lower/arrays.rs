//! Reference-element array storage and checked reads, owned by the Wasm representation boundary.

use crate::libraries::RuntimeExceptionKind;

use super::*;

const I32_GE_U: u8 = 0x4f;

fn reference_element(array: Ty) -> Result<Ty, Unsupported> {
    if !array.non_null().is_reference_array() {
        return Err(format!(
            "reference-element array storage for `{}`",
            describe(array)
        ));
    }
    let element = array
        .non_null()
        .array_read_elem()
        .ok_or_else(|| "a reference array with no readable element type".to_string())?;
    if !matches!(carrier(element)?, Some(ValType::Ref { .. })) {
        return Err(format!(
            "a reference array requiring scalar boxing for `{}`",
            describe(element)
        ));
    }
    Ok(element)
}

impl Body<'_> {
    pub(super) fn reference_array_literal(
        &mut self,
        array: Ty,
        spreads: &[bool],
        elements: &[u32],
    ) -> Result<(), Unsupported> {
        reference_element(array)?;
        if spreads.len() != elements.len() {
            return Err("a malformed array literal's spread inventory".into());
        }
        if spreads.contains(&true) {
            return Err("an array literal with a spread".into());
        }
        for &element in elements {
            self.value(element, Ty::nullable(Ty::obj_name(crate::types::wk::any())))?;
        }
        let length = u32::try_from(elements.len())
            .map_err(|_| "an array literal exceeding Wasm's length space".to_string())?;
        self.code.array_new_fixed(self.runtime.references, length);
        Ok(())
    }

    pub(super) fn reference_array_get(
        &mut self,
        receiver: u32,
        index: u32,
    ) -> Result<(), Unsupported> {
        let array = self
            .type_of(receiver)?
            .ok_or_else(|| "an array receiver without a checked type".to_string())?;
        let element = reference_element(array)?;
        let storage = self.local(ValType::nullable(self.runtime.references));
        let offset = self.local(ValType::I32);
        self.value(receiver, array)?;
        self.code
            .ref_cast(true, HeapType::Concrete(self.runtime.references))
            .local_set(storage);
        self.value(index, Ty::Int)?;
        self.code.local_set(offset);
        if self.array_range_checks {
            let exception = self.runtime.exceptions.ensure_class(
                self.module,
                self.classifiers,
                RuntimeExceptionKind::IndexOutOfBounds.classifier(),
            )?;
            // Unsigned comparison includes every negative index as well as size and larger.
            self.code
                .local_get(offset)
                .local_get(storage)
                .ref_as_non_null()
                .array_len()
                .op(I32_GE_U)
                .if_(BlockType::Empty);
            self.runtime.exceptions.construct_class(
                &mut self.code,
                exception,
                None,
                self.runtime.string,
            );
            self.code.throw(self.runtime.exceptions.tag).end();
        }
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset)
            .array_get(self.runtime.references);
        // The physical element is eqref, even when the checked read's type is String. Preserve
        // that distinction so the ordinary coercion emits the required concrete GC cast.
        self.coerce_emitted_value(Ty::nullable(Ty::obj_name(crate::types::wk::any())), element)
    }
}
