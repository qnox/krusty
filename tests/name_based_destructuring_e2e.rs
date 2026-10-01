//! Name-based `[a, b]` destructuring (`// LANGUAGE: +NameBasedDestructuring`, kotlinc's
//! `-Xname-based-destructuring`). A drop-in accepts it only when the feature is enabled. Without the
//! flag kotlinc still parses the brackets and reports that the feature is available since language
//! version 2.5, and every later declaration in the file stays in scope. Both `[a, b]` and `(a, b)`
//! desugar to the same positional `componentN` calls (proven byte-identical against kotlinc), so the
//! compiled-and-run result is "OK".

use super::common;

/// Strict stdlib/JDK run: missing tooling or a rejected source panics with diagnostics.
fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "Nb")
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
    assert_eq!(run(FOR_AND_VAL), "OK");
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
    assert_eq!(run(VAR_CAPTURED), "OK");
}

#[test]
fn name_based_destructuring_rejected_without_flag() {
    // Same source WITHOUT the `// LANGUAGE:` directive: krusty must reject `[a, b]` (compile fails →
    // `None`), exactly as default-flags kotlinc does. A `Some` here would mean we wrongly accepted it.
    let src = FOR_AND_VAL.replace("// LANGUAGE: +NameBasedDestructuring\n", "");
    // Only meaningful when the toolchain is present (otherwise both branches skip).
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert!(
        common::compile_in_process(&src, "Nb", &[stdlib], Some(jdk.as_path()),).is_none(),
        "krusty accepted `[a, b]` destructuring without +NameBasedDestructuring"
    );
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

#[test]
fn name_based_for_loop() {
    const SRC: &str =
        "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
        data class P(val a: Int, val b: Int)\n\
        fun box(): String {\n\
        \x20 var sum = 0\n\
        \x20 for ((y = b, x = a) in listOf(P(1, 2), P(3, 4))) {\n\
        \x20   sum += x * 10 + y\n\
        \x20 }\n\
        \x20 return if (sum == 12 + 34) \"OK\" else \"fail: \" + sum\n\
        }\n";
    assert_eq!(run_stdlib(SRC).expect("name-based for loop"), "OK");
}

#[test]
fn name_based_for_withindex_library() {
    // A LIBRARY receiver (`IndexedValue` from `withIndex()`) read by property name.
    const SRC: &str =
        "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
        fun box(): String {\n\
        \x20 val s = StringBuilder()\n\
        \x20 for ((i = index, v = value) in listOf(\"a\", \"b\").withIndex()) {\n\
        \x20   s.append(\"\" + i + v)\n\
        \x20 }\n\
        \x20 return if (s.toString() == \"0a1b\") \"OK\" else \"fail: \" + s\n\
        }\n";
    assert_eq!(run_stdlib(SRC).expect("name-based withIndex"), "OK");
}

#[test]
fn name_based_lambda_shortform() {
    // Short-form paren lambda param under the flag binds each var by its own property name.
    const SRC: &str =
        "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
        data class P(val a: Int, val b: Int)\n\
        fun box(): String {\n\
        \x20 val f: (P) -> Int = { (b, a) -> a * 10 + b }\n\
        \x20 return if (f(P(1, 2)) == 12) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run_stdlib(SRC).expect("name-based lambda"), "OK");
}

/// A `for` bracket pattern without the feature is one language-version error. The loop is still
/// parsed, so the ledger is that gate and nothing else.
#[test]
fn ungated_bracket_destructure_in_a_loop_is_a_language_version_error() {
    const SRC: &str = r#"class C {
    operator fun component1(): Int = 1
    operator fun component2(): Int = 2
}

class One {
    operator fun iterator(): One = this
    operator fun hasNext(): Boolean = false
    operator fun next(): C = C()
}

fun box(): String {
    var result = 0
    for ([a, b] in One()) {
        result += a + b
    }
    return if (result == 0) "OK" else "fail"
}
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", SRC)], &[]);
}

/// A local `val` bracket pattern without the feature is one language-version error. The
/// declaration stays bound, so the ledger is that gate and nothing else.
#[test]
fn ungated_bracket_destructure_in_a_declaration_is_a_language_version_error() {
    const SRC: &str = r#"class C {
    operator fun component1(): Int = 1
    operator fun component2(): Int = 2
}

fun box(): String {
    val [x, y] = C()
    return if (x + y == 3) "OK" else "fail"
}
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", SRC)], &[]);
}

/// A lambda-parameter bracket pattern without the feature is one language-version error. The
/// parameter stays bound, so the ledger is that gate and nothing else.
#[test]
fn ungated_bracket_destructure_in_a_lambda_parameter_is_a_language_version_error() {
    const SRC: &str = r#"class C {
    operator fun component1(): Int = 1
    operator fun component2(): Int = 2
}

fun box(): String {
    val read: (C) -> Int = { [p, q] -> p + q }
    return if (read(C()) == 3) "OK" else "fail"
}
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", SRC)], &[]);
}

/// The same gate must not drop the file from the module. Another file still sees the class and
/// the extension declared beside the bracket destructure.
#[test]
fn ungated_bracket_destructure_stays_visible_across_files() {
    const LIB: &str = r#"class C {
    operator fun component1(): Int = 1
    operator fun component2(): Int = 2
}

class One {
    operator fun iterator(): One = this
    operator fun hasNext(): Boolean = false
    operator fun next(): C = C()
}

fun hash(): Int {
    var result = 0
    for ([a, b] in One()) {
        result += a + b
    }
    return result
}

fun <T> T.tag(): T = this
"#;
    const USE: &str = r#"class Payload(val value: Int)

class Builder {
    lateinit var description: Payload
}

fun build(block: Builder.() -> Unit): Builder {
    val builder = Builder()
    block(builder)
    return builder
}

fun box(): String {
    val built = build {
        description = Payload(7).tag()
    }
    return if (built.description.value == 7 && hash() == 0) "OK" else "fail"
}
"#;
    common::assert_errors_match_kotlinc(&[("Lib.kt", LIB), ("Use.kt", USE)], &[]);
}
