//! Classifiers declared in executable code, and those nested in them. JVM emission gives them LOCAL
//! metadata visibility, local class ids and no nullability annotations.

use std::collections::HashSet;

use crate::ir::{IrClass, IrFile};
use crate::types::TypeName;

/// kotlinc's `IrClass.isLocal` for a declared classifier: a class declared in executable code (a
/// local class or an anonymous object) or nested, at any depth, in one. An enum entry's body is an
/// anonymous object too, so what it nests is local, although the body itself keeps an ordinary
/// member's nullability annotations. kotlinc's lambda and callable-reference classes are local
/// too; their writers here annotate nothing to begin with.
pub(super) fn is_local(ir: &IrFile, class: &IrClass) -> bool {
    class.is_local_class
        || class.is_anonymous_object
        || class
            .fq_name_id()
            .existing_nested_owners()
            .into_iter()
            .find_map(|owner| ir.class_id_by_name(owner))
            .is_some_and(|owner| {
                let owner = &ir.classes[owner as usize];
                owner.enum_entry_of.is_some() || is_local(ir, owner)
            })
}

/// The type parameters a nested class's `@Metadata` reserves ids for: every enclosing class's own
/// parameters, outermost first. kotlinc serializes a nested class under its parent's serializer,
/// whose interner eagerly holds the parent's parameters — so the nested class's own parameters
/// continue after them whether or not anything references them (`class Outer<E> { class Nested<E> }`
/// gives the nested `E` id 1).
pub(super) fn enclosing_type_parameters(ir: &IrFile, class: &IrClass) -> Vec<String> {
    let mut owners = class.fq_name_id().existing_nested_owners();
    owners.reverse(); // recorded deepest-first; ids count from the outermost class
    owners
        .into_iter()
        .flat_map(|owner| {
            // A same-file nested class's owner always has a class signature; a miss here silently
            // shifts every reserved id after it.
            let signature = ir.class_signature_name(owner);
            debug_assert!(
                signature.is_some(),
                "nested class owner has no class signature"
            );
            signature
                .map(|signature| {
                    signature
                        .type_params
                        .iter()
                        .map(|parameter| parameter.semantic_name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// How `class`'s `@Metadata` numbers the type parameters it captures. kotlinc serializes a class
/// declared in executable code with no enclosing serializer, so its captured parameters are
/// numbered on first use; a class nested in another is serialized under the outer one, whose
/// parameters hold the ids before its own (see [`enclosing_type_parameters`]).
pub(super) fn captured_type_parameters<'a>(
    ir: &IrFile,
    class: &'a IrClass,
    enclosing: &'a [String],
) -> crate::metadata::class_builder::CapturedTypeParameters<'a> {
    use crate::metadata::class_builder::CapturedTypeParameters;
    if class.is_local_class || class.is_anonymous_object {
        CapturedTypeParameters::NumberedOnUse(&class.captured_type_params)
    } else if is_local(ir, class) {
        // Only a class nested in an enum entry's body reaches this branch — one nested in a local
        // class or anonymous object is itself a local class (`is_local_class`) and takes the first
        // branch. An enum entry body cannot capture a type parameter, so the reservation is empty
        // and the numbering choice is moot; the recorded captures are kept as the reservation.
        CapturedTypeParameters::Reserved(&class.captured_type_params)
    } else {
        CapturedTypeParameters::Reserved(enclosing)
    }
}

/// Every classifier of the file whose `@Metadata` class id is local: those for which [`is_local`]
/// holds, and enum entry bodies.
pub(super) fn names(ir: &IrFile) -> HashSet<TypeName> {
    ir.classes
        .iter()
        .filter(|class| class.enum_entry_of.is_some() || is_local(ir, class))
        .map(|class| class.fq_name_id())
        .collect()
}

/// The enum entry bodies of the file. Their local class ids keep the entry's `Enum.ENTRY` name
/// rather than the raw internal name a local class's id takes, and so do the ids nested in them.
pub(super) fn enum_entry_bodies(ir: &IrFile) -> HashSet<TypeName> {
    ir.classes
        .iter()
        .filter(|class| class.enum_entry_of.is_some())
        .map(|class| class.fq_name_id())
        .collect()
}
