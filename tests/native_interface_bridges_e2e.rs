//! A method reached through an INTERFACE that declares it at another representation.
//!
//! An interface's member numbers are placed program-wide rather than in one class's table, so a
//! representation change cannot be answered by replacing an entry in that table: there is no entry
//! there. It is answered where the number is FILLED instead — the class records a bridge against
//! the interface's key, and the interface region reads it before it reads the slot map. That is
//! the arrangement a property accessor's interface bridge already used.
//!
//! Two facts the shape forced, both of which pointing the number straight at the implementation
//! would have hidden:
//!
//! - A number that needs a bridge must not be SHARED with a spelling that does not.
//!   `interface Z1 : A<String>, B<String, Int>` overriding `foo(String, Int)` is callable through
//!   `A`'s number as it stands and through `B`'s only after conversion, so `Z1`'s own spelling has
//!   to join `A`'s number rather than `B`'s.
//! - What an implementor inherits has to be chosen against ITS hierarchy. `Z1` and `Z2` above both
//!   fill `A`'s number, and one recorded answer meant the last interface laid out won — a class
//!   implementing the other answered with a body it does not have.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The result's representation differs, reached through the interface and through the class.
#[test]
fn an_interface_method_implemented_at_another_representation_is_bridged() {
    let source = "interface Top<D> {\n\
         \x20   fun fetch(): D\n\
         \x20   fun render(d: D): String\n\
         }\n\
         fun <D> Top<D>.describe(d: D): String = render(fetch()) + \"/\" + render(d)\n\
         class Bottom(val n: Int) : Top<Int> {\n\
         \x20   override fun fetch(): Int = n\n\
         \x20   override fun render(d: Int): String = (d * 2).toString()\n\
         }\n\
         fun box(): String {\n\
         \x20   val b = Bottom(10)\n\
         \x20   if (b.fetch() != 10) return \"fail direct\"\n\
         \x20   val top: Top<Int> = b\n\
         \x20   if (top.fetch() != 10) return \"fail through the interface\"\n\
         \x20   if (top.render(3) != \"6\") return \"fail parameter\"\n\
         \x20   if (b.describe(4) != \"20/8\") return \"fail generic extension: \" + b.describe(4)\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "InterfaceBridge");
    expect_native_box(source, "InterfaceBridge", "OK");
}

/// Two interfaces declaring the same member at different representations, joined by a third.
///
/// `A.foo(T, Int)` already carries the second operand as a machine integer and `B.foo(T, U)`
/// carries both as references, so `Z1`'s override is callable through `A`'s number as it stands
/// and through `B`'s only after conversion. Declared in both orders, because which number `Z1`'s
/// own spelling joins is what the ordering decides.
#[test]
fn a_number_needing_a_bridge_is_not_shared_with_a_spelling_that_does_not() {
    let source = "interface A<T> { fun foo(t: T, u: Int) = \"A\" }\n\
         interface B<T, U> { fun foo(t: T, u: U) = \"B\" }\n\
         interface Z1 : A<String>, B<String, Int> { override fun foo(t: String, u: Int) = \"Z1\" }\n\
         interface Z2 : B<String, Int>, A<String> { override fun foo(t: String, u: Int) = \"Z2\" }\n\
         class Z1C : Z1\n\
         class Z2C : Z2\n\
         fun box(): String {\n\
         \x20   val z1 = Z1C()\n\
         \x20   val z2 = Z2C()\n\
         \x20   if (z1.foo(\"\", 0) != \"Z1\") return \"fail Z1 direct: \" + z1.foo(\"\", 0)\n\
         \x20   if ((z1 as A<String>).foo(\"\", 0) != \"Z1\") return \"fail Z1 through A\"\n\
         \x20   if ((z1 as B<String, Int>).foo(\"\", 0) != \"Z1\") return \"fail Z1 through B\"\n\
         \x20   if (z2.foo(\"\", 0) != \"Z2\") return \"fail Z2 direct: \" + z2.foo(\"\", 0)\n\
         \x20   if ((z2 as A<String>).foo(\"\", 0) != \"Z2\") return \"fail Z2 through A\"\n\
         \x20   if ((z2 as B<String, Int>).foo(\"\", 0) != \"Z2\") return \"fail Z2 through B\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "SharedInterfaceNumber");
    expect_native_box(source, "SharedInterfaceNumber", "OK");
}

/// A method spelled like a property's accessor is a method.
///
/// A property with a field is read through that field, so its accessor is synthesized; a method
/// that happens to spell the accessor's name is a declaration of its own. Keying the method as the
/// property's getter took it out of the method numbering entirely — the base's slot kept the
/// base's body, and a call through the base jumped into whatever stood there.
///
/// `val data` beside `fun getData()` is what the corpus's `extensionFunctions/*ExtensionSuper.kt`
/// pair is written around (KT-42176), which is why the shape here is theirs: what the program can
/// observe is the METHOD, through the interface and through a generic extension. It deliberately
/// does not read `.data` from outside — on the JVM the two share one signature, and what that
/// reads is a question about that target rather than about this numbering.
#[test]
fn a_method_spelled_like_an_accessor_is_still_a_method() {
    let source = "interface Top<D> {\n\
         \x20   fun getData(): D\n\
         }\n\
         fun <D> Top<D>.getString() = getData().toString()\n\
         abstract class Middle : Top<Int>\n\
         class Bottom(val data: Int) : Middle() {\n\
         \x20   override fun getData(): Int = data * 3\n\
         }\n\
         fun box(): String {\n\
         \x20   val b = Bottom(10)\n\
         \x20   if (b.getData() != 30) return \"fail direct\"\n\
         \x20   val top: Top<Int> = b\n\
         \x20   if (top.getData() != 30) return \"fail through the interface\"\n\
         \x20   if (b.getString() != \"30\") return \"fail generic extension: \" + b.getString()\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "AccessorNamedMethod");
    expect_native_box(source, "AccessorNamedMethod", "OK");
}
