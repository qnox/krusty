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
    assert_identical(
        "root_class_named_function",
        SRC,
        "Root_class_named_functionKt",
    );
}

/// The class header (its type parameters' bounds and its supertypes) is written by the class's own
/// serializer, which names the parameters the class owns; a member still addresses them by id.
#[test]
fn a_class_header_names_its_own_type_parameters() {
    const SRC: &str = "package app\n\
        \n\
        interface Ordered<T>\n\
        open class Base<X>\n\
        \n\
        class Ranked<T : Ordered<T>, U : T>(val first: T) : Base<T>(), Ordered<U> {\n\
        \x20   fun second(u: U): T = first\n\
        }\n";
    assert_identical("class_header_names", SRC, "app/Ranked");
}

/// A nested class is serialized under its outer classes, whose parameters keep the ids before the
/// nested class's own even when it is not `inner` and cannot use them.
#[test]
fn a_nested_class_numbers_its_type_parameters_after_its_outer_classes() {
    const SRC: &str = "package app\n\
        \n\
        interface Ordered<T>\n\
        \n\
        class Outer<P> {\n\
        \x20   class Middle<Q> {\n\
        \x20       class Leaf<R : Ordered<R>>(val r: R) {\n\
        \x20           fun <S> pick(s: S): R = r\n\
        \x20       }\n\
        \x20   }\n\
        }\n";
    assert_identical("nested_type_parameter_ids", SRC, "app/Outer$Middle$Leaf");
    assert_identical("nested_type_parameter_ids", SRC, "app/Outer$Middle");
}

/// An inner class's own bound on an outer parameter (`Q : P`) addresses `P` by its joint id alone:
/// the outer serializer owns it, so its name never enters the inner class's string table.
#[test]
fn an_inner_class_bound_addresses_an_outer_parameter_by_id() {
    const SRC: &str = "package app\n\
        \n\
        open class Base<X>\n\
        \n\
        class Outer<P> {\n\
        \x20   inner class Inner<Q : P> : Base<P>() {\n\
        \x20       fun pick(p: P, q: Q): P = p\n\
        \x20   }\n\
        }\n";
    assert_identical("inner_outer_bound", SRC, "app/Outer$Inner");
}

/// An inner class two levels down addresses both outer classes' parameters by joint id and names
/// only its own.
#[test]
fn a_doubly_inner_class_addresses_both_outer_parameters_by_id() {
    const SRC: &str = "package app\n\
        \n\
        interface Link<A, B>\n\
        \n\
        class Outer<P> {\n\
        \x20   inner class Middle<M : P> {\n\
        \x20       inner class Leaf<L : M> : Link<P, M> {\n\
        \x20           fun all(p: P, m: M, l: L): L = l\n\
        \x20       }\n\
        \x20   }\n\
        }\n";
    assert_identical("doubly_inner_ids", SRC, "app/Outer$Middle$Leaf");
    assert_identical("doubly_inner_ids", SRC, "app/Outer$Middle");
}
