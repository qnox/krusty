//! `::prop.isInitialized` — whether a `lateinit` property has been assigned yet.
//!
//! This is the ONE read of a `lateinit` field that must not carry the throw-if-null guard every
//! other read of one carries: the guard answers this question by throwing, and this answers it
//! with a `Boolean`. Null IS the evidence — which is why `lateinit` is only allowed on a type with
//! a null to be distinguished by — so the whole of the test is the raw field against null. kotlinc
//! emits the same: no reflection and no `KProperty` value, whatever the `::prop` spelling suggests.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// False before the assignment and true after it.
#[test]
fn an_uninitialized_property_answers_false_and_an_assigned_one_true() {
    let source = "class Holder {\n\
         \x20   lateinit var value: String\n\
         \x20   fun ready(): Boolean = ::value.isInitialized\n\
         }\n\
         fun box(): String {\n\
         \x20   val h = Holder()\n\
         \x20   if (h.ready()) return \"fail before\"\n\
         \x20   h.value = \"set\"\n\
         \x20   if (!h.ready()) return \"fail after\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("LateinitIsInitialized", source);
}

/// The test does NOT throw where an ordinary read would, which is the whole reason it exists.
#[test]
fn the_test_does_not_throw_where_a_read_would() {
    let source = "class Holder {\n\
         \x20   lateinit var value: String\n\
         \x20   fun ready(): Boolean = ::value.isInitialized\n\
         \x20   fun read(): String = value\n\
         }\n\
         fun box(): String {\n\
         \x20   val h = Holder()\n\
         \x20   if (h.ready()) return \"fail ready\"\n\
         \x20   try {\n\
         \x20       h.read()\n\
         \x20       return \"fail no throw\"\n\
         \x20   } catch (e: kotlin.UninitializedPropertyAccessException) {\n\
         \x20       return \"OK\"\n\
         \x20   }\n\
         }\n";
    every_backend_agrees_with_kotlinc("LateinitTestDoesNotThrow", source);
}

/// Two properties are tested apart: assigning one does not make the other answer true.
#[test]
fn each_property_is_tested_on_its_own_storage() {
    let source = "class Holder {\n\
         \x20   lateinit var first: String\n\
         \x20   lateinit var second: String\n\
         \x20   fun firstReady(): Boolean = ::first.isInitialized\n\
         \x20   fun secondReady(): Boolean = ::second.isInitialized\n\
         }\n\
         fun box(): String {\n\
         \x20   val h = Holder()\n\
         \x20   h.first = \"set\"\n\
         \x20   if (!h.firstReady()) return \"fail first\"\n\
         \x20   if (h.secondReady()) return \"fail second\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("LateinitPerProperty", source);
}

/// Each INSTANCE has its own storage, so one object's assignment says nothing about another's.
#[test]
fn each_instance_is_tested_on_its_own_storage() {
    let source = "class Holder {\n\
         \x20   lateinit var value: String\n\
         \x20   fun ready(): Boolean = ::value.isInitialized\n\
         }\n\
         fun box(): String {\n\
         \x20   val assigned = Holder()\n\
         \x20   assigned.value = \"set\"\n\
         \x20   val fresh = Holder()\n\
         \x20   if (!assigned.ready()) return \"fail assigned\"\n\
         \x20   if (fresh.ready()) return \"fail fresh\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("LateinitPerInstance", source);
}
