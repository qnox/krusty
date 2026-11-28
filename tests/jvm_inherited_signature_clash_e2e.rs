//! Distinct Kotlin member slots of one class that share a JVM signature: kotlinc's
//! ACCIDENTAL_OVERRIDE and CONFLICTING_INHERITED_JVM_DECLARATIONS, compared with kotlinc in full.

use super::common;
use super::diagnostics_parity_support::complete_errors;

fn assert_kotlinc_diagnostics(file: &str, source: &str) {
    assert_kotlinc_module_diagnostics(&[(file, source)]);
}

fn assert_kotlinc_module_diagnostics(files: &[(&str, &str)]) {
    let source = files;
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(files, std::slice::from_ref(&stdlib));
    assert_ne!(result.reference_code, 0, "kotlinc accepted {source:?}");
    assert_ne!(result.krusty_code, 0, "krusty accepted {source:?}");
    let mut observed = complete_errors(&result.krusty_stderr, true);
    observed.extend(complete_errors(&result.krusty_stdout, false));
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error for {source:?}"
    );
    assert_eq!(observed, expected, "diagnostic mismatch for {source:?}");
}

#[test]
fn a_declaration_whose_erasure_is_an_inherited_methods_is_an_accidental_override() {
    assert_kotlinc_diagnostics(
        "ErasedParameter.kt",
        "open class Base {\n    fun take(values: List<String>) {}\n}\n\
         class Derived : Base() {\n    fun take(values: List<Int>) {}\n}\n",
    );
}

#[test]
fn a_declaration_that_takes_an_overridden_declarations_bridge_is_an_accidental_override() {
    // `Derived.take(Any)` is `take(Object)`, the bridge `take(String)` needs for `Box<T>.take`;
    // the override itself is not reported.
    assert_kotlinc_diagnostics(
        "BridgeSignature.kt",
        "open class Box<T> {\n    open fun take(value: T) {}\n}\n\
         class Derived : Box<String>() {\n    override fun take(value: String) {}\n    \
         fun take(value: Any) {}\n}\n",
    );
}

#[test]
fn a_declaration_that_takes_a_grandparents_signature_is_an_accidental_override() {
    assert_kotlinc_diagnostics(
        "GrandparentSignature.kt",
        "interface Sink<X> {\n    fun put(value: X)\n}\n\
         open class StringSink : Sink<String> {\n    override fun put(value: String) {}\n}\n\
         open class AnySink : StringSink() {\n    fun put(value: Any) {}\n}\n",
    );
}

#[test]
fn an_inherited_interface_default_keeps_its_own_signature() {
    assert_kotlinc_diagnostics(
        "InterfaceDefault.kt",
        "interface Box<T> {\n    fun take(value: T) {}\n}\n\
         class Derived : Box<String> {\n    fun take(value: Any) {}\n}\n",
    );
}

#[test]
fn two_inherited_declarations_with_one_erasure_clash_on_the_class() {
    assert_kotlinc_diagnostics(
        "InheritedClash.kt",
        "interface Strings<T> {\n    fun take(values: List<T>) {}\n}\n\
         interface Numbers<T> {\n    fun take(values: List<T>) {}\n}\n\
         class Both : Strings<String>, Numbers<Int>\n",
    );
}

#[test]
fn two_overrides_whose_bridges_share_a_signature_clash_on_the_class() {
    assert_kotlinc_diagnostics(
        "BridgeClash.kt",
        "open class Base<T> {\n    open fun echo(value: T): T = value\n}\n\
         interface Echo<T> {\n    fun echo(value: T): T = value\n}\n\
         class Specialized : Base<Int>(), Echo<String> {\n    \
         override fun echo(value: String): String = value\n    \
         override fun echo(value: Int): Int = value\n}\n",
    );
}

/// kotlinc's Java override checker lets `Foo.foo(T)` override the Kotlin extension `B.foo()` it
/// inherits, so `FooImpl`'s one `E.foo()` overrides both and occupies `foo(Object)` alone.
#[test]
fn a_java_method_that_takes_an_inherited_extensions_receiver_overrides_it() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[
            ("Base.kt", "interface Base<B> {\n    fun B.foo()\n}\n"),
            (
                "Foo.java",
                "public interface Foo<T> extends Base<T> {\n    void foo(T t);\n}\n",
            ),
            (
                "main.kt",
                "class FooImpl<E> : Foo<E> {\n    override fun E.foo() {}\n}\n",
            ),
        ],
        std::slice::from_ref(&stdlib),
    );
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}

/// An `internal` member's JVM name carries the module suffix, so a public declaration of the same
/// Kotlin name and erasure in a subclass of another file takes a different signature.
#[test]
fn an_internal_member_of_another_file_keeps_its_module_name() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[
            (
                "Base.kt",
                "open class Base {\n    internal fun take(values: List<String>) {}\n}\n",
            ),
            (
                "Derived.kt",
                "class Derived : Base() {\n    fun take(values: List<Int>) {}\n}\n",
            ),
        ],
        std::slice::from_ref(&stdlib),
    );
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}

/// A sibling file's `internal` overload, whose JVM name carries the module suffix, leaves the
/// signatures of its public overloads comparable.
#[test]
fn an_internal_overload_of_another_file_does_not_hide_a_public_ones_signature() {
    assert_kotlinc_module_diagnostics(&[
        (
            "Base.kt",
            "open class Base {\n    internal fun take(x: List<String>) {}\n    \
             fun take(x: Set<String>) {}\n}\n",
        ),
        (
            "Derived.kt",
            "class Derived : Base() {\n    fun take(x: Set<Int>) {}\n}\n",
        ),
    ]);
}

/// A dependency superclass's overload the file never calls still takes its JVM signature: a
/// declaration that erases to it is an accidental override, listed as kotlinc lists it.
#[test]
fn an_unused_dependency_overload_takes_its_signature() {
    let library = common::kotlinc_library(
        "package lib\nopen class Base {\n    fun take(values: List<String>) {}\n    \
         fun take(value: Int) {}\n}\n",
    )
    .expect("kotlinc compiles the dependency");
    let stdlib = common::stdlib_jar();
    let source =
        "import lib.Base\nclass Derived : Base() {\n    fun take(values: List<Int>) {}\n}\n";
    let result = common::compiler_diagnostics(&[("Derived.kt", source)], &[library, stdlib]);
    assert_ne!(result.reference_code, 0, "kotlinc accepted {source:?}");
    assert_ne!(result.krusty_code, 0, "krusty accepted {source:?}");
    let mut observed = complete_errors(&result.krusty_stderr, true);
    observed.extend(complete_errors(&result.krusty_stdout, false));
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error for {source:?}"
    );
    assert_eq!(observed, expected, "diagnostic mismatch for {source:?}");
}
