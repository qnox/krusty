//! Array storage and checked reads, owned by the Wasm representation boundary.
//!
//! `Array<T>` stores references. `IntArray` stores `i32` elements in its own identity object.
//! Other primitive arrays stay declined.

use krusty::backend::BackendCompilerIntrinsic;
use krusty::ir::{IrCheckedOperation, IrPropertyDispatch};
use krusty::libraries::RuntimeExceptionKind;

#[cfg(test)]
mod tests;

use super::*;

const I32_ADD: u8 = 0x6a;
const I32_GE_U: u8 = 0x4f;

/// `IntArray`: a non-reference array whose element is `Int`.
pub(super) fn is_int_array(array: Ty) -> bool {
    let array = array.non_null();
    !array.is_reference_array() && array.array_elem() == Some(Ty::Int)
}

/// A getter frame whose length this representation can answer: a reference array, or `IntArray`.
fn sized_array(source: Ty) -> bool {
    let source = source.non_null();
    source.is_reference_array() || is_int_array(source)
}

/// The complete selected getter frame, before any receiver is evaluated.
/// The source type is the representation the length is read from.
fn array_size_receiver(operation: &IrCheckedOperation) -> Result<(u32, Ty), Unsupported> {
    let IrCheckedOperation::ExternalPropertyRead {
        dispatch: IrPropertyDispatch::Ordinary,
        receiver: Some(receiver),
        arguments,
        parameters,
        result: Ty::Int,
        source_receiver: Some(source),
        ..
    } = operation
    else {
        return Err("an authenticated array size read with a mismatched getter frame".into());
    };
    if !arguments.is_empty() || !parameters.is_empty() || !sized_array(*source) {
        return Err("an authenticated array size read with a mismatched getter frame".into());
    }
    Ok((*receiver, source.non_null()))
}

fn reference_element(array: Ty) -> Result<Ty, Unsupported> {
    if !array.non_null().is_reference_array() {
        return Err(format!(
            "reference-element array storage for `{}`",
            describe(array)
        ));
    }
    // A scalar element is stored as its box and read back through it, as `Array<Int>` holds
    // `Integer`s on the JVM: the ordinary coercions to and from `Any?` box and unbox it.
    array
        .non_null()
        .array_read_elem()
        .ok_or_else(|| "a reference array with no readable element type".to_string())
}

