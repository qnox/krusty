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

#[test]
fn a_call_to_another_file_leaves_defaulted_arguments_out() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    var calls = 0
                    fun next(): Int { calls += 1; return calls }
                    fun count(): Int = calls
                    fun join(a: String, b: Int = a.length + 1, c: String = "c$b", d: Int = next()): String =
                        "$a/$b/$c/$d"
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val all = join("x", 5, "y", 9)
                        val trailing = join("ab")
                        val middle = join("q", c = "z")
                        val named = join(d = 7, a = "n")
                        val result = "$all $trailing $middle $named ${count()}"
                        return if (result == "x/5/y/9 ab/3/c3/1 q/2/z/2 n/2/c2/7 2") "OK" else result
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn an_extension_in_another_file_fills_its_defaults_from_the_receiver() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun String.pad(width: Int = length + 2, fill: Char = '.'): String {
                        var out = this
                        while (out.length < width) out += fill
                        return out
                    }
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val result = "ab".pad() + "|" + "c".pad(fill = '-') + "|" + "d".pad(4)
                        return if (result == "ab..|c--|d...") "OK" else result
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_default_reads_the_defining_files_top_level_state_after_its_initializer_ran() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun spell(): String = "O" + "K"
                    val greeting: String = spell()
                    fun greet(text: String = greeting): String = text
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String = greet()
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_cross_file_defaulted_call_past_thirty_two_parameters_uses_the_next_mask_word() {
    let parameters: Vec<String> = (0..34).map(|k| format!("p{k}: Int = {k}")).collect();
    let sum: Vec<String> = (0..34).map(|k| format!("p{k}")).collect();
    let defs = format!(
        "package demo\nfun wide({}): Int = {}\n",
        parameters.join(", "),
        sum.join(" + ")
    );
    // 0 + 1 + … + 33 = 561; overriding p0 with 100 and p33 with 0 gives 561 + 100 - 33.
    expect_native_sources(
        &[
            ("defs", &defs),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val all = wide()
                        val some = wide(100, p33 = 0)
                        return if (all == 561 && some == 628) "OK" else "FAIL: $all $some"
                    }
                "#,
            ),
        ],
        "OK",
    );
}

#[test]
fn a_number_returned_from_another_file_is_rendered_into_a_string_template() {
    expect_native_sources(
        &[
            (
                "defs",
                r#"
                    package demo
                    fun count(): Int = 2
                "#,
            ),
            (
                "box",
                r#"
                    package demo
                    fun box(): String {
                        val result = "n ${count()}"
                        return if (result == "n 2") "OK" else result
                    }
                "#,
            ),
        ],
        "OK",
    );
}
