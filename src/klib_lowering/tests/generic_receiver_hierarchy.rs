//! A dependency member's declaring class can be a generic supertype of its receiver. Its applied
//! arguments come from the caller's explicit class evidence, including captured projections.

use super::{distribution_root, stdlib};
use crate::assignable::TypeOracle;
use crate::klib_lowering::KlibCalleeFacts;
use crate::types::{type_name, Ty, TypeName};

struct ReorderedHierarchy;

impl TypeOracle for ReorderedHierarchy {
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
        match (ty.kotlin_class_internal(), ty.type_args()) {
            (Some(owner), [element]) if owner == type_name("fixture/Child") => {
                vec![Ty::obj_args("fixture/Middle", &[Ty::Int, *element])]
            }
            (Some(owner), [first, second]) if owner == type_name("fixture/Middle") => {
                vec![Ty::obj_args("fixture/Parent", &[*second, *first])]
            }
            _ => Vec::new(),
        }
    }
}

#[test]
fn a_dependency_receiver_keeps_applied_arguments_through_its_authenticated_hierarchy() {
    let projected = Ty::out_projection(Ty::nullable(Ty::String));
    let receiver = Ty::obj_args("fixture/Child", &[projected]);
    let parent = type_name("fixture/Parent");
    let facts = KlibCalleeFacts::default().with_type_oracle(&ReorderedHierarchy);
    assert_eq!(
        facts.applied_supertype(receiver, parent),
        Some(Ty::obj_args_name(parent, &[projected, Ty::Int]))
    );
    assert_eq!(
        facts.applied_supertype(receiver, type_name("fixture/Unrelated")),
        None
    );
    // Absence of class evidence permits reflexivity, without inventing an inherited edge.
    let no_classes = KlibCalleeFacts::default();
    assert_eq!(
        no_classes.applied_supertype(receiver, type_name("fixture/Child")),
        Some(receiver)
    );
    assert_eq!(no_classes.applied_supertype(receiver, parent), None);
}

#[test]
fn the_stdlib_property_receiver_preserves_its_projection_at_the_callable_declaration() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let oracle = crate::symbol_resolver::SourceOracle(&libraries);
    let facts = KlibCalleeFacts::default().with_type_oracle(&oracle);
    let projected = Ty::out_projection(Ty::nullable(Ty::obj_name(crate::types::wk::any())));
    let callable: TypeName = type_name("kotlin/reflect/KCallable");
    let expected = Ty::obj_args_name(callable, &[projected]);
    for property in ["kotlin/reflect/KProperty", "kotlin/reflect/KProperty0"] {
        assert_eq!(
            facts.applied_supertype(Ty::obj_args(property, &[projected]), callable),
            Some(expected),
            "the provider-owned hierarchy of {property} retains its applied element"
        );
    }
}
