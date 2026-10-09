//! A top-level function or property defined in another file of the same module.
//!
//! The call is a direct symbol both files derive from the checked callable identity, and a
//! property is reached through the getter and setter entry points both files derive from the
//! property identity. The file that does not contain `box` still has to be linked: its function is
//! not in the entry file.

use crate::common::{expect_box_ok_files_with_stdlib, expect_native_sources};

/// A portable multi-file program: the JVM answer is established first, then Native must agree.
fn expect_portable_sources(sources: &[(&str, &str)]) {
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    expect_box_ok_files_with_stdlib(sources, stem);
    expect_native_sources(sources, "OK");
}

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

#[test]
fn a_package_property_is_read_from_the_file_that_stores_it() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                    package demo
                    val label: String = "OK"
                    var count: Int = 1
                "#,
        ),
        (
            "box",
            r#"
                    package demo
                    fun box(): String {
                        count += 2
                        return if (label == "OK" && count == 3) "OK" else "FAIL"
                    }
                "#,
        ),
    ]);
}

#[test]
fn an_inline_function_updates_a_package_property_in_its_own_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                    package demo
                    var order: String = ""
                    inline fun note(piece: String) {
                        order = order + piece
                    }
                "#,
        ),
        (
            "box",
            r#"
                    package demo
                    fun box(): String {
                        note("O")
                        note("K")
                        return order
                    }
                "#,
        ),
    ]);
}

#[test]
fn an_assignment_evaluates_its_value_before_the_defining_file_initializes() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                    package demo
                    val initialized: Int = observeInitialization()
                    var slot: Int = 0
                "#,
        ),
        (
            "box",
            r#"
                    package demo
                    var order: Int = 0
                    fun observeInitialization(): Int {
                        order = order * 10 + 2
                        return 0
                    }
                    fun argument(): Int {
                        order = order * 10 + 1
                        return 7
                    }
                    fun box(): String {
                        slot = argument()
                        return if (order == 12 && slot == 7) "OK" else "FAIL: $order $slot"
                    }
                "#,
        ),
    ]);
}

#[test]
fn a_lateinit_package_property_throws_until_assigned() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                    package demo
                    lateinit var label: String
                    fun publish() {
                        label = "OK"
                    }
                "#,
        ),
        (
            "box",
            r#"
                    package demo
                    fun box(): String {
                        try {
                            return label
                        } catch (e: kotlin.UninitializedPropertyAccessException) {
                            publish()
                            return if (label == "OK") "OK" else "FAIL"
                        }
                    }
                "#,
        ),
    ]);
}

/// Native only for now: krusty's JVM backend reads a cross-file value-class package property as
/// its box (the getter returns `int`, the caller boxes it as `Integer` and casts that to `Meters`),
/// so it cannot be this program's oracle until that is fixed.
#[test]
fn a_package_value_class_property_is_stored_as_its_value() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                package demo
                @JvmInline value class Meters(val value: Int)
                @JvmInline value class Name(val text: String)
                var distance: Meters = Meters(40)
                val owner: Name = Name("OK")
                fun meters(value: Int): Meters = Meters(value)
                fun raw(distance: Meters): Int = distance.value
                fun text(name: Name): String = name.text
            "#,
            ),
            (
                "box",
                r#"
                package demo
                fun box(): String {
                    distance = meters(raw(distance) + 1)
                    if (raw(distance) != 41) return "FAIL: ${raw(distance)}"
                    return text(owner)
                }
            "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn an_explicit_backing_field_is_read_at_the_public_type() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                val total: Number
                    field = 41
                fun next(): Int = total + 1
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val seen: Number = total
                    if (seen !is Int) return "FAIL: not an Int"
                    return if (seen.toInt() == 41 && next() == 42) "OK" else "FAIL: $seen"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_source_written_package_accessor_runs_in_its_own_file() {
    expect_portable_sources(&[
        (
            "defs",
            r#"
                package demo
                var backing: String = "O"
                var greeting: String
                    get() = backing + "K"
                    set(value) {
                        backing = value.substring(0, 1)
                    }
                val length: Int
                    get() = backing.length
            "#,
        ),
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    greeting = "OX"
                    return if (length == 1) greeting else "FAIL: $length"
                }
            "#,
        ),
    ]);
}
