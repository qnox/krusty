//! The receiver of an invoke runs before its arguments. Facts that evaluation proved — a cast,
//! a not-null assertion, an assignment — are visible to those arguments. A fact is visible only
//! when every completing path proved it. A cast in an argument does not flow back onto the receiver.

use super::common;

const SOURCE: &str = r#"
operator fun String.invoke(unused: String): String = "String.invoke(String)"
operator fun String.invoke(unused: Any): String = "String.invoke(Any)"
operator fun Any.invoke(unused: String): String = "Any.invoke(String)"
operator fun Any.invoke(unused: Any): String = "Any.invoke(Any)"
operator fun Boolean.invoke(unused: String): String = "Boolean.invoke(String)"
operator fun Boolean.invoke(unused: Any): String = "Boolean.invoke(Any)"
operator fun Int.invoke(unused: Int): String = "Int.invoke(Int)"
operator fun Int.invoke(unused: Int?): String = "Int.invoke(Int?)"

open class Base
class Derived : Base()
operator fun Derived.invoke(unused: Derived): String = "Derived.invoke(Derived)"
operator fun Derived.invoke(unused: Base): String = "Derived.invoke(Base)"
operator fun Base.invoke(unused: Base): String = "Base.invoke(Base)"

fun box(): String {
    val checks = listOf(
        implicitArgumentCast() to "Any.invoke(String)",
        explicitArgumentCast() to "Any.invoke(String)",
        implicitReceiverCast() to "String.invoke(String)",
        explicitReceiverCast() to "String.invoke(String)",
        implicitInvokeExplicitRecieverArgumentCast() to "Any.invoke(String)",
        explicitInvokeImplicitRecieverArgumentCast() to "Any.invoke(String)",
        explicitInvokeExplicitRecieverArgumentCast() to "Any.invoke(String)",
        notNullReceiver() to "String.invoke(String)",
        notNullInSequence() to "Int.invoke(Int)",
        assignmentOnEveryBranch(true) to "Derived.invoke(Derived)",
        assignmentOnEveryBranch(false) to "Derived.invoke(Derived)",
        assignmentOnOneBranch(true) to "Base.invoke(Base)",
        castOnEveryBranch(true) to "String.invoke(String)",
        castOnEveryBranch(false) to "String.invoke(String)",
        castOnOneBranch(true) to "Any.invoke(Any)",
        castOnOneBranch(false) to "Any.invoke(Any)",
        shortCircuitLeft() to "Boolean.invoke(String)",
        shortCircuitRight(true) to "Boolean.invoke(Any)",
        elvisRight() to "Any.invoke(Any)",
        whenEveryBranch(true) to "String.invoke(String)",
        whenOneBranch(false) to "Any.invoke(Any)",
    )
    val result = checks.mapNotNull { (actual, expected) ->
        if (actual == expected) null else "$actual != $expected"
    }
    return if (result.isEmpty()) "OK" else result.joinToString("\n")
}

fun implicitArgumentCast(): String {
    val a: Any = ""
    return a(a as String)
}

fun explicitArgumentCast(): String {
    val a: Any = ""
    return a.invoke(a as String)
}

fun implicitReceiverCast(): String {
    val a: Any = ""
    return (a as String)(a)
}

fun explicitReceiverCast(): String {
    val a: Any = ""
    return (a as String).invoke(a)
}

fun implicitInvokeExplicitRecieverArgumentCast(): String {
    val a: Any = ""
    with (a) {
        return this(this as String)
    }
}

fun explicitInvokeImplicitRecieverArgumentCast(): String {
    val a: Any = ""
    with (a) {
        return invoke(this as String)
    }
}

fun explicitInvokeExplicitRecieverArgumentCast(): String {
    val a: Any = ""
    with (a) {
        return this.invoke(this as String)
    }
}

fun notNullReceiver(): String {
    val a: String? = "n"
    return a!!(a)
}

