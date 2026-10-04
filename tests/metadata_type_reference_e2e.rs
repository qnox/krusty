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

/// A class-level payload — a type-parameter bound or a supertype — is written with the class as
/// the current declaration, so its own type parameter is NAMED there
/// (`Type.type_parameter_name`); a member refers to the same parameter by id.
#[test]
fn a_class_names_its_own_type_parameter_in_its_bounds_and_supertypes() {
    const SRC: &str = "package app\n\
        \n\
        class Node<T : Comparable<T>> : Comparable<Node<T>> {\n\
        \x20   override fun compareTo(other: Node<T>): Int = 0\n\
        }\n";
    assert_identical("class_level_own_type_parameter", SRC, "app/Node");
}

/// A nested class's own type parameters take the ids after its enclosing classes' parameters,
/// which kotlinc's parent serializers intern eagerly: the nested `E` is id 1 even though nothing
/// in `Nested` references the outer `E`.
#[test]
fn a_nested_class_reserves_the_enclosing_type_parameter_ids() {
    const SRC: &str = "package app\n\
        \n\
        class Outer<E> {\n\
        \x20   class Nested<E>(val e: E)\n\
        }\n";
    assert_identical("nested_type_parameter_ids", SRC, "app/Outer$Nested");
}

/// The reservation counts the WHOLE chain, outermost first: `Deep`'s own `Z` is id 2 after
/// `Outer`'s `X` (0) and `Middle`'s `Y` (1), and a reference to a grandparent's parameter is
/// addressed by its reserved id.
#[test]
fn a_deeply_nested_class_reserves_every_enclosing_type_parameter_id() {
    const SRC: &str = "package app\n\
        \n\
        class Outer<X> {\n\
        \x20   class Middle<Y> {\n\
        \x20       class Deep<Z>(val z: Z)\n\
        \x20   }\n\
        }\n";
    assert_identical(
        "deeply_nested_type_parameter_ids",
        SRC,
        "app/Outer$Middle$Deep",
    );
}

/// An inner class's members address an enclosing class's type parameter by id
/// (`Type.type_parameter`), never by name.
#[test]
fn an_inner_class_addresses_an_enclosing_type_parameter_by_id() {
    const SRC: &str = "package app\n\
        \n\
        class Outer<E> {\n\
        \x20   inner class Inner(val e: E)\n\
        }\n";
    assert_identical(
        "inner_class_captured_type_parameter",
        SRC,
        "app/Outer$Inner",
    );
}

/// An inner class's OWN level — a type-parameter bound and a supertype — addresses an enclosing
/// class's parameter by the same id-only encoding as its members (`T : E` writes `E` as the
/// reserved id 0; the class's own `T` is named, as at every class level).
#[test]
fn an_inner_class_addresses_an_enclosing_type_parameter_by_id_in_its_bounds_and_supertypes() {
    const SRC: &str = "package app\n\
        \n\
        class Outer<E> {\n\
        \x20   inner class Inner<T : E> : Comparable<E> {\n\
        \x20       override fun compareTo(other: E): Int = 0\n\
        \x20   }\n\
        }\n";
    assert_identical(
        "inner_class_level_captured_type_parameter",
        SRC,
        "app/Outer$Inner",
    );
}

/// The reader side of that id-only reference: a dependent module resolves an inner class's
/// members whose types name an enclosing class's parameter — against a krusty-built library and
/// against the real kotlinc's (the per-classfile decode carries a placeholder, rebound at the
/// classpath boundary from the enclosing class's metadata, declared bound included). A
/// construction through the outer receiver substitutes that receiver's argument for the
/// parameter (`Outer<String>().Inner<String>("b")` passes a `String` where the constructor
/// declares `E`); a later member READ through the inner instance still reads at the parameter's
/// declared bound (the classpath classifier publishes only the class's own parameters).
#[test]
fn an_inner_class_member_reads_an_enclosing_type_parameter_across_modules() {
    const LIB: &str = "package lib\n\
        \n\
        class Outer<E : CharSequence> {\n\
        \x20   inner class Inner<T : E>(val e: E) : Comparable<E> {\n\
        \x20       override fun compareTo(other: E): Int = 0\n\
        \x20       fun take(x: E): E = x\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "import lib.Outer\n\
        \n\
        fun box(): String {\n\
        \x20   val inner = Outer<String>().Inner<String>(\"b\")\n\
        \x20   val bound: CharSequence = inner.e\n\
        \x20   val read: Any? = inner.e\n\
        \x20   return if (read == \"b\" && bound == \"b\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("inner_enclosing_tp", LIB, MAIN)
            .expect("krusty-built library"),
        "OK"
    );
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN).expect("kotlinc-built library"),
        "OK"
    );
}

