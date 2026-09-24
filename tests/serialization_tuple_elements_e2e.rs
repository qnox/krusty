//! `Pair`, `Triple` and `Map.Entry` are not `@Serializable`, but the runtime ships a serializer for
//! each, and kotlinc's plugin selects it by the classifier: a `Pair<A, B>` element is
//! `new PairSerializer(<A>, <B>)`, cached in `$childSerializers` like a collection's. krusty knew
//! only the collection serializers, so a property of any of these types was rejected as an
//! unsupported construct.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{member_body, plugin_and_runtime};
use super::serialization_test_support::both_compilers_box;

/// Each tuple round-trips under both compilers, as a property, nested in a collection, nullable,
/// over a nullable argument, and through a reified format call.
#[test]
fn a_tuple_element_serializes_under_both_compilers() {
    let src = "import kotlinx.serialization.Serializable\n\
        import kotlinx.serialization.json.Json\n\
        \n\
        @Serializable data class Cell(val n: Int)\n\
        \n\
        @Serializable\n\
        data class Tuples(\n\
        \x20   val two: Pair<String, Int>,\n\
        \x20   val three: Triple<Int, Cell, String?>,\n\
        \x20   val entry: Map.Entry<String, Cell>,\n\
        \x20   val pairs: List<Pair<Cell, Long>>,\n\
        \x20   val maybe: Pair<Int, Int>?,\n\
        )\n\
        \n\
        fun box(): String {\n\
        \x20   val value = Tuples(\n\
        \x20       \"a\" to 1, Triple(2, Cell(3), null), mapOf(\"k\" to Cell(4)).entries.first(),\n\
        \x20       listOf(Cell(5) to 6L), null,\n\
        \x20   )\n\
        \x20   val text = Json.encodeToString(Tuples.serializer(), value)\n\
        \x20   val back = Json.decodeFromString(Tuples.serializer(), text)\n\
        \x20   val same = back.two == value.two && back.three == value.three &&\n\
        \x20       back.pairs == value.pairs && back.maybe == null &&\n\
        \x20       back.entry.key == \"k\" && back.entry.value == Cell(4)\n\
        \x20   val reified = Json.encodeToString(Cell(7) to Cell(8))\n\
        \x20   val pair = Json.decodeFromString<Pair<Cell, Cell>>(reified)\n\
        \x20   return listOf(text, same, reified, pair).joinToString(\" | \")\n\
        }\n";
    assert_eq!(
        both_compilers_box(src, "tuple_elements"),
        "{\"two\":{\"first\":\"a\",\"second\":1},\
         \"three\":{\"first\":2,\"second\":{\"n\":3},\"third\":null},\
         \"entry\":{\"k\":{\"n\":4}},\"pairs\":[{\"first\":{\"n\":5},\"second\":6}],\
         \"maybe\":null} | true | {\"first\":{\"n\":7},\"second\":{\"n\":8}} | \
         (Cell(n=7), Cell(n=8))"
    );
}

/// A tuple over a class type parameter is constructed from the serializer supplied to the
/// generated generic serializer, rather than from the parameter's upper-bound classifier.
#[test]
fn a_tuple_over_a_type_parameter_uses_the_supplied_serializer() {
    let src = "import kotlinx.serialization.Serializable\n\
        import kotlinx.serialization.json.Json\n\
        \n\
        @Serializable data class Cell(val n: Int)\n\
        @Serializable data class GenericTuples<T>(val pair: Pair<T, T>)\n\
        \n\
        fun box(): String {\n\
        \x20   val value = GenericTuples(Cell(9) to Cell(10))\n\
        \x20   val serializer = GenericTuples.serializer(Cell.serializer())\n\
        \x20   val text = Json.encodeToString(serializer, value)\n\
        \x20   val back = Json.decodeFromString(serializer, text)\n\
        \x20   return \"$text | ${back == value}\"\n\
        }\n";
    assert_eq!(
        both_compilers_box(src, "generic_tuple_elements"),
        "{\"pair\":{\"first\":{\"n\":9},\"second\":{\"n\":10}}} | true"
    );
}

/// The cached factory for each tuple is the construction kotlinc emits: the tuple's runtime
/// serializer over its argument serializers, each narrowed to `KSerializer`.
#[test]
fn a_tuple_element_is_built_the_way_kotlinc_builds_it() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = "import kotlinx.serialization.Serializable\n\
        \n\
        @Serializable data class Cell(val n: Int)\n\
        \n\
        @Serializable\n\
        data class Tuples(\n\
        \x20   val two: Pair<String, Int>,\n\
        \x20   val three: Triple<Int, Cell, String>,\n\
        \x20   val entry: Map.Entry<String, Cell>,\n\
        )\n";
    let Some(built) = compare_with_kotlinc_plugin("Tuples", src, "Tuples", &cp, "25", &extra)
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for (member, serializer) in [
        ("_childSerializers$_anonymous_()", "PairSerializer"),
        ("_childSerializers$_anonymous_$0()", "TripleSerializer"),
        ("_childSerializers$_anonymous_$1()", "MapEntrySerializer"),
        ("static {}", "LazyKt.lazy"),
    ] {
        let want = member_body(&built.reference, member);
        assert!(
            want.iter().any(|line| line.contains(serializer)),
            "Tuples.{member}: the reference builds a {serializer}: {want:?}"
        );
        assert_eq!(member_body(&built.krusty, member), want, "Tuples.{member}");
    }
}
