//! The supertypes a class's `@Metadata` lists, measured against kotlinc 2.4.20: exactly the
//! declaration's supertype list in source order, wherever the superclass appears in it.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const DECLARATIONS: &str = "package app\n\
    \n\
    interface Tagged\n\
    interface Marked\n\
    interface Shown { fun show() {} }\n\
    open class Base\n\
    open class Holder<T>\n\
    typealias Basis = Base\n\
    typealias Labelled = Tagged\n";

fn source(class: &str) -> String {
    format!("{DECLARATIONS}\n{class}\n")
}

#[test]
fn a_superclass_between_interfaces_keeps_its_source_position() {
    let src = source("class Middle : Tagged, Base(), Marked");
    assert_identical("SupertypeMiddle", &src, "app/Middle");
}

#[test]
fn a_superclass_after_the_interfaces_keeps_its_source_position() {
    let src = source("abstract class Last : Marked, Tagged, Base()");
    assert_identical("SupertypeLast", &src, "app/Last");
}

#[test]
fn a_superclass_written_first_stays_first() {
    let src = source("class First : Base(), Tagged, Marked");
    assert_identical("SupertypeFirst", &src, "app/First");
}

#[test]
fn a_delegated_interface_keeps_its_source_position_before_the_superclass() {
    let src = source("class Delegating(shown: Shown) : Shown by shown, Tagged, Base()");
    assert_identical("SupertypeDelegating", &src, "app/Delegating");
}

#[test]
fn a_generic_superclass_keeps_its_source_position() {
    let src = source("class Generic : Tagged, Holder<Marked>(), Marked");
    assert_identical("SupertypeGeneric", &src, "app/Generic");
}

#[test]
fn an_aliased_superclass_keeps_its_abbreviation_at_its_source_position() {
    let src = source("class Aliased : Labelled, Basis(), Marked");
    assert_identical("SupertypeAliased", &src, "app/Aliased");
}

#[test]
fn a_parenless_superclass_keeps_its_source_position() {
    let src = source("class Parenless : Tagged, Base {\n    constructor() : super()\n}");
    assert_identical("SupertypeParenless", &src, "app/Parenless");
}

#[test]
fn a_local_class_superclass_keeps_its_source_position() {
    let src = source("fun make(): Any {\n    class Local : Tagged, Base()\n    return Local()\n}");
    assert_identical("SupertypeLocal", &src, "app/SupertypeLocalKt$make$Local");
}

#[test]
fn an_anonymous_object_superclass_keeps_its_source_position() {
    let src = source("fun make(): Any = object : Tagged, Base() {}");
    assert_identical(
        "SupertypeAnonymous",
        &src,
        "app/SupertypeAnonymousKt$make$1",
    );
}
