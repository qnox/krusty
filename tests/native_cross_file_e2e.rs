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
fn a_top_level_property_in_another_file_is_initialized_before_the_call() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    val base: Int = 21
                    fun value(): Int = base
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String = if (value() == 21) "OK" else "FAIL"
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_file_initializer_runs_once_however_many_calls_arrive() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    var hits: Int = 0
                    fun note(): Int {
                        hits += 1
                        return hits
                    }
                    val marker: Int = note()
                    fun value(): Int = marker
                    fun seen(): Int = hits
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val first = value()
                        val second = value()
                        return if (first == 1 && second == 1 && seen() == 1) "OK" else "FAIL"
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn call_arguments_are_evaluated_before_the_defining_file_is_initialized() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    val initialized: Int = observeInitialization()
                    fun consume(value: Int): Int = value + initialized
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    var order: Int = 0
                    fun argument(): Int {
                        order = order * 10 + 1
                        return 7
                    }
                    fun observeInitialization(): Int {
                        order = order * 10 + 2
                        return 0
                    }
                    fun box(): String {
                        consume(argument())
                        return if (order == 12) "OK" else "FAIL: $order"
                    }
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
