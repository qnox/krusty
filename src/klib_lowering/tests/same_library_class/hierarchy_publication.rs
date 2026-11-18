//! Materialized classes publish the checked applied hierarchy, including inherited interfaces.

use super::*;
use crate::ir::{IrClass, IrFile};
use crate::types::type_name;

const LIBRARY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/klib_materialized_hierarchy/Library.kt"
));

#[test]
fn a_produced_class_publishes_its_inherited_applied_interfaces() {
    let Some((directory, libraries)) = produce_library("materialized-hierarchy", LIBRARY) else {
        return;
    };
    let mut unit = DependencyBodyUnit::default();
    lower_factory(&mut unit, &libraries, "make", &["Child", "Parent"]);
    let child = type_name("fixture/Child");
    let hierarchy = &unit.ir().classifier_hierarchies[&child];
    for (name, depth) in [
        ("fixture/Parent", 1),
        ("fixture/Leaf", 2),
        ("fixture/Root", 3),
    ] {
        let entry = hierarchy
            .iter()
            .find(|entry| entry.classifier == type_name(name))
            .unwrap_or_else(|| panic!("missing {name}: {hierarchy:?}"));
        assert_eq!(entry.applied, Ty::obj_args(name, &[Ty::String]));
        assert_eq!(entry.depth, depth);
    }
    let before = hierarchy.clone();
    lower_factory(&mut unit, &libraries, "other", &["Unrelated"]);
    assert_eq!(unit.ir().classifier_hierarchies[&child], before);
    let unrelated = &unit.ir().classifier_hierarchies[&type_name("fixture/Unrelated")];
    assert!(unrelated.iter().all(
        |entry| ![type_name("fixture/Root"), type_name("fixture/Leaf")].contains(&entry.classifier)
    ));
    validated(unit.ir());
    let _ = std::fs::remove_dir_all(directory);
}

/// Public constructor calls need the selected provider headers, just as an emitted call does.
/// Keep the existing private-class fixture helper's smaller contract unchanged.
fn lower_factory(
    unit: &mut DependencyBodyUnit,
    libraries: &crate::klib_libraries::KlibLibraries,
    name: &str,
    constructed_classes: &[&str],
) {
    use crate::symbol_source::SymbolNamespace;

    let package = SymbolNamespace::Package(type_name("fixture"));
    let symbols = libraries.symbols(package, name);
    let [function] = symbols.callables.functions() else {
        panic!("one producer factory {name}")
    };
    let selected = libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap();
    let root = selected.klib_body_callable().unwrap();
    let constructors = constructed_classes
        .iter()
        .flat_map(|name| {
            let symbols = libraries.symbols(package, name);
            let classifier = symbols.classifier.as_ref().expect("producer class");
            assert_eq!(classifier.constructors.len(), 1);
            classifier
                .constructors
                .iter()
                .map(|constructor| {
                    let target = constructor.external_identity.unwrap();
                    (target, libraries.external_callable(target).unwrap())
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let callables = std::iter::once(root)
        .chain(constructors.iter().map(|(_, constructor)| {
            assert!(constructor.callable.params.is_empty());
            constructor.klib_body_callable().unwrap()
        }))
        .collect::<Vec<_>>();
    let checked = crate::resolve::for_checked_klib_bodies(
        libraries,
        libraries.declaration_bodies(),
        callables.iter().copied(),
        &[],
    )
    .unwrap_or_else(|identity| panic!("{name}: no checked facts for {identity:?}"));
    let callees = crate::klib_lowering::KlibCalleeFacts::new(callables.iter().copied())
        .with_checked_dependency_facts(&checked);
    let externals = constructors
        .iter()
        .map(|(target, constructor)| {
            (
                constructor.klib_body_callable().unwrap().identity().clone(),
                crate::klib_lowering::EmittedDependencyCallable::constructor(*target),
            )
        })
        .collect::<ProducedExternals>();
    unit.lower_function_with_declarations(
        root,
        libraries.declaration_bodies(),
        &callees,
        libraries.intrinsic_facts(),
        &externals,
        libraries,
    )
    .unwrap_or_else(|decline| panic!("{name}: {decline}"));
}

#[test]
fn a_declined_publication_rolls_back_only_the_appended_hierarchies() {
    use crate::klib_lowering::{class_hierarchies, lowering_checkpoint::LoweringCheckpoint};

    let mut ir = IrFile::default();
    let retained = type_name("fixture/Retained");
    ir.add_class(IrClass::synthetic(retained));
    class_hierarchies::publish(&mut ir, 0, None).unwrap();
    let before = ir.classifier_hierarchies.clone();
    let checkpoint = LoweringCheckpoint::of(&ir);
    let appended = type_name("fixture/Appended");
    ir.add_class(IrClass::synthetic(appended));
    let mut malformed = IrClass::synthetic(type_name("fixture/MissingApplication"));
    malformed.type_params.push("T".to_owned());
    ir.add_class(malformed);
    let decline = class_hierarchies::publish(&mut ir, 1, None).unwrap_err();
    assert_eq!(
        decline,
        crate::klib_lowering::KlibBodyDeclineReason::SignatureMismatch(
            "a generic materialized class without its self application".to_owned()
        )
    );
    assert!(ir.classifier_hierarchies.contains_key(&appended));
    checkpoint.restore(&mut ir);
    assert_eq!(ir.classes.len(), 1);
    assert_eq!(ir.classifier_hierarchies, before);
    ir.add_class(IrClass::synthetic(appended));
    class_hierarchies::publish(&mut ir, 1, None).unwrap();
    assert!(ir.classifier_hierarchies.contains_key(&appended));
}
