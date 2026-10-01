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
    declare_with(env, owner, cw, ACCESS);
}

/// Declare an interface's array: every interface field is `public`, so kotlinc's is too.
pub(super) fn declare_in_interface(env: &EmitEnv<'_>, owner: TypeName, cw: &mut ClassWriter) {
    declare_with(env, owner, cw, ACCESS | 0x0001);
}

fn declare_with(env: &EmitEnv<'_>, owner: TypeName, cw: &mut ClassWriter, access: u16) {
    if exists(env, owner) {
        let field = (access, array::FIELD, array::DESCRIPTOR);
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

impl<'a> EmitEnv<'a> {
    /// The local delegated properties each class's Kotlin metadata lists.
    pub(super) fn local_delegated(&self) -> &'a LocalDelegatedProperties {
        &self.property_reference_realizations.local_delegated
    }
}
