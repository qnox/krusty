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

/// A context property owns its type parameters just like an extension property does. Its property
/// type and context parameters name them, while each accessor declares the same parameters in its
/// own metadata scope after the enclosing class parameter.
#[test]
fn a_context_property_publishes_its_own_type_parameters() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        package app\n\
        \n\
        class Entries<K, V>\n\
        \n\
        class Holder<Z> {\n\
        \x20   context(key: K, value: V)\n\
        \x20   var <K, V> entries: Entries<K, V>\n\
        \x20       get() = Entries<K, V>()\n\
        \x20       set(given) {}\n\
        }\n";
    assert_identical("generic_context_property", SRC, "app/Holder");
}

#[test]
fn top_level_context_declarations_keep_the_compatibility_type_list() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        package app\n\
        \n\
        context(key: K)\n\
        val <K> current: K get() = key\n\
        context(key: K)\n\
        fun <K> currentFun(): K = key\n";
    assert_identical(
        "TopLevelGenericContextProperty",
        SRC,
        "app/TopLevelGenericContextPropertyKt",
    );
}

#[test]
fn a_context_function_keeps_the_compatibility_type_list() {
    const SRC: &str = "// LANGUAGE: +ContextParameters\n\
        package app\n\
        \n\
        class Holder {\n\
        \x20   context(key: K)\n\
        \x20   fun <K> current(): K = key\n\
        }\n";
    assert_identical("generic_context_function", SRC, "app/Holder");
}

/// kotlinc's string table keys plain strings and class ids in ONE map, by the class's
/// `pkg/Outer.Inner` name: a root-package alias reference reuses the alias's own name string
/// (`Cargo`) instead of interning `LCargo;`.
#[test]
fn a_class_id_reuses_an_equal_plain_string() {
    const SRC: &str = "typealias Cargo = Payload\n\
        class Payload\n\
        fun declared(): Cargo = Payload()\n";
    assert_identical("root_alias_reference", SRC, "Root_alias_referenceKt");
}

/// The converse: a plain string equal to an earlier class name (`fun Payload()` after the class
/// `Payload` was named) reuses that class id's index.
#[test]
fn a_plain_string_reuses_an_equal_class_id() {
    const SRC: &str = "class Payload(val v: Int)\n\
        fun make(c: Payload): Payload = c\n\
        fun Payload(): Payload = Payload(0)\n";
    assert_identical("root_class_named_function", SRC, "Root_class_named_functionKt");
}
