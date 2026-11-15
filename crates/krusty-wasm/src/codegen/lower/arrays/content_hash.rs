//! The contents hash a generated `hashCode` takes of an array property.
//!
//! kotlinc hashes a data class's array property by its contents rather than by the array's own
//! identity `hashCode`: `Arrays.hashCode` on the JVM and `contentHashCode()` on the other targets.
//! A null array hashes as `0`; otherwise the hash starts at `1` and folds each element in as
//! `31 * hash + element.hashCode()`, with `0` for a null element. A primitive element hashes as
//! that primitive's own `hashCode` (an `Int` is itself). A reference element hashes through the
//! runtime, which answers `0` for null. Equality of the same property stays the array's identity.

use krusty::backend::class_tables::AnyMember;

use super::*;

impl Body<'_> {
    /// Push the contents hash of property value `value`, declared of array type `ty`. Every array
    /// this representation lays out is hashed; one it declines to store is declined here too.
    pub(in super::super) fn generated_array_hash(
        &mut self,
        value: u32,
        ty: Ty,
    ) -> Result<(), Unsupported> {
        let array = ty.non_null();
        let (form, stored) = self.array_form(array)?;
        let reference = array.is_reference_array();
        if reference {
            let element = array
                .array_read_elem()
                .ok_or_else(|| "a reference array with no readable element type".to_string())?;
            // Each element answers through its own `hashCode` slot, which must have a body.
            if self.may_reach_unrealized(element, AnyMember::HashCode)? {
                return Err("`kotlin.Any.hashCode`'s identity hash".to_string());
            }
        } else if !self
            .runtime
            .hashing
            .emit_scalar_hash(&mut Code::default(), stored)
        {
            return Err(format!("the content hash of a `{}` property", describe(ty)));
        }
        let holder = self.local(ValType::nullable(form.object));
        let elements = self.local(ValType::reference(form.storage));
        let index = self.local(ValType::I32);
        let hash = self.local(ValType::I32);
        self.value(value, ty)?;
        self.code
            .ref_cast(true, HeapType::Concrete(form.object))
            .local_tee(holder)
            .ref_is_null()
            .if_(BlockType::Value(ValType::I32))
            .i32_const(0)
            .else_()
            .local_get(holder)
            .ref_as_non_null()
            .struct_get(form.object, super::STORAGE_FIELD)
            .local_set(elements);
        if !self.runtime.hashing.emit_array_content_hash(
            &mut self.code,
            crate::hashing::ArrayHash {
                storage: form.storage,
                read: form.read,
                element: stored,
                reference,
                values: elements,
                index,
                hash,
            },
        ) {
            return Err(format!("the content hash of a `{}` property", describe(ty)));
        }
        self.code.end();
        Ok(())
    }
}
