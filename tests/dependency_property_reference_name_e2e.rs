//! What a `KProperty` for a DEPENDENCY property answers when asked its name.
//!
//! `KCallable.name` is the property's own Kotlin name, and neither thing near it can stand in.
//!
//! The reference SITE's spelling is a different fact: a lookup may reach the declaration under an
//! import alias, and `::aliased` still answers `original`.
//!
//! The ACCESSOR's name is a physical call target. A JVM realization may rename it — `removeAt` is
//! realized as `java/util/List.remove` — and where the signature mentions a value class, kotlinc
//! appends a hash of the erasure, so `UIntRange.start` is realized as `getStart-pVg5ArA`. Reading
//! either back is guesswork; the declaration's metadata carries the name, and the provider
//! publishes it.
//!
//! Every case runs the identical fixture under kotlinc too, so these assert agreement with the
//! reference compiler rather than transcribing what krusty currently prints.

use super::common;

#[test]
fn a_value_class_property_reference_answers_the_property_name_not_the_mangled_accessor() {
    common::expect_box_same_as_kotlinc(
        "fun box(): String {\n\
         \x20   val s = UIntRange::start\n\
         \x20   if (s.name != \"start\") return \"fail start: ${s.name}\"\n\
         \x20   val e = UIntRange::endInclusive\n\
         \x20   if (e.name != \"endInclusive\") return \"fail endInclusive: ${e.name}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "ValueClassPropertyReferenceName",
    );
}

/// The same question where the accessor is RENAMED rather than mangled, and where it is neither.
#[test]
fn a_dependency_property_reference_answers_the_property_name_whatever_realizes_it() {
    common::expect_box_same_as_kotlinc(
        "fun box(): String {\n\
         \x20   val length = String::length\n\
         \x20   if (length.name != \"length\") return \"fail length: ${length.name}\"\n\
         \x20   val indices = String::indices\n\
         \x20   if (indices.name != \"indices\") return \"fail indices: ${indices.name}\"\n\
         \x20   val size = IntArray::size\n\
         \x20   if (size.name != \"size\") return \"fail size: ${size.name}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "DependencyPropertyReferenceName",
    );
}

#[test]
fn an_import_alias_does_not_replace_the_dependency_propertys_declared_name() {
    common::expect_box_same_as_kotlinc(
        "import kotlin.text.indices as positions\n\n\
         fun box(): String {\n\
         \x20   val property = String::positions\n\
         \x20   return if (property.name == \"indices\") \"OK\" else \"fail: ${property.name}\"\n\
         }\n",
        "DependencyPropertyReferenceAliasName",
    );
}
