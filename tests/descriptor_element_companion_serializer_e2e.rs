//! `element<T>()` resolves T's serializer the way kotlinc's `serializer<T>()` intrinsic does.
//!
//! The intrinsic takes a fast path first: when T's companion declares the
//! `serializer(KSerializer…)` accessor, it calls that accessor — `Reading.Companion.serializer()` —
//! whatever serializer the class's own `@Serializable(with = …)` names. Only a type without one
//! falls to the general lookup (`StringSerializer.INSTANCE`, a constructed `ArrayListSerializer`).
//!
//! krusty knew neither half for a dependency's sealed class with an `object` custom serializer:
//! the type was never offered to the classifier-fact lookup, so the planned `element` call stayed
//! an unrealized plugin operation and the whole module was refused with
//!
//! ```text
//! error: krusty: this construct is not yet supported by the IR backend
//! ```
//!
//! `JsonElement` is the library type that shape was first seen on; `dep.Reading` is a
//! repository-owned copy of it compiled as a dependency, so the rule does not rest on one jar.

use super::common::{compare_with_kotlinc_plugin, kotlinc_lib_dir, method_instructions};
use super::serialization_test_support::{
    both_compilers_box_against_dependency, reference_dependency, runtime_jars,
};

const DEPENDENCY: &str = "package dep\n\
\n\
import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.descriptors.PrimitiveKind\n\
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
\n\
@Serializable(with = ReadingSerializer::class)\n\
sealed class Reading {\n\
\x20   data class Number(val value: Int) : Reading()\n\
\x20   data class Text(val value: String) : Reading()\n\
}\n\
\n\
object ReadingSerializer : KSerializer<Reading> {\n\
\x20   override val descriptor: SerialDescriptor =\n\
\x20       PrimitiveSerialDescriptor(\"dep.Reading\", PrimitiveKind.STRING)\n\
\x20   override fun serialize(encoder: Encoder, value: Reading) = encoder.encodeString(\n\
\x20       when (value) {\n\
\x20           is Reading.Number -> value.value.toString()\n\
\x20           is Reading.Text -> value.value\n\
\x20       }\n\
\x20   )\n\
\x20   override fun deserialize(decoder: Decoder): Reading {\n\
\x20       val text = decoder.decodeString()\n\
\x20       return text.toIntOrNull()?.let { Reading.Number(it) } ?: Reading.Text(text)\n\
\x20   }\n\
}\n";

const CONSUMER: &str = "import dep.Reading\n\
import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.descriptors.SerialDescriptor\n\
import kotlinx.serialization.descriptors.buildClassSerialDescriptor\n\
import kotlinx.serialization.descriptors.element\n\
import kotlinx.serialization.encoding.Decoder\n\
import kotlinx.serialization.encoding.Encoder\n\
import kotlinx.serialization.json.JsonElement\n\
\n\
object EnvelopeSerializer : KSerializer<String> {\n\
\x20   override val descriptor: SerialDescriptor = buildClassSerialDescriptor(\"Envelope\") {\n\
\x20       element<String>(\"type\")\n\
\x20       element<Reading>(\"reading\")\n\
\x20       element<Reading?>(\"maybe\")\n\
\x20       element<JsonElement>(\"value\")\n\
\x20       element<List<Reading>>(\"readings\")\n\
\x20   }\n\
\x20   override fun serialize(encoder: Encoder, value: String) = encoder.encodeString(value)\n\
\x20   override fun deserialize(decoder: Decoder): String = decoder.decodeString()\n\
}\n";

/// The descriptor each element resolved to, under both compilers against the same dependency.
#[test]
fn a_library_class_with_an_object_serializer_is_a_descriptor_element() {
    let src = format!(
        "{CONSUMER}\n\
         fun box(): String {{\n\
         \x20   val d = EnvelopeSerializer.descriptor\n\
         \x20   return (0 until d.elementsCount).joinToString {{ i ->\n\
         \x20       val e = d.getElementDescriptor(i)\n\
         \x20       d.getElementName(i) + \":\" + e.serialName + \":\" + e.isNullable\n\
         \x20   }}\n\
         }}\n"
    );
    let outcome = both_compilers_box_against_dependency(DEPENDENCY, &src, "library_object_element");
    assert_eq!(
        outcome,
        "type:kotlin.String:false, reading:dep.Reading:false, maybe:dep.Reading?:true, \
         value:kotlinx.serialization.json.JsonElement:false, \
         readings:kotlin.collections.ArrayList:false"
    );
}

/// The serializer each element builds, in order, as owner/member rows: every field read, call and
/// construction except the defaulted `emptyList()` (whose position relative to the serializer
/// belongs to how the inlined `element` body evaluates its defaults, not to the serializer lookup),
/// the receiver's null check and the lambda's `Unit` result.
fn serializer_rows(disassembly: &str) -> Vec<String> {
    method_instructions(disassembly, "descriptor$lambda$0(")
        .into_iter()
        .filter_map(|row| row.split_once(": ").map(|(_, code)| code.to_string()))
        .filter(|code| {
            [
                "getstatic",
                "invokevirtual",
                "invokespecial",
                "invokestatic",
                "invokeinterface",
                "new",
                "checkcast",
            ]
            .iter()
            .any(|opcode| code.starts_with(opcode))
                && !code.contains("CollectionsKt.emptyList")
                && !code.contains("Intrinsics.checkNotNullParameter")
                && !code.contains("kotlin/Unit")
        })
        .collect()
}

/// Each element's serializer is kotlinc's, row for row: the companion accessor for both library
/// classes (bare, nullable, and as a list's argument), never the `with =` object directly.
#[test]
fn a_library_class_element_reads_its_companion_accessor_like_kotlinc() {
    let plugin = kotlinc_lib_dir()
        .expect("the reference compiler must be provisioned")
        .join("kotlinx-serialization-compiler-plugin.jar");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let mut classpath = vec![reference_dependency(
        DEPENDENCY,
        "library_object_element_bytes",
    )];
    classpath.extend(runtime_jars());
    let built = compare_with_kotlinc_plugin(
        "LibraryObjectElement",
        CONSUMER,
        "EnvelopeSerializer",
        &classpath,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    let want = serializer_rows(&built.reference);
    assert!(
        want.iter()
            .any(|row| row.contains("dep/Reading$Companion.serializer"))
            && want
                .iter()
                .any(|row| row.contains("JsonElement$Companion.serializer")),
        "kotlinc contract: the companion accessor serves each library element:\n{}",
        built.reference
    );
    assert_eq!(
        serializer_rows(&built.krusty),
        want,
        "descriptor element serializers:\n{}",
        built.krusty
    );
}
