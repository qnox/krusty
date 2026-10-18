//! A data class's generated `hashCode` calls an interface-typed property's `hashCode` through
//! `java/lang/Object`, as kotlinc does, also when the interface is declared in another file of the
//! module. Naming the interface as the `invokevirtual` owner fails at run time with
//! `IncompatibleClassChangeError`.

use super::common;

const ID: &str = "interface Id {\n\
    \x20   fun props(): String = toString()\n\
    \x20   companion object { operator fun invoke(v: String): Id = DefaultId(v) }\n\
    }\n\
    data class DefaultId(val value: String) : Id\n";

const INSTANCE: &str = "data class Instance(val name: String, val id: Id)\n\
    fun box(): String {\n\
    \x20   val a = Instance(\"x\", Id(\"y\"))\n\
    \x20   val b = Instance(\"x\", Id(\"y\"))\n\
    \x20   return if (a.hashCode() == b.hashCode() && a == b) \"OK\" else \"fail\"\n\
    }\n";

#[test]
fn a_sibling_file_interface_property_hashes_through_object_like_kotlinc() {
    let classes =
        common::classes_against_kotlinc_module(&[("Id.kt", ID), ("Instance.kt", INSTANCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_sibling_file_interface_property_hashes_at_run_time() {
    let stdlib = common::stdlib_jar();
    let result = common::compile_and_run_box_files(
        &[("Id.kt", ID), ("Instance.kt", INSTANCE)],
        &[stdlib],
        Some(&common::jdk_modules()),
    );
    if let Some(result) = result {
        assert_eq!(result, "OK");
    }
}
