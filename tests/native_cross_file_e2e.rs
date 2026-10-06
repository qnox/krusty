//! A top-level function defined in another file of the same module.
//!
//! The call is a direct symbol both files derive from the checked callable identity. The file
//! that does not contain `box` still has to be linked: its function is not in the entry file.

use crate::common::expect_native_sources;

#[test]
fn a_file_calls_a_top_level_function_defined_in_another_file() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun value(): Int = 21
                    fun double(n: Int): Int = n * 2
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String = if (double(value()) == 42) "OK" else "FAIL"
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn overloads_defined_in_different_files_stay_distinct() {
    expect_native_sources(
        &[
            (
                "ints",
                r#"
                    package demo
                    fun pick(n: Int): Int = n + 1
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun pick(text: String): Int = text.length
                    fun box(): String =
                        if (pick(1) == 2 && pick("ab") == 2) "OK" else "FAIL"
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_string_returned_from_another_file_is_that_string() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun label(): String = "OK"
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String = label()
                "#,
            ),
        ],
        "OK",
    );
}
