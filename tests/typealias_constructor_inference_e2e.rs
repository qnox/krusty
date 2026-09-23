//! A typealias constructor in an INFERRED declaration.
//!
//! `val p = AliasedCell(MyClass())` is an unresolved reference unless the alias's expansion is
//! installed before declaration inference runs. The deferred run types a declaration by running the
//! real checker over its body, and source alias expansions were registered AFTER that run — so the
//! same call resolved inside a function body and declined at the top level.
use super::common;

#[test]
fn a_generic_typealias_constructor_types_an_inferred_declaration() {
    const SRC: &str = "class Cell<T>(val x: T)\n\
        typealias AliasedCell<TT> = Cell<TT>\n\
        class MyClass\n\
        val p = AliasedCell(MyClass())\n\
        fun box(): String = if (p.x is MyClass) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "GenericTypeAliasCtorInference");
}

#[test]
fn a_non_generic_typealias_constructor_types_an_inferred_declaration() {
    const SRC: &str = "class Cell<T>(val x: T)\n\
        class MyClass\n\
        typealias Fixed = Cell<MyClass>\n\
        val p = Fixed(MyClass())\n\
        fun box(): String = if (p.x is MyClass) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "FixedTypeAliasCtorInference");
}

/// Omitted alias arguments are type variables of the constructor call, not written arguments. The
/// stdlib's `typealias LinkedHashMap<K, V> = java.util.LinkedHashMap<K, V>` leaves `K` and `V` open
/// in its expansion; binding the Java constructor's own `K`/`V` to those open alias formals rejected
/// `LinkedHashMap(a)` with `Map<K, V>` against `Map<out K, out V>!` and typed the result as the
/// unrelated `LinkedHashMap<K, V>`.
#[test]
fn an_omitted_stdlib_alias_argument_is_inferred_from_the_constructor_arguments() {
    const SRC: &str = "fun <K, V> merge(a: Map<K, V>, b: Map<K, V>): Map<K, V> {\n\
        \x20   val out = LinkedHashMap(a)\n\
        \x20   out.putAll(b)\n\
        \x20   return out\n\
        }\n\
        fun keys(a: Map<String, Int>): List<String> = ArrayList(a.keys)\n\
        fun box(): String {\n\
        \x20   val merged = merge(mapOf(1 to \"O\"), mapOf(2 to \"K\"))\n\
        \x20   val copy: HashMap<Int, String> = HashMap(merged)\n\
        \x20   return if (keys(mapOf(\"x\" to 1)) == listOf(\"x\")) copy[1] + copy[2] else \"keys\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "OmittedStdlibAliasArguments");
}

/// The alias expansion decides where each inferred argument lands and which target arguments the
/// alias fixes on its own: `Flipped<X, Y> = Entry<Y, X>` swaps them, and `Keyed<V>` fixes `String`.
#[test]
fn an_omitted_source_alias_argument_is_inferred_through_the_alias_expansion() {
    const SRC: &str = "class Entry<A, B>(val a: A, val b: B)\n\
        typealias Flipped<X, Y> = Entry<Y, X>\n\
        typealias Keyed<V> = Entry<String, V>\n\
        typealias Same<A, B> = Entry<A, B>\n\
        fun box(): String {\n\
        \x20   val flipped = Flipped(1, \"K\")\n\
        \x20   val keyed = Keyed(\"O\", 2)\n\
        \x20   val same = Same(3L, 'c')\n\
        \x20   val text: String = keyed.a + flipped.b\n\
        \x20   val sum: Int = flipped.a + keyed.b\n\
        \x20   val code: Long = same.a + same.b.code\n\
        \x20   return if (sum == 3 && code == 102L) text else \"fail $sum $code\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "OmittedSourceAliasArguments");
}