/// The receiver's enclosing-class argument is part of the constructor's expected parameter
/// types: `Outer<String>().Inner(42)` must FAIL, because the constructor declares `val e: E` and
/// the receiver fixes `E = String`. kotlinc rejects this with an argument type mismatch.
#[test]
fn an_inner_class_argument_must_match_the_enclosing_type_argument() {
    const LIB: &str = "package lib\n\
        \n\
        class Outer<E : CharSequence> {\n\
        \x20   inner class Inner<T : E>(val e: E)\n\
        }\n";
    const MAIN: &str = "import lib.Outer\n\
        \n\
        fun box(): String {\n\
        \x20   val inner = Outer<String>().Inner(42)\n\
        \x20   return \"fail\"\n\
        }\n";
    let expected = vec![
        "argument type mismatch: actual type is 'Int', but 'String' was expected.".to_string(),
    ];
    assert_eq!(
        common::diagnostics_against("inner_enclosing_tp_arg", LIB, MAIN)
            .expect("krusty-built library"),
        expected,
    );
    assert_eq!(
        common::diagnostics_against_ref("inner_enclosing_tp_arg", LIB, MAIN)
            .expect("kotlinc-built library"),
        expected,
    );
}

/// A `$` inside a top-level backticked classifier name is not another owner boundary. The
/// classfile's `InnerClasses` row says that `Outer$Literal$Inner` belongs directly to
/// `Outer$Literal`; the unrelated `Outer` declaration must not contribute an earlier parameter id.
#[test]
fn an_enclosing_type_parameter_follows_inner_classes_not_dollar_segments() {
    const LIB: &str = "package lib\n\
        \n\
        interface Marker { fun text(): String }\n\
        interface Decoy\n\
        class Word(private val value: String) : Marker {\n\
        \x20   override fun text(): String = value\n\
        }\n\
        class Outer<D : Decoy>\n\
        class `Outer$Literal`<E : Marker> {\n\
        \x20   inner class Inner(val value: E) {\n\
        \x20       fun read(): E = value\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "import lib.Marker\n\
        import lib.`Outer$Literal`\n\
        import lib.Word\n\
        \n\
        fun box(): String {\n\
        \x20   val outer = `Outer$Literal`<Word>()\n\
        \x20   val value: Marker = outer.Inner(Word(\"OK\")).read()\n\
        \x20   return value.text()\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("dollar_enclosing_tp", LIB, MAIN)
            .expect("krusty-built library"),
        "OK"
    );
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN).expect("kotlinc-built library"),
        "OK"
    );
}

/// Source spelling is not type-parameter identity. Applying `Inner<Int>` must not substitute its
/// own `E` into the captured `Outer<String>.E`; the provider qualifies the captured parameter by
/// its declaring classifier before publishing the member signature.
#[test]
fn same_spelled_inner_and_outer_parameters_stay_distinct_across_modules() {
    const LIB: &str = "package lib\n\
        \n\
        class Outer<E : CharSequence>(val outer: E) {\n\
        \x20   inner class Inner<E : Number>(val inner: E) {\n\
        \x20       fun outerValue() = this@Outer.outer\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "import lib.Outer\n\
        \n\
        fun box(): String {\n\
        \x20   val value = Outer<String>(\"OK\").Inner<Int>(7)\n\
        \x20   val outer: CharSequence = value.outerValue()\n\
        \x20   val inner: Number = value.inner\n\
        \x20   return if (outer == \"OK\" && inner == 7) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::expect_box_run_against("shadowed_inner_enclosing_tp", LIB, MAIN)
            .expect("krusty-built library"),
        "OK"
    );
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN).expect("kotlinc-built library"),
        "OK"
    );
}

/// A value class's underlying type is a class-level payload, so its own type parameter is named
/// there — and it surfaces at all only because a non-public underlying property has the type
/// recorded on the class itself.
#[test]
fn a_value_class_names_its_own_type_parameter_in_its_underlying_type() {
    const SRC: &str = "package app\n\
        \n\
        @JvmInline\n\
        value class Token<T>(private val value: T)\n";
    assert_identical("value_class_underlying_type_parameter", SRC, "app/Token");
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
