//! Declaring and filling a class's `$$delegatedProperties`, the array
//! [`crate::jvm::property_references::DelegatedPropertyArrays`] chose for its delegated-property
//! operands.

use super::*;
use crate::jvm::property_references::delegated_arrays as array;

/// `static final synthetic`, package-private: kotlinc's access for the array.
const ACCESS: u16 = 0x1018;

fn elements<'a>(env: &EmitEnv<'a>, owner: TypeName) -> Option<&'a [crate::ir::ExprId]> {
    env.property_reference_realizations
        .delegated_arrays
        .elements(owner)
}

/// Whether `owner` has an array, so its `<clinit>` must exist to fill it.
pub(super) fn exists(env: &EmitEnv<'_>, owner: TypeName) -> bool {
    elements(env, owner).is_some()
}

/// Declare `owner`'s array at the head of its field table, interned with the field visit.
pub(super) fn declare(env: &EmitEnv<'_>, owner: TypeName, cw: &mut ClassWriter) {
    if exists(env, owner) {
        let field = (ACCESS, array::FIELD, array::DESCRIPTOR);
        cw.add_field_late_leading(field, Some(array::SIGNATURE), None);
    }
}

impl Emitter<'_> {
    /// Fill `owner`'s array, as kotlinc's `<clinit>` does before anything else: build it through a
    /// local, one element per slot, then store it. Such a `<clinit>` has no line table in kotlinc.
    /// `internal_name` is `owner`'s class-file spelling, which the field reference names.
    pub(super) fn emit_delegated_property_array(
        &mut self,
        env: &EmitEnv<'_>,
        owner: TypeName,
        internal_name: &str,
        code: &mut CodeBuilder,
    ) {
        let Some(elements) = elements(env, owner) else {
            return;
        };
        super::vararg::emit_packed_array(self, &array::array_type(), elements, code);
        let reference = self
            .cw
            .fieldref(internal_name, array::FIELD, array::DESCRIPTOR);
        code.putstatic(reference, 1);
        self.cw.omit_method_lines("<clinit>", "()V");
    }
}
