//! A property's own `@Serializable(with = X::class)` DECODES through `X`, as it encodes through it.
//!
//! `serialize` and `childSerializers` both consult the property's explicit serializer ahead of its
//! type. `deserialize` did not, which failed two ways:
//!
//! * a property whose type has NO derivable serializer (a plain class) made the whole `deserialize`
//!   a throwing stub — the type could not be decoded, so nothing was — although `X` was right there;
//! * a property whose type HAS one (a `String`) decoded through the type's builtin, so a value `X`
//!   wrote as an `Int` was read back as a `String` and the round trip broke.

use super::serialization_test_support::both_compilers_box;

const MAIN: &str = "import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
import kotlinx.serialization.json.Json\n\
\n\
class Opaque(val raw: String)\n\
\n\
object OpaqueSerializer : KSerializer<Opaque> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"Opaque\", PrimitiveKind.STRING)\n\
\x20   override fun serialize(encoder: Encoder, value: Opaque) {\n\
\x20       encoder.encodeString(value.raw)\n\
\x20   }\n\
\x20   override fun deserialize(decoder: Decoder): Opaque = Opaque(decoder.decodeString())\n\
}\n\
\n\
object LengthSerializer : KSerializer<String> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"Length\", PrimitiveKind.INT)\n\
\x20   override fun serialize(encoder: Encoder, value: String) {\n\
\x20       encoder.encodeInt(value.length)\n\
\x20   }\n\
\x20   override fun deserialize(decoder: Decoder): String = \"x\".repeat(decoder.decodeInt())\n\
}\n\
\n\
@Serializable\n\
class Holder(\n\
\x20   @Serializable(with = OpaqueSerializer::class) val opaque: Opaque,\n\
\x20   @Serializable(with = OpaqueSerializer::class) val maybe: Opaque?,\n\
\x20   @Serializable(with = LengthSerializer::class) val length: String,\n\
)\n\
\n\
fun box(): String {\n\
\x20   val full = Json.encodeToString(Holder.serializer(), Holder(Opaque(\"a\"), Opaque(\"b\"), \"abc\"))\n\
\x20   if (full != \"{\\\"opaque\\\":\\\"a\\\",\\\"maybe\\\":\\\"b\\\",\\\"length\\\":3}\") return \"FAIL encode: \" + full\n\
\x20   val back = Json.decodeFromString(Holder.serializer(), full)\n\
\x20   if (back.opaque.raw != \"a\" || back.maybe?.raw != \"b\" || back.length != \"xxx\") return \"FAIL decode\"\n\
\x20   val absent = Json.decodeFromString(Holder.serializer(), \"{\\\"opaque\\\":\\\"c\\\",\\\"maybe\\\":null,\\\"length\\\":1}\")\n\
\x20   if (absent.opaque.raw != \"c\" || absent.maybe != null || absent.length != \"x\") return \"FAIL null\"\n\
\x20   return \"OK\"\n\
}\n";

#[test]
fn a_property_serializer_decodes_what_it_encoded() {
    assert_eq!(both_compilers_box(MAIN, "property_serializer_decode"), "OK");
}
