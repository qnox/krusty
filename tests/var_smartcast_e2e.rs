//! Flow smart casts on local mutable variables.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_diagnostics(actual: Vec<String>, expected: &[&str]) {
    assert_eq!(actual.len(), expected.len());
    let actual = actual.iter().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(actual.as_slice(), expected);
}

#[test]
fn var_null_check_smart_casts_in_branch() {
    const SRC: &str = "fun f(x: String?): Int {\n\
    var t = x\n\
    if (t != null) {\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (f(null) != -1) return \"FAIL null\"\n\
    return if (f(\"abc\") == 3) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("var null-check smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn var_early_return_guard_smart_casts_rest_of_block() {
    const SRC: &str = "fun f(x: String?): Int {\n\
    var t = x\n\
    if (t == null) return -1\n\
    return t.length\n\
}\n\
fun box(): String {\n\
    if (f(null) != -1) return \"FAIL null\"\n\
    return if (f(\"abcd\") == 4) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(run(SRC).expect("var guard smartcast compiles + runs"), "OK");
}

#[test]
fn var_contract_guard_smart_casts_through_reassignment() {
    const SRC: &str = "fun parseVersion(rawText: String?): Int {\n\
    var text = rawText\n\
    if (text.isNullOrEmpty()) {\n\
        return -1\n\
    }\n\
    text = text.trim()\n\
    val dash = text.lastIndexOf('-')\n\
    if (dash >= 0) {\n\
        text = text.substring(dash + 1)\n\
    }\n\
    return text.toInt()\n\
}\n\
fun box(): String {\n\
    if (parseVersion(null) != -1) return \"FAIL null\"\n\
    if (parseVersion(\"\") != -1) return \"FAIL empty\"\n\
    return if (parseVersion(\"idea-42\") == 42) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("contract-guard var smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn var_is_check_smart_casts() {
    const SRC: &str = "fun f(x: Any): Int {\n\
    var t = x\n\
    if (t is String) {\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (f(7) != -1) return \"FAIL int\"\n\
    return if (f(\"abcde\") == 5) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("var is-check smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn later_closure_mutation_does_not_block_an_earlier_smart_cast() {
    const SRC: &str = "fun f(): Int {\n\
    var text: String? = \"abc\"\n\
    if (text != null) {\n\
        val length = text.length\n\
        val mutate = { text = null }\n\
        return length\n\
    }\n\
    return -1\n\
}\n\
fun box(): String = if (f() == 3) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("earlier smart cast compiles and runs"),
        "OK"
    );
}

#[test]
fn closure_created_inside_a_proof_invalidates_later_reads() {
    const SRC: &str = "fun f(): Int {\n\
    var text: String? = \"abc\"\n\
    if (text != null) {\n\
        val mutate = { text = null }\n\
        return text.length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags(SRC),
        &["smart cast to 'String' is impossible, because 'text' is a local variable that is mutated in a capturing closure."],
    );
}

#[test]
fn var_smart_cast_visible_in_nested_block() {
    const SRC: &str = "fun f(x: String?, c: Boolean): Int {\n\
    var t = x\n\
    var acc = 0\n\
    if (t != null) {\n\
        while (acc < 1) {\n\
            acc += t.length\n\
        }\n\
        if (c) {\n\
            acc += t.length\n\
        }\n\
    }\n\
    return acc\n\
}\n\
fun box(): String = if (f(\"abc\", true) == 6) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("nested-block var smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn var_reassignment_to_non_null_keeps_smart_cast() {
    const SRC: &str = "fun f(x: String?): Int {\n\
    var t = x\n\
    if (t != null) {\n\
        t = t.trim()\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n\
fun box(): String = if (f(\" ab \") == 2) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("non-null reassignment keeps smartcast"),
        "OK"
    );
}

#[test]
fn var_reassignment_in_nested_branch_kills_smart_cast() {
    const SRC: &str = "fun f(c: Boolean): Int {\n\
    var t: String? = \"a\"\n\
    if (t != null) {\n\
        if (c) {\n\
            t = null\n\
        }\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags(SRC),
        &["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."],
    );
}

#[test]
fn closure_mutated_var_does_not_smart_cast() {
    const SRC: &str = "fun f(): Int {\n\
    var t: String? = \"a\"\n\
    val l = { t = null }\n\
    if (t != null) {\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags(SRC),
        &["smart cast to 'String' is impossible, because 't' is a local variable that is mutated in a capturing closure."],
    );
}

#[test]
fn closure_mutated_var_is_check_reports_failed_cast() {
    const SRC: &str = "fun f(p: String?): Int {\n\
    var t: String? = p\n\
    val l = { t = null }\n\
    if (t is String) {\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags(SRC),
        &["smart cast to 'String' is impossible, because 't' is a local variable that is mutated in a capturing closure."],
    );
}

#[test]
fn closure_mutated_var_else_branch_reports_nothing_nullable_receiver() {
    const SRC: &str = "fun f(): Int {\n\
    var t: String? = \"a\"\n\
    val l = { t = null }\n\
    if (t != null) {\n\
    } else {\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags(SRC),
        &["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Nothing?'."],
    );
}

#[test]
fn inline_lambda_write_then_null_check_smart_casts() {
    const SRC: &str = "fun f(p: String?): Int {\n\
    var t: String? = null\n\
    p?.let { t = it }\n\
    if (t != null) {\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (f(null) != -1) return \"FAIL null\"\n\
    return if (f(\"abc\") == 3) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("inline-lambda write then smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn inline_lambda_write_after_check_kills_smart_cast() {
    const SRC: &str = "fun f(): Int {\n\
    var t: String? = \"a\"\n\
    if (t != null) {\n\
        run { t = null }\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags_stdlib(SRC),
        &["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."],
    );
}

#[test]
fn same_rung_null_write_reports_nothing_nullable_receiver() {
    const SRC: &str = "fun f(): Int {\n\
    var t: String? = \"a\"\n\
    if (t != null) {\n\
        t = null\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags(SRC),
        &["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Nothing?'."],
    );
}

fn diags_stdlib(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

#[test]
fn same_rung_null_write_reports_nothing_nullable_receiver_for_calls() {
    const SRC: &str = "fun f(): Int {\n\
    var t: String? = \"a\"\n\
    if (t != null) {\n\
        t = null\n\
        return t.trim().length + t.substring(1).length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags_stdlib(SRC),
        &[
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Nothing?'.",
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Nothing?'.",
        ],
    );
}

#[test]
fn nullable_receiver_extension_call_reports_unsafe_call() {
    const SRC: &str = "fun f(s: String?): Int {\n\
    return s.trim().length\n\
}\n";
    assert_diagnostics(
        diags_stdlib(SRC),
        &["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."],
    );
}

#[test]
fn closure_mutated_var_reports_failed_cast_for_member_and_extension_calls() {
    const SRC: &str = "fun f(p: String?): Int {\n\
    var t: String? = p\n\
    val l = { t = null }\n\
    if (t != null) {\n\
        return t.length + t.trim().length\n\
    }\n\
    return -1\n\
}\n";
    assert_diagnostics(
        diags_stdlib(SRC),
        &[
            "smart cast to 'String' is impossible, because 't' is a local variable that is mutated in a capturing closure.",
            "smart cast to 'String' is impossible, because 't' is a local variable that is mutated in a capturing closure.",
        ],
    );
}
