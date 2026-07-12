//! Full-form destructuring (`+NameBasedDestructuring`): each component carries its own `val`/`var`
//! keyword — `(val a, val b) = e` (name-based, binds by property name), `[val a, val b] = e`
//! (positional, `componentN`), with optional `: T` and `= sourceProp` renaming. Distinct from the
//! leading-keyword short form `val (a, b) = e`.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn paren_full_form_binds_by_name_with_rename_and_var() {
    const SRC: &str = "// LANGUAGE: +NameBasedDestructuring\n\
        data class P(val pProp: Int, var pVar: String)\n\
        fun box(): String {\n\
        \x20 val src = P(1, \"x\")\n\
        \x20 (val pProp, val pVar) = src\n\
        \x20 if (pProp != 1 || pVar != \"x\") return \"FAIL a\"\n\
        \x20 (val number = pProp, val text = pVar) = src\n\
        \x20 if (number != 1 || text != \"x\") return \"FAIL b\"\n\
        \x20 (var mutableNumber = pProp) = src\n\
        \x20 mutableNumber += 1\n\
        \x20 if (mutableNumber != 2) return \"FAIL c\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("paren full form"), "OK");
}

#[test]
fn bracket_full_form_is_positional() {
    const SRC: &str = "// LANGUAGE: +NameBasedDestructuring\n\
        data class Tuple(val first: String, val second: Int)\n\
        fun box(): String {\n\
        \x20 val x = Tuple(\"OK\", 1)\n\
        \x20 [val a, val b] = x\n\
        \x20 if (a != \"OK\" || b != 1) return \"FAIL 1\"\n\
        \x20 [val b2: String, val a2: Int] = x\n\
        \x20 if (b2 != \"OK\" || a2 != 1) return \"FAIL 2\"\n\
        \x20 [var c, var d] = x\n\
        \x20 c = \"KO\"; d = 2\n\
        \x20 if (c != \"KO\" || d != 2) return \"FAIL 3\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("bracket full form"), "OK");
}

#[test]
fn full_form_rejected_without_feature() {
    // Without `+NameBasedDestructuring`, `(val a, val b) = e` is not valid Kotlin — krusty must
    // reject it (compile fails), matching a drop-in kotlinc.
    const SRC: &str = "data class P(val a: Int, val b: Int)\n\
        fun box(): String {\n\
        \x20 (val a, val b) = P(1, 2)\n\
        \x20 return if (a == 1 && b == 2) \"OK\" else \"FAIL\"\n\
        }\n\
        fun main() { println(box()) }\n";
    // Rejected → the harness returns None (compile failure) or a non-"OK" result.
    assert_ne!(run(SRC).as_deref(), Some("OK"));
}
