//! A GENERIC class in a sibling file that names a class-valued custom serializer.
//!
//! ```kotlin
//! // Slot.kt
//! @Serializable(with = SlotSerializer::class)
//! sealed class Slot<out T> { … }
//! class SlotSerializer<T>(private val inner: KSerializer<T>) : KSerializer<Slot<T>>
//!
//! // Holder.kt
//! @Serializable
//! data class Holder(val field: Slot<String>)
//! ```
//!
//! Three conditions are each required; change any one and it already compiled:
//! the annotated class is NOT in this file, it HAS type arguments, and its serializer is a CLASS
//! taking one `KSerializer` per type parameter rather than an `object`.
//!
//! The same-file halves were fixed earlier, and the external arm covered only the shape with no type
//! arguments. Nothing derived this one, so the placeholder reached emit and a residual plugin node
//! fails the whole FILE:
//!
//! ```text
//! error: krusty: this construct is not yet supported by the IR backend
//! ```
use super::serialization_test_support::both_compilers_box_files;

const SLOT: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
\n\
@Serializable(with = SlotSerializer::class)\n\
sealed class Slot<out T> {\n\
\x20   data class Held<T>(val value: T) : Slot<T>()\n\
}\n\
\n\
class SlotSerializer<T>(\n\
\x20   private val inner: KSerializer<T>,\n\
) : KSerializer<Slot<T>> {\n\
\x20   override val descriptor: SerialDescriptor = inner.descriptor\n\
\n\
\x20   override fun serialize(encoder: Encoder, value: Slot<T>) {\n\
\x20       encoder.encodeSerializableValue(inner, (value as Slot.Held<T>).value)\n\
\x20   }\n\
\n\
\x20   override fun deserialize(decoder: Decoder): Slot<T> =\n\
\x20       Slot.Held(decoder.decodeSerializableValue(inner))\n\
}\n";

/// The failing shape: the generic annotated class is a DIRECT property type.
#[test]
fn a_sibling_generic_custom_serializer_serves_a_direct_property() {
    let main = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Holder(val field: Slot<String>)\n\
\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(Slot.Held(\"deep\")))\n\
\x20   return if (json == \"{\\\"field\\\":\\\"deep\\\"}\") \"OK\" else \"FAIL: \" + json\n\
}\n";
    assert_eq!(
        both_compilers_box_files(&[("Slot.kt", SLOT), ("Main.kt", main)], "sibling_generic_direct"),
        "OK"
    );
}

/// The corpus shape: the generic annotated class reached through a MAP value.
#[test]
fn a_sibling_generic_custom_serializer_serves_a_map_value() {
    let main = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Holder(val fields: Map<String, Slot<String>>)\n\
\n\
fun box(): String {\n\
\x20   val json =\n\
\x20       Json.encodeToString(Holder.serializer(), Holder(mapOf(\"k\" to Slot.Held(\"deep\"))))\n\
\x20   return if (json == \"{\\\"fields\\\":{\\\"k\\\":\\\"deep\\\"}}\") \"OK\" else \"FAIL: \" + json\n\
}\n";
    assert_eq!(
        both_compilers_box_files(
            &[("Slot.kt", SLOT), ("Main.kt", main)],
            "sibling_generic_map_value"
        ),
        "OK"
    );
}

const PLAIN: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
\n\
@Serializable(with = PlainSerializer::class)\n\
class Plain(val raw: String)\n\
\n\
object PlainSerializer : KSerializer<Plain> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"Plain\", PrimitiveKind.STRING)\n\
\n\
\x20   override fun serialize(encoder: Encoder, value: Plain) {\n\
\x20       encoder.encodeString(value.raw)\n\
\x20   }\n\
\n\
\x20   override fun deserialize(decoder: Decoder): Plain = Plain(decoder.decodeString())\n\
}\n";

/// The control that isolates the type-argument condition: the same sibling split with a NON-generic
/// annotated class and an `object` serializer already worked and must keep working.
#[test]
fn a_sibling_non_generic_custom_serializer_still_resolves() {
    let main = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Holder(val field: Plain)\n\
\n\
fun box(): String {\n\
\x20   val json = Json.encodeToString(Holder.serializer(), Holder(Plain(\"deep\")))\n\
\x20   return if (json == \"{\\\"field\\\":\\\"deep\\\"}\") \"OK\" else \"FAIL: \" + json\n\
}\n";
    assert_eq!(
        both_compilers_box_files(
            &[("Plain.kt", PLAIN), ("Main.kt", main)],
            "sibling_non_generic"
        ),
        "OK"
    );
}
