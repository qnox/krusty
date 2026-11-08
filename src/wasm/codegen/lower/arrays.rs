//! Reference-element array storage and checked reads, owned by the Wasm representation boundary.

use crate::libraries::RuntimeExceptionKind;

use super::*;

const I32_ADD: u8 = 0x6a;
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
            return self.spread_reference_array_literal(spreads, elements);
        }
        for &element in elements {
            self.value(element, Ty::nullable(Ty::obj_name(crate::types::wk::any())))?;
        }
        let length = u32::try_from(elements.len())
            .map_err(|_| "an array literal exceeding Wasm's length space".to_string())?;
        self.code.array_new_fixed(self.runtime.references, length);
        self.wrap_reference_array();
        Ok(())
    }

    /// An array literal with spreads (`arrayOf(*a, x, *b)`). Each operand is evaluated in source
    /// order, and a spread array's elements are taken when it is reached, as the JVM's spread builder
    /// takes them: a later operand that writes to an earlier spread array does not change the result.
    /// The result is then allocated at the summed length and filled from those parts.
    fn spread_reference_array_literal(
        &mut self,
        spreads: &[bool],
        elements: &[u32],
    ) -> Result<(), Unsupported> {
        let references = self.runtime.references;
        let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let element_carrier =
            carrier(any)?.ok_or_else(|| "a reference element without a carrier".to_string())?;
        let mut parts = Vec::with_capacity(elements.len());
        for (&element, &spread) in elements.iter().zip(spreads) {
            if !spread {
                let value = self.local(element_carrier);
                self.value(element, any)?;
                self.code.local_set(value);
                parts.push((value, false));
                continue;
            }
            let array = self
                .type_of(element)?
                .ok_or_else(|| "a spread operand without a checked type".to_string())?;
            reference_element(array)?;
            let source = self.local(ValType::nullable(references));
            let taken = self.local(ValType::nullable(references));
            self.value(element, array)?;
            self.reference_array_storage();
            self.code
                .local_set(source)
                .local_get(source)
                .ref_as_non_null()
                .array_len()
                .array_new_default(references)
                .local_set(taken)
                .local_get(taken)
                .ref_as_non_null()
                .i32_const(0)
                .local_get(source)
                .ref_as_non_null()
                .i32_const(0)
                .local_get(source)
                .ref_as_non_null()
                .array_len()
                .array_copy(references, references);
            parts.push((taken, true));
        }
        let fixed = parts.iter().filter(|(_, spread)| !spread).count();
        let fixed = i32::try_from(fixed)
            .map_err(|_| "an array literal exceeding Wasm's length space".to_string())?;
        self.code.i32_const(fixed);
        for &(part, _) in parts.iter().filter(|(_, spread)| *spread) {
            self.code
                .local_get(part)
                .ref_as_non_null()
                .array_len()
                .op(I32_ADD);
        }
        let result = self.local(ValType::nullable(references));
        let offset = self.local(ValType::I32);
        self.code
            .array_new_default(references)
            .local_set(result)
            .i32_const(0)
            .local_set(offset);
        for (part, spread) in parts {
            if spread {
                self.code
                    .local_get(result)
                    .ref_as_non_null()
                    .local_get(offset)
                    .local_get(part)
                    .ref_as_non_null()
                    .i32_const(0)
                    .local_get(part)
                    .ref_as_non_null()
                    .array_len()
                    .array_copy(references, references)
                    .local_get(offset)
                    .local_get(part)
                    .ref_as_non_null()
                    .array_len()
                    .op(I32_ADD)
                    .local_set(offset);
            } else {
                self.code
                    .local_get(result)
                    .ref_as_non_null()
                    .local_get(offset)
                    .local_get(part)
                    .array_set(references)
                    .local_get(offset)
                    .i32_const(1)
                    .op(I32_ADD)
                    .local_set(offset);
            }
        }
        self.code.local_get(result).ref_as_non_null();
        self.wrap_reference_array();
        Ok(())
    }

    /// Wrap the freshly allocated backing array on the stack in its Kotlin identity object.
    fn wrap_reference_array(&mut self) {
        let storage = self.local(ValType::reference(self.runtime.references));
        self.code
            .local_set(storage)
            .global_get(self.runtime.arrays.table)
            .call(self.runtime.identity.allocate)
            .local_get(storage)
            .struct_new(self.runtime.arrays.object);
    }

    /// Read the backing array of the checked Kotlin array value on the stack.
    fn reference_array_storage(&mut self) {
        let object = self.runtime.arrays.object;
        self.code
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, crate::wasm::reference_arrays::STORAGE_FIELD);
    }

    /// `array.size`: the length of the receiver's backing storage.
    pub(super) fn reference_array_size(&mut self, receiver: u32) -> Result<(), Unsupported> {
        let array = self
            .type_of(receiver)?
            .ok_or_else(|| "an array receiver without a checked type".to_string())?;
        reference_element(array)?;
        self.value(receiver, array)?;
        self.reference_array_storage();
        self.code.array_len();
        Ok(())
    }

    pub(super) fn reference_array_get(
        &mut self,
        receiver: u32,
        index: u32,
    ) -> Result<(), Unsupported> {
        let (storage, offset, element) = self.reference_slot_operands(receiver, index)?;
        let storage = self.reference_array_backing(storage);
        self.check_reference_slot(storage, offset)?;
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset)
            .array_get(self.runtime.references);
        // The physical element is eqref, even when the checked read's type is String. Preserve
        // that distinction so the ordinary coercion emits the required concrete GC cast.
        self.coerce_emitted_value(Ty::nullable(Ty::obj_name(crate::types::wk::any())), element)
    }

    /// `array[index] = value` on a reference array. The receiver, the index and the value are all
    /// evaluated, in that order, before the bounds check, as `Array.set` receives them as
    /// arguments: an out-of-range index still runs the value's side effects, and a value that
    /// throws wins over `IndexOutOfBoundsException`.
    pub(super) fn reference_array_set(
        &mut self,
        receiver: u32,
        index: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let (storage, offset, _) = self.reference_slot_operands(receiver, index)?;
        let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let stored = self.local(
            carrier(any)?.ok_or_else(|| "a reference element without a carrier".to_string())?,
        );
        self.value(value, any)?;
        self.code.local_set(stored);
        let storage = self.reference_array_backing(storage);
        self.check_reference_slot(storage, offset)?;
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset)
            .local_get(stored)
            .array_set(self.runtime.references);
        Ok(())
    }

    /// Evaluate a reference array receiver and an index, in that order, into locals. Returns the
    /// array-object and offset locals and the array's element type. Dereferencing its backing
    /// storage waits until every call operand, including a store's value, has been evaluated.
    fn reference_slot_operands(
        &mut self,
        receiver: u32,
        index: u32,
    ) -> Result<(u32, u32, Ty), Unsupported> {
        let array = self
            .type_of(receiver)?
            .ok_or_else(|| "an array receiver without a checked type".to_string())?;
        let element = reference_element(array)?;
        let storage = self.local(ValType::nullable(self.runtime.arrays.object));
        let offset = self.local(ValType::I32);
        self.value(receiver, array)?;
        self.code
            .ref_cast(true, HeapType::Concrete(self.runtime.arrays.object))
            .local_set(storage);
        self.value(index, Ty::Int)?;
        self.code.local_set(offset);
        Ok((storage, offset, element))
    }

    fn reference_array_backing(&mut self, array: u32) -> u32 {
        let storage = self.local(ValType::reference(self.runtime.references));
        self.code.local_get(array);
        self.reference_array_storage();
        self.code.local_set(storage);
        storage
    }

    /// Throw `IndexOutOfBoundsException` when the captured index is outside the captured array.
    fn check_reference_slot(&mut self, storage: u32, offset: u32) -> Result<(), Unsupported> {
        if !self.array_range_checks {
            return Ok(());
        }
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
        Ok(())
    }
}
