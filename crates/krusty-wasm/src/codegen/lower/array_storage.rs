//! The Wasm stdlib's view of an array's elements. Each array classifier declares an internal
//! `storage` property holding the `@WasmArrayOf` GC array of its elements (`IntArray.storage:
//! WasmIntArray`), and `wasm_array_copy` copies a range between two such arrays. This target lays
//! arrays out itself, so it answers the property from its own array object and lowers the copy to
//! `array.copy` over that object's storage type.

use krusty::backend::{BackendCompilerIntrinsic, DependencyGetterSignature};
use krusty::types::{type_name, Ty, TypeName};

use super::super::super::encode::HeapType;
use super::*;

/// Each array classifier, the storage classifier its `storage` property has, and the element
/// whose array form owns that storage: `Any?` for the reference array.
fn storage_classifiers() -> [(TypeName, TypeName, Ty); 9] {
    let any = Ty::nullable(Ty::obj_name(krusty::types::wk::any()));
    [
        ("kotlin/Array", "kotlin/wasm/internal/WasmAnyArray", any),
        (
            "kotlin/BooleanArray",
            "kotlin/wasm/internal/WasmByteArray",
            Ty::Boolean,
        ),
        (
            "kotlin/ByteArray",
            "kotlin/wasm/internal/WasmByteArray",
            Ty::Byte,
        ),
        (
            "kotlin/CharArray",
            "kotlin/wasm/internal/WasmCharArray",
            Ty::Char,
        ),
        (
            "kotlin/ShortArray",
            "kotlin/wasm/internal/WasmShortArray",
            Ty::Short,
        ),
        (
            "kotlin/IntArray",
            "kotlin/wasm/internal/WasmIntArray",
            Ty::Int,
        ),
        (
            "kotlin/LongArray",
            "kotlin/wasm/internal/WasmLongArray",
            Ty::Long,
        ),
        (
            "kotlin/FloatArray",
            "kotlin/wasm/internal/WasmFloatArray",
            Ty::Float,
        ),
        (
            "kotlin/DoubleArray",
            "kotlin/wasm/internal/WasmDoubleArray",
            Ty::Double,
        ),
    ]
    .map(|(array, storage, element)| (type_name(array), type_name(storage), element))
}

/// The element whose array form owns the storage of `array`'s `storage` property, when `result`
/// is that property's declared type.
fn storage_property_element(array: TypeName, name: &str, result: Ty) -> Option<Ty> {
    if name != "storage" {
        return None;
    }
    storage_classifiers()
        .into_iter()
        .find(|(owner, storage, _)| *owner == array && result == Ty::obj_name(*storage))
        .map(|(_, _, element)| element)
}

/// The element whose array form's storage a value of `storage` is.
fn storage_element(storage: Ty) -> Option<Ty> {
    let classifier = storage.obj_internal()?;
    storage_classifiers()
        .into_iter()
        .find(|(_, candidate, _)| *candidate == classifier && storage == Ty::obj_name(classifier))
        .map(|(_, _, element)| element)
}

/// Whether this target answers a library body's read of `getter` itself.
pub(in crate::codegen) fn realizes_storage_getter(getter: DependencyGetterSignature<'_>) -> bool {
    getter.parameters.is_empty()
        && storage_property_element(getter.owner, getter.name, getter.result).is_some()
}

