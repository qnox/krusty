//! Operations over constants fold to their value, as kotlinc's `ConstEvaluationLowering` folds a
//! call to an `@IntrinsicConstEvaluation` builtin (and `==`, `<`, `&&`, ...) whose arguments are
//! all constants, and merges the constant parts of a string template.
use super::common;

fn assert_matches_kotlinc(name: &str, src: &str) {
    let class = format!("{name}Kt");
    common::byte_diff_against_kotlinc_cp(name, src, &class, &[common::stdlib_jar()])
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
}

#[test]
fn builtin_operations_over_constants_fold() {
    assert_matches_kotlinc(
        "FoldedOperations",
        "fun sink(value: Any?) {}\n\
         fun arithmetic() {\n\
         \x20   sink(1 + 2)\n\
         \x20   sink(2147483647 - 2)\n\
         \x20   sink(2147483647 + 1)\n\
         \x20   sink(3L * 4)\n\
         \x20   sink(7 % 2)\n\
         \x20   sink(1.0 + 2)\n\
         \x20   sink(0.1 + 0.2)\n\
         \x20   sink(7.0 / 0)\n\
         \x20   sink(-(3))\n\
         \x20   sink(1 shl 33)\n\
         \x20   sink(-8 ushr 1)\n\
         \x20   sink(-8L shr 65)\n\
         \x20   sink(0x7f.inv())\n\
         \x20   sink(6 and 3 or 8 xor 1)\n\
         }\n\
         fun conversions() {\n\
         \x20   sink(300.toByte())\n\
         \x20   sink(70000.toShort())\n\
         \x20   sink(10.toLong())\n\
         \x20   sink(2.5.toInt())\n\
         \x20   sink(1e20.toLong())\n\
         \x20   sink(65.toChar())\n\
         \x20   sink('a' + 1)\n\
         \x20   sink('b' - 'a')\n\
         }\n\
         fun comparisons() {\n\
         \x20   sink(1 < 2)\n\
         \x20   sink('a' >= 'b')\n\
         \x20   sink(5 == 5)\n\
         \x20   sink(1.0 == 1.0)\n\
         \x20   sink(true && false)\n\
         \x20   sink(true || false)\n\
         \x20   sink(!true)\n\
         \x20   sink(true xor false)\n\
         \x20   sink(3.compareTo(4))\n\
         \x20   sink(true.compareTo(false))\n\
         }\n\
         fun strings() {\n\
         \x20   sink(\"abc\".length)\n\
         \x20   sink(\"x\".plus(null))\n\
         \x20   sink(\"x\".plus(1 + 2))\n\
         \x20   sink(\"abc\"[1])\n\
         \x20   sink(\"abc\".get(0))\n\
         \x20   sink(\"\\uD83D\\uDE00\"[1])\n\
         }\n\
         fun equalities() {\n\
         \x20   sink(null == null)\n\
         \x20   sink(\"a\" == null)\n\
         \x20   sink(null != \"a\")\n\
         \x20   sink(\"a\" == \"a\")\n\
         \x20   sink(\"a\" != \"b\")\n\
         \x20   sink('a' == 'a')\n\
         \x20   sink(0.0 == -0.0)\n\
         }\n",
    );
}

#[test]
fn operations_kotlinc_cannot_evaluate_stay_operations() {
    assert_matches_kotlinc(
        "UnfoldedOperations",
        "fun sink(value: Any?) {}\n\
         fun f(x: Int) {\n\
         \x20   sink(7 / 0)\n\
         \x20   sink(7L % 0L)\n\
         \x20   sink(\"abc\"[5])\n\
         \x20   sink(\"abc\"[-1])\n\
         \x20   sink(\"abc\"[x])\n\
         \x20   sink(\"abc\" === \"abc\")\n\
         \x20   sink(x + 1 + 2)\n\
         \x20   sink(1 + 2 + x)\n\
         }\n",
    );
}

#[test]
fn constant_template_parts_merge() {
    assert_matches_kotlinc(
        "FoldedTemplates",
        "fun sink(value: Any?) {}\n\
         fun f(x: Int, s: String) {\n\
         \x20   sink(\"a${1}b${x}c${2 + 3}d$s\")\n\
         \x20   sink(\"$x${'q'}${true}${1.5}${null}\")\n\
         \x20   sink(\"${1L}${1.0f}${-0.0}\")\n\
         \x20   sink(\"pre${\"\"}$x\")\n\
         }\n",
    );
}

