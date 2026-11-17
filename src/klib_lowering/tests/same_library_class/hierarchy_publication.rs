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
    lower_fixture(
        &mut unit,
        &libraries,
        "make",
        &Default::default(),
        &libraries,
    );
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
    lower_fixture(
        &mut unit,
        &libraries,
        "other",
        &Default::default(),
        &libraries,
    );
    assert_eq!(unit.ir().classifier_hierarchies[&child], before);
    let unrelated = &unit.ir().classifier_hierarchies[&type_name("fixture/Unrelated")];
    assert!(unrelated.iter().all(
        |entry| ![type_name("fixture/Root"), type_name("fixture/Leaf")].contains(&entry.classifier)
    ));
    validated(unit.ir());
    let _ = std::fs::remove_dir_all(directory);
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
