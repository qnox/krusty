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