/// A condition folded to a constant still marks its source line, on a `nop`, before the branch is
/// decided statically, as kotlinc's `visitWhen` does.
#[test]
fn a_folded_condition_keeps_its_line_on_a_nop() {
    assert_matches_kotlinc(
        "FoldedConditions",
        "fun sink(value: Any?) {}\n\
         fun f(x: Int) {\n\
         \x20   if (1 != 0) {\n\
         \x20   }\n\
         \x20   if (2 < 1) sink(x) else sink(1)\n\
         \x20   if (x > 3) {\n\
         \x20       sink(0)\n\
         \x20   } else if ('a' == 'b') {\n\
         \x20       sink(1)\n\
         \x20   } else {\n\
         \x20       sink(x)\n\
         \x20   }\n\
         \x20   sink(if (true && x > 0) 1 else 2)\n\
         }\n",
    );
}

#[test]
fn folded_values_are_kotlin_values() {
    let src = "var failed = \"\"\n\
               fun check(case: Int, ok: Boolean) { if (!ok) failed += \" $case\" }\n\
               fun box(): String {\n\
               \x20   val nan = 0.0 / 0.0\n\
               \x20   check(1, (-2147483647 - 1) / -1 == -2147483647 - 1)\n\
               \x20   check(2, (-2147483647 - 1) % -1 == 0)\n\
               \x20   check(3, !(0.0 / 0.0 < 1.0))\n\
               \x20   check(4, !(0.0 / 0.0 == 0.0 / 0.0))\n\
               \x20   check(5, 0.0 == -0.0)\n\
               \x20   check(6, (0.0 / 0.0).compareTo(1.0) == 1)\n\
               \x20   check(7, (-0.0).compareTo(0.0) == -1)\n\
               \x20   check(8, 1 shl 33 == 2)\n\
               \x20   check(9, -1 ushr 28 == 15)\n\
               \x20   check(10, -1L ushr 60 == 15L)\n\
               \x20   check(11, 'a' + 2 == 'c')\n\
               \x20   check(12, 'c' - 'a' == 2)\n\
               \x20   check(13, 300.toByte() == 44.toByte())\n\
               \x20   check(14, (-1.5).toInt() == -1)\n\
               \x20   check(15, 1e20.toInt() == 2147483647)\n\
               \x20   check(16, nan.toLong() == 0L)\n\
               \x20   check(17, \"\\uD83D\\uDE00\".length == 2)\n\
               \x20   if (failed.length > 0) return \"failed$failed\"\n\
               \x20   return \"${1.0f / 3}|${1e7}|${1e-4}|${100.0}|${'x'}${true}${null}|${7L * 3}\"\n\
               }\n";
    let actual =
        common::compile_and_run_box(src, "constant_evaluation", &[common::stdlib_jar()], None)
            .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "0.33333334|1.0E7|1.0E-4|100.0|xtruenull|21");
}

/// kotlinc's exit code and complete stderr for `src` compiled on its own with the stdlib.
fn kotlinc_diagnostics(tag: &str, src: &str) -> (i32, String) {
    let root = common::scratch_dir()
        .expect("a scratch directory is available")
        .join(format!("constant_evaluation_{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("classes")).expect("create the kotlinc output directory");
    let source = root.join("Probe.kt");
    std::fs::write(&source, src).expect("write the probe source");
    common::kotlinc_compile(&[
        "-d".to_string(),
        root.join("classes").to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("the reference kotlinc is provisioned")
}

#[test]
fn overflowing_constant_arithmetic_folds_without_a_diagnostic() {
    let src = "const val WRAPPED = 2147483647 + 1\n\
               const val LONG_WRAPPED: Long = 9223372036854775807L * 2\n\
               val shifted = 1 shl 40\n\
               fun negated(): Int = -(-2147483647 - 1)\n";
    assert_eq!(kotlinc_diagnostics("overflow", src), (0, String::new()));
    let krusty =
        common::compile_in_process_diagnostics(src, "Overflow", &[common::stdlib_jar()], None);
    assert_eq!(krusty, Vec::<String>::new());
    assert_matches_kotlinc("FoldedOverflow", src);
}
