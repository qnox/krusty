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
fn null_branch_assignment_is_non_null_after_if() {
    const SRC: &str = "class Box(val n: Int)\n\
fun f(x: Box?): Int {\n\
    var b = x\n\
    if (b == null) {\n\
        b = Box(1)\n\
    }\n\
    return b.n\n\
}\n\
fun box(): String = if (f(null) == 1 && f(Box(4)) == 4) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("null-branch assignment smart-casts after if"),
        "OK"
    );
}

#[test]
fn both_branch_assignments_are_non_null_after_if() {
    const SRC: &str = "class Box(val n: Int)\n\
fun f(c: Boolean): Int {\n\
    var b: Box? = null\n\
    if (c) b = Box(1) else b = Box(2)\n\
    return b.n\n\
}\n\
fun box(): String = if (f(true) == 1 && f(false) == 2) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("both-branch assignment smart-casts after if"),
        "OK"
    );
}

#[test]
fn one_sided_assignment_stays_nullable_after_if() {
    const SRC: &str = "class Box(val n: Int)\n\
fun f(c: Boolean, x: Box?): Int {\n\
    var b = x\n\
    if (c) b = Box(1)\n\
    return b.n\n\
}\n";
    assert_diagnostics(
        diags(SRC),
        &["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Box?'."],
    );
}

#[test]
fn null_write_on_one_edge_drops_the_flow_type() {
    const SRC: &str = "class Box(val n: Int)\n\
fun f(c: Boolean): Int {\n\
    var b: Box? = Box(1)\n\
    if (c) b = null\n\
    return b.n\n\
}\n";
    assert_diagnostics(
        diags(SRC),
        &["only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Box?'."],
    );
}

#[test]
fn when_edges_that_agree_are_non_null_after() {
    const SRC: &str = "class Box(val n: Int)\n\
fun f(x: Box?): Int {\n\
    var b = x\n\
    when {\n\
        b == null -> b = Box(2)\n\
        else -> {}\n\
    }\n\
    return b.n\n\
}\n\
fun box(): String = if (f(null) == 2 && f(Box(5)) == 5) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("when edges agree after the when"), "OK");
}

#[test]
fn else_edge_keeps_an_earlier_assignment() {
    const SRC: &str = "fun f(c: Boolean, x: String?): Int {\n\
    var t = x\n\
    t = \"hello\"\n\
    if (c) {\n\
        t = null\n\
    } else {\n\
        return t.length\n\
    }\n\
    return -1\n\
}\n\
fun box(): String = if (f(false, null) == 5 && f(true, null) == -1) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("else edge keeps the assignment the then edge skipped"),
        "OK"
    );
}

#[test]
fn null_branch_assignment_unboxes_a_nullable_primitive() {
    const SRC: &str = "fun f(x: Int?): Int {\n\
    var n = x\n\
    if (n == null) n = 7\n\
    return n + 1\n\
}\n\
fun box(): String = if (f(null) == 8 && f(2) == 3) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("nullable primitive assignment smart-casts after if"),
        "OK"
    );
}

#[test]
fn operator_after_null_branch_assignment() {
    const SRC: &str = "fun box(): String {\n\
    val result = mutableMapOf<String, MutableList<Int>>()\n\
    val key = \"k\"\n\
    var list = result[key]\n\
    if (list == null) {\n\
        list = mutableListOf()\n\
        result[key] = list\n\
    }\n\
    list += 1\n\
    return if (result[key]?.single() == 1) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("operator applies to the joined non-null type"),
        "OK"
    );
}

#[test]
fn nothing_branch_leaves_the_other_edge() {
    const SRC: &str = "fun f(x: String?): Int {\n\
    var t = x\n\
    if (t == null) {\n\
        throw IllegalStateException()\n\
    }\n\
    return t.length\n\
}\n\
fun box(): String = if (f(\"ab\") == 2) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("Nothing branch is not part of the join"),
        "OK"
    );
}

#[test]
fn finally_keeps_the_value_a_skipped_assignment_does_not_replace() {
    const SRC: &str = "fun box(): String {\n\
    var result = \"fail\"\n\
    try {\n\
        var x: Any = 42\n\
        try {\n\
            try {\n\
                throw Error()\n\
            } finally {\n\
                x = \"OK\"\n\
            }\n\
            x = 117\n\
        } finally {\n\
            result = x.toString()\n\
        }\n\
    } catch (_: Throwable) { }\n\
    return result\n\
}\n";
    assert_eq!(
        run(SRC).expect("finally reads the assignment that ran"),
        "OK"
    );
}

#[test]
fn finally_keeps_the_value_when_a_throwing_finally_skips_the_next_assignment() {
    const SRC: &str = "fun box(): String {\n\
    var result = \"fail\"\n\
    try {\n\
        var x: Any = 42\n\
        try {\n\
            try {\n\
                x = \"OK\"\n\
            } finally {\n\
                throw Error()\n\
            }\n\
            x = 117\n\
        } finally {\n\
            result = x.toString()\n\
        }\n\
    } catch (_: Throwable) { }\n\
    return result\n\
}\n";
    assert_eq!(
        run(SRC).expect("throwing finally skips the dead assignment"),
        "OK"
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
