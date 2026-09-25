//! A property reference to a `const val` of an object or companion.
//!
//! A reference's `get` reaches the property's storage, and for a class member that is a field of the
//! receiver. A `const val` has no such field: Kotlin folds it at every use site and keeps the value
//! in a STATIC, so the reference answers that same value and reads it from there.
//!
//! WHOSE static is not always the declaring object's. A COMPANION's `const val` lives on the OUTER
//! class, which is where kotlinc puts it and what this layout follows, so both owners are admitted
//! — and only under the property's own name, and only for a CONST: an ordinary property reached
//! this way would be a guess about where its value is.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer to be `"OK"` and the NATIVE backend to agree, without going through
/// krusty's JVM backend.
///
/// Reaching a `const val` through a reference or a delegate is a separate gap in krusty's JVM
/// backend: it emits a call to a getter the constant does not have
/// (`NoSuchMethodError: Sample$Companion.getMaxValue()`, `Property.getPROPERTY_VALUE()`). That
/// failure predates this module and is unrelated to it — the same programs fail identically with
/// the native change removed. So the reference compiler supplies the expectation directly here,
/// and the JVM backend is left to its own fix.
fn the_native_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_native_box(source, stem, "OK");
}

/// A COMPANION's `const val`, whose static the outer class owns.
#[test]
fn a_reference_to_a_companion_const_reads_it() {
    let source = "class Sample {\n\
         \x20   companion object {\n\
         \x20       const val maxValue = 42\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val reference = Sample::maxValue\n\
         \x20   if (reference.get() != 42) return \"fail value \" + reference.get()\n\
         \x20   return if (reference.name == \"maxValue\") \"OK\" else \"fail name \" + reference.name\n\
         }\n";
    the_native_backend_agrees_with_kotlinc("CompanionConstReference", source);
}

/// An OBJECT's `const val`, whose static the object itself owns — and the object's initializer
/// still runs, which is what separates reaching the constant from reaching the object.
#[test]
fn a_reference_to_an_objects_const_is_delegated_to() {
    let source = "var sideEffect = \"Fail\"\n\
         object Property {\n\
         \x20   init {\n\
         \x20       sideEffect = \"OK\"\n\
         \x20   }\n\
         \x20   const val PROPERTY_VALUE: String = \"O\"\n\
         }\n\
         const val TOP_LEVEL_PROPERTY_VALUE: String = \"K\"\n\
         val value1: String by Property::PROPERTY_VALUE\n\
         val value2: String by ::TOP_LEVEL_PROPERTY_VALUE\n\
         fun box(): String {\n\
         \x20   if (sideEffect != \"OK\") return \"fail the object's initializer\"\n\
         \x20   return value1 + value2\n\
         }\n";
    the_native_backend_agrees_with_kotlinc("ObjectConstReference", source);
}

/// Two references to the same constant are EQUAL, as two references to any one declaration are —
/// the site binds no receiver, so the program has one instance of it.
#[test]
fn two_references_to_one_constant_are_equal() {
    let source = "class Sample {\n\
         \x20   companion object {\n\
         \x20       const val maxValue = 1\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Sample::maxValue != Sample::maxValue) return \"fail equality\"\n\
         \x20   return if (Sample::maxValue.get() == 1) \"OK\" else \"fail value\"\n\
         }\n";
    the_native_backend_agrees_with_kotlinc("ConstReferenceEquality", source);
}
