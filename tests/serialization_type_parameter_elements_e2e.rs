//! A generic `@Serializable` class serializes an element that mentions its type parameter through
//! the type-parameter serializer its `$serializer` was constructed with.
//!
//! kotlinc's generic `$serializer` holds one `typeSerialK` field per type parameter and builds every
//! element that mentions one in place, around that field: `List<T>` is
//! `new ArrayListSerializer(this.typeSerial0)`, `Map<String, T>` a `LinkedHashMapSerializer` over
//! `StringSerializer` and the field, `Box<T>` the companion's `serializer(this.typeSerial0)`, and
//! `Box<T?>` that call around `this.typeSerial0.nullable`. Only an element that mentions no type
//! parameter comes from the class's `$childSerializers` cache. krusty
//! resolved a type-parameter element only when it was the bare parameter, so `List<T>` and
//! `Map<String, T>` were rejected as unsupported constructs.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{member_body, plugin_and_runtime};
use super::serialization_test_support::{both_compilers_box, both_compilers_box_files};

const ENTRY: &str = "@Serializable data class Entry(val id: Int, val label: String)\n";

const GENERIC: &str = "@Serializable\n\
data class Bag<T>(\n\
\x20   val items: List<T>,\n\
\x20   val byKey: Map<String, T>,\n\
\x20   val one: T,\n\
\x20   val maybe: List<T?>,\n\
\x20   val projected: MutableList<out T?>,\n\
\x20   val nullableOne: T?,\n\
\x20   val fixed: List<String>,\n\
)\n\
\n\
@Serializable\n\
data class Shelf<K, V>(\n\
\x20   val bags: List<Bag<V>>,\n\
\x20   val index: Map<Int, List<K>>,\n\
\x20   val first: Bag<K>?,\n\
\x20   val nullableArgument: Bag<K?>,\n\
)\n\
\n\
@Serializable\n\
data class Grid(val cells: Map<String, List<Int?>>, val rows: List<List<String>>)\n";

const BOX: &str = "fun box(): String {\n\
\x20   val bag = Bag(\n\
\x20       listOf(Entry(1, \"a\")), mapOf(\"k\" to Entry(2, \"b\")), Entry(3, \"c\"),\n\
\x20       listOf(null, Entry(4, \"d\")), mutableListOf(null, Entry(5, \"e\")), null, listOf(\"x\"),\n\
\x20   )\n\
\x20   val bags = Bag.serializer(Entry.serializer())\n\
\x20   val shelf = Shelf(\n\
\x20       listOf(bag), mapOf(7 to listOf(\"s\")),\n\
\x20       Bag(listOf(\"p\"), mapOf(\"q\" to \"r\"), \"o\", listOf(null), mutableListOf(\"u\", null), null, listOf()),\n\
\x20       Bag<String?>(listOf(null), mapOf(\"z\" to null), null, listOf(null), mutableListOf(\"v\", null), null, listOf()),\n\
\x20   )\n\
\x20   val shelves = Shelf.serializer(String.serializer(), Entry.serializer())\n\
\x20   val grid = Grid(mapOf(\"c\" to listOf(1, null)), listOf(listOf(\"r\")))\n\
\x20   val a = Json.encodeToString(bags, bag)\n\
\x20   val b = Json.encodeToString(shelves, shelf)\n\
\x20   val c = Json.encodeToString(Grid.serializer(), grid)\n\
\x20   return listOf(\n\
\x20       a, Json.decodeFromString(bags, a) == bag,\n\
\x20       b, Json.decodeFromString(shelves, b) == shelf,\n\
\x20       c, Json.decodeFromString(Grid.serializer(), c) == grid,\n\
\x20   ).joinToString(\" | \")\n\
}\n";

const IMPORTS: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.builtins.serializer\n\
import kotlinx.serialization.json.Json\n";

