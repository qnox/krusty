//! `@Metadata` of classifiers declared in executable code, measured against kotlinc 2.4.20.
//!
//! A local class, an anonymous object and a class nested in either record their members like any
//! class, with LOCAL visibility. An anonymous object records no constructor. The string table names
//! a local classifier by its raw internal name, then `.`-separated nested segments, marked local and
//! opening its own record. A member whose signature names a local classifier records its JVM
//! descriptor, since a reader cannot map that classifier's id to a JVM name.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const LOCAL: &str = "package app\n\
    \n\
    fun make(): Int {\n\
    \x20   class Local(val x: Int) {\n\
    \x20       fun self(): Local = this\n\
    \x20       fun pair(other: Local) = x + other.x\n\
    \x20       inner class Part\n\
    \x20   }\n\
    \x20   val o = object {\n\
    \x20       fun me() = this\n\
    \x20       val local = Local(2)\n\
    \x20   }\n\
    \x20   return o.local.self().pair(Local(1))\n\
    }\n";

#[test]
fn a_local_class_records_its_members() {
    assert_identical("LocalClass", LOCAL, "app/LocalClassKt$make$Local");
}

#[test]
fn a_class_nested_in_a_local_class_is_named_within_it() {
    assert_identical("NestedLocal", LOCAL, "app/NestedLocalKt$make$Local$Part");
}

#[test]
fn an_anonymous_object_naming_a_local_class_records_its_descriptors() {
    assert_identical("LocalObject", LOCAL, "app/LocalObjectKt$make$o$1");
}

const OBJECTS: &str = "package app\n\
    \n\
    interface Greeter { fun greet(): String }\n\
    abstract class Base(val seed: Int) { abstract fun next(): Int }\n\
    \n\
    fun make(): Int {\n\
    \x20   val plain = object { fun f() = 1; val p = 2 }\n\
    \x20   val g = object : Greeter {\n\
    \x20       var count = 0\n\
    \x20       override fun greet() = \"hi\"\n\
    \x20       private fun hidden() = 3\n\
    \x20   }\n\
    \x20   val b = object : Base(1) { override fun next() = seed + 1 }\n\
    \x20   return plain.f() + g.greet().length + b.next()\n\
    }\n";

#[test]
fn an_object_without_supertypes_records_any_and_its_members() {
    assert_identical("PlainObject", OBJECTS, "app/PlainObjectKt$make$plain$1");
}

#[test]
fn an_object_implementing_an_interface_records_its_members() {
    assert_identical("InterfaceObject", OBJECTS, "app/InterfaceObjectKt$make$g$1");
}

#[test]
fn an_object_extending_a_class_records_no_constructor() {
    assert_identical("BaseObject", OBJECTS, "app/BaseObjectKt$make$b$1");
}
