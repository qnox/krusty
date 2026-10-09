//! Classifiers declared in executable code, and those nested in them. JVM emission gives them LOCAL
//! metadata visibility, local class ids and no nullability annotations.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrClass, IrFile};
use crate::types::TypeName;

/// kotlinc's `IrClass.isLocal` for a declared classifier: a class declared in executable code (a
/// local class or an anonymous object) or nested, at any depth, in one. An enum entry's body is an
/// anonymous object too, so what it nests is local, although the body itself keeps an ordinary
/// member's nullability annotations. kotlinc's lambda and callable-reference classes are local
/// too; their writers here annotate nothing to begin with.
pub(super) fn is_local(ir: &IrFile, class: &IrClass) -> bool {
    is_local_in(ir, class, &|owner| ir.class_id_by_name(owner))
}

/// [`is_local`], finding each declared owner through `class_id`, which must answer as
/// [`IrFile::class_id_by_name`] does.
fn is_local_in(
    ir: &IrFile,
    class: &IrClass,
    class_id: &dyn Fn(TypeName) -> Option<crate::ir::ClassId>,
) -> bool {
    class.is_local_class
        || class.is_anonymous_object
        || class
            .fq_name_id()
            .existing_nested_owners()
            .into_iter()
            .find_map(class_id)
            .is_some_and(|owner| {
                let owner = &ir.classes[owner as usize];
                owner.is_enum_entry || is_local_in(ir, owner, class_id)
            })
}

/// How `class`'s `@Metadata` numbers the type parameters it captures. kotlinc serializes a class
/// declared in executable code with no enclosing serializer, so its captured parameters are
/// numbered on first use. A class nested in another is serialized under the outer one, whose
/// interner already holds every enclosing class's parameters: `enclosing` (see
/// [`enclosing_type_parameters`]) keep the ids before the nested class's own whether or not an
/// `inner` class captures them.
pub(super) fn captured_type_parameters<'a>(
    class: &'a IrClass,
    enclosing: &'a [String],
) -> crate::metadata::class_builder::CapturedTypeParameters<'a> {
    use crate::metadata::class_builder::CapturedTypeParameters;
    if class.is_local_class || class.is_anonymous_object {
        CapturedTypeParameters::NumberedOnUse(&class.captured_type_params)
    } else {
        CapturedTypeParameters::Reserved(enclosing)
    }
}

/// The semantic identities of every type parameter `class`'s enclosing classes declare, outermost
/// class first.
pub(super) fn enclosing_type_parameters(ir: &IrFile, class: &IrClass) -> Vec<String> {
    let mut owners = class.fq_name_id().existing_nested_owners();
    owners.reverse();
    owners
        .into_iter()
        .filter_map(|owner| ir.class_signature_name(owner))
        .flat_map(|signature| &signature.type_params)
        .map(|parameter| parameter.semantic_name.clone())
        .collect()
}

/// Every classifier of the file whose `@Metadata` class id is local: those for which [`is_local`]
/// holds, and enum entry bodies.
///
/// Each class's metadata asks for this set, so its owner lookups go through one index of the file's
/// classes rather than a scan of them per lookup. Locality starts at a local class, an anonymous
/// object or an enum entry body: a file declaring none of them has no local class id, and its
/// classes' owners need no lookup at all.
pub(super) fn names(ir: &IrFile) -> HashSet<TypeName> {
    if !ir
        .classes
        .iter()
        .any(|class| class.is_local_class || class.is_anonymous_object || class.is_enum_entry)
    {
        return HashSet::new();
    }
    let mut class_ids = HashMap::with_capacity(ir.classes.len());
    for (id, class) in ir.classes.iter().enumerate() {
        // `class_id_by_name` answers with the first class of a name.
        class_ids
            .entry(class.fq_name)
            .or_insert(crate::ir::ClassId::try_from(id).expect("too many classes for a class id"));
    }
    let class_id = |owner: TypeName| class_ids.get(&owner).copied();
    ir.classes
        .iter()
        .filter(|class| class.is_enum_entry || is_local_in(ir, class, &class_id))
        .map(|class| class.fq_name_id())
        .collect()
}

/// The enum entry bodies of the file. Their local class ids keep the entry's `Enum.ENTRY` name
/// rather than the raw internal name a local class's id takes, and so do the ids nested in them.
pub(super) fn enum_entry_bodies(ir: &IrFile) -> HashSet<TypeName> {
    ir.classes
        .iter()
        .filter(|class| class.is_enum_entry)
        .map(|class| class.fq_name_id())
        .collect()
}
