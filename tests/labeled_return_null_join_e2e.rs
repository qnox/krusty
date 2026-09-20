//! A labelled `return@… null` contributes the null-literal type to the lambda's inferred result.
//! Joining that with the tail expression must give `T?`, never `Any`: `run { if (c) return@run
//! null; 1L }` is `Long?`. The inference join normalises the null literal on one side of the merge
//! only, so this shape collapsed to `Any` and every caller that needed the real element type then
//! rejected it — `mapNotNull { … return@mapNotNull null … }.toList()` reported
//! `inferred type is List<Any>`.

use super::common;

fn expect_accepted(src: &str, what: &str) {
    let diagnostics = common::front_end_diagnostics_files_with_stdlib(&[src]);
    assert!(
        diagnostics.is_empty(),
        "{what}: unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn labeled_null_return_joins_with_the_tail_type() {
    const SRC: &str = "fun g(v: Long?) {}\n\
fun f(c: Boolean) { val x = run { if (c) return@run null; 1L }; g(x) }\n";
    expect_accepted(SRC, "run with a labelled null exit");
}

/// Exercise the inference join through a repository-owned generic callable name. This keeps the
/// regression independent of any stdlib member/intrinsic handling: the lambda contributes `null`
/// first and a user classifier second, and both compilers must infer `Token?` and run identically.
#[test]
fn user_generic_hof_joins_a_labeled_null_with_a_user_type() {
    const SRC: &str = "class Token(val text: String)\n\
fun <T> relayValue(flag: Boolean, body: (Boolean) -> T): T = body(flag)\n\
fun token(flag: Boolean) = relayValue(flag) { stop ->\n\
    if (stop) return@relayValue null\n\
    Token(\"OK\")\n\
}\n\
fun box(): String {\n\
    val absent = token(true)\n\
    val present = token(false)\n\
    return if (absent == null && present?.text == \"OK\") \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(common::kotlinc_box_result(SRC), "OK");
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}

#[test]
fn labeled_null_return_keeps_the_element_type_of_map_not_null() {
    const SRC: &str = "fun pids(lines: List<String>): List<Long> =\n\
    lines.mapNotNull { line ->\n\
        val pid = line.toLongOrNull() ?: return@mapNotNull null\n\
        pid\n\
    }.toList()\n";
    expect_accepted(SRC, "mapNotNull with a labelled null exit");
}

#[test]
fn labeled_null_return_keeps_a_reference_element_type() {
    const SRC: &str = "fun names(lines: List<String>): List<String> =\n\
    lines.asSequence().mapNotNull { line ->\n\
        if (line.isEmpty()) return@mapNotNull null\n\
        line\n\
    }.toList()\n";
    expect_accepted(SRC, "sequence mapNotNull with a labelled null exit");
}

/// The accepted program must also compute the right thing: a labelled null exit drops the element
/// and every other line keeps its parsed value.
#[test]
fn labeled_null_return_filters_and_keeps_the_values() {
    const SRC: &str = "fun box(): String {\n\
    val parsed = listOf(\"1\", \"x\", \"2\").mapNotNull { s ->\n\
        val v = s.toLongOrNull() ?: return@mapNotNull null\n\
        v\n\
    }.toList()\n\
    return if (parsed == listOf(1L, 2L)) \"OK\" else \"FAIL: \" + parsed\n\
}\n";
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}
