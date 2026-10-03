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

const NAME_BASED: &str =
    "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n";

fn name_based_flags() -> Vec<String> {
    vec![
        "-XXLanguage:+NameBasedDestructuring".to_string(),
        "-XXLanguage:+EnableNameBasedDestructuringShortForm".to_string(),
    ]
}

fn holder() -> &'static str {
    r#"
object O {
    var counter = 0
    val first: Int
        get() = counter++
}

class Once(val value: O) {
    operator fun iterator(): Cursor = Cursor(value)
}

class Cursor(val value: O) {
    private var pending = true
    operator fun hasNext(): Boolean = pending
    operator fun next(): O {
        pending = false
        return value
    }
}
"#
}

fn renamed_underscore(form: &str, stem: &str) {
    let source = format!("{NAME_BASED}{}\nfun box(): String {{\n    {form}\n    return if (O.counter == 1) \"OK\" else \"FAIL: ${{O.counter}}\"\n}}\n", holder());
    common::expect_box_same_as_kotlinc(&source, stem);
}

#[test]
fn short_declaration_underscore_rename_reads_the_property() {
    renamed_underscore("val (_ = first) = O", "ShortDeclUnderscore");
}

#[test]
fn full_declaration_underscore_rename_reads_the_property() {
    renamed_underscore("(val _ = first) = O", "FullDeclUnderscore");
}

#[test]
fn short_loop_underscore_rename_reads_the_property() {
    renamed_underscore("for ((_ = first) in Once(O)) {}", "ShortLoopUnderscore");
}

#[test]
fn full_loop_underscore_rename_reads_the_property() {
    renamed_underscore("for ((val _ = first) in Once(O)) {}", "FullLoopUnderscore");
}

#[test]
fn short_lambda_underscore_rename_reads_the_property() {
    renamed_underscore(
        "val f: (O) -> Unit = { (_ = first) -> }; f(O)",
        "ShortLambdaUnderscore",
    );
}

#[test]
fn full_lambda_underscore_rename_reads_the_property() {
    renamed_underscore(
        "val f: (O) -> Unit = { (val _ = first) -> }; f(O)",
        "FullLambdaUnderscore",
    );
}

fn implicit_underscore(body: &str) {
    let source = format!("{NAME_BASED}class P(val first: Int, val second: Int)\n{body}\n");
    let sources = [("Main.kt", source.as_str())];
    let expected = common::reference_error_ledger(&sources, &name_based_flags());
    assert!(!expected.is_empty(), "kotlinc reported no error");
    assert_eq!(
        common::krusty_error_ledger(&sources),
        expected,
        "krusty's complete ledger against kotlinc {}",
        krusty::kotlin_version::target()
    );
}

#[test]
fn short_declaration_implicit_underscore_is_forbidden() {
    implicit_underscore("fun box(p: P) { val (_) = p }");
}

#[test]
fn full_declaration_implicit_underscore_is_forbidden() {
    implicit_underscore("fun box(p: P) { (val _) = p }");
}

#[test]
fn short_loop_implicit_underscore_is_forbidden() {
    implicit_underscore(
        "class Bag(private val item: P) {\n    operator fun iterator(): BagCursor = BagCursor(item)\n}\nclass BagCursor(val item: P) {\n    private var pending = true\n    operator fun hasNext(): Boolean = pending\n    operator fun next(): P { pending = false; return item }\n}\nfun box(b: Bag) { for ((_) in b) {} }",
    );
}

#[test]
fn full_loop_implicit_underscore_is_forbidden() {
    implicit_underscore(
        "class Bag(private val item: P) {\n    operator fun iterator(): BagCursor = BagCursor(item)\n}\nclass BagCursor(val item: P) {\n    private var pending = true\n    operator fun hasNext(): Boolean = pending\n    operator fun next(): P { pending = false; return item }\n}\nfun box(b: Bag) { for ((val _) in b) {} }",
    );
}

#[test]
fn short_lambda_implicit_underscore_is_forbidden() {
    implicit_underscore("fun box(p: P) {\n    val f: (P) -> Unit = { (_) -> }\n    f(p)\n}");
}

#[test]
fn full_lambda_implicit_underscore_is_forbidden() {
    implicit_underscore("fun box(p: P) {\n    val f: (P) -> Unit = { (val _) -> }\n    f(p)\n}");
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

/// The type written on a whole destructured lambda parameter (`(a, b): P ->`, `[a, b]: P ->`) is
/// that parameter's type, so a lambda with no expected function type still destructures, by position
/// and by name (corpus regressions/directInvokeNameBasedDestructuring.kt).
const TYPED_DESTRUCTURED_LAMBDA_PARAMETER: &str = r#"
// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm
object First
object Second
class Joined(val first: First, val second: Second)
class P(val first: First, val second: Second) {
    operator fun component1() = first
    operator fun component2() = second
}
fun positional() = { [a, b]: P -> Joined(a, b) }(P(First, Second))
fun byName() = { (first, second): P -> Joined(first, second) }(P(First, Second))
fun renamed() = { (a = first, b = second): P -> Joined(a, b) }(P(First, Second))
fun underscore() = { (_ = first, second): P -> second }(P(First, Second))
fun box(): String {
    val stored = { (second, first): P -> Joined(first, second) }
    val positional = positional()
    val named = byName()
    val renamed = renamed()
    val retained = stored(P(First, Second))
    val complete = positional.first === First && positional.second === Second &&
        named.first === First && named.second === Second &&
        renamed.first === First && renamed.second === Second &&
        underscore() === Second && retained.first === First && retained.second === Second
    return if (complete) "OK" else "fail"
}
"#;

#[test]
fn a_typed_destructured_lambda_parameter_needs_no_expected_type() {
    common::assert_accepted_like_kotlinc(TYPED_DESTRUCTURED_LAMBDA_PARAMETER);
    assert_eq!(run(TYPED_DESTRUCTURED_LAMBDA_PARAMETER), "OK");
}
