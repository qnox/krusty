//! Property references as values through krusty's own code generator: `::foo`, `C::p`, `x::p`.
//!
//! Each is an object with its own emitted type, answering `get`, `set` and `name` out of slots the
//! site's own bodies fill — there is no IR for those bodies, because common lowering leaves the
//! reference checked so that each target may choose its representation.

use super::common::expect_native_box;

#[test]
fn a_top_level_property_reference_reads_and_names() {
    expect_native_box(
        "val foo = \"lol\"\n\
         fun box(): String {\n\
         \x20   val property = ::foo\n\
         \x20   if (property.get() != \"lol\") return \"fail value: \" + property.get()\n\
         \x20   if (property.name != \"foo\") return \"fail name: \" + property.name\n\
         \x20   return \"OK\"\n\
         }\n",
        "TopLevelPropertyReference",
        "OK",
    );
}

#[test]
fn a_mutable_top_level_property_reference_writes() {
    expect_native_box(
        "var counter = 1\n\
         fun box(): String {\n\
         \x20   val property = ::counter\n\
         \x20   if (property.get() != 1) return \"fail 1: \" + property.get()\n\
         \x20   property.set(7)\n\
         \x20   if (counter != 7) return \"fail 2: $counter\"\n\
         \x20   if (property.get() != 7) return \"fail 3: \" + property.get()\n\
         \x20   return \"OK\"\n\
         }\n",
        "MutableTopLevelPropertyReference",
        "OK",
    );
}

#[test]
fn a_member_property_reference_reads_and_writes_through_its_argument() {
    expect_native_box(
        "class Box(var value: String)\n\
         fun box(): String {\n\
         \x20   val one = Box(\"lorem\")\n\
         \x20   val property = Box::value\n\
         \x20   if (property.get(one) != \"lorem\") return \"fail 1: \" + property.get(one)\n\
         \x20   property.set(one, \"ipsum\")\n\
         \x20   if (one.value != \"ipsum\") return \"fail 2: \" + one.value\n\
         \x20   if (property.name != \"value\") return \"fail name: \" + property.name\n\
         \x20   val other = Box(\"dolor\")\n\
         \x20   return if (property.get(other) == \"dolor\") \"OK\" else \"fail 3\"\n\
         }\n",
        "MemberPropertyReference",
        "OK",
    );
}

#[test]
fn a_bound_property_reference_reads_the_receiver_it_bound() {
    expect_native_box(
        "class Box(val value: String)\n\
         fun box(): String {\n\
         \x20   val property = Box(\"bound\")::value\n\
         \x20   if (property.get() != \"bound\") return \"fail: \" + property.get()\n\
         \x20   return if (property.name == \"value\") \"OK\" else \"fail name\"\n\
         }\n",
        "BoundPropertyReference",
        "OK",
    );
}

#[test]
fn a_reference_to_a_primitive_property_answers_the_value_not_the_box() {
    // `get` answers a reference, so an `Int` property boxes on the way out and is unboxed back at
    // the call site. Arithmetic on the result is what catches a box read as a number.
    expect_native_box(
        "class Counter(var count: Int)\n\
         val scale = 3\n\
         fun box(): String {\n\
         \x20   val count = Counter::count\n\
         \x20   val counter = Counter(4)\n\
         \x20   if (count.get(counter) * ::scale.get() != 12) return \"fail product\"\n\
         \x20   count.set(counter, count.get(counter) + 1)\n\
         \x20   return if (counter.count == 5) \"OK\" else \"fail: \" + counter.count\n\
         }\n",
        "PrimitivePropertyReference",
        "OK",
    );
}

#[test]
fn a_property_reference_reaches_a_custom_accessor() {
    expect_native_box(
        "class Doubled(val base: Int) {\n\
         \x20   val twice: Int get() = base * 2\n\
         }\n\
         fun box(): String {\n\
         \x20   val twice = Doubled::twice\n\
         \x20   val answer = twice.get(Doubled(21))\n\
         \x20   return if (answer == 42) \"OK\" else \"fail: $answer\"\n\
         }\n",
        "AccessorPropertyReference",
        "OK",
    );
}

