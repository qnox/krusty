//! Kotlin array storage and checked element access, owned by the Wasm representation boundary.
//!
//! A reference array and a primitive array differ only in their [`ArrayForm`]: the storage type,
//! the element type that storage holds, and how a packed element is read back. Allocation,
//! spreads, reads, stores and bounds checks are shared. A generated `hashCode` hashes an array
//! property by its contents, and a generated `toString` renders one by its contents.

use crate::array_objects::{ArrayForm, ElementRead, STORAGE_FIELD, VIEW_FIELD};
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
const I32_GT_S: u8 = 0x4b;
const I32_LT_S: u8 = 0x48;
const I32_OR: u8 = 0x72;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArrayViewConversion {
    Wrap,
    Unwrap,
}

fn array_view_conversion(from: ArrayForm, to: ArrayForm) -> Option<ArrayViewConversion> {
    match (from.view, to.view) {
        (None, Some((signed, table))) if (signed, table) == (from.object, from.table) => {
            Some(ArrayViewConversion::Wrap)
        }
        (Some((signed, table)), None) if (signed, table) == (to.object, to.table) => {
            Some(ArrayViewConversion::Unwrap)
        }
        _ => None,
    }
}

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

    pub(super) fn is_array_storage_property(
        &self,
        target: krusty::fir::ExternalPropertyId,
    ) -> bool {
        self.callables.property(target).is_some_and(|property| {
            property.name.as_ref() == "storage"
                && krusty::types::Ty::obj_name(property.owner).is_array()
        })
    }

    /// `array.storage`: the Wasm array behind the Kotlin array object.
    pub(super) fn checked_array_storage(
        &mut self,
        operation: &IrCheckedOperation,
    ) -> Result<(), Unsupported> {
        let IrCheckedOperation::ExternalPropertyRead {
            dispatch: IrPropertyDispatch::Ordinary,
            receiver: Some(receiver),
            arguments,
            parameters,
            source_receiver: Some(source),
            ..
        } = operation
        else {
            return Err(
                "an authenticated array storage read with a mismatched getter frame".into(),
            );
        };
        if !arguments.is_empty() || !parameters.is_empty() || !source.non_null().is_array() {
            return Err(
                "an authenticated array storage read with a mismatched getter frame".into(),
            );
        }
        let (receiver, source) = (*receiver, *source);
        let (form, _) = self.array_form(source)?;
        let operand = self
            .type_of(receiver)?
            .ok_or_else(|| "an array operand without a checked type".to_string())?;
        self.value(receiver, operand)?;
        self.array_storage(form);
        Ok(())
    }

    /// `wasm_array_copy(destination, destinationIndex, source, sourceIndex, length)`.
    ///
    /// The operands are already the Wasm arrays (`Array.storage`), and the type argument names
    /// which array class they are.
    pub(super) fn wasm_array_copy(
        &mut self,
        params: &[Ty],
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let [destination, destination_index, source, source_index, length] = *args else {
            return Err("an authenticated wasm array copy with malformed operands".into());
        };
        let [destination_ty, destination_index_ty, source_ty, source_index_ty, length_ty] = *params
        else {
            return Err("an authenticated wasm array copy with malformed operands".into());
        };
        if destination_index_ty != Ty::Int || source_index_ty != Ty::Int || length_ty != Ty::Int {
            return Err("an authenticated wasm array copy with a non-integral index".into());
        }
        let storage = self.wasm_array_storage(destination_ty)?;
        if self.wasm_array_storage(source_ty)? != storage {
            return Err("a wasm array copy between two array classes".into());
        }
        self.value(destination, destination_ty)?;
        // A storage value held as `eq` is still this array class's storage. `array.copy` names
        // that exact array type.
        self.code.ref_cast(
            true,
            super::super::super::encode::HeapType::Concrete(storage),
        );
        self.value(destination_index, Ty::Int)?;
        self.value(source, source_ty)?;
        self.code.ref_cast(
            true,
            super::super::super::encode::HeapType::Concrete(storage),
        );
        self.value(source_index, Ty::Int)?;
        self.value(length, Ty::Int)?;
        self.code.array_copy(storage, storage);
        Ok(())
    }

    fn wasm_array_storage(&mut self, ty: Ty) -> Result<u32, Unsupported> {
        let name = ty.non_null().kotlin_class_internal().ok_or_else(|| {
            "a wasm array copy whose operand is not a wasm array class".to_string()
        })?;
        let array = if name.matches("kotlin/wasm/internal/WasmAnyArray") {
            Ty::array(Ty::nullable(Ty::obj_name(krusty::types::wk::any())))
        } else if name.matches("kotlin/wasm/internal/WasmByteArray") {
            Ty::array(Ty::Byte)
        } else if name.matches("kotlin/wasm/internal/WasmShortArray") {
            Ty::array(Ty::Short)
        } else if name.matches("kotlin/wasm/internal/WasmIntArray") {
            Ty::array(Ty::Int)
        } else if name.matches("kotlin/wasm/internal/WasmLongArray") {
            Ty::array(Ty::Long)
        } else if name.matches("kotlin/wasm/internal/WasmFloatArray") {
            Ty::array(Ty::Float)
        } else if name.matches("kotlin/wasm/internal/WasmDoubleArray") {
            Ty::array(Ty::Double)
        } else if name.matches("kotlin/wasm/internal/WasmCharArray") {
            Ty::array(Ty::Char)
        } else {
            return Err(format!(
                "a wasm array copy of `{}`",
                name.render().replace('/', ".")
            ));
        };
        Ok(self.array_form(array)?.0.storage)
    }

    /// The form of `array`'s values and the element type its storage holds.
    pub(super) fn array_form(&mut self, array: Ty) -> Result<(ArrayForm, Ty), Unsupported> {
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

    /// Wrap the freshly allocated backing array on the stack in its Kotlin identity object. An
    /// unsigned array class wraps a signed array object over the same storage.
    fn wrap_array(&mut self, form: ArrayForm) {
        let storage = self.local(ValType::reference(form.storage));
        self.code.local_set(storage);
        if let Some((view, view_table)) = form.view {
            let signed = self.local(ValType::reference(view));
            self.code
                .global_get(view_table)
                .call(self.runtime.identity.allocate)
                .local_get(storage)
                .struct_new(view)
                .local_set(signed);
            self.code
                .global_get(form.table)
                .call(self.runtime.identity.allocate)
                .local_get(storage)
                .local_get(signed)
                .struct_new(form.object);
        } else {
            self.code
                .global_get(form.table)
                .call(self.runtime.identity.allocate)
                .local_get(storage)
                .struct_new(form.object);
        }
    }

    /// Authenticate the runtime-owned view and its declared backing before evaluating operands.
    pub(super) fn require_array_view(
        &mut self,
        owner: Ty,
        underlying: Ty,
    ) -> Result<(), Unsupported> {
        let (owner, _) = self.array_form(owner)?;
        let (underlying, _) = self.array_form(underlying)?;
        if array_view_conversion(owner, underlying) != Some(ArrayViewConversion::Unwrap) {
            return Err("an array view apart from its declared signed backing".into());
        }
        Ok(())
    }

    /// Replace the `from` array object on the stack by a `to` object over the same backing
    /// storage: how a value class over a signed array (`UIntArray`) and that array alias each
    /// other, each with its own class identity. The unsigned object holds the signed object it
    /// wraps, so unwrapping answers the array it was made from, and the same one each time.
    pub(super) fn rewrap_array(&mut self, from: Ty, to: Ty) -> Result<(), Unsupported> {
        let (from, _) = self.array_form(from)?;
        let (to, _) = self.array_form(to)?;
        match array_view_conversion(from, to) {
            Some(ArrayViewConversion::Wrap) => {
                let signed = self.local(REFERENCE);
                self.code.local_set(signed).local_get(signed);
                self.array_storage(from);
                let storage = self.local(ValType::reference(from.storage));
                self.code
                    .local_set(storage)
                    .global_get(to.table)
                    .call(self.runtime.identity.allocate)
                    .local_get(storage)
                    .local_get(signed)
                    .ref_cast(false, HeapType::Concrete(from.object))
                    .struct_new(to.object);
            }
            Some(ArrayViewConversion::Unwrap) => {
                self.code
                    .ref_cast(false, HeapType::Concrete(from.object))
                    .struct_get(from.object, VIEW_FIELD);
            }
            _ => return Err("a conversion between two array classes of one kind".to_string()),
        }
        Ok(())
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

    /// `array.iterator()`: a walker over this reference array.
    pub(super) fn array_iterator(&mut self, receiver: u32) -> Result<(), Unsupported> {
        let array = self
            .type_of(receiver)?
            .ok_or_else(|| "an array iterator without a receiver type".to_string())?;
        let (form, _) = self.array_form(array)?;
        if form.object != self.runtime.array_iterators.array {
            return Err("an iterator of an array this runtime does not walk".to_string());
        }
        self.value(receiver, array)?;
        self.code
            .ref_cast(false, HeapType::Concrete(form.object))
            .i32_const(0)
            .struct_new(self.runtime.array_iterators.object);
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

    /// `AbstractList.Companion.checkRangeIndexes`. The arguments are evaluated first. A negative
    /// `fromIndex` or a `toIndex` past `size` throws `IndexOutOfBoundsException`; a `fromIndex`
    /// past `toIndex` throws `IllegalArgumentException`. The messages name the three bounds.
    pub(super) fn check_range_indexes(
        &mut self,
        receiver: Option<u32>,
        from_index: u32,
        to_index: u32,
        size: u32,
    ) -> Result<(), Unsupported> {
        // The companion is a library object this target does not emit. The check uses only
        // the three bounds.
        let _ = receiver;
        let from_index = self.range_bound(from_index)?;
        let to_index = self.range_bound(to_index)?;
        let size = self.range_bound(size)?;
        self.code
            .local_get(from_index)
            .i32_const(0)
            .op(I32_LT_S)
            .local_get(to_index)
            .local_get(size)
            .op(I32_GT_S)
            .op(I32_OR)
            .if_(BlockType::Empty);
        self.throw_range_exception(
            RuntimeExceptionKind::IndexOutOfBounds,
            from_index,
            to_index,
            Some(size),
        )?;
        self.code.end();
        self.code
            .local_get(from_index)
            .local_get(to_index)
            .op(I32_GT_S)
            .if_(BlockType::Empty);
        self.throw_range_exception(
            RuntimeExceptionKind::IllegalArgument,
            from_index,
            to_index,
            None,
        )?;
        self.code.end();
        Ok(())
    }

    fn range_bound(&mut self, bound: u32) -> Result<u32, Unsupported> {
        let local = self.local(ValType::I32);
        self.value(bound, Ty::Int)?;
        self.code.local_set(local);
        Ok(local)
    }

    /// Throw the range exception whose message is `fromIndex: <from>, toIndex: <to>[, size: <size>]`
    /// or `fromIndex: <from> > toIndex: <to>`.
    fn throw_range_exception(
        &mut self,
        kind: RuntimeExceptionKind,
        from_index: u32,
        to_index: u32,
        size: Option<u32>,
    ) -> Result<(), Unsupported> {
        let structure =
            self.runtime
                .exception_class(self.module, self.classifiers, kind.classifier())?;
        let between = match size {
            Some(_) => ", toIndex: ",
            None => " > toIndex: ",
        };
        self.string_constant("fromIndex: ");
        self.code
            .local_get(from_index)
            .call(self.runtime.string_of_i32)
            .call(self.runtime.string_concat);
        self.string_constant(between);
        self.code.call(self.runtime.string_concat);
        self.code
            .local_get(to_index)
            .call(self.runtime.string_of_i32)
            .call(self.runtime.string_concat);
        if let Some(size) = size {
            self.string_constant(", size: ");
            self.code.call(self.runtime.string_concat);
            self.code
                .local_get(size)
                .call(self.runtime.string_of_i32)
                .call(self.runtime.string_concat);
        }
        let message = self.local(STRING);
        self.code.local_set(message);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            structure,
            Some(message),
            self.runtime.string,
        )?;
        self.code.throw(self.runtime.exceptions.tag);
        Ok(())
    }
}
