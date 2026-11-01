//! A typealias constructor in an INFERRED declaration.
//!
//! `val p = AliasedCell(MyClass())` is an unresolved reference unless the alias's expansion is
//! installed before declaration inference runs. The deferred run types a declaration by running the
//! real checker over its body, and source alias expansions were registered AFTER that run — so the
//! same call resolved inside a function body and declined at the top level.
use super::common;

#[path = "typealias_constructor_inference_e2e/anonymous_super_lambdas.rs"]
mod anonymous_super_lambdas;
#[path = "typealias_constructor_inference_e2e/parameterized_parent_aliases.rs"]
mod parameterized_parent_aliases;

#[test]
fn a_generic_typealias_constructor_types_an_inferred_declaration() {
    const SRC: &str = "class Cell<T>(val x: T)\n\
        typealias AliasedCell<TT> = Cell<TT>\n\
        class MyClass\n\
        val p = AliasedCell(MyClass())\n\
        fun box(): String = if (p.x is MyClass) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "GenericTypeAliasCtorInference");
}

#[test]
fn a_non_generic_typealias_constructor_types_an_inferred_declaration() {
    const SRC: &str = "class Cell<T>(val x: T)\n\
        class MyClass\n\
        typealias Fixed = Cell<MyClass>\n\
        val p = Fixed(MyClass())\n\
        fun box(): String = if (p.x is MyClass) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "FixedTypeAliasCtorInference");
}

/// Omitted alias arguments are type variables of the constructor call, not written arguments. The
/// stdlib's `typealias LinkedHashMap<K, V> = java.util.LinkedHashMap<K, V>` leaves `K` and `V` open
/// in its expansion; binding the Java constructor's own `K`/`V` to those open alias formals rejected
/// `LinkedHashMap(a)` with `Map<K, V>` against `Map<out K, out V>!` and typed the result as the
/// unrelated `LinkedHashMap<K, V>`.
#[test]
fn an_omitted_stdlib_alias_argument_is_inferred_from_the_constructor_arguments() {
    const SRC: &str = "fun <K, V> merge(a: Map<K, V>, b: Map<K, V>): Map<K, V> {\n\
        \x20   val out = LinkedHashMap(a)\n\
        \x20   out.putAll(b)\n\
        \x20   return out\n\
        }\n\
        fun keys(a: Map<String, Int>): List<String> = ArrayList(a.keys)\n\
        fun box(): String {\n\
        \x20   val merged = merge(mapOf(1 to \"O\"), mapOf(2 to \"K\"))\n\
        \x20   val copy: HashMap<Int, String> = HashMap(merged)\n\
        \x20   return if (keys(mapOf(\"x\" to 1)) == listOf(\"x\")) copy[1] + copy[2] else \"keys\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "OmittedStdlibAliasArguments");
}

/// The alias expansion decides where each inferred argument lands and which target arguments the
/// alias fixes on its own: `Flipped<X, Y> = Entry<Y, X>` swaps them, and `Keyed<V>` fixes `String`.
#[test]
fn an_omitted_source_alias_argument_is_inferred_through_the_alias_expansion() {
    const SRC: &str = "class Entry<A, B>(val a: A, val b: B)\n\
        typealias Flipped<X, Y> = Entry<Y, X>\n\
        typealias Keyed<V> = Entry<String, V>\n\
        typealias Same<A, B> = Entry<A, B>\n\
        fun box(): String {\n\
        \x20   val flipped = Flipped(1, \"K\")\n\
        \x20   val keyed = Keyed(\"O\", 2)\n\
        \x20   val same = Same(3L, 'c')\n\
        \x20   val text: String = keyed.a + flipped.b\n\
        \x20   val sum: Int = flipped.a + keyed.b\n\
        \x20   val code: Long = same.a + same.b.code\n\
        \x20   return if (sum == 3 && code == 102L) text else \"fail $sum $code\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "OmittedSourceAliasArguments");
}

const ALIAS_SUPERCLASS_CONSTRUCTOR: &str = r#"
open class Carrier<T>(val value: T)
typealias Nested<T> = Carrier<List<T>>
private fun <T> copy(value: T) = object : Nested<T>(listOf(value)) {}
fun box(): String {
    copy("OK")
    return "OK"
}
"#;

fn assert_complete_class_abi(classes: &common::ClassSets) {
    let inventory = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| {
                let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
                let header = format!(
                    "{:?}",
                    (
                        info.access,
                        info.this_class,
                        info.super_class,
                        info.interfaces(),
                        info.signature,
                        info.inner_classes,
                    )
                );
                (name.clone(), (header, common::member_table(bytes)))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let actual = inventory(&classes.krusty);
    let expected = inventory(&classes.reference);
    assert_eq!(actual, expected, "complete class ABI");
}

// PLATFORM: complete JVM class headers and member signatures for the expanded superclass.
#[test]
fn alias_superclass_constructor_complete_abi_matches_kotlinc() {
    assert_complete_class_abi(&common::classes_against_kotlinc_module(&[(
        "AliasSuperclassConstructor.kt",
        ALIAS_SUPERCLASS_CONSTRUCTOR,
    )]));
}

#[test]
fn alias_superclass_constructor_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        ALIAS_SUPERCLASS_CONSTRUCTOR,
        "alias_superclass_constructor",
    );
}

