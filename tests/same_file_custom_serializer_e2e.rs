//! A class-level `@Serializable(with = …)` declared in the SAME file is honored.
//!
//! ```kotlin
//! @Serializable(with = FlexSerializer::class)
//! class Flex(val raw: String)
//!
//! @Serializable
//! data class Holder(val payload: Flex)
//! ```
//!
//! krusty refused the file outright:
//!
//! ```text
//! error: krusty: this construct is not yet supported by the IR backend
//! [lower] JVM emission declined residual IR nodes:
//!   [(…, PluginPlaceholder { plugin: "serialization", kind: "serialize-body", … })]
//! ```
//!
//! The serialization plugin leaves that placeholder rather than emit a half-built serializer, and a
//! residual node fails the whole FILE.
//!
//! The same class declared in a SIBLING file always worked — that asymmetry is the discriminator, and
//! it is the reverse of what one would guess: the sibling case resolves through the external-serializer
//! map, while a same-file class is expected to derive its own `$serializer` and its class-level
//! `with =` was not consulted.

use super::serialization_test_support::both_compilers_box;

const SERIALIZER: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable(with = FlexSerializer::class)\n\
class Flex(val raw: String)\n\
\n\
object FlexSerializer : KSerializer<Flex> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"Flex\", PrimitiveKind.STRING)\n\
\n\
\x20   override fun serialize(encoder: Encoder, value: Flex) {\n\
\x20       encoder.encodeString(value.raw)\n\
\x20   }\n\
\n\
\x20   override fun deserialize(decoder: Decoder): Flex = Flex(decoder.decodeString())\n\
}\n\
\n";

/// The failing shape: the annotated class is a DIRECT property type.
#[test]
fn a_same_file_custom_serializer_serves_a_direct_property() {
    let main = format!(
        "{SERIALIZER}\
@Serializable\n\
data class Holder(val payload: Flex)\n\
fun box(): String {{\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(Flex(\"deep\")))\n\
\x20   return if (json == \"{{\\\"payload\\\":\\\"deep\\\"}}\") \"OK\" else \"FAIL: \" + json\n\
}}\n"
    );
    assert_eq!(both_compilers_box(&main, "same_file_direct"), "OK");
}

/// The corpus shape: the annotated class reached through a collection ELEMENT.
#[test]
fn a_same_file_custom_serializer_serves_a_map_value() {
    let main = format!(
        "{SERIALIZER}\
@Serializable\n\
data class Holder(val fields: Map<String, Flex>)\n\
fun box(): String {{\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(mapOf(\"k\" to Flex(\"deep\"))))\n\
\x20   return if (json == \"{{\\\"fields\\\":{{\\\"k\\\":\\\"deep\\\"}}}}\") \"OK\" else \"FAIL: \" + json\n\
}}\n"
    );
    assert_eq!(both_compilers_box(&main, "same_file_map_value"), "OK");
}

/// The control that isolates the same-file case: a LIST element behaves like the map value.
#[test]
fn a_same_file_custom_serializer_serves_a_list_element() {
    let main = format!(
        "{SERIALIZER}\
@Serializable\n\
data class Holder(val fields: List<Flex>)\n\
fun box(): String {{\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(listOf(Flex(\"deep\"))))\n\
\x20   return if (json == \"{{\\\"fields\\\":[\\\"deep\\\"]}}\") \"OK\" else \"FAIL: \" + json\n\
}}\n"
    );
    assert_eq!(both_compilers_box(&main, "same_file_list_element"), "OK");
}

/// The control that shows nothing about ordinary derivation changed: a `@Serializable` class with no
/// custom serializer still derives its own.
#[test]
fn an_ordinary_serializable_property_still_derives() {
    const MAIN: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Inner(val raw: String)\n\
\n\
@Serializable\n\
data class Holder(val payload: Inner)\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(Inner(\"deep\")))\n\
\x20   return if (json == \"{\\\"payload\\\":{\\\"raw\\\":\\\"deep\\\"}}\") \"OK\" else \"FAIL: \" + json\n\
}\n";
    assert_eq!(both_compilers_box(MAIN, "ordinary_derivation"), "OK");
}

/// A custom serializer declared as a CLASS takes one `KSerializer` per type parameter of the class
/// it serves — `BoxSerializer<T>(itemSerializer)` — so it is CONSTRUCTED with the element's own
/// serializer rather than read as a singleton. Before this worked, the plan declined; before the
/// preceding commit restricted it to objects, it emitted `getstatic BoxSerializer.INSTANCE` and the
/// program died at run time with `NoSuchFieldError`, which is why this test runs the program.
#[test]
fn a_class_valued_custom_serializer_is_constructed_with_its_argument_serializer() {
    const MAIN: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable(with = BoxSerializer::class)\n\
class Box<T>(val item: T)\n\
\n\
class BoxSerializer<T>(private val itemSerializer: KSerializer<T>) : KSerializer<Box<T>> {\n\
\x20   override val descriptor: SerialDescriptor = itemSerializer.descriptor\n\
\n\
\x20   override fun serialize(encoder: Encoder, value: Box<T>) {\n\
\x20       itemSerializer.serialize(encoder, value.item)\n\
\x20   }\n\
\n\
\x20   override fun deserialize(decoder: Decoder): Box<T> = Box(itemSerializer.deserialize(decoder))\n\
}\n\
\n\
@Serializable\n\
data class Holder(val payload: Box<String>)\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(Box(\"deep\")))\n\
\x20   return if (json == \"{\\\"payload\\\":\\\"deep\\\"}\") \"OK\" else \"FAIL: \" + json\n\
}\n";
    assert_eq!(
        both_compilers_box(MAIN, "constructed_custom_serializer"),
        "OK"
    );
}

/// Recursive composition retained from #886: the custom serializer's argument is itself generated,
/// and the constructed serializer is reached both directly and below a map-value serializer.
#[test]
fn a_constructed_custom_serializer_composes_below_a_collection() {
    const MAIN: &str = r#"import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json

@Serializable(with = BoxSerializer::class)
class Box<out T>(val item: T)

class BoxSerializer<T>(private val inner: KSerializer<T>) : KSerializer<Box<T>> {
    override val descriptor: SerialDescriptor = inner.descriptor
    override fun serialize(encoder: Encoder, value: Box<T>) = inner.serialize(encoder, value.item)
    override fun deserialize(decoder: Decoder): Box<T> = Box(inner.deserialize(decoder))
}

@Serializable
data class Leaf(val value: String)

@Serializable
data class Holder(val direct: Box<Leaf>, val mapped: Map<String, Box<Leaf>>)

fun box(): String {
    val value = Holder(Box(Leaf("a")), mapOf("k" to Box(Leaf("b"))))
    val json = Json.encodeToString(Holder.serializer(), value)
    val decoded = Json.decodeFromString(Holder.serializer(), json)
    return if (decoded.direct.item.value + decoded.mapped.getValue("k").item.value == "ab") {
        "OK"
    } else {
        "FAIL: " + json
    }
}
"#;
    assert_eq!(
        both_compilers_box(MAIN, "constructed_custom_serializer_collection"),
        "OK"
    );
}
