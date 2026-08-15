//! Kotlin `for` loops resolve the three convention calls (`iterator`, `hasNext`, and `next`)
//! through the ordinary callable tower. Source members, top-level extensions, and member
//! extensions must therefore produce the same exact checker-to-lowering handoff as dependency
//! declarations.

use super::common;

fn assert_kotlinc_accepts(tag: &str, source: &str) {
    let (code, stderr) = common::kotlinc_source_result(tag, source);
    assert_eq!(code, 0, "kotlinc rejected {tag}: {stderr}");
}

#[test]
fn source_member_iterator_protocol_runs() {
    const SOURCE: &str = r#"
class Cursor(private val text: String) {
    private var index = 0
    operator fun hasNext(): Boolean = index < text.length
    operator fun next(): Char = text[index++]
}

class Letters(private val text: String) {
    operator fun iterator(): Cursor = Cursor(text)
}

// Ordinary members win over extensions even when both satisfy the convention.
operator fun Letters.iterator(): Cursor = Cursor("NO")

fun box(): String {
    var result = ""
    for (letter in Letters("OK")) result += letter
    return result
}
"#;

    assert_kotlinc_accepts("SourceMemberIteratorProtocol", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn top_level_extension_iterator_protocol_runs() {
    const SOURCE: &str = r#"
class Letters(val text: String)
class Cursor(val text: String, var index: Int = 0)

operator fun Letters.iterator(): Cursor = Cursor(text)
operator fun Cursor.hasNext(): Boolean = index < text.length
operator fun Cursor.next(): Char = text[index++]

fun box(): String {
    var result = ""
    for (letter in Letters("OK")) result += letter
    return result
}
"#;

    assert_kotlinc_accepts("TopLevelExtensionIteratorProtocol", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn type_parameter_bound_uses_static_iterator_convention() {
    const SOURCE: &str = r#"
open class Letters(val text: String)
class Cursor(val text: String, var index: Int = 0)

operator fun Letters.iterator(): Cursor = Cursor(text)
operator fun Cursor.hasNext(): Boolean = index < text.length
operator fun Cursor.next(): Char = text[index++]

fun <T : Letters> collect(letters: T): String {
    var result = ""
    for (letter in letters) result += letter
    return result
}

fun box(): String = collect(Letters("OK"))
"#;

    assert_kotlinc_accepts("TypeParameterIteratorConvention", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn implicit_receiver_member_extension_next_runs() {
    const SOURCE: &str = r#"
class Cursor {
    var available = true
    operator fun hasNext(): Boolean = if (available) {
        available = false
        true
    } else {
        false
    }
}

class Letters {
    operator fun iterator(): Cursor = Cursor()
}

operator fun Cursor.next(): String = "top-level"

class Scope {
    // A member extension is a nearer scope-tower rung than the top-level extension above.
    operator fun Cursor.next(): String = "OK"

    fun collect(): String {
        var result = ""
        for (letter in Letters()) result += letter
        return result
    }
}

fun box(): String = Scope().collect()
"#;

    assert_kotlinc_accepts("ImplicitReceiverMemberExtensionNext", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn selected_convention_requires_operator_modifier() {
    const SOURCE: &str = r#"
class Cursor {
    operator fun hasNext(): Boolean = false
    operator fun next(): String = "unused"
}

class Letters {
    fun iterator(): Cursor = Cursor()
}

fun test() {
    for (letter in Letters()) println(letter)
}
"#;

    let diagnostics = common::front_end_diagnostics(SOURCE, &[], None);
    assert!(
        diagnostics.iter().any(|message| {
            message.contains("'operator' modifier is required") && message.contains("iterator")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn speculative_hof_capability_probe_emits_no_iteration_diagnostic() {
    const SOURCE: &str = r#"
class Cursor

class Value {
    fun iterator(): Cursor = Cursor()
}

fun Value.consume(block: () -> Unit) = block()

fun box(): String {
    Value().consume { }
    return "OK"
}
"#;

    assert_kotlinc_accepts("SpeculativeHofIteratorProbe", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn private_top_level_iterator_extensions_run() {
    const SOURCE: &str = r#"
class Letters(val text: String)
class Cursor(val text: String, var index: Int = 0)

private operator fun Letters.iterator(): Cursor = Cursor(text)
private operator fun Cursor.hasNext(): Boolean = index < text.length
private operator fun Cursor.next(): Char = text[index++]

fun box(): String {
    var result = ""
    for (letter in Letters("OK")) result += letter
    return result
}
"#;

    assert_kotlinc_accepts("PrivateIteratorExtensions", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn primitive_receiver_and_iterator_extensions_run() {
    const SOURCE: &str = r#"
private var cursor = 0

operator fun Int.iterator(): Int {
    cursor = 0
    return this
}
operator fun Int.hasNext(): Boolean = cursor < this
operator fun Int.next(): Int = cursor++

fun box(): String {
    var result = ""
    for (value in 2) result += value
    return if (result == "01") "OK" else "fail: $result"
}
"#;

    assert_kotlinc_accepts("PrimitiveIteratorExtensions", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn generic_extension_return_specializes_through_receiver_supertype() {
    const SOURCE: &str = r#"
open class Bag<T>(val value: T)
class Strings(value: String) : Bag<String>(value)
class Cursor<T>(private val value: T) {
    private var available = true
    operator fun hasNext(): Boolean = available
    operator fun next(): T {
        available = false
        return value
    }
}

operator fun <T> Bag<T>.iterator(): Cursor<T> = Cursor(value)

fun box(): String {
    var result = ""
    for (value in Strings("OK")) result += value
    return result
}
"#;

    assert_kotlinc_accepts("GenericIteratorExtensionSubtype", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn caller_iterator_extension_does_not_rewrite_stdlib_hof_body() {
    const SOURCE: &str = r#"
operator fun CharSequence.iterator(): Iterator<Char> = emptyList<Char>().iterator()

fun box(): String {
    var result = ""
    val text: CharSequence = "OK"
    text.forEach { result += it }
    return result
}
"#;

    assert_kotlinc_accepts("CallerIteratorDoesNotRewriteForEach", SOURCE);
    assert_eq!(common::expect_box_run_with_stdlib(SOURCE, "Main"), "OK");
}

#[test]
fn protected_iterator_on_foreign_base_receiver_is_rejected() {
    const SOURCE: &str = r#"
class Cursor {
    operator fun hasNext(): Boolean = false
    operator fun next(): String = "unused"
}

open class Base {
    protected operator fun iterator(): Cursor = Cursor()
}

class Derived : Base() {
    fun collect(other: Base) {
        for (value in other) println(value)
    }
}
"#;

    let diagnostics = common::front_end_diagnostics(SOURCE, &[], None);
    assert!(
        diagnostics.iter().any(|message| {
            message.contains("cannot access 'iterator'") && message.contains("protected")
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn java_iterator_convention_methods_run_without_kotlin_operator_metadata() {
    let java = [
        (
            "fixtures/JLetters.java".to_string(),
            "package fixtures; public final class JLetters { public JCursor iterator() { return new JCursor(); } }".to_string(),
        ),
        (
            "fixtures/JCursor.java".to_string(),
            "package fixtures; public final class JCursor { private boolean available = true; public boolean hasNext() { return available; } public String next() { available = false; return \"OK\"; } }".to_string(),
        ),
    ];
    let Some((library, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = library.parent().map(std::path::Path::to_path_buf);
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let classpath = vec![library, stdlib];
    const SOURCE: &str = r#"
import fixtures.JLetters

fun box(): String {
    var result = ""
    for (value in JLetters()) result += value
    return result
}
"#;
    let classes = common::compile_in_process(SOURCE, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(SOURCE, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run box");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.trim(), "OK");
}
