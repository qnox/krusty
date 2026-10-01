//! JVM element loads and stores for the array intrinsic operations.
//!
//! The array classifiers (`IntArray`, `Array<T>`, ...) have no loadable JVM class: the provider
//! attaches `ArrayGet`/`ArraySet` to their exact element declarations, target realization turns a
//! selected call into that intrinsic, and this module only executes it as array bytecode.

use crate::jvm::array_representation::{array_load_op, array_store_op};
use crate::jvm::classfile::CodeBuilder;
use crate::jvm::names::type_descriptor;
use crate::types::Ty;

use super::{box_prim_free, boxed_prim_of, unbox_prim_from_descriptor, Emitter};

/// Semantic scalar adapter for a JVM reference-array element. Unsigned values share a primitive
/// carrier with signed values but their array stores must call the Kotlin value-class box adapter.
pub(super) fn reference_array_scalar_adapter(element: Ty) -> Option<Ty> {
    element
        .non_null()
        .is_unsigned()
        .then_some(element.non_null())
        .or_else(|| boxed_prim_of(element))
}

impl Emitter<'_> {
    pub(super) fn emit_array_get(&mut self, array: u32, index: u32, code: &mut CodeBuilder) {
        let element = self.array_elem(array);
        let reference_array = self.value_ty(array).is_reference_array();
        if self.must_spill_across(index) {
            self.emit_operands(&[array, index], code);
        } else {
            self.emit_value(array, code);
            self.emit_value(index, code);
        }
        let (operation, words) = array_load_op(element, reference_array);
        code.array_load(operation, words);
        if let Some(primitive) = reference_array
            .then(|| reference_array_scalar_adapter(element))
            .flatten()
        {
            let array_descriptor = type_descriptor(self.value_ty(array));
            let component = array_descriptor
                .strip_prefix('[')
                .unwrap_or("Ljava/lang/Object;");
            unbox_prim_from_descriptor(self.cw, code, component, primitive);
        }
        // `Array<UIntArray>` holds the box. The Kotlin value of that element is the carrier, so a
        // following index can load a primitive from it.
        if reference_array {
            if let Some(owner) = crate::jvm::names::boxed_primitive_array_element(element) {
                let owner = crate::jvm::names::classfile_internal_name_of(owner);
                let descriptor = format!("(){}", type_descriptor(element));
                let method = self.cw.methodref(owner, "unbox-impl", &descriptor);
                code.invokevirtual(method, 0, 1);
            }
        }
    }

    pub(super) fn emit_array_set(
        &mut self,
        array: u32,
        index: u32,
        value: u32,
        code: &mut CodeBuilder,
    ) {
        let element = self.array_elem(array);
        let reference_array = self.value_ty(array).is_reference_array();
        if self.must_spill_across(index) || self.must_spill_across(value) {
            self.emit_operands(&[array, index, value], code);
        } else {
            self.emit_value(array, code);
            self.emit_value(index, code);
            self.emit_value(value, code);
        }
        if let Some(primitive) = reference_array
            .then(|| reference_array_scalar_adapter(element))
            .flatten()
        {
            box_prim_free(self.cw, code, primitive);
        }
        let (operation, words) = array_store_op(element, reference_array);
        code.array_store(operation, words);
    }
}
