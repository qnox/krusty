//! How `@Metadata` describes a value class and the `equals`/`hashCode`/`toString` kotlinc
//! synthesizes for it, measured against kotlinc 2.4.20.
//!
//! A `@JvmInline value class` and a boxed one (`+FullValueClasses`, no `@JvmInline`) are both
//! value classes in Kotlin metadata: the class carries `IS_VALUE`, and each `Any` member its source
//! does not override is a SYNTHESIZED function record after the declared ones. Only the inline one
//! names an underlying property. An abstract or sealed value class instead declares each such
//! member open, as a DECLARATION with no JVM method; a regular subclass declares none.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const INLINE: &str = "package app\n\
    \n\
    @JvmInline value class Plain(val x: Int)\n\
    @JvmInline value class Boxed<T>(val value: T)\n\
    @JvmInline value class Shown(val x: Int) {\n\
    \x20   override fun toString(): String = \"Shown\"\n\
    }\n\
    interface Measured { fun size(): Int }\n\
    @JvmInline value class Sized(val x: Int) : Measured {\n\
    \x20   override fun size(): Int = x\n\
    }\n";

#[test]
fn an_inline_value_class_describes_its_synthesized_members() {
    assert_identical("ValueClassSynthesized", INLINE, "app/Plain");
}

#[test]
fn a_generic_inline_value_class_describes_its_synthesized_members() {
    assert_identical("ValueClassSynthesized", INLINE, "app/Boxed");
}

#[test]
fn an_inline_value_class_with_a_declared_to_string_synthesizes_only_the_rest() {
    assert_identical("ValueClassSynthesized", INLINE, "app/Shown");
}

#[test]
fn an_inline_value_class_implementing_an_interface_describes_its_synthesized_members() {
    assert_identical("ValueClassSynthesized", INLINE, "app/Sized");
}

const FULL: &str = "// LANGUAGE: +FullValueClasses\n\
    package app\n\
    \n\
    value class Single(val x: Int)\n\
    value class Pair(val x: Int, val y: String)\n\
    value class Box<T>(val value: T)\n\
    value class Shown(val x: Int, val y: Int) {\n\
    \x20   override fun toString(): String = \"Shown\"\n\
    }\n\
    abstract value class Base(a: Int)\n\
    value class Derived(val x: Int) : Base(x) {\n\
    \x20   override fun hashCode(): Int = x\n\
    }\n\
    abstract value class Named(a: Int) {\n\
    \x20   override fun toString(): String = \"Named\"\n\
    }\n\
    sealed value class Shape\n\
    value class Circle(val r: Int) : Shape()\n\
    class Regular(val v: Int) : Base(v)\n";

/// Both compilers' class files for `class` of [`FULL`]: the member table and `@Metadata` must be
/// kotlinc's.
fn assert_full_value_class_identical(class: &str) {
    let comparison = common::compare_with_kotlinc_plugin(
        "FullValueClassSynthesized",
        FULL,
        class,
        &[common::stdlib_jar()],
        "17",
        &["-XXLanguage:+FullValueClasses".to_string()],
    )
    .expect("reference kotlinc and javap are provisioned");
    assert_eq!(
        common::member_table(&comparison.krusty_bytes),
        common::member_table(&comparison.reference_bytes),
        "{class}: kotlinc's member table"
    );
    assert_eq!(
        common::raw_kotlin_metadata(&comparison.krusty_bytes),
        common::raw_kotlin_metadata(&comparison.reference_bytes),
        "{class}: kotlinc's @Metadata"
    );
}

#[test]
fn a_boxed_value_class_is_a_value_class_with_synthesized_members() {
    assert_full_value_class_identical("app/Single");
}

#[test]
fn a_multi_field_value_class_is_a_value_class_with_synthesized_members() {
    assert_full_value_class_identical("app/Pair");
}

#[test]
fn a_generic_boxed_value_class_is_a_value_class_with_synthesized_members() {
    assert_full_value_class_identical("app/Box");
}

#[test]
fn a_boxed_value_class_with_a_declared_to_string_synthesizes_only_the_rest() {
    assert_full_value_class_identical("app/Shown");
}

#[test]
fn a_boxed_value_class_extending_an_abstract_one_synthesizes_what_it_does_not_declare() {
    assert_full_value_class_identical("app/Derived");
}

#[test]
fn an_abstract_value_class_declares_the_members_it_does_not_override_open() {
    assert_full_value_class_identical("app/Base");
}

#[test]
fn an_abstract_value_class_with_a_declared_to_string_declares_only_the_rest() {
    assert_full_value_class_identical("app/Named");
}

#[test]
fn a_sealed_value_class_declares_its_members_open() {
    assert_full_value_class_identical("app/Shape");
}

#[test]
fn a_value_subclass_of_a_sealed_value_class_synthesizes_its_members() {
    assert_full_value_class_identical("app/Circle");
}

#[test]
fn a_regular_subclass_of_an_abstract_value_class_declares_no_structural_members() {
    assert_full_value_class_identical("app/Regular");
}
