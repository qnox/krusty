//! A lambda an inline function declares is lowered in its own file and again where a caller in
//! another file of the module expands it. Both copies must link under distinct symbols.

use super::common::expect_native_sources;

#[test]
fn an_inlined_crossinline_lambda_links_beside_the_function_that_declares_it() {
    expect_native_sources(
        &[
            (
                "lib",
                include_str!("box_regressions/inlineCrossinlineLambdaAcrossFiles.kt")
                    .split("// FILE: main.kt\n")
                    .next()
                    .unwrap()
                    .trim_start_matches(
                        "// WITH_STDLIB\n// LANGUAGE: +ContextParameters\n// FILE: lib.kt\n",
                    ),
            ),
            (
                "main",
                include_str!("box_regressions/inlineCrossinlineLambdaAcrossFiles.kt")
                    .split("// FILE: main.kt\n")
                    .nth(1)
                    .unwrap(),
            ),
        ],
        "OK",
    );
}
