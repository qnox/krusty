//! Conditional branches constrain generic calls whose result type is otherwise unbound.

use super::common;

fn assert_diagnostics(src: &str, expected: &[&str]) {
    let diagnostics =
        common::checker_diags_with_stdlib(src).expect("checker diagnostics available");
    assert_eq!(
        diagnostics.len(),
        expected.len(),
        "unexpected diagnostic count: {diagnostics:?}"
    );
    assert_eq!(diagnostics, expected);
}

#[test]
fn elvis_right_side_binds_from_non_null_left() {
    const SRC: &str = "fun <T> id(x: T): T = x\n\
        fun box() {\n\
            val n: Result<Int>? = null\n\
            val f: String = id(n ?: Result.failure(RuntimeException()))\n\
        }\n";
    assert_diagnostics(
        SRC,
        &["initializer type mismatch: expected 'String', actual 'Result<Int>'."],
    );
}

#[test]
fn if_branches_bind_generic_calls_from_sibling() {
    const THEN_CALL: &str = "fun <T> id(x: T): T = x\n\
        fun box() {\n\
            val n: Result<Int>? = null\n\
            val g: String = id(if (true) Result.failure(RuntimeException()) else n)\n\
        }\n";
    assert_diagnostics(
        THEN_CALL,
        &["initializer type mismatch: expected 'String', actual 'Result<Int>?'."],
    );
    const ELSE_CALL: &str = "fun <T> id(x: T): T = x\n\
        fun box() {\n\
            val n: Result<Int>? = null\n\
            val g: String = id(if (true) n else Result.failure(RuntimeException()))\n\
        }\n";
    assert_diagnostics(
        ELSE_CALL,
        &["initializer type mismatch: expected 'String', actual 'Result<Int>?'."],
    );
}

#[test]
fn when_arms_bind_generic_calls_from_sibling() {
    const ELSE_CALL: &str = "fun <T> id(x: T): T = x\n\
        fun box() {\n\
            val n: Result<Int>? = null\n\
            val h: String = id(when { true -> n; else -> Result.failure(RuntimeException()) })\n\
        }\n";
    assert_diagnostics(
        ELSE_CALL,
        &["initializer type mismatch: expected 'String', actual 'Result<Int>?'."],
    );
    const FIRST_ARM_CALL: &str = "fun <T> id(x: T): T = x\n\
        fun box() {\n\
            val n: Result<Int>? = null\n\
            val h: String = id(when { true -> Result.failure(RuntimeException()); else -> n })\n\
        }\n";
    assert_diagnostics(
        FIRST_ARM_CALL,
        &["initializer type mismatch: expected 'String', actual 'Result<Int>?'."],
    );
}

#[test]
fn sibling_recheck_preserves_branch_narrowing() {
    const IF_SRC: &str = "fun <T> from(value: String): T = throw RuntimeException()\n\
        fun box(text: String?) {\n\
            val result: String = if (text != null) from(text) else 0\n\
        }\n";
    assert_diagnostics(
        IF_SRC,
        &["initializer type mismatch: expected 'String', actual 'Int'."],
    );

    const WHEN_SRC: &str = "fun <T> from(value: String): T = throw RuntimeException()\n\
        fun box(text: String?) {\n\
            val result: String = when {\n\
                text == null -> 0\n\
                else -> from(text)\n\
            }\n\
        }\n";
    assert_diagnostics(
        WHEN_SRC,
        &["initializer type mismatch: expected 'String', actual 'Int'."],
    );
}

#[test]
fn empty_list_binds_from_sibling_branch() {
    const SRC: &str = "fun <T> id(x: T): T = x\n\
        fun box() {\n\
            val x: String = id(if (true) emptyList() else listOf(\"a\"))\n\
        }\n";
    assert_diagnostics(
        SRC,
        &["initializer type mismatch: expected 'String', actual 'List<String>'."],
    );
}

#[test]
fn rebound_conditional_results_run() {
    const LIB: &str = "fun <T> id(x: T): T = x\n\
        fun conditionalResults(): String {\n\
            val n: Result<Int>? = null\n\
            val elvis = id(n ?: Result.failure(RuntimeException(\"elvis\")))\n\
            val ifThen = id(if (true) Result.failure(RuntimeException(\"if-then\")) else n)\n\
            val ifElse = id(if (false) n else Result.failure(RuntimeException(\"if-else\")))\n\
            val whenFirst = id(when { true -> Result.failure(RuntimeException(\"when-first\")); else -> n })\n\
            val whenElse = id(when { false -> n; else -> Result.failure(RuntimeException(\"when-else\")) })\n\
            val ok = elvis.isFailure &&\n\
                ifThen?.isFailure == true && ifElse?.isFailure == true &&\n\
                whenFirst?.isFailure == true && whenElse?.isFailure == true\n\
            return if (ok) \"OK\" else \"FAIL\"\n\
        }\n";
    const MAIN: &str = "fun box(): String = conditionalResults()\n";
    assert_eq!(
        common::expect_box_run_against("conditional-result-rebind", LIB, MAIN)
            .expect("both compilers run"),
        "OK"
    );
}

#[test]
fn truly_unbound_call_still_cannot_infer() {
    const SRC: &str = "fun box(): String {\n\
        val x = Result.failure(RuntimeException(\"z\"))\n\
        return \"FAIL\"\n\
    }\n";
    assert_diagnostics(
        SRC,
        &["cannot infer type for type parameter 'T'. Specify it explicitly."],
    );
}
