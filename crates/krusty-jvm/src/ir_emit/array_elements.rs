//! JVM element loads and stores for the array intrinsic operations.
//!
//! The array classifiers (`IntArray`, `Array<T>`, ...) have no loadable JVM class: the provider
//! attaches `ArrayGet`/`ArraySet` to their exact element declarations, target realization turns a
//! selected call into that intrinsic, and this module only executes it as array bytecode.

use krusty::jvm::array_representation::{array_load_op, array_store_op};
use krusty::jvm::classfile::CodeBuilder;
use krusty::jvm::names::type_descriptor;
use krusty::types::Ty;

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

/// Box a reference-array element that is still a JVM scalar. An element that is already the boxed
/// value, including `null`, stays as it is: a second unsigned `box-impl` does not accept a reference.
pub(super) fn box_reference_scalar(
    emitter: &mut Emitter<'_>,
    primitive: Option<Ty>,
    produced: Ty,
    code: &mut CodeBuilder,
) {
    if let Some(primitive) = primitive.filter(|_| produced.is_jvm_scalar()) {
        box_prim_free(emitter.cw, code, primitive);
    }
}

impl Emitter<'_> {
    pub(super) fn emit_array_get(
        &mut self,
        array: u32,
        index: u32,
        result: Ty,
        code: &mut CodeBuilder,
    ) {
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
        // A non-null unsigned element is the carrier its consumer compares. A nullable element is
        // the box or `null`; unboxing it would pass that `null` to `unbox-impl`.
        if let Some(primitive) = reference_array
            .then(|| reference_array_scalar_adapter(element))
            .flatten()
            .filter(|_| result.is_jvm_scalar())
        {
            let array_descriptor = type_descriptor(self.value_ty(array));
            let component = array_descriptor
                .strip_prefix('[')
                .unwrap_or("Ljava/lang/Object;");
            unbox_prim_from_descriptor(self.cw, code, component, primitive);
        }
        // `Array<UIntArray>` holds the box, and the load leaves it boxed. The value-class pass owns
        // the boundary where a consumer needs the carrier (the class's `get-pVg5ArA` receiver)
        // and leaves the box where a reference is wanted (`val a: Any = back[1]`).
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
        // `null` and an already-boxed value class are references. Only a carrier still on the
        // stack is passed to `box-impl`.
        let produced = self.value_ty(value);
        let primitive = reference_array
            .then(|| reference_array_scalar_adapter(element))
            .flatten();
        box_reference_scalar(self, primitive, produced, code);
        let (operation, words) = array_store_op(element, reference_array);
        code.array_store(operation, words);
    }
}