#[test]
fn a_property_reference_reaches_the_receivers_override() {
    // `get` reads the property the ordinary way, so an open property still dispatches.
    expect_native_box(
        "open class Base {\n\
         \x20   open val tag: String get() = \"base\"\n\
         }\n\
         class Derived : Base() {\n\
         \x20   override val tag: String get() = \"derived\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val tag = Base::tag\n\
         \x20   val answer = tag.get(Derived())\n\
         \x20   return if (answer == \"derived\") \"OK\" else \"fail: $answer\"\n\
         }\n",
        "OverriddenPropertyReference",
        "OK",
    );
}

#[test]
fn two_references_to_one_property_are_equal() {
    // Kotlin compares callable references by the DECLARATION they name. A site that binds no
    // receiver has one instance for the whole program, so identity equality answers that.
    expect_native_box(
        "val foo = \"x\"\n\
         fun box(): String = if (::foo == ::foo) \"OK\" else \"fail\"\n",
        "PropertyReferenceEquality",
        "OK",
    );
}

#[test]
fn two_bound_references_compare_by_declaration_and_receiver() {
    // A bound reference is a fresh object each time it is written, so identity alone would answer
    // `false`. Kotlin compares the declaration and the bound receiver, and so does the emitted
    // `equals`.
    expect_native_box(
        "class Box(val value: String)\n\
         fun box(): String {\n\
         \x20   val one = Box(\"a\")\n\
         \x20   val other = Box(\"a\")\n\
         \x20   if (one::value != one::value) return \"fail: same receiver\"\n\
         \x20   if (one::value == other::value) return \"fail: different receivers\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "BoundPropertyReferenceEquality",
        "OK",
    );
}

#[test]
fn a_delegated_member_property_reads_and_writes_through_its_delegate() {
    // The `KProperty` the delegate is handed is a property reference with no receiver bound, so
    // the metadata a delegated property needs is the same object `C::p` is.
    expect_native_box(
        "import kotlin.reflect.KProperty\n\
         class Delegate {\n\
         \x20   var inner = 1\n\
         \x20   operator fun getValue(t: Any?, p: KProperty<*>): Int = inner\n\
         \x20   operator fun setValue(t: Any?, p: KProperty<*>, i: Int) { inner = i }\n\
         }\n\
         class A { var prop: Int by Delegate() }\n\
         fun box(): String {\n\
         \x20   val a = A()\n\
         \x20   if (a.prop != 1) return \"fail get\"\n\
         \x20   a.prop = 2\n\
         \x20   return if (a.prop == 2) \"OK\" else \"fail set\"\n\
         }\n",
        "DelegatedMemberProperty",
        "OK",
    );
}

#[test]
fn a_delegate_reads_the_property_name_it_is_handed() {
    expect_native_box(
        "import kotlin.reflect.KProperty\n\
         class Named {\n\
         \x20   operator fun getValue(t: Any?, p: KProperty<*>): String = p.name\n\
         }\n\
         class A { val first: String by Named() }\n\
         fun box(): String {\n\
         \x20   val answer = A().first\n\
         \x20   return if (answer == \"first\") \"OK\" else \"fail: $answer\"\n\
         }\n",
        "DelegatedPropertyName",
        "OK",
    );
}

#[test]
fn a_delegated_local_property_carries_its_own_name() {
    // A LOCAL delegated property has no storage and no accessors: its metadata exists so the
    // delegate can ask the property its name, and that is the whole of what it answers.
    expect_native_box(
        "import kotlin.reflect.KProperty\n\
         class Named {\n\
         \x20   operator fun getValue(t: Any?, p: KProperty<*>): String = p.name\n\
         }\n\
         fun box(): String {\n\
         \x20   val OK: String by Named()\n\
         \x20   return OK\n\
         }\n",
        "DelegatedLocalProperty",
        "OK",
    );
}