impl Body<'_> {
    pub(super) fn is_array_size_property(&self, target: krusty::fir::ExternalPropertyId) -> bool {
        self.callables.property(target).is_some_and(|property| {
            self.callables
                .callable(property.getter)
                .is_some_and(|getter| {
                    getter.compiler_operation() == Some(BackendCompilerIntrinsic::ArraySize)
                })
        })
    }

    /// Length of a getter whose dispatch, result, and source receiver are the checked frame.
    /// `IntArray` reads its own `i32` storage; every other admitted receiver is a reference array.
    pub(super) fn checked_array_size(
        &mut self,
        operation: &IrCheckedOperation,
    ) -> Result<(), Unsupported> {
        let (receiver, source) = array_size_receiver(operation)?;
        let array = self.array_receiver(receiver)?;
        self.value(receiver, array)?;
        if is_int_array(source) {
            if !is_int_array(array) {
                return Err(
                    "an authenticated array size read with a mismatched getter frame".into(),
                );
            }
            self.int_array_storage();
        } else if array.non_null().is_reference_array() {
            self.reference_array_storage();
        } else {
            return Err("an array size read without reference-array storage".into());
        }
        self.code.array_len();
        Ok(())
    }

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
            self.value(
                element,
                Ty::nullable(Ty::obj_name(krusty::types::wk::any())),
            )?;
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
        let any = Ty::nullable(Ty::obj_name(krusty::types::wk::any()));
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
            .struct_get(object, crate::reference_arrays::STORAGE_FIELD);
    }

    pub(super) fn array_get(&mut self, receiver: u32, index: u32) -> Result<(), Unsupported> {
        let array = self.array_receiver(receiver)?;
        if is_int_array(array) {
            self.int_array_get(receiver, index)
        } else {
            self.reference_array_get(receiver, index)
        }
    }

    pub(super) fn array_set(
        &mut self,
        receiver: u32,
        index: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let array = self.array_receiver(receiver)?;
        if is_int_array(array) {
            self.int_array_set(receiver, index, value)
        } else {
            self.reference_array_set(receiver, index, value)
        }
    }

    pub(super) fn array_size(&mut self, receiver: u32) -> Result<(), Unsupported> {
        let array = self.array_receiver(receiver)?;
        self.value(receiver, array)?;
        if is_int_array(array) {
            self.int_array_storage();
        } else {
            reference_element(array)?;
            self.reference_array_storage();
        }
        self.code.array_len();
        Ok(())
    }

    fn array_receiver(&mut self, receiver: u32) -> Result<Ty, Unsupported> {
        self.type_of(receiver)?
            .ok_or_else(|| "an array receiver without a checked type".to_string())
    }

    pub(super) fn new_int_array(&mut self, size: u32) -> Result<(), Unsupported> {
        // The size expression runs once. A negative length is `IllegalArgumentException` on the
        // Kotlin exception path: `array.new_default` would trap, and a trap never reaches `catch`.
        self.value(size, Ty::Int)?;
        let length = self.local(ValType::I32);
        self.code.local_set(length);
        let exception = self.runtime.exceptions.ensure_class(
            self.module,
            self.classifiers,
            RuntimeExceptionKind::IllegalArgument.classifier(),
        )?;
        self.code
            .local_get(length)
            .i32_const(0)
            .op(LT_S32)
            .if_(BlockType::Empty);
        self.code.local_get(length).call(self.runtime.string_of_i32);
        let message = self.local(STRING);
        self.code.local_set(message);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            exception,
            Some(message),
            self.runtime.string,
        );
        self.code.throw(self.runtime.exceptions.tag).end();
        self.code
            .local_get(length)
            .array_new_default(self.runtime.ints);
        self.wrap_int_array();
        Ok(())
    }

    pub(super) fn int_array_literal(
        &mut self,
        spreads: &[bool],
        elements: &[u32],
    ) -> Result<(), Unsupported> {
        if spreads.len() != elements.len() {
            return Err("a malformed array literal's spread inventory".into());
        }
        if spreads.contains(&true) {
            return Err("a spread into an IntArray".into());
        }
        for &element in elements {
            self.value(element, Ty::Int)?;
        }
        let length = u32::try_from(elements.len())
            .map_err(|_| "an array literal exceeding Wasm's length space".to_string())?;
        self.code.array_new_fixed(self.runtime.ints, length);
        self.wrap_int_array();
        Ok(())
    }

    fn wrap_int_array(&mut self) {
        let storage = self.local(ValType::reference(self.runtime.ints));
        let object = self.runtime.int_arrays.object;
        self.code
            .local_set(storage)
            .global_get(self.runtime.int_arrays.table)
            .call(self.runtime.identity.allocate)
            .local_get(storage)
            .struct_new(object);
    }

    fn int_array_storage(&mut self) {
        let object = self.runtime.int_arrays.object;
        self.code
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, crate::reference_arrays::STORAGE_FIELD);
    }

    fn int_array_get(&mut self, receiver: u32, index: u32) -> Result<(), Unsupported> {
        let (storage, offset) = self.int_slot_operands(receiver, index)?;
        let storage = self.int_array_backing(storage);
        self.check_reference_slot(storage, offset)?;
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset)
            .array_get(self.runtime.ints);
        Ok(())
    }

    fn int_array_set(&mut self, receiver: u32, index: u32, value: u32) -> Result<(), Unsupported> {
        let (storage, offset) = self.int_slot_operands(receiver, index)?;
        let stored = self.local(ValType::I32);
        self.value(value, Ty::Int)?;
        self.code.local_set(stored);
        let storage = self.int_array_backing(storage);
        self.check_reference_slot(storage, offset)?;
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset)
            .local_get(stored)
            .array_set(self.runtime.ints);
        Ok(())
    }

    fn int_slot_operands(&mut self, receiver: u32, index: u32) -> Result<(u32, u32), Unsupported> {
        let array = self.array_receiver(receiver)?;
        if !is_int_array(array) {
            return Err(format!("IntArray storage for `{}`", describe(array)));
        }
        let storage = self.local(ValType::nullable(self.runtime.int_arrays.object));
        let offset = self.local(ValType::I32);
        self.value(receiver, array)?;
        self.code
            .ref_cast(true, HeapType::Concrete(self.runtime.int_arrays.object))
            .local_set(storage);
        self.value(index, Ty::Int)?;
        self.code.local_set(offset);
        Ok((storage, offset))
    }

    fn int_array_backing(&mut self, array: u32) -> u32 {
        let storage = self.local(ValType::reference(self.runtime.ints));
        self.code.local_get(array);
        self.int_array_storage();
        self.code.local_set(storage);
        storage
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
        self.coerce_emitted_value(
            Ty::nullable(Ty::obj_name(krusty::types::wk::any())),
            element,
        )
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
        let any = Ty::nullable(Ty::obj_name(krusty::types::wk::any()));
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
