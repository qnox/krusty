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
