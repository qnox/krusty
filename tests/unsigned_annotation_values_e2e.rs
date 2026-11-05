//! JVM annotation element tags preserve the complete checked unsigned value inventory.

use super::common;

#[test]
fn unsigned_arrays_match_every_complete_reference_class() {
    let source = include_str!("fixtures/unsigned_annotation_values/UnsignedAnnotations.kt");
    let classes = common::classes_against_kotlinc_module(&[("UnsignedAnnotations.kt", source)]);
    let expected = [
        "unsignedannotations/Bits",
        "unsignedannotations/Subject",
        "unsignedannotations/UnsignedAnnotationsKt",
    ];
    assert_eq!(
        classes
            .krusty
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        expected,
        "complete emitted class inventory"
    );
    assert_eq!(
        classes
            .reference
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        expected,
        "complete official class inventory"
    );
    for (name, actual) in &classes.krusty {
        assert!(
            actual == &classes.reference[name],
            "complete class bytes differ for {name}"
        );
    }
}
