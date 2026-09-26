//! `ValueParameter.flags` bit 1 (`DECLARES_DEFAULT_VALUE`), measured against kotlinc 2.4.20: a
//! parameter records it only when its own declaration writes the default. An override's parameter
//! inherits the default for callers but never declares it, and neither does a member that only
//! implements an interface function through a subclass's fake override.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const OVERRIDE_SRC: &str = "package app\n\
    \n\
    abstract class Base {\n\
    \x20   abstract fun pick(a: Int = 1, b: Int): Int\n\
    }\n\
    \n\
    class Derived : Base() {\n\
    \x20   override fun pick(a: Int, b: Int): Int = a\n\
    }\n";

#[test]
fn the_declaring_function_records_its_default() {
    assert_identical("DeclaredDefault", OVERRIDE_SRC, "app/Base");
}

#[test]
fn an_override_does_not_declare_an_inherited_default() {
    assert_identical("InheritedDefault", OVERRIDE_SRC, "app/Derived");
}

#[test]
fn an_implementation_through_a_fake_override_declares_no_default() {
    let src = "package app\n\
        \n\
        interface Source {\n\
        \x20   fun pick(a: Int = 2): Int\n\
        }\n\
        \n\
        open class Impl {\n\
        \x20   open fun pick(a: Int): Int = a\n\
        }\n\
        \n\
        class Both : Impl(), Source\n";
    assert_identical("FakeOverrideDefault", src, "app/Impl");
}
