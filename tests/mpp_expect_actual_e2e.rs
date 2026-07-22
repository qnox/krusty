//! Multiplatform `expect`/`actual` (JVM model): a platform source set and its `dependsOn` chain
//! compile as ONE set; `strip_matched_expects` drops every `expect` header a matching `actual`
//! (or `actual typealias`) replaces. Gated on `// LANGUAGE: +MultiPlatformProjects`, like kotlinc.

use super::common;

fn run(src: &str) {
    let Some(got) = common::compile_and_run_with_stdlib(src, "MainKt") else {
        panic!("expected the box to compile and run");
    };
    assert_eq!(got, "OK");
}

/// Expect fun + expect val (extension receivers distinguish the two `k`s) replaced by actuals.
#[test]
fn expect_fun_and_extension_val_actualized() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect fun greet(): String
expect val Int.k: String
expect val String.k: String

actual fun greet(): String = "O"
actual val Int.k: String get() = "K"
actual val String.k: String get() = ""

fun box(): String = greet() + 1.k + "".k
"#);
}

/// `expect class` replaced by an `actual class`; common code constructs and calls it.
#[test]
fn expect_class_actualized_by_class() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect class Box() {
    fun value(): String
}

fun common(): String = Box().value()

actual class Box actual constructor() {
    actual fun value(): String = "OK"
}

fun box(): String = common()
"#);
}

/// `expect class` replaced by an `actual typealias` to an existing class.
#[test]
fn expect_class_actualized_by_typealias() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect class S

fun use(s: S): S = s

actual typealias S = String

fun box(): String = use("OK")
"#);
}

/// Overloaded expect funs match by arity.
#[test]
fn expect_overloads_match_by_arity() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
expect fun f(): String
expect fun f(n: Int): String

actual fun f(): String = "O"
actual fun f(n: Int): String = "K"

fun box(): String = f() + f(1)
"#);
}

/// An UNMATCHED `expect` stays in the tree and fails checking — skip, never mis-grade.
#[test]
fn unmatched_expect_fails_compile() {
    let src = r#"// LANGUAGE: +MultiPlatformProjects
expect fun missing(): String

fun box(): String = missing()
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "MainKt").is_none(),
        "an expect without an actual must not compile"
    );
}

/// Without the language flag, `expect` gets no special treatment (body-less fun fails).
#[test]
fn expect_without_flag_is_not_stripped() {
    let src = r#"expect fun greet(): String
actual fun greet(): String = "OK"

fun box(): String = greet()
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "MainKt").is_none(),
        "expect/actual outside +MultiPlatformProjects must not resolve"
    );
}

/// The `open val` accessor of an actualized class stays non-final so a common-code subclass can
/// override it (the property analog of the open-member finality rule).
#[test]
fn open_property_accessor_overridable_across_expect_actual() {
    run(r#"// LANGUAGE: +MultiPlatformProjects
class C2 : C1() {
    override val k = "K"
}

expect open class C1() {
    open val k: String
}

actual open class C1 {
    actual open val k = "FAIL"
}

fun box(): String = if (C2().k == "K") "OK" else "FAIL"
"#);
}
