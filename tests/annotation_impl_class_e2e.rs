//! kotlinc emits an annotation's synthetic implementation class only in a source file that
//! CONSTRUCTS it (`Marker("x")`), one per annotation per file. Declaring an annotation emits nothing
//! extra. krusty emitted one per DECLARATION, so every module that merely declares annotations —
//! which in intellij-community is most of them — carried class files kotlinc never writes, and could
//! not be byte-compared at all.

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

#[test]
fn a_constructed_annotation_implementation_is_byte_identical_to_kotlinc() {
    let source = r#"package sample
        annotation class Marker(val name: String)
        fun make(): Marker = Marker("OK")
    "#;
    let Some(result) = common::byte_diff_against_kotlinc(
        "AnnotationImplParity",
        source,
        "sample/AnnotationImplParityKt$annotationImpl$sample_Marker$0",
    ) else {
        return;
    };
    assert!(result.is_ok(), "{}", result.unwrap_err());
}

#[test]
fn an_annotation_declared_in_another_source_file_is_constructed_at_the_use_site() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(
        &[
            (
                "Marker",
                r#"package sample
                   annotation class Marker(val name: String = "default")"#,
            ),
            (
                "Use",
                r#"package sample
                   fun box(): String = Marker("OK").name"#,
            ),
        ],
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("cross-file annotation construction must compile");

    let mut names = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| name.contains("annotationImpl"))
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        ["sample/UseKt$annotationImpl$sample_Marker$0"],
        "the implementation belongs to the constructing file, not the declaration file"
    );
    assert_eq!(
        common::run_box(&classes, "sample.UseKt", std::slice::from_ref(&stdlib)).as_deref(),
        Some("OK")
    );
}

#[test]
fn all_calls_in_one_file_share_the_first_scopes_annotation_implementation() {
    let source = r#"
        package sample
        annotation class Marker(val name: String)

        fun top(): Marker = Marker("top")
        class Host {
            fun first(): Marker = Marker("first")
            fun second(): Marker = Marker("second")
        }
    "#;
    let Some(names) = emitted_classes(source) else {
        return;
    };
    let implementations = names
        .iter()
        .filter(|name| name.contains("annotationImpl"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        implementations,
        ["sample/Host$annotationImpl$sample_Marker$0"]
    );
}

#[test]
fn an_annotation_used_only_as_an_annotation_argument_emits_no_implementation() {
    let source = r#"
        annotation class Inner(val value: String)
        annotation class Outer(val inner: Inner)
        @Outer(Inner("value")) class Marked
    "#;
    let Some(names) = emitted_classes(source) else {
        return;
    };
    assert!(!names.iter().any(|name| name.contains("annotationImpl")));
}

#[test]
fn a_nested_annotation_from_the_classpath_uses_the_same_construction_model() {
    let dependency = common::compile_lib(
        "annotation_impl_nested_classpath",
        r#"package dependency
           class Holder {
               annotation class Marker(val name: String)
           }"#,
    )
    .expect("compile annotation dependency");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        r#"package consumer
           fun box(): String = dependency.Holder.Marker("OK").name"#,
        "Use",
        &[dependency.clone(), stdlib.clone()],
        Some(jdk.as_path()),
    )
    .expect("construct classpath annotation");

    let implementations = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| name.contains("annotationImpl"))
        .collect::<Vec<_>>();
    assert_eq!(
        implementations,
        ["consumer/UseKt$annotationImpl$dependency_Holder_Marker$0"]
    );
    assert_eq!(
        common::run_box(&classes, "consumer.UseKt", &[dependency, stdlib]).as_deref(),
        Some("OK")
    );
}
