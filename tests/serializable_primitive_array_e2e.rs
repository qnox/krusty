//! A primitive array has a built-in element serializer.
//!
//! ```kotlin
//! @Serializable
//! data class FindDefectsRequest(val assertion: String? = null, val screen: ByteArray)
//! ```
//!
//! krusty refused the file with `krusty: this construct is not yet supported by the IR backend`:
//! the builtin table had no entry for any primitive array, so no element serializer was derived and
//! the plugin's placeholder failed the whole file (Maestro's `maestro-ai`, and every compilation
//! behind it). kotlinc reads each one from the runtime's singleton:
//! `getstatic kotlinx/serialization/internal/ByteArraySerializer.INSTANCE`.

use super::serialization_test_support::both_compilers_box;

/// Every primitive array as a property, each round-tripped through JSON.
#[test]
fn every_primitive_array_property_serializes_through_its_builtin() {
    const MAIN: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
class Arrays(\n\
\x20   val bytes: ByteArray,\n\
\x20   val shorts: ShortArray,\n\
\x20   val ints: IntArray,\n\
\x20   val longs: LongArray,\n\
\x20   val floats: FloatArray,\n\
\x20   val doubles: DoubleArray,\n\
\x20   val chars: CharArray,\n\
\x20   val booleans: BooleanArray,\n\
)\n\
\n\
fun box(): String {\n\
\x20   val value = Arrays(\n\
\x20       byteArrayOf(1), shortArrayOf(2), intArrayOf(3), longArrayOf(4),\n\
\x20       floatArrayOf(5f), doubleArrayOf(6.0), charArrayOf('c'), booleanArrayOf(true),\n\
\x20   )\n\
\x20   val json = Json.encodeToString(Arrays.serializer(), value)\n\
\x20   val expected = \"{\\\"bytes\\\":[1],\\\"shorts\\\":[2],\\\"ints\\\":[3],\\\"longs\\\":[4],\" +\n\
\x20       \"\\\"floats\\\":[5.0],\\\"doubles\\\":[6.0],\\\"chars\\\":[\\\"c\\\"],\\\"booleans\\\":[true]}\"\n\
\x20   if (json != expected) return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Arrays.serializer(), json)\n\
\x20   if (!back.bytes.contentEquals(value.bytes)) return \"FAIL: bytes\"\n\
\x20   if (!back.shorts.contentEquals(value.shorts)) return \"FAIL: shorts\"\n\
\x20   if (!back.ints.contentEquals(value.ints)) return \"FAIL: ints\"\n\
\x20   if (!back.longs.contentEquals(value.longs)) return \"FAIL: longs\"\n\
\x20   if (!back.floats.contentEquals(value.floats)) return \"FAIL: floats\"\n\
\x20   if (!back.doubles.contentEquals(value.doubles)) return \"FAIL: doubles\"\n\
\x20   if (!back.chars.contentEquals(value.chars)) return \"FAIL: chars\"\n\
\x20   if (!back.booleans.contentEquals(value.booleans)) return \"FAIL: booleans\"\n\
\x20   return \"OK\"\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "primitive_array_properties"), "OK");
}

/// The Maestro shape: a nullable defaulted property beside a `ByteArray`, and a nullable array.
#[test]
fn a_byte_array_beside_a_defaulted_property_and_a_nullable_array_round_trip() {
    const MAIN: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Request(val assertion: String? = null, val screen: ByteArray, val mask: IntArray? = null)\n\
\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Request.serializer(), Request(screen = byteArrayOf(7, 8)))\n\
\x20   if (json != \"{\\\"screen\\\":[7,8]}\") return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Request.serializer(), \"{\\\"screen\\\":[9],\\\"mask\\\":[1,2]}\")\n\
\x20   if (back.assertion != null || !back.screen.contentEquals(byteArrayOf(9))) return \"FAIL: screen\"\n\
\x20   return if (back.mask?.contentEquals(intArrayOf(1, 2)) == true) \"OK\" else \"FAIL: mask\"\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "byte_array_request"), "OK");
}

/// A primitive array reached as a collection element.
#[test]
fn a_primitive_array_collection_element_serializes_through_its_builtin() {
    const MAIN: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
class Frames(val frames: List<ByteArray>)\n\
\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Frames.serializer(), Frames(listOf(byteArrayOf(1), byteArrayOf())))\n\
\x20   if (json != \"{\\\"frames\\\":[[1],[]]}\") return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Frames.serializer(), json)\n\
\x20   return if (back.frames.size == 2 && back.frames[0].contentEquals(byteArrayOf(1))) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "byte_array_element"), "OK");
}
