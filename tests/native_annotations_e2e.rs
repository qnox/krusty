//! Declaring and APPLYING an annotation, on a target that has no reflection.
//!
//! An annotation on a declaration is metadata, and metadata this target does not keep: nothing
//! emitted can be asked what annotations a class carries, so applying one changes nothing a
//! program can observe and the declaration costs the generator nothing to accept. Whole files were
//! declining for it anyway, on the declaration alone.
//!
//! An annotation INSTANCE is a different thing, and it IS realized: Kotlin defines its `equals`,
//! `hashCode` and `toString` over its arguments — arrays by content — and this backend synthesizes
//! those three rather than leaving `kotlin.Any`'s identity ones in their slots. The rules and the
//! shapes they cover are in `tests/native_annotation_instances_e2e.rs`; the two cases here are the
//! ones the old refusal was stated as.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

#[test]
fn a_declared_and_applied_annotation_leaves_the_program_alone() {
    let src = "annotation class Marker\n\
               @Marker class Holder(val value: String)\n\
               @Marker fun make(): Holder = Holder(\"OK\")\n\
               fun box(): String = make().value\n";
    expect_box_ok_with_stdlib(src, "DeclaredAndAppliedAnnotation");
    expect_native_box(src, "DeclaredAndAppliedAnnotation", "OK");
}

#[test]
fn an_annotation_with_arguments_is_still_only_metadata() {
    // The arguments are written at the use site and never evaluated into a value here — which is
    // the whole reason applying one is free while constructing one is not.
    let src = "annotation class Tag(val name: String, val order: Int = 1)\n\
               @Tag(\"holder\", 2) class Holder(val value: String)\n\
               @Tag(name = \"make\") fun make(): Holder = Holder(\"OK\")\n\
               fun box(): String = make().value\n";
    expect_box_ok_with_stdlib(src, "AnnotationWithArguments");
    expect_native_box(src, "AnnotationWithArguments", "OK");
}

#[test]
fn an_annotation_reaches_every_target_a_program_can_write_it_on() {
    // A property, its backing field, a parameter, a local and an expression each carry the
    // annotation through a different part of lowering, so one program asks all of them.
    let src = "@Target(\n\
               \x20   AnnotationTarget.CLASS, AnnotationTarget.FUNCTION, AnnotationTarget.PROPERTY,\n\
               \x20   AnnotationTarget.FIELD, AnnotationTarget.VALUE_PARAMETER,\n\
               \x20   AnnotationTarget.LOCAL_VARIABLE, AnnotationTarget.EXPRESSION\n\
               )\n\
               @Retention(AnnotationRetention.SOURCE)\n\
               annotation class Everywhere\n\
               @Everywhere class Holder(@Everywhere val value: String) {\n\
               \x20   @Everywhere val doubled: String get() = value + value\n\
               }\n\
               @Everywhere fun make(@Everywhere text: String): Holder {\n\
               \x20   @Everywhere val holder = Holder(text)\n\
               \x20   return holder\n\
               }\n\
               fun box(): String = if (make(\"O\").doubled == \"OO\") \"OK\" else \"fail\"\n";
    expect_box_ok_with_stdlib(src, "AnnotationOnEveryTarget");
    expect_native_box(src, "AnnotationOnEveryTarget", "OK");
}

#[test]
fn a_nested_annotation_declaration_is_accepted_with_its_owner() {
    let src = "class Owner {\n\
               \x20   annotation class Inner\n\
               \x20   @Inner fun answer(): String = \"OK\"\n\
               }\n\
               fun box(): String = Owner().answer()\n";
    expect_box_ok_with_stdlib(src, "NestedAnnotationDeclaration");
    expect_native_box(src, "NestedAnnotationDeclaration", "OK");
}

#[test]
fn two_annotation_instances_with_equal_arguments_are_equal() {
    // Kotlin answers `true` here because an annotation instance's `equals` is its MEMBERS', not
    // its identity. This was the program the refusal was stated as, back when identity equality
    // would have answered `false` while still compiling and running.
    let src = "annotation class Tag(val name: String)\n\
         fun box(): String {\n\
         \x20   val first = Tag(\"x\")\n\
         \x20   val second = Tag(\"x\")\n\
         \x20   if (first !== second) {\n\
         \x20       if (first != second) return \"fail equals\"\n\
         \x20       if (first.hashCode() != second.hashCode()) return \"fail hashCode\"\n\
         \x20   }\n\
         \x20   if (first == Tag(\"y\")) return \"fail a differing member\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(src, "ConstructedAnnotation");
    expect_native_box(src, "ConstructedAnnotation", "OK");
}

#[test]
fn a_constructed_annotations_member_is_read_from_the_instance() {
    let src = "annotation class Tag(val name: String)\n\
         fun box(): String = Tag(\"OK\").name\n";
    expect_box_ok_with_stdlib(src, "ConstructedAnnotationMember");
    expect_native_box(src, "ConstructedAnnotationMember", "OK");
}
