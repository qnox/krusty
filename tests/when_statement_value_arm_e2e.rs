//! Statement-position `when` expressions coerce every arm to `Unit` while preserving arm effects.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

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
