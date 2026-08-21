//! False-branch narrowings from subjectless `when` arms apply to later conditions and bodies.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_kotlinc_accepts(tag: &str, src: &str) {
    let (code, diagnostics) = common::kotlinc_source_result(tag, src);
    assert_eq!(code, 0, "kotlinc rejected {tag}: {diagnostics}");
}

#[test]
fn else_branch_after_null_equality_guard_smartcasts() {
    const SRC: &str = "fun lengthOr(s: String?, fallback: Int): Int =\n\
    when {\n\
        s == null -> fallback\n\
        else -> s.length\n\
    }\n\
fun box(): String {\n\
    return if (lengthOr(\"abc\", -1) == 3 && lengthOr(null, -1) == -1) \"OK\" else \"FAIL\"\n\
}\n";
    assert_kotlinc_accepts("WhenNullElse", SRC);
    assert_eq!(run(SRC).expect("krusty rejected the source"), "OK");
}

#[test]
fn middle_branch_after_null_equality_guard_smartcasts() {
    const SRC: &str = "var got = 0\n\
fun record(s: String?) {\n\
    when {\n\
        s == null -> got = -1\n\
        s.isEmpty() -> got = 0\n\
        else -> got = s.length\n\
    }\n\
}\n\
fun box(): String {\n\
    record(null)\n\
    if (got != -1) return \"FAIL: null\"\n\
    record(\"\")\n\
    if (got != 0) return \"FAIL: empty\"\n\
    record(\"abcd\")\n\
    return if (got == 4) \"OK\" else \"FAIL: $got\"\n\
}\n";
    assert_kotlinc_accepts("WhenNullMiddle", SRC);
    assert_eq!(run(SRC).expect("krusty rejected the source"), "OK");
}

#[test]
fn separate_null_checks_accumulate_for_later_conditions_and_bodies() {
    const SRC: &str = "fun size(a: String?, b: String?): Int =\n\
    when {\n\
        a == null -> -1\n\
        b == null -> -2\n\
        a.isEmpty() -> b.length\n\
        else -> a.length + b.length\n\
    }\n\
fun box(): String {\n\
    if (size(null, \"b\") != -1) return \"FAIL null a\"\n\
    if (size(\"a\", null) != -2) return \"FAIL null b\"\n\
    if (size(\"\", \"bc\") != 2) return \"FAIL empty\"\n\
    return if (size(\"a\", \"bc\") == 3) \"OK\" else \"FAIL size\"\n\
}\n";
    assert_kotlinc_accepts("WhenSeparateNulls", SRC);
    assert_eq!(run(SRC).expect("krusty rejected the source"), "OK");
}

#[test]
fn compound_false_branch_narrows_both_receivers() {
    const SRC: &str = "fun size(a: String?, b: String?): Int =\n\
    when {\n\
        a == null || b == null -> -1\n\
        else -> a.length + b.length\n\
    }\n\
fun box(): String = if (size(\"a\", \"bc\") == 3 && size(null, \"b\") == -1) \"OK\" else \"FAIL\"\n";
    assert_kotlinc_accepts("WhenCompoundNulls", SRC);
    assert_eq!(run(SRC).expect("krusty rejected the source"), "OK");
}

#[test]
fn compound_true_branch_narrows_both_receivers() {
    const SRC: &str = "fun size(a: String?, b: String?): Int =\n\
    when {\n\
        a != null && b != null -> a.length + b.length\n\
        else -> -1\n\
    }\n\
fun box(): String = if (size(\"a\", \"bc\") == 3 && size(null, \"b\") == -1) \"OK\" else \"FAIL\"\n";
    assert_kotlinc_accepts("WhenCompoundNonNull", SRC);
    assert_eq!(run(SRC).expect("krusty rejected the source"), "OK");
}

#[test]
fn negated_type_test_narrows_the_else_arm() {
    const SRC: &str = "fun size(value: Any?): Int =\n\
    when {\n\
        value !is String -> -1\n\
        else -> value.length\n\
    }\n\
fun box(): String = if (size(1) == -1 && size(\"abc\") == 3) \"OK\" else \"FAIL\"\n";
    assert_kotlinc_accepts("WhenNegatedType", SRC);
    assert_eq!(run(SRC).expect("krusty rejected the source"), "OK");
}

#[test]
fn value_class_receiver_smartcasts_after_null_guard() {
    const SRC: &str = "var got = \"\"\n\
fun take(r: Result<Int>?) {\n\
    when {\n\
        r == null -> got += \"null;\"\n\
        r.isFailure -> got += \"failure;\"\n\
        else -> got += \"${r.getOrNull()!!};\"\n\
    }\n\
}\n\
fun box(): String {\n\
    take(null)\n\
    take(Result.success(7))\n\
    return if (got == \"null;7;\") \"OK\" else \"FAIL: $got\"\n\
}\n";
    assert_kotlinc_accepts("WhenNullableResult", SRC);
    assert_eq!(
        common::expect_box_run(
            SRC,
            "Main",
            &[common::stdlib_jar()],
            Some(common::jdk_modules().as_path()),
        ),
        "OK"
    );
}

#[test]
fn no_null_guard_still_rejects_plain_calls() {
    const SRC: &str = "fun take(s: String?) {\n\
    when {\n\
        s.isEmpty() -> {}\n\
        else -> s.length\n\
    }\n\
}\n\
fun box(): String = \"OK\"\n";
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(SRC),
        [
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."
        ]
    );
}

#[test]
fn captured_mutation_reports_the_exact_declined_smartcast() {
    const SRC: &str = "fun length(): Int {\n\
    var text: String? = \"abc\"\n\
    val mutate = { text = null }\n\
    return when {\n\
        text == null -> -1\n\
        else -> text.length\n\
    }\n\
}\n";
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(SRC),
        [
            "smart cast to 'String' is impossible, because 'text' is a local variable that is mutated in a capturing closure."
        ]
    );
}