#[test]
fn two_delegated_properties_are_handed_their_own_metadata() {
    // One type per property, so two delegated properties of one class must not share an object —
    // the name each delegate reads is the only thing telling them apart.
    expect_native_box(
        "import kotlin.reflect.KProperty\n\
         class Named {\n\
         \x20   operator fun getValue(t: Any?, p: KProperty<*>): String = p.name\n\
         }\n\
         class A {\n\
         \x20   val first: String by Named()\n\
         \x20   val second: String by Named()\n\
         }\n\
         fun box(): String {\n\
         \x20   val a = A()\n\
         \x20   val joined = a.first + a.second\n\
         \x20   return if (joined == \"firstsecond\") \"OK\" else \"fail: $joined\"\n\
         }\n",
        "DelegatedPropertyMetadataIdentity",
        "OK",
    );
}

#[test]
fn an_extension_property_reference_passes_its_receiver_to_the_accessor() {
    // An extension property has no object of its own to keep a field in, so both directions are
    // calls to the accessor with the receiver as its leading argument.
    expect_native_box(
        "val String.id: String get() = this\n\
         fun box(): String {\n\
         \x20   val reference = String::id\n\
         \x20   if (reference.get(\"123\") != \"123\") return \"fail: \" + reference.get(\"123\")\n\
         \x20   if (reference.name != \"id\") return \"fail name: \" + reference.name\n\
         \x20   return reference.get(\"OK\")\n\
         }\n",
        "ExtensionPropertyReference",
        "OK",
    );
}

#[test]
fn a_bound_extension_property_reference_keeps_the_receiver_it_bound() {
    expect_native_box(
        "val String.doubled: String get() = this + this\n\
         fun box(): String {\n\
         \x20   val reference = \"ab\"::doubled\n\
         \x20   val answer = reference.get()\n\
         \x20   return if (answer == \"abab\") \"OK\" else \"fail: $answer\"\n\
         }\n",
        "BoundExtensionPropertyReference",
        "OK",
    );
}

#[test]
fn a_mutable_extension_property_reference_writes_through_its_setter() {
    expect_native_box(
        "class Cell(var stored: String)\n\
         var Cell.text: String\n\
         \x20   get() = stored\n\
         \x20   set(value) {\n\
         \x20       stored = value + \"!\"\n\
         \x20   }\n\
         fun box(): String {\n\
         \x20   val reference = Cell::text\n\
         \x20   val cell = Cell(\"a\")\n\
         \x20   if (reference.get(cell) != \"a\") return \"fail get\"\n\
         \x20   reference.set(cell, \"b\")\n\
         \x20   return if (cell.stored == \"b!\") \"OK\" else \"fail: \" + cell.stored\n\
         }\n",
        "MutableExtensionPropertyReference",
        "OK",
    );
}

#[test]
fn a_top_level_property_with_accessors_is_referenced_through_them() {
    // A reference names a property, not a slot, and a property with its own accessors has no slot
    // to name: its value is computed. The reference reaches it the only way anything does — by
    // calling the accessor — and the accessor here takes NO receiver, which is what separates this
    // from the extension case above rather than a receiver it could pass and does not have.
    expect_native_box(
        "var stored = \"a\"\n\
         var computed: String\n\
         \x20   get() = stored + \"!\"\n\
         \x20   set(value) {\n\
         \x20       stored = value + \"?\"\n\
         \x20   }\n\
         fun box(): String {\n\
         \x20   val reference = ::computed\n\
         \x20   if (reference.get() != \"a!\") return \"fail get: \" + reference.get()\n\
         \x20   if (reference.name != \"computed\") return \"fail name: \" + reference.name\n\
         \x20   reference.set(\"b\")\n\
         \x20   if (stored != \"b?\") return \"fail set: \" + stored\n\
         \x20   return if (reference.get() == \"b?!\") \"OK\" else \"fail read back\"\n\
         }\n",
        "TopLevelAccessorPropertyReference",
        "OK",
    );
}

