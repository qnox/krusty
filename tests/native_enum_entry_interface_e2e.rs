//! An enum whose ENTRIES implement its interfaces, rather than the enum class itself.
//!
//! `enum class Test : IFoo { FOO { override fun foo() = "OK" } }` leaves `IFoo.foo` unimplemented on
//! the enum class. The class model refuses a CONCRETE class that reaches an interface's abstract
//! trap for an interface it implements, because Kotlin would not have compiled such a class — the
//! implementation exists and the model failed to find it.
//!
//! An enum whose every entry has a BODY is the exception, and it is not one by fiat: such a class is
//! not instantiable as itself. Every instance is an entry subclass, the slot is filled there, and
//! the same check runs for each of those — so a genuinely missing implementation is still caught.
//! Kotlin marks such a class abstract for exactly this reason. An entry WITHOUT a body is an
//! instance of the enum class, and then the trap is reachable and the refusal stands.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// Reached through the INTERFACE, which is the number the entry's table has to fill.
#[test]
fn an_enum_entry_implements_its_enums_interface() {
    let source = "interface IFoo {\n\
         \x20   fun foo(): String\n\
         }\n\
         enum class Test : IFoo {\n\
         \x20   FOO {\n\
         \x20       override fun foo() = \"O\"\n\
         \x20   },\n\
         \x20   BAR {\n\
         \x20       override fun foo() = \"K\"\n\
         \x20   }\n\
         }\n\
         fun through(value: IFoo): String = value.foo()\n\
         fun box(): String {\n\
         \x20   if (Test.FOO.foo() + Test.BAR.foo() != \"OK\") return \"fail direct\"\n\
         \x20   return through(Test.FOO) + through(Test.BAR)\n\
         }\n";
    expect_box_ok_with_stdlib(source, "EnumEntryInterface");
    expect_native_box(source, "EnumEntryInterface", "OK");
}

/// TWO interfaces, one of them implemented on the enum class and the other left to the entries.
#[test]
fn an_enum_splits_its_interfaces_between_the_class_and_its_entries() {
    let source = "interface IFoo {\n\
         \x20   fun foo(): String\n\
         }\n\
         interface IBar {\n\
         \x20   fun bar(): String\n\
         }\n\
         enum class Test : IFoo, IBar {\n\
         \x20   ONLY {\n\
         \x20       override fun foo() = \"O\"\n\
         \x20   };\n\
         \x20   override fun bar() = \"K\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val foo: IFoo = Test.ONLY\n\
         \x20   val bar: IBar = Test.ONLY\n\
         \x20   return foo.foo() + bar.bar()\n\
         }\n";
    expect_box_ok_with_stdlib(source, "EnumEntryInterfaceSplit");
    expect_native_box(source, "EnumEntryInterfaceSplit", "OK");
}

/// An entry with NO body is an instance of the enum class, so the class must implement the member
/// itself — and here it does, which is the shape the exception above must not swallow.
#[test]
fn an_enum_without_entry_bodies_implements_its_interface_itself() {
    let source = "interface IFoo {\n\
         \x20   fun foo(): String\n\
         }\n\
         enum class Test(val text: String) : IFoo {\n\
         \x20   O(\"O\"),\n\
         \x20   K(\"K\");\n\
         \x20   override fun foo() = text\n\
         }\n\
         fun through(value: IFoo): String = value.foo()\n\
         fun box(): String = through(Test.O) + through(Test.K)\n";
    expect_box_ok_with_stdlib(source, "EnumNoEntryBodies");
    expect_native_box(source, "EnumNoEntryBodies", "OK");
}
