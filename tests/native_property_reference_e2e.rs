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
