//! Conditional branches constrain generic calls whose result type is otherwise unbound.

use super::common;

fn assert_diagnostics(src: &str, expected: &[(usize, usize, &str)]) {
    let result = common::compiler_diagnostics(
        &[("Main.kt", src)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    let expected = expected
        .iter()
        .map(|(line, column, message)| common::CompilerError {
            file: "Main.kt".to_string(),
            line: *line,
            column: *column,
            message: (*message).to_string(),
        })
        .collect::<Vec<_>>();
    let mut krusty = common::compiler_errors(&result.krusty_stdout);
    krusty.extend(common::compiler_errors(&result.krusty_stderr));
    assert_eq!(result.reference_code, 1, "kotlinc unexpectedly accepted");
    assert_eq!(result.krusty_code, 1, "krusty unexpectedly accepted");
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_eq!(krusty, expected);
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
        &[(
            4,
            17,
            "initializer type mismatch: expected 'String', actual 'Result<Int>'.",
        )],
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
        &[(
            4,
            17,
            "initializer type mismatch: expected 'String', actual 'Result<Int>?'.",
        )],
    );
    const ELSE_CALL: &str = "fun <T> id(x: T): T = x\n\
        fun box() {\n\
            val n: Result<Int>? = null\n\
            val g: String = id(if (true) n else Result.failure(RuntimeException()))\n\
        }\n";
    assert_diagnostics(
        ELSE_CALL,
        &[(
            4,
            17,
            "initializer type mismatch: expected 'String', actual 'Result<Int>?'.",
        )],
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
        &[(
            4,
            17,
            "initializer type mismatch: expected 'String', actual 'Result<Int>?'.",
        )],
    );
    const FIRST_ARM_CALL: &str = "fun <T> id(x: T): T = x\n\
        fun box() {\n\
            val n: Result<Int>? = null\n\
            val h: String = id(when { true -> Result.failure(RuntimeException()); else -> n })\n\
        }\n";
    assert_diagnostics(
        FIRST_ARM_CALL,
        &[(
            4,
            17,
            "initializer type mismatch: expected 'String', actual 'Result<Int>?'.",
        )],
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
        &[(
            3,
            20,
            "initializer type mismatch: expected 'String', actual 'Int'.",
        )],
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
        &[(
            3,
            20,
            "initializer type mismatch: expected 'String', actual 'Int'.",
        )],
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
        &[(
            3,
            17,
            "initializer type mismatch: expected 'String', actual 'List<String>'.",
        )],
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
fn a_sibling_collection_instantiates_an_unbound_result_through_its_supertype() {
    const SRC: &str = "fun <T> grow(start: Collection<T>, preserveOrder: Boolean, f: (T) -> Collection<T>): Collection<T> {\n\
            if (start.isEmpty()) return start\n\
            val result = if (preserveOrder) LinkedHashSet(start) else HashSet(start)\n\
            var elementsToCheck = result\n\
            var oldSize = 0\n\
            while (result.size > oldSize) {\n\
                oldSize = result.size\n\
                val toAdd = if (preserveOrder) linkedSetOf() else hashSetOf<T>()\n\
                elementsToCheck.forEach { toAdd.addAll(f(it)) }\n\
                result.addAll(toAdd)\n\
                elementsToCheck = toAdd\n\
            }\n\
            return result\n\
        }\n\
        fun <T> either(flag: Boolean): MutableCollection<T> =\n\
            if (flag) arrayListOf<T>() else linkedSetOf()\n\
        fun <T> reversed(flag: Boolean): MutableCollection<T> =\n\
            if (flag) linkedSetOf() else arrayListOf<T>()\n\
        fun <T> picked(which: Int): MutableCollection<T> = when (which) {\n\
            0 -> arrayListOf()\n\
            1 -> linkedSetOf()\n\
            else -> hashSetOf<T>()\n\
        }\n\
        fun box(): String {\n\
            val grown = grow(listOf(\"a\"), false) { value -> if (value == \"a\") listOf(\"b\") else emptyList() }\n\
            val ordered = grow(listOf(1), true) { value -> if (value == 1) listOf(2) else emptyList() }\n\
            val forward = either<String>(true)\n\
            forward.add(\"c\")\n\
            val backward = reversed<Int>(false)\n\
            backward.add(3)\n\
            val chosen = picked<String>(1)\n\
            chosen.add(\"d\")\n\
            val ok = grown.contains(\"a\") && grown.contains(\"b\") && grown.size == 2 &&\n\
                ordered.contains(1) && ordered.contains(2) && ordered.size == 2 &&\n\
                forward.contains(\"c\") && backward.contains(3) && chosen.contains(\"d\")\n\
            return if (ok) \"OK\" else \"FAIL\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "SiblingCollectionSupertype");
}

#[test]
fn an_unrelated_sibling_still_cannot_instantiate_a_collection() {
    const IF_SRC: &str = "fun box(flag: Boolean) {\n\
            val mixed = if (flag) linkedSetOf() else 1\n\
        }\n";
    assert_diagnostics(
        IF_SRC,
        &[(
            2,
            23,
            "cannot infer type for type parameter 'T'. Specify it explicitly.",
        )],
    );
    const BOTH_SRC: &str = "fun box(flag: Boolean) {\n\
            val mixed = if (flag) linkedSetOf() else hashSetOf()\n\
        }\n";
    assert_diagnostics(
        BOTH_SRC,
        &[
            (
                2,
                23,
                "cannot infer type for type parameter 'T'. Specify it explicitly.",
            ),
            (
                2,
                42,
                "cannot infer type for type parameter 'T'. Specify it explicitly.",
            ),
        ],
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
        &[(
            2,
            16,
            "cannot infer type for type parameter 'T'. Specify it explicitly.",
        )],
    );
}
