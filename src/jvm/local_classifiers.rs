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
    // Only a source member classifier is serialized under enclosing class serializers. A local,
    // anonymous, lambda, callable-reference, continuation, or other generated class may have a
    // physically nested JVM name, but that name is not a Kotlin declaration-owner edge and must
    // not reserve metadata parameter ids.
    if !class.is_source_declared || is_local(ir, class) {
        return Vec::new();
    }
    let mut owners = Vec::new();
    let mut owner = class.declaration_owner;
    while let Some(enclosing) = owner {
        let Some(enclosing_class) = ir
            .class_id_by_name(enclosing)
            .map(|id| &ir.classes[id as usize])
        else {
            // Without one structural link the joint metadata id space is unknowable. Reserve
            // nothing rather than shifting the class's own parameters onto a different id.
            return Vec::new();
        };
        owners.push(enclosing_class);
        owner = enclosing_class.declaration_owner;
    }
    owners.reverse(); // walked innermost-first; ids count from the outermost class
    owners
        .into_iter()
        .flat_map(|owner_class| {
            if owner_class.type_params.is_empty() {
                // Nongeneric declarations legitimately have no `IrGenericSig` entry and reserve
                // no ids. Treating that absence as malformed made every ordinary nested class
                // panic during metadata emission.
                return Vec::new();
            }
            let signature = ir
                .class_signature_name(owner_class.fq_name_id())
                .expect("a generic source classifier has a class signature");
            signature
                .type_params
                .iter()
                .map(|parameter| parameter.semantic_name.clone())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_class(ir: &mut IrFile, name: TypeName, type_params: &[&str]) {
        let mut class = IrClass::synthetic(name);
        class.is_source_declared = true;
        class.type_params = type_params.iter().map(|name| name.to_string()).collect();
        if !type_params.is_empty() {
            ir.insert_class_signature_name(
                name,
                crate::ir::IrGenericSig {
                    type_params: type_params
                        .iter()
                        .map(|name| crate::ir::IrTypeParameter {
                            name: name.to_string(),
                            semantic_name: name.to_string(),
                            bounds: Vec::new(),
                            variance: crate::types::TypeVariance::Invariant,
                            reified: false,
                        })
                        .collect(),
                    params: Vec::new(),
                    ret: None,
                    supers: Vec::new(),
                },
            );
        }
        ir.add_class(class);
    }

    #[test]
    fn executable_and_generated_classes_do_not_infer_metadata_owners_from_their_jvm_names() {
        let ir = IrFile::default();

        let generated = IrClass::synthetic(crate::types::type_name("fixture/FileKt$lambda$1"));
        assert!(enclosing_type_parameters(&ir, &generated).is_empty());

        let mut local = IrClass::synthetic(crate::types::type_name("fixture/FileKt$body$Local"));
        local.is_source_declared = true;
        local.is_local_class = true;
        assert!(enclosing_type_parameters(&ir, &local).is_empty());
    }

    #[test]
    fn a_nongeneric_source_owner_reserves_no_metadata_parameter_ids() {
        let outer = crate::types::type_name("fixture/Outer");
        let nested = crate::types::type_name_nested_child(outer, "Nested");
        let mut ir = IrFile::default();
        source_class(&mut ir, outer, &[]);
        let mut nested_class = IrClass::synthetic(nested);
        nested_class.is_source_declared = true;
        nested_class.declaration_owner = Some(outer);
        assert!(enclosing_type_parameters(&ir, &nested_class).is_empty());
    }

    /// A `$` inside a top-level backticked classifier name is not an owner boundary:
    /// `fixture/Outer$Literal` declares no owner, so the unrelated `fixture/Outer` declaration and
    /// its parameter `D` reserve no ids for `Outer$Literal`'s own `Inner`.
    #[test]
    fn a_dollar_in_a_source_spelling_is_not_a_declaration_owner_boundary() {
        let outer = crate::types::type_name("fixture/Outer");
        let literal = crate::types::type_name("fixture/Outer$Literal");
        let inner = crate::types::type_name_nested_child(literal, "Inner");
        let mut ir = IrFile::default();
        source_class(&mut ir, outer, &["D"]);
        source_class(&mut ir, literal, &["E"]);
        let mut inner_class = IrClass::synthetic(inner);
        inner_class.is_source_declared = true;
        inner_class.declaration_owner = Some(literal);
        assert_eq!(enclosing_type_parameters(&ir, &inner_class), ["E"]);

        let literal_class = &ir.classes[ir.class_id_by_name(literal).unwrap() as usize];
        assert!(enclosing_type_parameters(&ir, literal_class).is_empty());
    }

    /// A missing link in the declaration-owner chain makes the joint id space unknowable; the
    /// class's own parameters keep their ids instead of being shifted past a guessed reservation.
    #[test]
    fn a_missing_declaration_owner_link_reserves_nothing() {
        let owner = crate::types::type_name("fixture/Missing");
        let nested = crate::types::type_name_nested_child(owner, "Nested");
        let ir = IrFile::default();
        let mut nested_class = IrClass::synthetic(nested);
        nested_class.is_source_declared = true;
        nested_class.declaration_owner = Some(owner);
        assert!(enclosing_type_parameters(&ir, &nested_class).is_empty());
    }
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