const EXPECTED: &str =
    "{\"items\":[{\"id\":1,\"label\":\"a\"}],\"byKey\":{\"k\":{\"id\":2,\"label\":\"b\"}},\
\"one\":{\"id\":3,\"label\":\"c\"},\"maybe\":[null,{\"id\":4,\"label\":\"d\"}],\
\"projected\":[null,{\"id\":5,\"label\":\"e\"}],\"nullableOne\":null,\
\"fixed\":[\"x\"]} | true | \
{\"bags\":[{\"items\":[{\"id\":1,\"label\":\"a\"}],\"byKey\":{\"k\":{\"id\":2,\"label\":\"b\"}},\
\"one\":{\"id\":3,\"label\":\"c\"},\"maybe\":[null,{\"id\":4,\"label\":\"d\"}],\
\"projected\":[null,{\"id\":5,\"label\":\"e\"}],\"nullableOne\":null,\
\"fixed\":[\"x\"]}],\
\"index\":{\"7\":[\"s\"]},\"first\":{\"items\":[\"p\"],\"byKey\":{\"q\":\"r\"},\"one\":\"o\",\
\"maybe\":[null],\"projected\":[\"u\",null],\"nullableOne\":null,\"fixed\":[]},\
\"nullableArgument\":{\"items\":[null],\"byKey\":{\"z\":null},\"one\":null,\"maybe\":[null],\
\"projected\":[\"v\",null],\"nullableOne\":null,\"fixed\":[]}} | true | \
{\"cells\":{\"c\":[1,null]},\"rows\":[[\"r\"]]} | true";

/// Every shape of type-parameter element round-trips under both compilers.
#[test]
fn a_type_parameter_element_serializes_under_both_compilers() {
    let src = format!("{IMPORTS}\n{ENTRY}\n{GENERIC}\n{BOX}");
    assert_eq!(
        both_compilers_box(&src, "type_parameter_elements"),
        EXPECTED
    );
}

/// The element classifier declared in another file of the module does not change how the generic
/// class reaches it: through the serializer the caller passes for the type parameter.
#[test]
fn a_type_parameter_element_over_a_sibling_file_classifier_serializes_under_both_compilers() {
    let declarations = format!("import kotlinx.serialization.Serializable\n\n{ENTRY}");
    let generic = format!("{IMPORTS}\n{GENERIC}\n{BOX}");
    assert_eq!(
        both_compilers_box_files(
            &[("Entry.kt", &declarations), ("Main.kt", &generic)],
            "sibling_type_parameter_elements",
        ),
        EXPECTED
    );
}

/// kotlinc's element serializers: built in place around `typeSerialK` in `childSerializers()` and
/// `deserialize()`, every serializer operand of a factory narrowed to `KSerializer` (a `checkcast`
/// only where its static type is a concrete serializer class), and a non-generic element read from
/// the cache.
#[test]
fn a_type_parameter_element_is_built_the_way_kotlinc_builds_it() {
    let Some((plugin, cp)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let src = format!("import kotlinx.serialization.Serializable\n\n{ENTRY}\n{GENERIC}");
    for (class, members) in [
        (
            "Bag$$serializer",
            &[
                "childSerializers()",
                "Bag<T> deserialize(",
                "typeParametersSerializers()",
            ][..],
        ),
        (
            "Shelf$$serializer",
            &["childSerializers()", "Shelf<K, V> deserialize("][..],
        ),
        (
            "Grid",
            &[
                "_childSerializers$_anonymous_()",
                "_childSerializers$_anonymous_$0()",
            ][..],
        ),
    ] {
        let Some(built) =
            compare_with_kotlinc_plugin("TypeParameters", &src, class, &cp, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        for member in members {
            let want = member_body(&built.reference, member);
            assert!(
                want.len() > 1,
                "{class}.{member}: the reference declares it"
            );
            assert_eq!(member_body(&built.krusty, member), want, "{class}.{member}");
        }
    }
}
