//! `if (this is B)` flow-narrows the implicit receiver to the subtype `B` inside the guarded branch,
//! so a bare member of `B` resolves through `this`. The lowerer inserts a `checkcast` on the loaded
//! `this` before the field read / getter call.

use crate::common;

#[test]
fn this_smartcast_implicit_receiver() {
    match common::run_box_corpus_case("smartCasts/implicitReceiver.kt") {
        Some(s) => assert_eq!(s, "OK"),
        None => panic!("unexpectedly skipped"),
    }
}

#[test]
fn this_smartcast_implicit_receiver_in_when() {
    match common::run_box_corpus_case("smartCasts/implicitReceiverInWhen.kt") {
        Some(s) => assert_eq!(s, "OK"),
        None => panic!("unexpectedly skipped"),
    }
}

#[test]
fn this_smartcast_member_property() {
    let src = r#"
open class Shape {
    class Circle : Shape() {
        val r = 3
    }

    fun describe(): Int {
        if (this is Circle) return r
        return -1
    }
}

fun box(): String {
    val c: Shape = Shape.Circle()
    return if (c.describe() == 3) "OK" else "FAIL: ${c.describe()}"
}
"#;
    common::expect_box_ok_with_stdlib(src, "ThisSmartcastMember");
}

/// Official box `smartCasts/avoidSmartCastToDerivedForPrivate.kt`. The smart cast makes `this` a
/// `Derived`, which does not inherit `Base.baz`. The call is still `Base`'s private member.
#[test]
fn private_member_survives_a_smart_cast_to_a_subclass() {
    const SRC: &str = "open class Base {\n\
    fun foo(): String {\n\
        return when (this) {\n\
            is Derived -> baz()\n\
            else -> \"fail 1\"\n\
        }\n\
    }\n\
\n\
    private fun baz(): String = \"OK\"\n\
}\n\
\n\
class Derived : Base()\n\
\n\
fun box(): String {\n\
    return Derived().foo()\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Base");
}

/// A member actually declared on the smart-cast type wins over the private member of the original
/// class. kotlinc calls `Derived.baz` here.
#[test]
fn a_subclass_member_wins_over_a_private_member_after_smart_cast() {
    const SRC: &str = "open class Base {\n\
    fun foo(): String {\n\
        return when (this) {\n\
            is Derived -> baz()\n\
            else -> \"fail\"\n\
        }\n\
    }\n\
    private fun baz(): String = \"base\"\n\
}\n\
class Derived : Base() {\n\
    fun baz(): String = \"derived\"\n\
}\n\
fun box(): String = if (Derived().foo() == \"derived\") \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(SRC, "Base");
}

const CAST_STATEMENT: &str = "class Token\n\
    class Slot<T> {\n\
    \x20   var stored: Any? = null\n\
    \x20   fun put(value: T) { stored = value }\n\
    }\n\
    fun <E> Slot<out E>.implicitPut(value: E) {\n\
    \x20   this as Slot<E>\n\
    \x20   put(value)\n\
    }\n\
    fun <E> Slot<out E>.explicitPut(value: E) {\n\
    \x20   this as Slot<E>\n\
    \x20   this.put(value)\n\
    }\n\
    fun box(): String {\n\
    \x20   val firstToken = Token()\n\
    \x20   val secondToken = Token()\n\
    \x20   val slot = Slot<Token>()\n\
    \x20   slot.implicitPut(firstToken)\n\
    \x20   val first = slot.stored\n\
    \x20   slot.explicitPut(secondToken)\n\
    \x20   return if (first === firstToken && slot.stored === secondToken) \"OK\" else \"fail\"\n\
    }\n";

/// A cast statement `this as Slot<E>` on a `Slot<out E>` receiver proves the explicitly applied
/// target for the rest of the block, so `put(value: E)` applies through the implicit and the
/// explicit receiver instead of seeing the out-projected `put(value: Nothing)`.
#[test]
fn a_cast_statement_on_this_narrows_to_its_explicit_type_arguments() {
    common::assert_class_matches_kotlinc(
        "ThisCastStatement",
        CAST_STATEMENT,
        "ThisCastStatementKt",
    );
    common::expect_box_same_as_kotlinc(CAST_STATEMENT, "ThisCastStatementRun");
}
