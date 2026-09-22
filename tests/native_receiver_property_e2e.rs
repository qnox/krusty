//! Properties reached through a receiver their owner does not supply, through krusty's own code
//! generator: a member extension property (`class C { val Foo.bar get() = … }`) and a member
//! property with context parameters.
//!
//! Neither has storage to reach — an extension property cannot have a backing field, because there
//! is no object of its own to keep one in — so every access is a call to the accessor the checked
//! lowering built. What makes these different from a top-level extension property is that the
//! accessor is an INSTANCE METHOD of the owner and can be overridden, so the call has to go through
//! the owner's vtable slot rather than straight to the body: a subclass's accessor must win.

use super::common::expect_native_box;

#[test]
fn a_member_extension_property_reads_through_its_owner() {
    expect_native_box(
        "class Doubler {\n\
         \x20   val Int.doubled: Int get() = this * 2\n\
         \x20   fun of(value: Int): Int = value.doubled\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = Doubler().of(21)\n\
         \x20   return if (answer == 42) \"OK\" else \"fail: $answer\"\n\
         }\n",
        "MemberExtensionProperty",
        "OK",
    );
}

#[test]
fn a_member_extension_property_reaches_the_override() {
    // The accessor is an instance method, so a subclass's must win — which is the whole reason the
    // call goes through the owner's dispatch slot instead of straight to the base's body.
    expect_native_box(
        "open class Base {\n\
         \x20   open val Int.tagged: String get() = \"base$this\"\n\
         \x20   fun of(value: Int): String = value.tagged\n\
         }\n\
         class Derived : Base() {\n\
         \x20   override val Int.tagged: String get() = \"derived$this\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val base: Base = Derived()\n\
         \x20   val answer = base.of(7)\n\
         \x20   return if (answer == \"derived7\") \"OK\" else \"fail: $answer\"\n\
         }\n",
        "MemberExtensionPropertyOverride",
        "OK",
    );
}

#[test]
fn a_member_extension_property_is_written_through_its_owner() {
    expect_native_box(
        "class Cell {\n\
         \x20   var stored: String = \"\"\n\
         \x20   var Int.slot: String\n\
         \x20       get() = stored + this\n\
         \x20       set(value) {\n\
         \x20           stored = value + this\n\
         \x20       }\n\
         \x20   fun write(key: Int, value: String) {\n\
         \x20       key.slot = value\n\
         \x20   }\n\
         \x20   fun read(key: Int): String = key.slot\n\
         }\n\
         fun box(): String {\n\
         \x20   val cell = Cell()\n\
         \x20   cell.write(1, \"a\")\n\
         \x20   val answer = cell.read(2)\n\
         \x20   return if (answer == \"a12\") \"OK\" else \"fail: $answer\"\n\
         }\n",
        "MemberExtensionPropertyWrite",
        "OK",
    );
}

#[test]
fn a_member_extension_property_keeps_both_its_receivers_apart() {
    // `this` is the extension receiver and `this@Owner` the dispatch receiver; reading one for the
    // other answers with a plausible value of the right type, which is why it needs its own test.
    expect_native_box(
        "class Owner(val tag: String) {\n\
         \x20   val Owner.joined: String get() = this@Owner.tag + this.tag\n\
         \x20   fun of(other: Owner): String = other.joined\n\
         }\n\
         fun box(): String {\n\
         \x20   val answer = Owner(\"d\").of(Owner(\"e\"))\n\
         \x20   return if (answer == \"de\") \"OK\" else \"fail: $answer\"\n\
         }\n",
        "MemberExtensionPropertyReceivers",
        "OK",
    );
}

#[test]
fn a_member_property_with_a_context_parameter_reads_through_its_owner() {
    expect_native_box(
        "// LANGUAGE: +ContextParameters\n\
         class Tag(val text: String)\n\
         class Owner {\n\
         \x20   context(tag: Tag)\n\
         \x20   val labelled: String get() = \"[\" + tag.text + \"]\"\n\
         }\n\
         context(tag: Tag)\n\
         fun read(owner: Owner): String = owner.labelled\n\
         fun box(): String {\n\
         \x20   val answer = with(Tag(\"x\")) { read(Owner()) }\n\
         \x20   return if (answer == \"[x]\") \"OK\" else \"fail: $answer\"\n\
         }\n",
        "ContextMemberProperty",
        "OK",
    );
}
