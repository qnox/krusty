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

/// How `class`'s `@Metadata` numbers the type parameters it captures. kotlinc serializes a class
/// declared in executable code with no enclosing serializer, so its captured parameters are
/// numbered on first use. A class nested in another is serialized under the outer one, whose
/// interner already holds every enclosing class's parameters: those keep the ids before the nested
/// class's own whether or not an `inner` class captures them.
pub(super) fn captured_type_parameters<'a>(
    ir: &IrFile,
    class: &'a IrClass,
) -> crate::metadata::class_builder::CapturedTypeParameters<'a> {
    use crate::metadata::class_builder::CapturedTypeParameters;
    if class.is_local_class || class.is_anonymous_object {
        return CapturedTypeParameters::NumberedOnUse(&class.captured_type_params);
    }
    let enclosing = class
        .fq_name_id()
        .existing_nested_owners()
        .into_iter()
        .filter_map(|owner| ir.class_id_by_name(owner))
        .map(|owner| ir.classes[owner as usize].type_params.len())
        .sum();
    CapturedTypeParameters::Reserved {
        enclosing,
        captured: &class.captured_type_params,
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
