//! What the checked classifier hierarchy publishes to common IR: the applied supertypes of each
//! source classifier, and what an override inherits from the declarations it overrides.

use super::{lower_single_source, lower_single_source_with_jvm_stdlib};
use crate::types::{SemanticCallRole, Ty};

#[test]
fn common_ir_receives_the_complete_applied_classifier_hierarchy() {
    let ir = lower_single_source(
        "interface Root<T>\n\
         interface Middle<U> : Root<U>\n\
         class Leaf : Middle<String>\n",
        "Hierarchy",
    );
    let leaf = crate::types::type_name("Leaf");
    let middle = crate::types::type_name("Middle");
    let root = crate::types::type_name("Root");
    let hierarchy = ir
        .classifier_hierarchies
        .get(&leaf)
        .expect("source class hierarchy must cross the FIR/common-IR boundary");

    assert_eq!(
        hierarchy
            .iter()
            .map(|entry| (entry.classifier, entry.applied, entry.depth))
            .collect::<Vec<_>>(),
        vec![
            (leaf, Ty::obj("Leaf"), 0),
            (middle, Ty::obj_args("Middle", &[Ty::String]), 1),
            (root, Ty::obj_args("Root", &[Ty::String]), 2),
            // A classifier that declares no supertype still has Kotlin's implicit root.
            (crate::types::wk::any(), Ty::obj("kotlin/Any"), 3),
        ]
    );
    ir.validate_determined_types()
        .expect("applied hierarchy types must be pending-free");
}

/// An override of `Any.toString`, and an override of that override, both play its role: the
/// frontend's override resolution publishes the inherited role and lowering carries it to IR.
#[test]
fn overrides_of_any_to_string_carry_its_role_into_ir() {
    let ir = lower_single_source_with_jvm_stdlib(
        "open class Base { override fun toString(): String = \"B\" }\n\
         class Leaf : Base() { override fun toString(): String = \"L\" }\n\
         class Plain { fun describe(): String = \"P\" }\n",
        "InheritedRoles",
    );
    let mut roles = ir
        .checked_callable_functions
        .iter()
        .filter_map(|(callable, &function)| {
            ir.callable_semantic_roles
                .get(callable)
                .map(|role| (ir.functions[function as usize].name.clone(), *role))
        })
        .collect::<Vec<_>>();
    roles.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        roles,
        vec![
            ("toString".to_owned(), SemanticCallRole::KotlinAnyToString),
            ("toString".to_owned(), SemanticCallRole::KotlinAnyToString),
        ]
    );
}
