//! The contents a generated `toString` shows for an array property.
//!
//! kotlinc renders a data class's array property by its contents rather than by the array's own
//! identity `toString`: `[` and each element's `toString()` separated by `, ` and then `]`, or
//! `null` for a null array. This is `Arrays.toString` on the JVM and `contentToString()` on the
//! other targets, so `data class D(val xs: IntArray)` prints `D(xs=[1, 2])`. Each element renders
//! as a string template renders a value of the array's element type.

use super::ElementRead;
use super::*;

impl Body<'_> {
    /// Push the contents rendering of property value `value`, declared of array type `ty`.
    /// Every array this representation lays out is rendered; one it declines to store, or whose
    /// element a string template cannot render, is declined here too.
    pub(in super::super) fn generated_array_to_string(
        &mut self,
        value: u32,
        ty: Ty,
    ) -> Result<(), Unsupported> {
        let array = ty.non_null();
        let (form, stored) = self.array_form(array)?;
        let element = if array.is_reference_array() {
            Some(
                array
                    .array_read_elem()
                    .ok_or_else(|| "a reference array with no readable element type".to_string())?,
            )
        } else if !matches!(
            stored,
            Ty::Boolean | Ty::Byte | Ty::Short | Ty::Char | Ty::Int | Ty::Long
        ) {
            return Err(format!(
                "the content rendering of a `{}` property",
                describe(ty)
            ));
        } else {
            None
        };
        let holder = self.local(ValType::nullable(form.object));
        let elements = self.local(ValType::reference(form.storage));
        let index = self.local(ValType::I32);
        let text = self.local(STRING);
        let concat = self.runtime.string_concat;
        self.value(value, ty)?;
        self.code
            .ref_cast(true, HeapType::Concrete(form.object))
            .local_tee(holder)
            .ref_is_null()
            .if_(BlockType::Value(STRING));
        self.string_constant("null");
        self.code
            .else_()
            .local_get(holder)
            .ref_as_non_null()
            .struct_get(form.object, super::STORAGE_FIELD)
            .local_set(elements);
        self.string_constant("[");
        self.code
            .local_set(text)
            .i32_const(0)
            .local_set(index)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(index)
            .local_get(elements)
            .ref_as_non_null()
            .array_len()
            .op(GE_S32)
            .br_if(1)
            // Every element after the first is preceded by the separator.
            .local_get(index)
            .if_(BlockType::Empty)
            .local_get(text)
            .ref_as_non_null();
        self.string_constant(", ");
        self.code
            .call(concat)
            .local_set(text)
            .end()
            .local_get(text)
            .ref_as_non_null()
            .local_get(elements)
            .ref_as_non_null()
            .local_get(index);
        match form.read {
            ElementRead::Plain => {
                self.code.array_get(form.storage);
            }
            ElementRead::Signed => {
                self.code.array_get_s(form.storage);
            }
            ElementRead::Unsigned => {
                self.code.array_get_u(form.storage);
            }
        }
        match element {
            None => self.stacked_string_of(stored)?,
            Some(element) => {
                // The physical element is eqref; the ordinary coercion casts it to the element's
                // own carrier, as a checked read of it does.
                self.coerce_emitted_value(
                    Ty::nullable(Ty::obj_name(krusty::types::wk::any())),
                    element,
                )?;
                self.stacked_string_of(element)?;
            }
        }
        self.code
            .call(concat)
            .local_set(text)
            .local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0)
            .end()
            .end()
            .local_get(text)
            .ref_as_non_null();
        self.string_constant("]");
        self.code.call(concat).end().ref_as_non_null();
        Ok(())
    }
}