#[test]
fn a_direct_superclass_argument_keeps_the_enclosing_value() {
    let source = r#"
open class Carrier<T>(val value: T)
private fun <T> copy(value: T) = object : Carrier<List<T>>(listOf(value)) {}
fun box(): String {
    copy("OK")
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(source, "direct_superclass_lexical_value");
}

#[test]
fn an_aliased_superclass_argument_without_a_property_collision_runs_like_kotlinc() {
    let source = r#"
open class Carrier<T>(val stored: T)
typealias Nested<T> = Carrier<List<T>>
private fun <T> copy(value: T) = object : Nested<T>(listOf(value)) {}
fun box(): String = copy("OK").stored.single()
"#;
    common::expect_box_same_as_kotlinc(source, "aliased_superclass_distinct_property");
}

#[test]
fn a_named_class_superclass_argument_keeps_its_constructor_parameter() {
    let source = r#"
open class Carrier<T>(val value: T)
class Named<T>(value: T) : Carrier<List<T>>(listOf(value))
fun box(): String = Named("OK").value.single()
"#;
    common::expect_box_same_as_kotlinc(source, "named_superclass_constructor_parameter");
}

#[test]
fn an_aliased_superclass_lambda_keeps_the_enclosing_value() {
    let source = r#"
open class Carrier<T>(val value: () -> T)
typealias Nested<T> = Carrier<List<T>>
private fun <T> copy(value: T) = object : Nested<T>({ listOf(value) }) {}
fun box(): String = copy("OK").value().single()
"#;
    common::expect_box_same_as_kotlinc(source, "aliased_superclass_lexical_lambda");
}

#[test]
fn constructor_edge_baseline_lambda_capture_without_a_property_collision() {
    let source = r#"
open class Carrier<T>(val stored: () -> T)
typealias Nested<T> = Carrier<List<T>>
private fun <T> copy(value: T) = object : Nested<T>({ listOf(value) }) {}
fun box(): String = copy("OK").stored().single()
"#;
    common::expect_box_same_as_kotlinc(source, "anonymous_super_argument_lambda_capture_baseline");
}

#[test]
fn a_direct_superclass_lambda_keeps_the_enclosing_generic_value() {
    let source = r#"
open class Carrier<T>(val stored: () -> T)
private fun <T> copy(value: T) = object : Carrier<T>({ value }) {}
fun box(): String = copy("OK").stored()
"#;
    common::expect_box_same_as_kotlinc(source, "direct_superclass_generic_lambda_capture");
}

#[test]
fn a_nested_aliased_superclass_lambda_keeps_the_enclosing_generic_value() {
    let source = r#"
open class Carrier<T>(val stored: () -> T)
typealias Nested<T> = Carrier<List<T>>
private fun <T> copy(value: T) = object : Nested<T>({ { listOf(value) }() }) {}
fun box(): String = copy("OK").stored().single()
"#;
    common::expect_box_same_as_kotlinc(source, "nested_aliased_superclass_generic_lambda_capture");
}

#[test]
fn an_anonymous_superclass_lambda_retains_the_enclosing_mutable_cell() {
    let source = r#"
open class Carrier(val stored: () -> String)
fun box(): String {
    var value = "before"
    val copy = object : Carrier({ value }) {}
    value = "OK"
    return copy.stored()
}
"#;
    common::expect_box_same_as_kotlinc(source, "anonymous_superclass_lambda_shared_cell");
}

const SHADOWED_SUPERCLASS_LAMBDA: &str = r#"
open class Carrier(val stored: (String) -> String)
fun box(): String {
    val value = "unselected"
    val copy = object : Carrier({ value -> value }) {}
    return copy.stored("OK")
}
"#;

#[test]
fn a_superclass_lambda_parameter_shadows_the_enclosing_value() {
    common::expect_box_same_as_kotlinc(
        SHADOWED_SUPERCLASS_LAMBDA,
        "anonymous_superclass_lambda_parameter_shadow",
    );
}

// PLATFORM: the complete JVM ABI proves the shadowed outer value adds no capture storage.
#[test]
fn a_shadowed_superclass_lambda_has_the_complete_reference_class_abi() {
    assert_complete_class_abi(&common::classes_against_kotlinc_module(&[(
        "ShadowedSuperclassLambda.kt",
        SHADOWED_SUPERCLASS_LAMBDA,
    )]));
}
