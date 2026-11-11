//! A reference `Array<T>` property serializes through `ReferenceArraySerializer`.
//!
//! ```kotlin
//! @Serializable
//! class Box(val names: Array<String>)
//! ```
//!
//! krusty refused the file with `krusty: this construct is not yet supported by the IR backend`:
//! no element serializer was derived for `kotlin.Array`, so the plugin's placeholder failed the
//! whole file. kotlinc caches `ReferenceArraySerializer(E::class, <E's serializer>)` in the
//! class's lazy child-serializer array, loading the element's boxed class token
//! (`ldc java/lang/Integer` for `Array<Int>`, `ldc [Ljava/lang/String;` for `Array<Array<String>>`).

use super::serialization_test_support::both_compilers_box;

/// String, class, nullable-element, and nullable-array properties round-trip through JSON.
#[test]
fn reference_array_properties_round_trip() {
    const MAIN: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Tag(val name: String)\n\
\n\
@Serializable\n\
class Holder(val names: Array<String>, val tags: Array<Tag>, val maybe: Array<String?>? = null)\n\
\n\
fun box(): String {\n\
\x20   val value = Holder(arrayOf(\"a\", \"b\"), arrayOf(Tag(\"t\")), arrayOf(null, \"x\"))\n\
\x20   val json = Json.encodeToString(Holder.serializer(), value)\n\
\x20   val expected = \"{\\\"names\\\":[\\\"a\\\",\\\"b\\\"],\\\"tags\\\":[{\\\"name\\\":\\\"t\\\"}],\\\"maybe\\\":[null,\\\"x\\\"]}\"\n\
\x20   if (json != expected) return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Holder.serializer(), json)\n\
\x20   if (back.names.toList() != listOf(\"a\", \"b\")) return \"FAIL: names\"\n\
\x20   if (back.tags.toList() != listOf(Tag(\"t\"))) return \"FAIL: tags\"\n\
\x20   if (back.maybe?.toList() != listOf(null, \"x\")) return \"FAIL: maybe\"\n\
\x20   val absent = Json.decodeFromString(Holder.serializer(), \"{\\\"names\\\":[],\\\"tags\\\":[]}\")\n\
\x20   if (absent.maybe != null) return \"FAIL: default\"\n\
\x20   return \"OK\"\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "reference_array_properties"), "OK");
}

/// A boxed primitive element, a collection element, and a nested array: each element class token is
/// the boxed one (`Integer`, `List`, `String[]`).
#[test]
fn boxed_collection_and_nested_array_elements_round_trip() {
    const MAIN: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
class Holder(val ints: Array<Int>, val lists: Array<List<String>>, val nested: Array<Array<String>>)\n\
\n\
fun box(): String {\n\
\x20   val value = Holder(arrayOf(1, 2), arrayOf(listOf(\"a\")), arrayOf(arrayOf(\"b\", \"c\")))\n\
\x20   val json = Json.encodeToString(Holder.serializer(), value)\n\
\x20   val expected = \"{\\\"ints\\\":[1,2],\\\"lists\\\":[[\\\"a\\\"]],\\\"nested\\\":[[\\\"b\\\",\\\"c\\\"]]}\"\n\
\x20   if (json != expected) return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Holder.serializer(), json)\n\
\x20   if (back.ints.toList() != listOf(1, 2)) return \"FAIL: ints\"\n\
\x20   if (back.lists.toList() != listOf(listOf(\"a\"))) return \"FAIL: lists\"\n\
\x20   if (back.nested.single().toList() != listOf(\"b\", \"c\")) return \"FAIL: nested\"\n\
\x20   if (back.nested.single()::class != Array<String>::class) return \"FAIL: nested class\"\n\
\x20   return \"OK\"\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "reference_array_elements"), "OK");
}
