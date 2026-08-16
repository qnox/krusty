//! kotlinc emits an annotation's synthetic implementation class only where the annotation is
//! CONSTRUCTED (`Marker("x")`), one per construction site. Declaring an annotation emits nothing
//! extra. krusty emitted one per DECLARATION, so every module that merely declares annotations —
//! which in intellij-community is most of them — carried class files kotlinc never writes, and could
//! not be byte-compared at all.
//!
//! Still divergent, see `docs/SPEC.md`: the implementation's NAME, and an annotation used only as an
//! annotation ARGUMENT (`@Outer(Inner("x"))`), which kotlinc encodes directly into the class file.

use super::common;

fn emitted_classes(source: &str) -> Option<Vec<String>> {
    let classes = common::compile_in_process(
        source,
        "Main",
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    )?;
    let mut names: Vec<String> = classes.into_iter().map(|(name, _)| name).collect();
    names.sort();
    Some(names)
}

#[test]
fn a_declared_annotation_emits_no_implementation_class() {
    let source = r#"
        annotation class Plain
        annotation class WithDefault(val name: String = "")
    "#;

    let Some(names) = emitted_classes(source) else {
        return;
    };
    assert!(
        !names.iter().any(|name| name.contains("annotationImpl")),
        "a declared-only annotation needs no implementation class: {names:?}"
    );
}

#[test]
fn a_constructed_annotation_still_emits_its_implementation_class() {
    let source = r#"
        annotation class Marker(val name: String = "")

        fun make(): Marker = Marker("x")
    "#;

    let Some(names) = emitted_classes(source) else {
        return;
    };
    assert!(
        names.iter().any(|name| name.contains("annotationImpl")),
        "constructing an annotation needs its implementation class: {names:?}"
    );
}

#[test]
fn a_constructed_annotation_reads_back_its_argument() {
    let source = r#"
        annotation class Marker(val name: String = "")

        fun box(): String = Marker("OK").name
    "#;

    assert_eq!(
        common::compile_and_run_box(
            source,
            "Main",
            std::slice::from_ref(&common::stdlib_jar()),
            Some(common::jdk_modules().as_path()),
        )
        .as_deref(),
        Some("OK")
    );
}
