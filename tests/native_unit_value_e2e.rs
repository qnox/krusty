//! `Unit` as a VALUE: a parameter of that type, an extension declared on it, and its identity.
//!
//! A `Unit` RETURN is nothing — a function that answers `Unit` answers the one value there is, and
//! a caller that needs it can name it without being handed it. A `Unit` PARAMETER is not nothing:
//! the runtime owns one singleton, a program can compare against it by identity and can declare
//! extensions on it, so the parameter carries the reference exactly as the JVM passes
//! `kotlin.Unit.INSTANCE`. That asymmetry is the whole of what these pin.
//!
//! The oracle is kotlinc and the backend under test is the NATIVE one. krusty's own JVM backend
//! does not yet lower these shapes — it emits a body the verifier rejects with
//! `Operand stack underflow` at the store that follows a `Unit`-valued expression — which is a
//! separate gap on that side, unchanged by anything here and confirmed present with these files'
//! own corpus cases (`codegen/box/basics/unit4.kt`,
//! `codegen/box/extensionFunctions/extensionFunctionDifferentReceivers.kt`,
//! `codegen/box/extensionProperties/extensionPropertyDifferentReceiver.kt`). So these ask kotlinc
//! what the answer is and require the native backend to give it, rather than requiring all three
//! to agree; add the JVM side here when it lowers them.

use super::common::{expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer and the NATIVE answer to agree; see the module note on the JVM side.
fn kotlinc_and_native_agree(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_native_box(source, stem, "OK");
}

/// A parameter of type `Unit`, passed and read back, keeping the singleton's identity.
#[test]
fn a_unit_parameter_carries_the_singleton() {
    let source = r#"
fun took(unit: Unit): Boolean = unit === Unit

fun passOn(unit: Unit): Unit = unit

fun box(): String {
    if (!took(Unit)) return "fail identity"
    if (passOn(Unit) !== Unit) return "fail pass on"
    val held: Any = Unit
    if (held !== Unit) return "fail as Any"
    return "OK"
}
"#;
    kotlinc_and_native_agree("native_unit_parameter", source);
}

/// A `when` whose arms all answer `Unit`, in every way Kotlin can spell it — the corpus shape.
#[test]
fn every_spelling_of_unit_answers_the_same_singleton() {
    let source = r#"
var global = 42

fun nothingMuch() {}

fun check(x: Int, unit: Unit): Boolean {
    var local = 5
    val y: Unit = when (x) {
        0 -> {}
        1 -> local = 6
        2 -> global = 43
        3 -> unit
        4 -> Unit
        5 -> nothingMuch()
        6 -> return true
        else -> { val z: Any = Unit; z as Unit }
    }
    if (local == 6 && global == 43) global = 44
    return y === Unit
}

fun box(): String {
    for (x in 0..7) {
        if (!check(x, Unit)) return "fail at " + x
    }
    return "OK"
}
"#;
    kotlinc_and_native_agree("native_unit_when_arms", source);
}

/// An extension function and an extension PROPERTY declared on `Unit`, whose receiver is the
/// singleton. `{ }()` is the corpus spelling: a lambda invoked for its `Unit` answer.
#[test]
fn unit_carries_the_extensions_declared_on_it() {
    let source = r#"
fun Unit.asFunction(): Boolean = true
val Unit.asProperty: Boolean get() = true

class A
fun A?.onNullable(): Boolean = true
val A?.nullableProperty: Boolean get() = true

fun box(): String {
    if (!Unit.asFunction()) return "fail function"
    if (!Unit.asProperty) return "fail property"
    if (!{ }().asFunction()) return "fail lambda function"
    if (!{ }().asProperty) return "fail lambda property"
    if (!null.onNullable()) return "fail nullable function"
    if (!null.nullableProperty) return "fail nullable property"
    return "OK"
}
"#;
    kotlinc_and_native_agree("native_unit_extensions", source);
}
