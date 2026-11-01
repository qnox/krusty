use super::common;

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

const PLACEMENT: &str = r#"
open class Carrier<T>(val stored: () -> T)
fun captureless() = object : Carrier<String>({ "OK" }) {}
fun <T> generic(value: T) = object : Carrier<T>({ value }) {}
fun <T> nested(value: T) = object : Carrier<List<T>>({ { listOf(value) }() }) {}
class Outer(val value: String) {
    fun make() = object : Carrier<String>({ value }) {}
    fun String.both() = object : Carrier<String>({ this@Outer.value + this@both }) {}
    fun combined() = "K".both()
}
fun String.extension() = object : Carrier<String>({ this@extension }) {}
class Named<T>(value: T) : Carrier<T>({ value })
fun box(): String {
    if (captureless().stored() != "OK") return "captureless"
    if (generic("OK").stored() != "OK") return "generic"
    if (nested("OK").stored().single() != "OK") return "nested"
    if (Outer("OK").make().stored() != "OK") return "outer"
    if ("OK".extension().stored() != "OK") return "extension"
    if (Outer("O").combined().stored() != "OK") return "both"
    if (Named("OK").stored() != "OK") return "named"
    return "OK"
}
"#;

// PLATFORM: compare all emitted JVM classes, complete headers and field/method tables.
#[test]
fn anonymous_super_lambda_placement_has_the_complete_reference_class_abi() {
    assert_complete_class_abi(&common::classes_against_kotlinc_module(&[(
        "Placement.kt",
        PLACEMENT,
    )]));
}

// PLATFORM: the escaped closure remains in the complete emitted method inventory.
#[test]
fn ordinary_immediate_and_escaping_lambdas_have_the_complete_reference_class_abi() {
    const SOURCE: &str = r#"
fun <T> nested(value: T) = { { listOf(value) }() }
fun <T> escape(value: T): () -> T = { value }
fun box(): String {
    val first = nested("OK")().single()
    return if (first == escape("OK")()) first else "escape"
}
"#;
    assert_complete_class_abi(&common::classes_against_kotlinc_module(&[(
        "Nested.kt",
        SOURCE,
    )]));
}

#[test]
fn immediate_literals_preserve_operand_order_captures_and_local_returns() {
    common::expect_box_same_as_kotlinc(
        include_str!("fixtures/immediateLambdaEscapingOperands.kt"),
        "immediate_function_literal_operands",
    );
    // PLATFORM: retain escaped function values through Native's closure allocation/invoke ABI.
    common::expect_native_box(
        include_str!("fixtures/immediateLambdaEscapingOperands.kt"),
        "immediate_function_literal_operands",
        "OK",
    );
}

#[test]
fn immediate_literals_without_escaping_values_use_the_shared_portable_corpus() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/immediateFunctionLiteralOperands.kt"),
        "portable_immediate_function_literal_operands",
    );
}
