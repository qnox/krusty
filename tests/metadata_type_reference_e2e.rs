//! How `@Metadata` writes a `Type`, measured against kotlinc 2.4.0, 2.4.10 and 2.4.20.
//!
//! kotlinc serializes every message's fields in ascending field-number order (`Type.flags`, field 1,
//! first; a function's `return_type`, 3, before its `type_parameter`, 4). It refers to a type
//! parameter the declaration being written owns by name (`Type.type_parameter_name`) and to an
//! enclosing class's by id (`Type.type_parameter`), bounds included.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_member_function_names_its_own_type_parameters() {
    const SRC: &str = "package app\n\
        \n\
        interface Ordered<T>\n\
        \n\
        class Box<T>(val value: T) {\n\
        \x20   fun <R : Ordered<R>> map(f: (T) -> R): R = f(value)\n\
        }\n";
    assert_identical("member_generic_function", SRC, "app/Box");
}

#[test]
fn a_member_extension_property_names_its_own_type_parameter() {
    const SRC: &str = "package app\n\
        \n\
        class Shelf<E>(val first: E, val second: E)\n\
        \n\
        class Holder<K> {\n\
        \x20   val <E> Shelf<E>.other: E get() = second\n\
        \x20   fun key(k: K): K = k\n\
        }\n";
    assert_identical("member_generic_property", SRC, "app/Holder");
}

#[test]
fn a_suspend_function_type_writes_its_flags_first() {
    const SRC: &str = "package app\n\
        \n\
        class Crate<T>\n\
        \n\
        abstract class Runner {\n\
        \x20   abstract fun blocks(): Crate<suspend (Int) -> String>\n\
        }\n";
    assert_identical("suspend_function_type", SRC, "app/Runner");
}

/// A setter is a declaration of its own, so its value parameter addresses the property's type
/// parameter by id while the property's return and receiver types name it.
#[test]
fn a_generic_property_setter_parameter_addresses_its_type_parameter_by_id() {
    const SRC: &str = "package app\n\
        \n\
        class Cell<V>(var value: V)\n\
        \n\
        class Holder {\n\
        \x20   var <V> Cell<V>.content: V\n\
        \x20       get() = value\n\
        \x20       set(given) { value = given }\n\
        }\n";
    assert_identical("generic_property_setter", SRC, "app/Holder");
}
