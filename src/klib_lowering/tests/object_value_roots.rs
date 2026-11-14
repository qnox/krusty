//! An object read authenticates its own private initialization without inventing a callable.

use super::*;

#[test]
fn standard_object_values_have_exact_private_initializer_plans() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let facts = KlibCalleeFacts::default();
    for owner in ["kotlin/properties/Delegates", "kotlin/Result$Companion"] {
        let class = libraries
            .klib_body_class_contract(type_name(owner))
            .expect("object contract");
        let plan = super::super::class_initialization::object_plan(
            &class,
            libraries.declaration_bodies(),
            &facts,
        )
        .expect("exact object initializer")
        .expect("ordinary object initialization");
        assert_eq!(plan.header.contract, &class);
        assert!(plan.header.function.constructor);
        assert!(plan.header.function.regular_parameters.is_empty());
        assert_eq!(
            crate::metadata::semantic::declaration_visibility(plan.header.function.base.flags),
            Visibility::Private
        );
        let mut foreign = class.clone();
        foreign.module.push_str("-foreign");
        assert!(matches!(
            super::super::class_initialization::object_plan(
                &foreign,
                libraries.declaration_bodies(),
                &facts,
            ),
            Ok(None) | Err(_)
        ));
        let mut mismatched = class.clone();
        mismatched.classifier = type_name("foreign/Object");
        assert!(super::super::class_initialization::object_plan(
            &mismatched,
            libraries.declaration_bodies(),
            &facts,
        )
        .is_err());
    }
}

#[test]
fn value_only_objects_publish_instances_without_constructor_or_callable_exports() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let objects = ["kotlin/properties/Delegates", "kotlin/Result$Companion"].map(|name| {
        libraries
            .klib_body_class_contract(type_name(name))
            .expect("object declaration")
    });
    let facts = KlibCalleeFacts::default()
        .with_objects(&objects, [])
        .unwrap();
    let mut unit = DependencyBodyUnit::default();
    unit.lower_objects_with_declarations(
        &objects,
        libraries.declaration_bodies(),
        &facts,
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &Default::default(),
        &libraries,
    )
    .unwrap();
    assert!(unit.initializing_constructors().next().is_some());
    assert_eq!(unit.allocating_constructors().count(), 0);
    assert_eq!(unit.callable_functions().count(), 0);
    let mut ir = unit.into_ir();
    let mut exports = crate::backend::DependencyExportSignatures::default();
    crate::backend::construction_entries::realize(&mut ir, &mut exports);
    assert_eq!(exports.constructors().len(), 0);
    assert_eq!(exports.callables().len(), 0);
    assert_eq!(exports.instance_entries().len(), 2);
    validated(&ir);
}

#[test]
fn missing_object_initialization_declines_and_rolls_back_the_whole_batch() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let object = libraries
        .klib_body_class_contract(type_name("kotlin/properties/Delegates"))
        .expect("object declaration");
    let mut missing = object.clone();
    missing.module = "absent-producer".into();
    let objects = [object.clone(), missing.clone()];
    assert!(KlibCalleeFacts::default()
        .with_objects(&objects, [])
        .is_err());

    let mut other = libraries
        .klib_body_class_contract(type_name("kotlin/Result$Companion"))
        .expect("other object declaration");
    other.module = "absent-producer".into();
    let objects = [object, other];
    let facts = KlibCalleeFacts::default()
        .with_objects(&objects, [])
        .unwrap();
    let mut unit = DependencyBodyUnit::default();
    assert!(unit
        .lower_objects_with_declarations(
            &objects,
            libraries.declaration_bodies(),
            &facts,
            crate::klib_libraries::KlibIntrinsicFacts::empty(),
            &Default::default(),
            &libraries
        )
        .is_err());
    assert!(unit.initializing_constructors().next().is_none());
    assert_eq!(unit.ir().classes.len(), 0);
    assert_eq!(unit.ir().functions.len(), 0);
    assert_eq!(unit.callable_functions().count(), 0);

    // A positively acknowledged earlier getter replaces initialization, never a declaration alone.
    let facts = KlibCalleeFacts::default()
        .with_objects([&missing], [(missing.classifier, missing.identity())])
        .unwrap();
    unit.lower_objects_with_declarations(
        std::slice::from_ref(&missing),
        libraries.declaration_bodies(),
        &facts,
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &Default::default(),
        &libraries,
    )
    .unwrap();
    assert!(unit.initializing_constructors().next().is_none());
    assert_eq!(unit.ir().classes.len(), 0);
}
