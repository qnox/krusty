use super::common;

// PLATFORM-SPECIFIC: the JVM driver compares every emitted class, including local accessors and
// nested lambda classes; the canonical Kotlin programs remain complete portable inputs.
fn assert_complete_jvm_inventory(stem: &str, source: &str) {
    let filename = format!("{stem}.kt");
    let classes = common::classes_against_kotlinc_module(&[(&filename, source)]);
    assert_eq!(
        classes
            .krusty
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        classes
            .reference
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        "complete {stem} class inventory",
    );
    for (name, expected) in &classes.reference {
        assert_eq!(
            &classes.krusty[name], expected,
            "complete {stem} class {name}"
        );
    }
}

// PLATFORM-SPECIFIC: retain the full official JVM class and runtime oracle for the canonical
// whole inline-capture replay program; the same Kotlin input enters the shared backend matrix.
#[test]
fn published_inline_captures_keep_parameter_and_local_declaration_identity() {
    let source = include_str!("box_regressions/PublishedInlineCaptureBindings.kt");
    assert_complete_jvm_inventory("PublishedInlineCaptureBindings", source);
    common::expect_box_same_as_kotlinc(source, "PublishedInlineCaptureBindingsRun");
}
