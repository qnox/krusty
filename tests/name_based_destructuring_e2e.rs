//! Name-based `[a, b]` destructuring (`// LANGUAGE: +NameBasedDestructuring`, kotlinc's
//! `-Xname-based-destructuring`). A drop-in must accept it ONLY when the feature is enabled and reject
//! it otherwise — matching `kotlinc`, which errors "the feature name based destructuring is
//! experimental" without the flag. Both `[a, b]` and `(a, b)` desugar to the same positional
//! `componentN` calls (proven byte-identical against kotlinc), so the compiled-and-run result is "OK".

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Nb")
}

const FOR_AND_VAL: &str = r#"
// LANGUAGE: +NameBasedDestructuring
class C(val i: Int) {
    operator fun component1() = i + 1
    operator fun component2() = i + 2
}
fun box(): String {
    var s = ""
    val arr = arrayOf(C(0), C(1), C(2))
    for ([a, b] in arr) { s += "$a:$b;" }
    if (s != "1:2;2:3;3:4;") return "for: $s"
    val [x, y] = C(10)
    if (x != 11 || y != 12) return "val: $x,$y"
    return "OK"
}
"#;

#[test]
fn name_based_destructuring_runs_when_enabled() {
    if let Some(out) = run(FOR_AND_VAL) {
        assert_eq!(out, "OK");
    }
}

const VAR_CAPTURED: &str = r#"
// LANGUAGE: +NameBasedDestructuring
class A {
    operator fun component1() = 1
    operator fun component2() = 2
}
fun box(): String {
    var [a, b] = A()
    val local = { a = 3 }
    local()
    return if (a == 3 && b == 2) "OK" else "fail"
}
"#;

#[test]
fn var_component_captured_and_mutated_in_lambda() {
    // A `var` destructured component captured AND written by a closure must be boxed (`Ref`), so the
    // closure's write is visible to the outer read. Regression for the lower_destructure boxing fix.
    if let Some(out) = run(VAR_CAPTURED) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn name_based_destructuring_rejected_without_flag() {
    // Same source WITHOUT the `// LANGUAGE:` directive: krusty must reject `[a, b]` (compile fails →
    // `None`), exactly as default-flags kotlinc does. A `Some` here would mean we wrongly accepted it.
    let src = FOR_AND_VAL.replace("// LANGUAGE: +NameBasedDestructuring\n", "");
    // Only meaningful when the toolchain is present (otherwise both branches skip).
    if let (Some(stdlib), Some(jdk)) = (common::stdlib_jar(), common::jdk_modules()) {
        assert!(
            common::compile_in_process(&src, "Nb", &[stdlib], Some(&jdk),).is_none(),
            "krusty accepted `[a, b]` destructuring without +NameBasedDestructuring"
        );
    }
}

// --- Short-form name-based renaming (`val (a = prop) = src`) → a by-name property read. ---

fn run_stdlib(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn name_based_rename() {
    const SRC: &str =
        "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
        data class P(val first: Int, val second: String)\n\
        fun box(): String {\n\
        \x20 val src = P(1, \"OK\")\n\
        \x20 val (number = first, text = second) = src\n\
        \x20 return if (number == 1 && text == \"OK\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run_stdlib(SRC).expect("name-based rename"), "OK");
}

#[test]
fn name_based_reorder() {
    const SRC: &str =
        "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
        data class P(val a: Int, val b: Int)\n\
        fun box(): String {\n\
        \x20 val src = P(1, 2)\n\
        \x20 val (y = b, x = a) = src\n\
        \x20 return if (x == 1 && y == 2) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run_stdlib(SRC).expect("name-based reorder"), "OK");
}