#[test]
fn a_top_level_delegated_property_asks_its_delegate_through_a_reference() {
    // The metadata object `getValue(thisRef, property)` is handed is the same reference `::prop`
    // is, so a top-level delegated property needs one whether or not the program writes `::`. Its
    // value lives in the delegate rather than in a slot, which is why the reference has to reach
    // it through the accessor the delegation built.
    expect_native_box(
        "import kotlin.reflect.KProperty\n\
         class Delegate(private var held: String) {\n\
         \x20   operator fun getValue(owner: Any?, property: KProperty<*>): String =\n\
         \x20       held + \"/\" + property.name\n\
         \x20   operator fun setValue(owner: Any?, property: KProperty<*>, value: String) {\n\
         \x20       held = value\n\
         \x20   }\n\
         }\n\
         val fixed: String by Delegate(\"one\")\n\
         var moving: String by Delegate(\"two\")\n\
         fun box(): String {\n\
         \x20   if (fixed != \"one/fixed\") return \"fail val: $fixed\"\n\
         \x20   if (moving != \"two/moving\") return \"fail var: $moving\"\n\
         \x20   moving = \"three\"\n\
         \x20   return if (moving == \"three/moving\") \"OK\" else \"fail write: $moving\"\n\
         }\n",
        "TopLevelDelegatedProperty",
        "OK",
    );
}

#[test]
fn a_reference_written_in_a_class_body_is_realized() {
    // A class's property initializer is lowered as part of its CONSTRUCTOR, so a reference written
    // there is reached before any top-level function's body is. It must already be declared.
    expect_native_box(
        "class Cell(val value: Int)\n\
         class Holder {\n\
         \x20   val unbound = Cell::value\n\
         \x20   val bound = Cell(7)::value\n\
         }\n\
         fun box(): String {\n\
         \x20   val holder = Holder()\n\
         \x20   if (holder.unbound.get(Cell(3)) != 3) return \"fail unbound: ${holder.unbound.get(Cell(3))}\"\n\
         \x20   if (holder.bound.get() != 7) return \"fail bound: ${holder.bound.get()}\"\n\
         \x20   return if (holder.unbound.name == \"value\") \"OK\" else \"fail name: ${holder.unbound.name}\"\n\
         }\n",
        "ReferenceInAClassBody",
        "OK",
    );
}

#[test]
fn a_reference_in_an_init_block_is_realized() {
    expect_native_box(
        "val top = \"OK\"\n\
         class Holder {\n\
         \x20   var seen = \"fail\"\n\
         \x20   init {\n\
         \x20       seen = ::top.get()\n\
         \x20   }\n\
         }\n\
         fun box(): String = Holder().seen\n",
        "ReferenceInAnInitBlock",
        "OK",
    );
}

#[test]
fn a_local_delegated_property_of_a_class_body_is_realized() {
    // The same ordering question for the other declaration pass: a local delegated property's
    // `KProperty` metadata, in a body a class defines.
    expect_native_box(
        "import kotlin.reflect.KProperty\n\
         class Named {\n\
         \x20   operator fun getValue(owner: Any?, property: KProperty<*>): String = property.name\n\
         }\n\
         class Holder {\n\
         \x20   val asked: String\n\
         \x20   init {\n\
         \x20       val inner by Named()\n\
         \x20       asked = inner\n\
         \x20   }\n\
         }\n\
         fun box(): String = if (Holder().asked == \"inner\") \"OK\" else \"fail: ${Holder().asked}\"\n",
        "LocalDelegateInAClassBody",
        "OK",
    );
}