impl Body<'_> {
    /// The element whose array form owns the storage `target` reads, when it is an array
    /// classifier's `storage` property.
    pub(super) fn array_storage_property(
        &self,
        target: krusty::fir::ExternalPropertyId,
    ) -> Option<Ty> {
        let property = self.callables.property(target)?;
        storage_property_element(property.owner, &property.name, property.result)
    }

    /// `array.storage`: the receiver array object's storage array.
    pub(super) fn array_storage_read(
        &mut self,
        element: Ty,
        receiver: u32,
    ) -> Result<(), Unsupported> {
        let (form, _) = self.array_form(Ty::array(element))?;
        let operand = self
            .type_of(receiver)?
            .ok_or_else(|| "an array storage read without a checked receiver type".to_string())?;
        self.value(receiver, operand)?;
        self.array_storage(form);
        Ok(())
    }

    /// Whether `target` is the authenticated `wasm_array_copy`.
    pub(super) fn is_array_storage_copy(&self, target: krusty::fir::ExternalCallableId) -> bool {
        self.callables.callable(target).is_some_and(|fact| {
            fact.compiler_operation() == Some(BackendCompilerIntrinsic::ArrayStorageCopy)
        })
    }

    /// `wasm_array_copy<T>(destination, destinationIndex, source, sourceIndex, length)`: one
    /// `array.copy` over the storage type of `T`, which traps on a null array or a range outside
    /// either, as kotlinc's instruction does.
    pub(super) fn array_storage_copy(
        &mut self,
        target: krusty::fir::ExternalCallableId,
        substitutions: &[krusty::ir::IrCheckedSubstitution],
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let [destination, destination_index, source, source_index, length] = args else {
            return Err("an authenticated array copy with malformed operands".into());
        };
        let storage = substitutions
            .iter()
            .find(|substitution| {
                matches!(
                    substitution.parameter,
                    krusty::fir::FirTypeParameterRef::External { callable, ordinal: 0 }
                        if callable == target
                )
            })
            .ok_or("an array copy without its checked type argument")?
            .value;
        let element = storage_element(storage)
            .ok_or_else(|| format!("an array copy of `{}`", describe(storage)))?;
        let (form, _) = self.array_form(Ty::array(element))?;
        for (array, index) in [(*destination, *destination_index), (*source, *source_index)] {
            self.value(array, storage)?;
            self.code.ref_cast(false, HeapType::Concrete(form.storage));
            self.value(index, Ty::Int)?;
        }
        self.value(*length, Ty::Int)?;
        self.code.array_copy(form.storage, form.storage);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn getter<'a>(owner: &str, name: &'a str, result: &str) -> DependencyGetterSignature<'a> {
        DependencyGetterSignature {
            owner: type_name(owner),
            name,
            parameters: &[],
            result: Ty::obj(result),
            value_declaration: None,
        }
    }

    #[test]
    fn only_an_array_classifiers_own_storage_property_is_realized() {
        for (array, storage, _) in storage_classifiers() {
            assert!(realizes_storage_getter(DependencyGetterSignature {
                owner: array,
                name: "storage",
                parameters: &[],
                result: Ty::obj_name(storage),
                value_declaration: None,
            }));
        }
        let int_storage = "kotlin/wasm/internal/WasmIntArray";
        assert!(!realizes_storage_getter(getter(
            "kotlin/LongArray",
            "storage",
            int_storage
        )));
        assert!(!realizes_storage_getter(getter(
            "kotlin/IntArray",
            "size",
            int_storage
        )));
        assert!(!realizes_storage_getter(getter(
            "spoof/IntArray",
            "storage",
            int_storage
        )));
        assert!(!realizes_storage_getter(DependencyGetterSignature {
            parameters: &[Ty::Int],
            ..getter("kotlin/IntArray", "storage", int_storage)
        }));
        assert!(!realizes_storage_getter(DependencyGetterSignature {
            result: Ty::nullable(Ty::obj(int_storage)),
            ..getter("kotlin/IntArray", "storage", int_storage)
        }));
    }

    #[test]
    fn a_copy_names_its_storage_by_the_exact_storage_classifier() {
        assert_eq!(
            storage_element(Ty::obj("kotlin/wasm/internal/WasmAnyArray")),
            Some(Ty::nullable(Ty::obj_name(krusty::types::wk::any())))
        );
        assert_eq!(
            storage_element(Ty::obj("kotlin/wasm/internal/WasmCharArray")),
            Some(Ty::Char)
        );
        assert_eq!(
            storage_element(Ty::nullable(Ty::obj("kotlin/wasm/internal/WasmIntArray"))),
            None
        );
        assert_eq!(storage_element(Ty::obj("kotlin/IntArray")), None);
    }
}