fun notNullInSequence(): String {
    var n: Int? = 1
    return (n!! + 0)(n)
}

fun assignmentOnEveryBranch(c: Boolean): String {
    var a: Base = Base()
    if (c) { a = Derived() } else { a = Derived() }
    return a(a)
}

fun assignmentOnOneBranch(c: Boolean): String {
    var a: Base = Base()
    if (c) { a = Derived() } else { a = Base() }
    return a(a)
}

fun castOnEveryBranch(c: Boolean): String {
    val a: Any = "b"
    return (if (c) a as String else a as String)(a)
}

fun castOnOneBranch(c: Boolean): String {
    val a: Any = "o"
    return (if (c) a as String else a)(a)
}

fun shortCircuitLeft(): String {
    val a: Any = "l"
    return ((a as String).isNotEmpty() && true)(a)
}

fun shortCircuitRight(c: Boolean): String {
    val a: Any = "r"
    return (c && (a as String).isNotEmpty())(a)
}

fun nullable(): Any? = null

fun elvisRight(): String {
    val a: Any = "e"
    return (nullable() ?: (a as String))(a)
}

fun whenEveryBranch(c: Boolean): String {
    val a: Any = "w"
    return (when {
        c -> a as String
        else -> a as String
    })(a)
}

fun whenOneBranch(c: Boolean): String {
    val a: Any = "w"
    return (when {
        c -> a as String
        else -> a
    })(a)
}
"#;

#[test]
fn an_invoke_receiver_smart_casts_following_arguments() {
    common::expect_box_same_as_kotlinc(SOURCE, "InvokeReceiverSmartcast");
}

#[test]
fn a_generic_cast_to_any_keeps_the_type_parameter() {
    const SOURCE: &str = r#"
inline fun <R, T> foo(x: R, y: R, block: (R) -> T): T {
    val a = x is Number
    val b = x is Any
    val seen = x as Any
    return if (a && b) block(x) else block(y)
}

inline fun <R, T> bar(x: R, y: R, block: (R) -> T): T {
    val seen = x as Object
    return block(x)
}

fun box(): String {
    if (foo(1, 2) { x -> x as Int } != 1) return "fail int"
    if (foo("abc", "def") { x -> x as String } != "def") return "fail string"
    if (bar(1, 2) { x -> x as Int } != 1) return "fail object"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(SOURCE, "GenericCastKeepsTypeParameter");
}

#[test]
fn a_when_of_callable_references_is_invoked() {
    const SOURCE: &str = r#"
fun decodeValue(value: String): Any {
    return when (value[0]) {
        'F' -> String::toFloat
        'B' -> String::toBoolean
        'I' -> String::toInt
        else -> throw IllegalArgumentException("bad")
    }(value.substring(2))
}

fun box(): String {
    if (decodeValue("I:1") != 1) return "fail int"
    if (decodeValue("B:true") != true) return "fail bool"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(SOURCE, "WhenOfCallableReferences");
}

#[test]
fn a_supertype_cast_keeps_the_type_parameter() {
    const SOURCE: &str = r#"
fun <T> keepAny(x: T): T {
    val seen: Any = x as Any
    return if (seen == seen) x else x
}

fun <T> keepObject(x: T): T {
    val seen: Object = x as Object
    return if (seen == seen) x else x
}

fun box(): String {
    if (keepAny(1) != 1) return "fail any"
    if (keepObject("ok") != "ok") return "fail object"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(SOURCE, "SupertypeCastKeepsTypeParameter");
}

#[test]
fn an_expected_invoke_callee_is_diagnosed_once() {
    const SOURCE: &str = "fun caller(): String {\n    return (unresolved as String)(\"x\")\n}\n";
    let diagnostics = common::front_end_diagnostics(SOURCE, &[], None);
    assert_eq!(
        diagnostics,
        vec![
            "unresolved reference 'unresolved'.".to_string(),
            "expression is not callable".to_string(),
        ]
    );
}
