//! A lambda or anonymous object an inline function declares is lowered in its own file and again
//! where a caller in another file of the module expands it. Every copy must link under its own
//! symbol.

use super::common::{expect_box_ok_files_with_stdlib, expect_native_sources};

/// The JVM/kotlinc run establishes the expected result before Native is held to it. Both runs see
/// every input of the shared regression: the preamble (`// WITH_STDLIB`, `// LANGUAGE:`) stays on
/// the first file.
fn expect_ok_on_jvm_and_native(program: &str, stem: &str) {
    let (kotlin, java) = krusty::conformance::split_files(program).unwrap();
    assert!(
        java.is_empty(),
        "{stem}: the regression declares no Java files"
    );
    let sources: Vec<(&str, &str)> = kotlin
        .iter()
        .map(|(name, text)| (name.trim_end_matches(".kt"), text.as_str()))
        .collect();
    expect_box_ok_files_with_stdlib(&sources, stem);
    expect_native_sources(&sources, "OK");
}

#[test]
fn an_inlined_crossinline_lambda_links_beside_the_function_that_declares_it() {
    expect_ok_on_jvm_and_native(
        include_str!("box_regressions/inlineCrossinlineLambdaAcrossFiles.kt"),
        "InlineCrossinlineLambdaAcrossFiles",
    );
}

#[test]
fn an_inlined_anonymous_object_links_in_every_file_that_expands_it() {
    expect_ok_on_jvm_and_native(
        include_str!("box_regressions/inlineAnonymousObjectAcrossFiles.kt"),
        "InlineAnonymousObjectAcrossFiles",
    );
}
