//! A class in a sibling file keeps the resolved identity of its class-valued custom serializer.
//!
//! The module provider previously discarded annotation arguments, so the serialization plugin
//! silently selected `<Class>$$serializer` even though a custom serializer means that generated
//! class does not exist. The emitted reference then failed only when the JVM loaded it. The
//! serializer is named through an import alias so the regression also requires ordinary scope and
//! import binding rather than treating the written class-literal spelling as identity.
use super::serialization_test_support::both_compilers_box_files;

const PLAIN: &str = "package model\n\
\n\
import codecs.PlainSerializer as Wire\n\
import kotlinx.serialization.Serializable\n\
\n\
@Serializable(with = Wire::class)\n\
class Plain(val raw: String)\n";

const SERIALIZER: &str = "package codecs\n\
\n\
import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
import model.Plain\n\
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

/// The sibling split is the discriminator: before the fix this emitted a reference to the absent
/// `Plain$$serializer`, then failed with `NoClassDefFoundError` at runtime.
#[test]
fn a_sibling_custom_serializer_keeps_its_resolved_identity() {
    let main = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
import model.Plain\n\
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
            &[
                ("Plain.kt", PLAIN),
                ("PlainSerializer.kt", SERIALIZER),
                ("Main.kt", main),
            ],
            "sibling_non_generic"
        ),
        "OK"
    );
}

const OPTIONAL: &str = "package model\n\
\n\
import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
\n\
@Serializable(with = OptionalSerializer::class)\n\
sealed class Optional<out T> {\n\
\x20   object Absent : Optional<Nothing>()\n\
\n\
\x20   data class Present<T>(val value: T) : Optional<T>()\n\
}\n\
\n\
class OptionalSerializer<T>(private val dataSerializer: KSerializer<T>) : KSerializer<Optional<T>> {\n\
\x20   override val descriptor: SerialDescriptor = dataSerializer.descriptor\n\
\n\
\x20   override fun serialize(encoder: Encoder, value: Optional<T>) {\n\
\x20       when (value) {\n\
\x20           is Optional.Absent -> error(\"absent\")\n\
\x20           is Optional.Present -> encoder.encodeSerializableValue(dataSerializer, value.value)\n\
\x20       }\n\
\x20   }\n\
\n\
\x20   override fun deserialize(decoder: Decoder): Optional<T> =\n\
\x20       Optional.Present(decoder.decodeSerializableValue(dataSerializer))\n\
}\n";

/// A sibling file's class whose `with =` names a serializer CLASS (one `KSerializer` constructor
/// parameter per type parameter) is reached the way kotlinc reaches it: through the served class's
/// generated `Companion.serializer(…)`, both as a collection element and as a direct property.
/// Before the fix such an element was underivable and the whole module was rejected with the
/// generic "this construct is not yet supported by the IR backend" error.
#[test]
fn a_sibling_custom_serializer_class_is_reached_through_the_companion() {
    let main = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
import model.Optional\n\
\n\
@Serializable\n\
data class Patch(val fields: Map<String, Optional<String>>, val single: Optional<Int>)\n\
\n\
fun box(): String {\n\
\x20   val patch = Patch(mapOf(\"a\" to Optional.Present(\"x\")), Optional.Present(3))\n\
\x20   val json = Json.encodeToString(Patch.serializer(), patch)\n\
\x20   val back = Json.decodeFromString(Patch.serializer(), json)\n\
\x20   if (json != \"{\\\"fields\\\":{\\\"a\\\":\\\"x\\\"},\\\"single\\\":3}\") return \"FAIL: \" + json\n\
\x20   return if (back == patch) \"OK\" else \"FAIL: \" + back\n\
}\n";
    assert_eq!(
        both_compilers_box_files(
            &[("Optional.kt", OPTIONAL), ("Main.kt", main)],
            "sibling_generic_class"
        ),
        "OK"
    );
}
