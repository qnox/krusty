//! A property an interface declares and a class supplies with NO override edge to name it.
//!
//! Interface delegation is the shape. `class Q(a: A) : A by a` synthesizes `Q`'s own `x` and its
//! accessors, and nothing records that they implement `A.x` — there is no source declaration to
//! carry the edge, so the interface's number found no implementation and the whole file declined.
//!
//! Kotlin has already decided they implement it: a class does not compile with an interface
//! property left unimplemented, and it cannot declare a second property of that name beside the
//! inherited one. So an interface in the class's hierarchy declaring the same name IS the member
//! those accessors fill, and matching by name is reading the language's rule rather than guessing.
//!
//! Two more things the shape needed, both of which the missing edge had been hiding: a CALL to
//! such an accessor has to name the same key the table holds — an abstract `val` in an interface
//! carries no accessor id, so its accessor reaches the method list as an ordinary method and is
//! tied back by name in both readings or in neither. And a class that supplies a property its
//! SUPERCLASS already had takes that slot, because the base's accessor may be synthesized and so
//! have no signature to match.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// A delegated property, read through the class and through the interface.
#[test]
fn a_delegated_property_fills_the_interfaces_number() {
    let source = "interface A {\n\
         \x20   val x: Int\n\
         \x20   var label: String\n\
         }\n\
         class C : A {\n\
         \x20   override val x: Int = 42\n\
         \x20   override var label: String = \"c\"\n\
         }\n\
         class Q(a: A) : A by a\n\
         fun box(): String {\n\
         \x20   val c = C()\n\
         \x20   val q = Q(c)\n\
         \x20   if (q.x != 42) return \"fail q.x\"\n\
         \x20   val a: A = q\n\
         \x20   if (a.x != 42) return \"fail a.x\"\n\
         \x20   q.label = \"set\"\n\
         \x20   if (c.label != \"set\") return \"fail the setter reached the delegate\"\n\
         \x20   if (a.label != \"set\") return \"fail a.label\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "DelegatedProperty");
    expect_native_box(source, "DelegatedProperty", "OK");
}

/// Delegation beside a superclass that already supplies the member.
///
/// `class E : B(), C by D()` — `B` implements `A.x` and the delegation supplies it again. kotlinc
/// answers with the DELEGATE (KT-70417), which is what taking the base's slot makes true: the
/// base's accessor is synthesized from its field and has no signature to match, so the inherited
/// slot is found by the property's name instead.
#[test]
fn a_delegation_beside_an_inherited_property_takes_the_inherited_slot() {
    let source = "interface A { var x: String }\n\
         open class B : A { override lateinit var x: String }\n\
         interface C : A\n\
         open class D : C {\n\
         \x20   override var x: String\n\
         \x20       get() = \"OK\"\n\
         \x20       set(_) {}\n\
         }\n\
         class E : B(), C by D()\n\
         fun box(): String {\n\
         \x20   val e = E()\n\
         \x20   e.x = \"Fail\"\n\
         \x20   return e.x\n\
         }\n";
    expect_box_ok_with_stdlib(source, "HiddenDelegate");
    expect_native_box(source, "HiddenDelegate", "OK");
}

/// A class satisfying an interface property with one it inherits, and two delegators of it.
///
/// `class P1(x: Int, yy: Y) : Abstract, X(x), Y by yy` in four supertype orders is the corpus's
/// `classes/inheritance.kt`, and the order is what it is about: the interface's number has to
/// reach the delegated accessor however the supertypes are written.
#[test]
fn the_order_of_supertypes_does_not_change_which_accessor_a_number_reaches() {
    let source = "open class X(val x: Int)\n\
         interface Y { val y: Int }\n\
         interface Abstract\n\
         class YImpl(override val y: Int) : Y\n\
         class P1(x: Int, yy: Y) : Abstract, X(x), Y by yy\n\
         class P2(x: Int, yy: Y) : X(x), Abstract, Y by yy\n\
         class P3(x: Int, yy: Y) : X(x), Y by yy, Abstract\n\
         class P4(x: Int, yy: Y) : Y by yy, Abstract, X(x)\n\
         fun box(): String {\n\
         \x20   val y = YImpl(-1)\n\
         \x20   if (P1(240, y).x + P1(240, y).y != 239) return \"fail P1\"\n\
         \x20   if (P2(240, y).x + P2(240, y).y != 239) return \"fail P2\"\n\
         \x20   if (P3(240, y).x + P3(240, y).y != 239) return \"fail P3\"\n\
         \x20   if (P4(240, y).x + P4(240, y).y != 239) return \"fail P4\"\n\
         \x20   val through: Y = P1(240, y)\n\
         \x20   if (through.y != -1) return \"fail through the interface\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "DelegationSupertypeOrder");
    expect_native_box(source, "DelegationSupertypeOrder", "OK");
}
