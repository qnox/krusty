//! How `@Metadata` writes an inner-class type, measured against kotlinc 2.4.20. The classifier
//! keeps only its own type arguments and `Type.outer_type` names the enclosing class's type with
//! the remaining ones, recursively along the inner-class chain.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const SRC: &str = "package app\n\
    \n\
    open class Outer {\n\
    \x20   open inner class Inner\n\
    \x20   class Nested\n\
    \x20   inner class Derived : Inner()\n\
    \x20   fun make(): Inner = Inner()\n\
    \x20   fun nested(): Nested = Nested()\n\
    \x20   fun anonymous(): Any = object : Inner() {}\n\
    \x20   inner class Middle {\n\
    \x20       inner class Innermost\n\
    \x20   }\n\
    }\n\
    \n\
    class Generic<T> {\n\
    \x20   inner class Member<U>\n\
    \x20   fun member(): Member<String> = Member()\n\
    }\n\
    \n\
    fun inner(outer: Outer): Outer.Inner = outer.Inner()\n\
    fun innermost(middle: Outer.Middle): Outer.Middle.Innermost = middle.Innermost()\n\
    fun generic(outer: Generic<Int>): Generic<Int>.Member<String>? = outer.member()\n";

const ALIASED_OUTER_ARGUMENT: &str = "package app\n\
    typealias Text = String\n\
    class Outer<T> { inner class Inner<U> }\n\
    interface Shape {\n\
    \x20   fun value(value: Outer<Text>.Inner<Int>): Outer<Text>.Inner<Int>\n\
    }\n";

const ANNOTATED_OUTER_ARGUMENT: &str = "package app\n\
    @Target(AnnotationTarget.TYPE)\n\
    annotation class Mark(val value: String)\n\
    class Outer<T> { inner class Inner<U> }\n\
    interface Shape {\n\
    \x20   fun value(value: Outer<@Mark(\"outer\") String>.Inner<Int>): Outer<@Mark(\"outer\") String>.Inner<Int>\n\
    }\n";

const DEFINITELY_NON_NULL_OUTER_ARGUMENT: &str = "package app\n\
    class Outer<T> { inner class Inner<U> }\n\
    interface Shape {\n\
    \x20   fun <T> value(value: Outer<T & Any>.Inner<Int>): Outer<T & Any>.Inner<Int>\n\
    }\n";

#[test]
fn a_supertype_naming_an_inner_class_carries_its_outer_type() {
    assert_identical("InnerOuterType", SRC, "app/Outer$Derived");
}

#[test]
fn a_member_signature_naming_an_inner_class_carries_its_outer_type() {
    assert_identical("InnerOuterType", SRC, "app/Outer");
}

#[test]
fn a_package_signature_splits_generic_arguments_between_inner_and_outer_types() {
    assert_identical("InnerOuterType", SRC, "app/InnerOuterTypeKt");
}

#[test]
fn an_inner_member_type_of_a_generic_class_names_the_outer_parameter() {
    assert_identical("InnerOuterType", SRC, "app/Generic");
}

#[test]
fn an_anonymous_object_extending_an_inner_class_carries_its_outer_type() {
    assert_identical("InnerOuterType", SRC, "app/Outer$anonymous$1");
}

#[test]
fn an_outer_type_argument_keeps_its_alias_spelling() {
    assert_identical("InnerOuterTypeAlias", ALIASED_OUTER_ARGUMENT, "app/Shape");
}

#[test]
fn an_outer_type_argument_keeps_its_type_use_annotation() {
    assert_identical(
        "InnerOuterTypeAnnotation",
        ANNOTATED_OUTER_ARGUMENT,
        "app/Shape",
    );
}

#[test]
fn an_outer_type_argument_keeps_its_definitely_non_null_flag() {
    assert_identical(
        "InnerOuterTypeDefinitelyNonNull",
        DEFINITELY_NON_NULL_OUTER_ARGUMENT,
        "app/Shape",
    );
}
