//! A lambda or anonymous object an inline function declares is lowered in its own file and again
//! where a caller in another file of the module expands it. Every copy must link under its own
//! symbol.

use super::common::{expect_box_ok_files_with_stdlib, expect_native_sources};

/// The files of a shared regression, `(stem, text)`, split at its `// FILE:` markers.
fn files(program: &'static str) -> Vec<(&'static str, &'static str)> {
    program
        .split("// FILE: ")
        .skip(1)
        .map(|file| {
            let (name, body) = file.split_once('\n').unwrap();
            (name.trim_end_matches(".kt"), body)
        })
        .collect()
}

/// The JVM/kotlinc run establishes the expected result before Native is held to it.
fn expect_ok_on_jvm_and_native(program: &'static str, stem: &str) {
    let sources = files(program);
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
