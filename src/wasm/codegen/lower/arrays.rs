//! Reference-element array storage and checked reads, owned by the Wasm representation boundary.

use crate::libraries::RuntimeExceptionKind;

use super::*;

const I32_GE_U: u8 = 0x4f;
const I32_ADD: u8 = 0x6a;

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
            return self.reference_array_literal_with_spreads(spreads, elements);
        }
        for &element in elements {
            self.value(element, Ty::nullable(Ty::obj_name(crate::types::wk::any())))?;
        }
        let length = u32::try_from(elements.len())
            .map_err(|_| "an array literal exceeding Wasm's length space".to_string())?;
        self.code.array_new_fixed(self.runtime.references, length);
        Ok(())
    }

    /// `arrayOf(a, *b, c)`: evaluate every operand left to right, then copy each into one new
    /// array. A spread operand is a reference array of the same storage, copied whole; the result
    /// never aliases it. A spread's elements are taken when the spread is evaluated, so a spread
    /// that a later operand follows is snapshotted there, as Kotlin's spread builder copies it.
    fn reference_array_literal_with_spreads(
        &mut self,
        spreads: &[bool],
        elements: &[u32],
    ) -> Result<(), Unsupported> {
        let references = self.runtime.references;
        let mut operands = Vec::with_capacity(elements.len());
        for (operand, (&element, &spread)) in elements.iter().zip(spreads).enumerate() {
            if spread {
                let array = self
                    .type_of(element)?
                    .ok_or_else(|| "a spread operand without a checked type".to_string())?;
                reference_element(array)?;
                let storage = self.local(ValType::nullable(references));
                self.value(element, array)?;
                self.code
                    .ref_cast(true, HeapType::Concrete(references))
                    .local_set(storage);
                let storage = if operand + 1 < elements.len() {
                    self.snapshot_reference_array(storage)
                } else {
                    storage
                };
                operands.push((true, storage));
            } else {
                let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
                let carried = carrier(any)?
                    .ok_or_else(|| "an array element without a Wasm value".to_string())?;
                let value = self.local(carried);
                self.value(element, any)?;
                self.code.local_set(value);
                operands.push((false, value));
            }
        }
        let singles = operands.iter().filter(|(spread, _)| !spread).count();
        let singles = i32::try_from(singles)
            .map_err(|_| "an array literal exceeding Wasm's length space".to_string())?;
        let result = self.local(ValType::nullable(references));
        let position = self.local(ValType::I32);
        self.code.i32_const(singles);
        for &(spread, storage) in &operands {
            if spread {
                self.code
                    .local_get(storage)
                    .ref_as_non_null()
                    .array_len()
                    .op(I32_ADD);
            }
        }
        self.code
            .array_new_default(references)
            .local_set(result)
            .i32_const(0)
            .local_set(position);
        for (spread, operand) in operands {
            self.code
                .local_get(result)
                .ref_as_non_null()
                .local_get(position);
            if spread {
                self.code
                    .local_get(operand)
                    .ref_as_non_null()
                    .i32_const(0)
                    .local_get(operand)
                    .ref_as_non_null()
                    .array_len()
                    .array_copy(references, references)
                    .local_get(position)
                    .local_get(operand)
                    .ref_as_non_null()
                    .array_len();
            } else {
                self.code
                    .local_get(operand)
                    .array_set(references)
                    .local_get(position)
                    .i32_const(1);
            }
            self.code.op(I32_ADD).local_set(position);
        }
        self.code.local_get(result).ref_as_non_null();
        Ok(())
    }

    /// `array.size` of a reference-element array: its storage length.
    pub(super) fn reference_array_size(&mut self, receiver: u32) -> Result<(), Unsupported> {
        let array = self
            .type_of(receiver)?
            .ok_or_else(|| "an array receiver without a checked type".to_string())?;
        reference_element(array)?;
        self.value(receiver, array)?;
        self.code
            .ref_cast(false, HeapType::Concrete(self.runtime.references))
            .array_len();
        Ok(())
    }

    pub(super) fn reference_array_get(
        &mut self,
        receiver: u32,
        index: u32,
    ) -> Result<(), Unsupported> {
        let (element, storage, offset) = self.checked_reference_slot(receiver, index)?;
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset)
            .array_get(self.runtime.references);
        // The physical element is eqref, even when the checked read's type is String. Preserve
        // that distinction so the ordinary coercion emits the required concrete GC cast.
        self.coerce_emitted_value(Ty::nullable(Ty::obj_name(crate::types::wk::any())), element)
    }

    /// Copy the reference array in local `storage` into a new array and return the local
    /// holding the copy.
    fn snapshot_reference_array(&mut self, storage: u32) -> u32 {
        let references = self.runtime.references;
        let snapshot = self.local(ValType::nullable(references));
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .array_len()
            .array_new_default(references)
            .local_set(snapshot)
            .local_get(snapshot)
            .ref_as_non_null()
            .i32_const(0)
            .local_get(storage)
            .ref_as_non_null()
            .i32_const(0)
            .local_get(storage)
            .ref_as_non_null()
            .array_len()
            .array_copy(references, references);
        snapshot
    }

    /// `array[index] = value` on a reference-element array. The receiver, index and value are
    /// evaluated in source order before the range check, as on the JVM.
    pub(super) fn reference_array_set(
        &mut self,
        receiver: u32,
        index: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let (_, storage, offset) = self.reference_slot(receiver, index)?;
        let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let carried =
            carrier(any)?.ok_or_else(|| "an array element without a Wasm value".to_string())?;
        let stored = self.local(carried);
        self.value(value, any)?;
        self.code.local_set(stored);
        self.check_reference_index(storage, offset)?;
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset)
            .local_get(stored)
            .array_set(self.runtime.references);
        Ok(())
    }

    /// Evaluate a reference array and an index into locals, throwing
    /// `IndexOutOfBoundsException` when the index is outside the array.
    fn checked_reference_slot(
        &mut self,
        receiver: u32,
        index: u32,
    ) -> Result<(Ty, u32, u32), Unsupported> {
        let slot = self.reference_slot(receiver, index)?;
        self.check_reference_index(slot.1, slot.2)?;
        Ok(slot)
    }

    /// Evaluate a reference array and an index into locals: the element type, the array local
    /// and the index local.
    fn reference_slot(&mut self, receiver: u32, index: u32) -> Result<(Ty, u32, u32), Unsupported> {
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
        Ok((element, storage, offset))
    }

    /// Throw `IndexOutOfBoundsException` when the index in `offset` is outside the array in
    /// `storage`, if array range checks are on.
    fn check_reference_index(&mut self, storage: u32, offset: u32) -> Result<(), Unsupported> {
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
