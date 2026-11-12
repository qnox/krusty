//! Kotlin array storage and checked element access, owned by the Wasm representation boundary.
//!
//! A reference array and a primitive array differ only in their [`ArrayForm`]: the storage type,
//! the element type that storage holds, and how a packed element is read back. Allocation,
//! spreads, reads, stores and bounds checks are shared. A generated `hashCode` hashes an array
//! property by its contents, and a generated `toString` renders one by its contents.

use crate::array_objects::{ArrayForm, ElementRead, STORAGE_FIELD};
use krusty::backend::BackendCompilerIntrinsic;
use krusty::ir::{IrCheckedOperation, IrPropertyDispatch};
use krusty::libraries::RuntimeExceptionKind;

mod content_hash;
mod content_rendering;
#[cfg(test)]
mod tests;

use super::*;

const I32_ADD: u8 = 0x6a;
const I32_GE_U: u8 = 0x4f;
const I32_LT_S: u8 = 0x48;

/// The complete selected getter frame, before any receiver is evaluated.
/// The source type is the array the length is read from.
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
    if !arguments.is_empty() || !parameters.is_empty() || !source.non_null().is_array() {
        return Err("an authenticated array size read with a mismatched getter frame".into());
    }
    Ok((*receiver, *source))
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

    pub(super) fn checked_array_size(
        &mut self,
        operation: &IrCheckedOperation,
    ) -> Result<(), Unsupported> {
        let (receiver, source) = array_size_receiver(operation)?;
        // The getter frame's receiver is the array the length is read from; the operand's own
        // checked type may be a type parameter bounded by it.
        let (form, _) = self.array_form(source)?;
        let operand = self
            .type_of(receiver)?
            .ok_or_else(|| "an array operand without a checked type".to_string())?;
        self.value(receiver, operand)?;
        self.array_storage(form);
        self.code.array_len();
        Ok(())
    }

    /// The form of `array`'s values and the element type its storage holds.
    fn array_form(&mut self, array: Ty) -> Result<(ArrayForm, Ty), Unsupported> {
        self.runtime
            .array_form(self.module, array)
            .ok_or_else(|| format!("array storage for `{}`", describe(array)))
    }

    /// The checked array type of `operand`, its form and its storage's element type.
    fn array_operand(&mut self, operand: u32) -> Result<(Ty, ArrayForm, Ty), Unsupported> {
        let array = self
            .type_of(operand)?
            .ok_or_else(|| "an array operand without a checked type".to_string())?;
        let (form, element) = self.array_form(array)?;
        Ok((array, form, element))
    }

    pub(super) fn array_literal(
        &mut self,
        array: Ty,
        spreads: &[bool],
        elements: &[u32],
    ) -> Result<(), Unsupported> {
        let (form, element_ty) = self.array_form(array)?;
        if spreads.len() != elements.len() {
            return Err("a malformed array literal's spread inventory".into());
        }
        if spreads.contains(&true) {
            return self.spread_array_literal(form, element_ty, spreads, elements);
        }
        for &element in elements {
            self.value(element, element_ty)?;
        }
        let length = u32::try_from(elements.len())
            .map_err(|_| "an array literal exceeding Wasm's length space".to_string())?;
        self.code.array_new_fixed(form.storage, length);
        self.wrap_array(form);
        Ok(())
    }

    /// An array literal with spreads (`arrayOf(*a, x, *b)`). Each operand is evaluated in source
    /// order, and a spread array's elements are taken when it is reached, as the JVM's spread builder
    /// takes them: a later operand that writes to an earlier spread array does not change the result.
    /// The result is then allocated at the summed length and filled from those parts.
    fn spread_array_literal(
        &mut self,
        form: ArrayForm,
        element_ty: Ty,
        spreads: &[bool],
        elements: &[u32],
    ) -> Result<(), Unsupported> {
        let storage = form.storage;
        let element_carrier =
            carrier(element_ty)?.ok_or_else(|| "an array element without a carrier".to_string())?;
        let mut parts = Vec::with_capacity(elements.len());
        for (&element, &spread) in elements.iter().zip(spreads) {
            if !spread {
                let value = self.local(element_carrier);
                self.value(element, element_ty)?;
                self.code.local_set(value);
                parts.push((value, false));
                continue;
            }
            let (array, spread_form, _) = self.array_operand(element)?;
            if spread_form.storage != storage {
                return Err(format!(
                    "a spread of `{}` into another array storage",
                    describe(array)
                ));
            }
            let source = self.local(ValType::nullable(storage));
            let taken = self.local(ValType::nullable(storage));
            self.value(element, array)?;
            self.array_storage(spread_form);
            self.code
                .local_set(source)
                .local_get(source)
                .ref_as_non_null()
                .array_len()
                .array_new_default(storage)
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
                .array_copy(storage, storage);
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
        let result = self.local(ValType::nullable(storage));
        let offset = self.local(ValType::I32);
        self.code
            .array_new_default(storage)
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
                    .array_copy(storage, storage)
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
                    .array_set(storage)
                    .local_get(offset)
                    .i32_const(1)
                    .op(I32_ADD)
                    .local_set(offset);
            }
        }
        self.code.local_get(result).ref_as_non_null();
        self.wrap_array(form);
        Ok(())
    }

    /// `arrayOfNulls(size)`, `IntArray(size)` and the allocation behind `Array(size) { … }`: an
    /// array of `size` default elements (null, zero or `false`). A negative size throws
    /// `IllegalArgumentException("Negative array size")`, as Kotlin/Wasm's array constructors do.
    pub(super) fn new_array(&mut self, array: Ty, size: u32) -> Result<(), Unsupported> {
        let (form, _) = self.array_form(array)?;
        let exception = self.runtime.exception_class(
            self.module,
            self.classifiers,
            RuntimeExceptionKind::IllegalArgument.classifier(),
        )?;
        let length = self.local(ValType::I32);
        self.value(size, Ty::Int)?;
        self.code
            .local_set(length)
            .local_get(length)
            .i32_const(0)
            .op(I32_LT_S)
            .if_(BlockType::Empty);
        let units = "Negative array size".encode_utf16().collect::<Vec<_>>();
        self.runtime
            .string_constant(self.module, &mut self.code, &units);
        let message = self.local(STRING);
        self.code.local_set(message);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            exception,
            Some(message),
            self.runtime.string,
        )?;
        self.code
            .throw(self.runtime.exceptions.tag)
            .end()
            .local_get(length)
            .array_new_default(form.storage);
        self.wrap_array(form);
        Ok(())
    }

    /// Wrap the freshly allocated backing array on the stack in its Kotlin identity object.
    fn wrap_array(&mut self, form: ArrayForm) {
        let storage = self.local(ValType::reference(form.storage));
        self.code
            .local_set(storage)
            .global_get(form.table)
            .call(self.runtime.identity.allocate)
            .local_get(storage)
            .struct_new(form.object);
    }

    /// Read the backing array of the checked Kotlin array value on the stack.
    fn array_storage(&mut self, form: ArrayForm) {
        self.code
            .ref_cast(false, HeapType::Concrete(form.object))
            .struct_get(form.object, STORAGE_FIELD);
    }

    /// `array.size`: the length of the receiver's backing storage.
    pub(super) fn array_size(&mut self, receiver: u32) -> Result<(), Unsupported> {
        let (array, form, _) = self.array_operand(receiver)?;
        self.value(receiver, array)?;
        self.array_storage(form);
        self.code.array_len();
        Ok(())
    }

    pub(super) fn array_get(&mut self, receiver: u32, index: u32) -> Result<(), Unsupported> {
        let (array, form, stored) = self.array_operand(receiver)?;
        let element = array
            .non_null()
            .array_read_elem()
            .ok_or_else(|| "an array with no readable element type".to_string())?;
        let (object, offset) = self.slot_operands(receiver, array, form, index)?;
        let storage = self.array_backing(form, object);
        self.check_slot(storage, offset)?;
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset);
        match form.read {
            ElementRead::Plain => self.code.array_get(form.storage),
            ElementRead::Signed => self.code.array_get_s(form.storage),
            ElementRead::Unsigned => self.code.array_get_u(form.storage),
        };
        // A reference array's physical element is eqref, even when the checked read's type is
        // String or a scalar. Preserve that distinction so the ordinary coercion emits the
        // required concrete GC cast or unboxing.
        self.coerce_emitted_value(stored, element)
    }

    /// `array[index] = value`. The receiver, the index and the value are all evaluated, in that
    /// order, before the bounds check, as `Array.set` receives them as arguments: an
    /// out-of-range index still runs the value's side effects, and a value that throws wins over
    /// `IndexOutOfBoundsException`.
    pub(super) fn array_set(
        &mut self,
        receiver: u32,
        index: u32,
        value: u32,
    ) -> Result<(), Unsupported> {
        let (array, form, stored) = self.array_operand(receiver)?;
        let (object, offset) = self.slot_operands(receiver, array, form, index)?;
        let element = self.local(
            carrier(stored)?.ok_or_else(|| "an array element without a carrier".to_string())?,
        );
        self.value(value, stored)?;
        self.code.local_set(element);
        let storage = self.array_backing(form, object);
        self.check_slot(storage, offset)?;
        self.code
            .local_get(storage)
            .ref_as_non_null()
            .local_get(offset)
            .local_get(element)
            .array_set(form.storage);
        Ok(())
    }

    /// Evaluate an array receiver and an index, in that order, into locals: the array object and
    /// the offset. Dereferencing its backing storage waits until every call operand, including a
    /// store's value, has been evaluated.
    fn slot_operands(
        &mut self,
        receiver: u32,
        array: Ty,
        form: ArrayForm,
        index: u32,
    ) -> Result<(u32, u32), Unsupported> {
        let object = self.local(ValType::nullable(form.object));
        let offset = self.local(ValType::I32);
        self.value(receiver, array)?;
        self.code
            .ref_cast(true, HeapType::Concrete(form.object))
            .local_set(object);
        self.value(index, Ty::Int)?;
        self.code.local_set(offset);
        Ok((object, offset))
    }

    fn array_backing(&mut self, form: ArrayForm, object: u32) -> u32 {
        let storage = self.local(ValType::reference(form.storage));
        self.code.local_get(object);
        self.array_storage(form);
        self.code.local_set(storage);
        storage
    }

    /// Throw `IndexOutOfBoundsException` when the captured index is outside the captured array.
    fn check_slot(&mut self, storage: u32, offset: u32) -> Result<(), Unsupported> {
        if !self.array_range_checks {
            return Ok(());
        }
        let exception = self.runtime.exception_class(
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
        )?;
        self.code.throw(self.runtime.exceptions.tag).end();
        Ok(())
    }
}
