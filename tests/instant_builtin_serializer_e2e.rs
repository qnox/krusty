//! `kotlin.time.Instant` has a built-in element serializer.
//!
//! ```kotlin
//! @Serializable
//! data class Stamped(val name: String, val at: Instant)
//! ```
//!
//! krusty refused the file: `krusty: this construct is not yet supported by the IR backend`. The
//! builtin table maps the primitives, `String` and `kotlin.uuid.Uuid`, but not `kotlin.time.Instant`,
//! so no element serializer could be derived and the plugin left its `serialize-body` placeholder —
//! which fails the whole FILE.
//!
//! The runtime ships `kotlinx/serialization/internal/InstantSerializer`, exactly parallel to the
//! `UuidSerializer` entry already in the table.

use super::serialization_test_support::both_compilers_box;

/// The failing shape: an `Instant`-typed property of a `@Serializable` class.
#[test]
fn an_instant_property_serializes_through_the_builtin() {
    const MAIN: &str = "import kotlin.time.ExperimentalTime\n\
import kotlin.time.Instant\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@OptIn(ExperimentalTime::class)\n\
@Serializable\n\
data class Stamped(val name: String, val at: Instant)\n\
\n\
@OptIn(ExperimentalTime::class)\n\
fun box(): String {\n\
\x20   val value = Stamped(\"x\", Instant.fromEpochSeconds(0))\n\
\x20   val json = Json.encodeToString(Stamped.serializer(), value)\n\
\x20   if (json != \"{\\\"name\\\":\\\"x\\\",\\\"at\\\":\\\"1970-01-01T00:00:00Z\\\"}\") return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Stamped.serializer(), json)\n\
\x20   return if (back == value) \"OK\" else \"FAIL: round trip \" + back\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "instant_property"), "OK");
}

/// The same type reached through a collection ELEMENT, which is the path the builtin table serves.
#[test]
fn an_instant_collection_element_serializes_through_the_builtin() {
    const MAIN: &str = "import kotlin.time.ExperimentalTime\n\
import kotlin.time.Instant\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@OptIn(ExperimentalTime::class)\n\
@Serializable\n\
data class Timeline(val stamps: List<Instant>)\n\
\n\
@OptIn(ExperimentalTime::class)\n\
fun box(): String {\n\
\x20   val value = Timeline(listOf(Instant.fromEpochSeconds(0)))\n\
\x20   val json = Json.encodeToString(Timeline.serializer(), value)\n\
\x20   if (json != \"{\\\"stamps\\\":[\\\"1970-01-01T00:00:00Z\\\"]}\") return \"FAIL: \" + json\n\
\x20   val back = Json.decodeFromString(Timeline.serializer(), json)\n\
\x20   return if (back == value) \"OK\" else \"FAIL: round trip \" + back\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "instant_element"), "OK");
}

/// The control: `Uuid`, the entry this one is modelled on, still works.
#[test]
fn the_uuid_builtin_still_works() {
    const MAIN: &str = "import kotlin.uuid.ExperimentalUuidApi\n\
import kotlin.uuid.Uuid\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@OptIn(ExperimentalUuidApi::class)\n\
@Serializable\n\
data class Keyed(val id: Uuid)\n\
\n\
@OptIn(ExperimentalUuidApi::class)\n\
fun box(): String {\n\
\x20   val value = Keyed(Uuid.parse(\"00000000-0000-0000-0000-000000000000\"))\n\
\x20   val json = Json.encodeToString(Keyed.serializer(), value)\n\
\x20   return if (json == \"{\\\"id\\\":\\\"00000000-0000-0000-0000-000000000000\\\"}\") \"OK\" else \"FAIL: \" + json\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "uuid_builtin"), "OK");
}
