//! A `when` in STATEMENT position may mix `Unit` arms with value-producing arms — kotlinc coerces
//! every arm to `Unit` (the value is popped), exactly like `if (c) println("x") else 42`. The IR
//! lowerer used to bail the whole file ("this construct is not yet supported by the IR backend")
//! because it could not distinguish a discarded statement from a value use; the checker's
//! discarded-expression mark supplies exactly that distinction. Real-world shape:
//! intellij-community's PluginAutoUpdateFUSCollector (`else -> autoupdateResult.getOrNull()!!` as a
//! `when` statement arm).

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

// The observed shape: subjectless statement `when`, value-producing `else` arm.
#[test]
fn subjectless_statement_when_with_value_arm() {
    const SRC: &str = "fun box(): String {\n\
        val r = Result.success(1)\n\
        when {\n\
            r.isFailure -> println(\"f\")\n\
            else -> r.getOrNull()!!\n\
        }\n\
        return \"OK\"\n\
    }\n";
    assert_eq!(run(SRC).expect("compiles and runs"), "OK");
}

// Subject form: value-producing non-`else` arm mixed with a `Unit` arm.
#[test]
fn subject_statement_when_with_value_arm() {
    const SRC: &str = "fun box(): String {\n\
        val x = 2\n\
        when (x) {\n\
            1 -> println(\"one\")\n\
            2 -> 42\n\
            else -> println(\"other\")\n\
        }\n\
        return \"OK\"\n\
    }\n";
    assert_eq!(run(SRC).expect("compiles and runs"), "OK");
}

// Exhaustive subject `when` with NO `else`: the last arm stays a real arm (a statement `when` is
// not forced to match), and the mixed shapes still coerce to `Unit`.
#[test]
fn exhaustive_statement_when_without_else_with_value_arm() {
    const SRC: &str = "fun box(): String {\n\
        val b = true\n\
        when (b) {\n\
            true -> println(\"t\")\n\
            false -> 7\n\
        }\n\
        return \"OK\"\n\
    }\n";
    assert_eq!(run(SRC).expect("compiles and runs"), "OK");
}

// A NON-exhaustive statement `when` without `else` whose subject matches nothing runs NO arm —
// the last arm must not degenerate into a catch-all just because the mixed arms join to a value
// type (that rewrite is only legal on the checker's exhaustiveness proof, which statement
// position never makes).
#[test]
fn non_exhaustive_statement_when_no_match_runs_nothing() {
    const SRC: &str = "var seen = \"\"\n\
        fun side(s: String): String { seen += s; return s }\n\
        fun box(): String {\n\
            val x = 3\n\
            when (x) {\n\
                1 -> println(\"one\")\n\
                2 -> side(\"b\")\n\
            }\n\
            return if (seen.isEmpty()) \"OK\" else \"FAIL:$seen\"\n\
        }\n";
    assert_eq!(run(SRC).expect("compiles and runs"), "OK");
}

// The value arm still RUNS for its side effect — coercion discards the value, not the evaluation.
#[test]
fn statement_when_value_arm_side_effect_happens() {
    const SRC: &str = "var seen = \"\"\n\
        fun side(): Int { seen += \"ran\"; return 1 }\n\
        fun box(): String {\n\
            val c = false\n\
            when {\n\
                c -> println(\"no\")\n\
                else -> side()\n\
            }\n\
            return if (seen == \"ran\") \"OK\" else \"FAIL\"\n\
        }\n";
    assert_eq!(run(SRC).expect("compiles and runs"), "OK");
}
