use super::{assert_complete_class_abi, common};

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

// The unchanged ten-class immediate-invocation ABI oracle and complete ledgers live in #2838.
// Its ordinary nested-lambda mismatch is byte-identical on the exact parent; PLACEMENT runtime
// remains checked below. This separate fixture isolates anonymous-super construction ownership.
// PLATFORM: compare every emitted class, complete headers and field/method tables.
#[test]
fn ordinary_anonymous_super_lambda_placement_has_the_complete_reference_class_abi() {
    const OWNERSHIP: &str = r#"
open class Carrier<T>(val stored: () -> T)
fun captureless() = object : Carrier<String>({ "OK" }) {}
fun <T> generic(value: T) = object : Carrier<T>({ value }) {}
class Outer(val value: String) {
    fun make() = object : Carrier<String>({ value }) {}
    fun String.both() = object : Carrier<String>({ this@Outer.value + this@both }) {}
    fun combined() = "K".both()
}
fun String.extension() = object : Carrier<String>({ this@extension }) {}
class Named<T>(value: T) : Carrier<T>({ value })
"#;
    assert_complete_class_abi(&common::classes_against_kotlinc_module(&[(
        "Ownership.kt",
        OWNERSHIP,
    )]));
}

#[test]
fn anonymous_and_named_super_lambdas_preserve_their_lexical_values() {
    common::expect_box_same_as_kotlinc(PLACEMENT, "anonymous_super_lambda_placement_values");
}

#[test]
fn anonymous_super_vararg_lambdas_keep_one_shared_capture_cell() {
    let source = r#"
open class Carrier(vararg val actions: () -> String)
fun box(): String {
    var value = "O"
    val copy = object : Carrier({ value }, { value += "K"; value }) {}
    if (copy.actions[0]() != "O") return "first"
    return copy.actions[1]()
}
"#;
    common::expect_box_same_as_kotlinc(source, "anonymous_super_vararg_shared_capture");
}

#[test]
fn anonymous_super_vararg_spread_preserves_neighbor_argument_order() {
    let source = r#"
open class Carrier(val first: String, vararg val actions: () -> String, val last: String)
fun box(): String {
    var log = ""
    fun edge(value: String): String { log += value; return value }
    val spread = arrayOf<() -> String>({ log + "S" })
    val copy = object : Carrier(edge("A"), *spread, { log + "E" }, last = edge("B")) {}
    if (log != "AB" || copy.first != "A" || copy.last != "B") return "order: " + log
    if (copy.actions[0]() != "ABS") return "spread"
    return if (copy.actions[1]() == "ABE") "OK" else "element"
}
"#;
    common::expect_box_same_as_kotlinc(source, "anonymous_super_vararg_neighbor_order");
}

#[test]
fn a_forwarded_lambda_inside_a_retained_vararg_closure_keeps_its_extension_receiver() {
    let source = r#"
open class Batch(vararg val actions: () -> String)
open class Carrier(val stored: () -> String)
fun String.make() = object : Batch({
    val inner = object : Carrier({ this@make }) {}
    inner.stored()
}) {}
fun box(): String = "OK".make().actions[0]()
"#;
    common::expect_box_same_as_kotlinc(source, "forwarded_inside_retained_super_receiver");
}

#[test]
fn a_retained_vararg_closure_inside_a_forwarded_lambda_keeps_its_extension_receiver() {
    let source = r#"
open class Batch(vararg val actions: () -> String)
open class Carrier(val stored: () -> String)
fun String.make() = object : Carrier({
    val inner = object : Batch({ this@make }) {}
    inner.actions[0]()
}) {}
fun box(): String = "OK".make().stored()
"#;
    common::expect_box_same_as_kotlinc(source, "retained_inside_forwarded_super_receiver");
}

#[test]
fn a_forwarded_lambda_distinguishes_same_typed_inner_and_declared_receivers() {
    let source = r#"
open class Carrier(val stored: () -> String)
fun String.make() = object : Carrier({
    with("K") { this@make + this }
}) {}
fun box(): String = "O".make().stored()
"#;
    common::expect_box_same_as_kotlinc(source, "forwarded_super_same_typed_receivers");
}

#[test]
fn a_forwarded_lambda_keeps_an_implicit_local_extension_call_receiver() {
    let source = r#"
open class Carrier(val stored: () -> String)
fun String.make(): Carrier {
    fun String.suffix() = this + "K"
    return object : Carrier({ suffix() }) {}
}
fun box(): String = "O".make().stored()
"#;
    common::expect_box_same_as_kotlinc(source, "forwarded_super_local_extension_receiver");
}

#[test]
fn a_superclass_companion_does_not_shift_the_forwarded_extension_receiver() {
    let source = r#"
open class Carrier(val stored: () -> String) { companion object }
fun String.make() = object : Carrier({ this@make }) {}
fun box(): String = "OK".make().stored()
"#;
    common::expect_box_same_as_kotlinc(source, "forwarded_super_companion_receiver");
}

const OWNED_RECEIVER: &str = r#"
open class Carrier(val stored: () -> String)
class Outer(val value: String) {
    fun result(): String {
        val copy = object : Carrier({ value + "K" }) {
            fun own() = value
        }
        if (copy.own() != value) return "class body"
        return copy.stored()
    }
}
fun box(): String = Outer("O").result()
"#;

// PLATFORM: retain the complete field/constructor ABI when the class body owns a receiver use.
#[test]
fn a_forwarded_and_class_owned_receiver_has_the_complete_reference_class_abi() {
    assert_complete_class_abi(&common::classes_against_kotlinc_module(&[(
        "OwnedReceiver.kt",
        OWNED_RECEIVER,
    )]));
}

#[test]
fn a_forwarded_and_class_owned_receiver_keeps_both_values() {
    common::expect_box_same_as_kotlinc(OWNED_RECEIVER, "forwarded_and_class_owned_super_receiver");
}

#[test]
fn a_constructor_packed_vararg_expression_keeps_its_extension_receiver() {
    let source = r#"
open class Batch(vararg val actions: () -> String)
fun String.make() = object : Batch(*arrayOf({ this@make })) {}
fun box(): String = "OK".make().actions[0]()
"#;
    common::expect_box_same_as_kotlinc(source, "constructor_packed_super_vararg_receiver");
}
