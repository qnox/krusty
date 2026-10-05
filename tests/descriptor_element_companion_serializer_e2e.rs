//! `element<T>()` resolves T's serializer the way kotlinc's `serializer<T>()` intrinsic does.
//!
//! The intrinsic takes a fast path first: when T's companion declares the
//! `serializer(KSerializer…)` accessor, it calls that accessor — `Reading.Companion.serializer()` —
//! whatever serializer the class's own `@Serializable(with = …)` names. Only a type without one
//! falls to the general lookup (`StringSerializer.INSTANCE`, a constructed `ArrayListSerializer`).
//!
//! krusty derived the element's serializer in the backend through the property-element lookup
//! instead, which never saw a dependency's sealed class named only by `element`'s type argument, so
//! the planned `element` call stayed an unrealized plugin operation and the whole module was
//! refused with
//!
//! ```text
//! error: krusty: this construct is not yet supported by the IR backend
//! ```
//!
//! `JsonElement` is the library type that shape was first seen on; `dep.Reading` is a
//! repository-owned copy of it compiled as a dependency, so the rule does not rest on one jar.
//!
//! The accessor is selected in the frontend by its full declared signature,
//! `serializer(KSerializer<T1>, …): KSerializer<C<T1, …>>`, and lowered as that exact declaration.
//! A same-name, same-arity companion function whose `KSerializer` type arguments differ is a
//! different function and is never called in its place.

use super::common::{
    backend_outcome_in_process, compare_with_kotlinc_plugin, kotlinc_lib_dir, method_instructions,
    BackendOutcome,
};
use super::serialization_test_support::{
    both_compilers_box_against_dependency, both_compilers_box_files, reference_dependency,
    runtime_jars,
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

/// A dependency whose companions declare a `serializer` with the accessor's name, arity and
/// `KSerializer` outer types, but serving another type: `KSerializer<String>` for `Lookalike`, and
/// `KSerializer<List<X>>` for `Pocket<T>`. Neither class is `@Serializable`, so neither has an
/// accessor at all.
const LOOKALIKE_DEPENDENCY: &str = "package dep\n\
\n\
import kotlinx.serialization.KSerializer\n\
import kotlinx.serialization.builtins.ListSerializer\n\
import kotlinx.serialization.builtins.serializer\n\
\n\
class Lookalike {\n\
\x20   companion object {\n\
\x20       fun serializer(): KSerializer<String> = String.serializer()\n\
\x20   }\n\
}\n\
\n\
class Pocket<T> {\n\
\x20   companion object {\n\
\x20       fun <X> serializer(element: KSerializer<X>): KSerializer<List<X>> =\n\
\x20           ListSerializer(element)\n\
\x20   }\n\
}\n";

/// The lookalike is never selected as the accessor. With no accessor and no serializer of its own,
/// the element's serializer is underivable, and krusty refuses the module rather than calling a
/// function that serves another type.
#[test]
fn a_lookalike_companion_serializer_is_not_the_accessor() {
    let dependency = reference_dependency(LOOKALIKE_DEPENDENCY, "lookalike_accessor");
    let mut classpath = vec![dependency];
    classpath.extend(runtime_jars());
    for (stem, element) in [
        ("lookalike_bare", "dep.Lookalike"),
        ("lookalike_generic", "dep.Pocket<String>"),
    ] {
        let src = format!(
            "import kotlinx.serialization.Serializable\n\
             import kotlinx.serialization.descriptors.buildClassSerialDescriptor\n\
             import kotlinx.serialization.descriptors.element\n\
             @Serializable data class Anchor(val id: Int)\n\
             val descriptor = buildClassSerialDescriptor(\"Probe\") {{\n\
             \x20   element<{element}>(\"probe\")\n\
             }}\n"
        );
        assert_eq!(
            backend_outcome_in_process(&src, stem, &classpath, None),
            Some(BackendOutcome::Rejected(vec![
                "krusty: this construct is not yet supported by the IR backend".to_owned()
            ])),
            "{element}: a lookalike companion `serializer` must not be selected"
        );
    }
}

/// The accessor of a classifier ANOTHER FILE of the module declares is the plugin-generated
/// member of its companion: the call names that module declaration, as for a same-file class.
#[test]
fn a_sibling_file_class_element_reads_its_companion_accessor() {
    let declarations = "import kotlinx.serialization.KSerializer\n\
        import kotlinx.serialization.Serializable\n\
        import kotlinx.serialization.builtins.serializer\n\
        import kotlinx.serialization.descriptors.PrimitiveKind\n\
        import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
        import kotlinx.serialization.encoding.Decoder\n\
        import kotlinx.serialization.encoding.Encoder\n\
        \n\
        @Serializable data class Leaf(val id: Int)\n\
        @Serializable sealed class Branch { @Serializable data class Twig(val n: Int) : Branch() }\n\
        @Serializable(with = MarkSerializer::class) class Mark(val text: String)\n\
        object MarkSerializer : KSerializer<Mark> {\n\
        \x20   override val descriptor = PrimitiveSerialDescriptor(\"Mark\", PrimitiveKind.STRING)\n\
        \x20   override fun serialize(encoder: Encoder, value: Mark) = encoder.encodeString(value.text)\n\
        \x20   override fun deserialize(decoder: Decoder) = Mark(decoder.decodeString())\n\
        }\n";
    let main = "import kotlinx.serialization.descriptors.buildClassSerialDescriptor\n\
        import kotlinx.serialization.descriptors.element\n\
        \n\
        val descriptor = buildClassSerialDescriptor(\"Holder\") {\n\
        \x20   element<Leaf>(\"leaf\")\n\
        \x20   element<Branch?>(\"branch\")\n\
        \x20   element<List<Mark>>(\"marks\")\n\
        }\n\
        \n\
        fun box(): String = (0 until descriptor.elementsCount).joinToString { i ->\n\
        \x20   val e = descriptor.getElementDescriptor(i)\n\
        \x20   descriptor.getElementName(i) + \":\" + e.serialName + \":\" + e.isNullable\n\
        }\n";
    let outcome = both_compilers_box_files(
        &[("Declarations.kt", declarations), ("Main.kt", main)],
        "sibling_class_element",
    );
    assert_eq!(
        outcome,
        "leaf:Leaf:false, branch:Branch?:true, marks:kotlin.collections.ArrayList:false"
    );
}

/// Candidate collection for the synthetic call follows the ordinary member hierarchy. The
/// declaration owner may therefore be a base of the companion while the companion remains the
/// runtime dispatch receiver.
#[test]
fn an_inherited_companion_serializer_accessor_is_selected() {
    let dependency = "package dep\n\
        import kotlinx.serialization.KSerializer\n\
        import kotlinx.serialization.descriptors.PrimitiveKind\n\
        import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
        import kotlinx.serialization.encoding.Decoder\n\
        import kotlinx.serialization.encoding.Encoder\n\
        class Inherited { companion object : Accessors() }\n\
        open class Accessors { fun serializer(): KSerializer<Inherited> = InheritedSerializer }\n\
        object InheritedSerializer : KSerializer<Inherited> {\n\
        \x20   override val descriptor = PrimitiveSerialDescriptor(\"dep.Inherited\", PrimitiveKind.STRING)\n\
        \x20   override fun serialize(encoder: Encoder, value: Inherited) = encoder.encodeString(\"v\")\n\
        \x20   override fun deserialize(decoder: Decoder) = Inherited()\n\
        }\n";
    let consumer = "import dep.Inherited\n\
        import kotlinx.serialization.descriptors.buildClassSerialDescriptor\n\
        import kotlinx.serialization.descriptors.element\n\
        val descriptor = buildClassSerialDescriptor(\"Holder\") { element<Inherited>(\"value\") }\n\
        fun box(): String = descriptor.getElementDescriptor(0).serialName\n";
    assert_eq!(
        both_compilers_box_against_dependency(
            dependency,
            consumer,
            "inherited_companion_serializer_accessor",
        ),
        "dep.Inherited"
    );
}

/// An exact-shape declaration is still not a callable candidate when it is inaccessible at the
/// element call site. The plugin must fall back instead of smuggling the private member into FIR.
#[test]
fn an_inaccessible_companion_serializer_accessor_is_not_selected() {
    let dependency = "package dep\n\
        import kotlinx.serialization.KSerializer\n\
        import kotlinx.serialization.descriptors.PrimitiveKind\n\
        import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n\
        import kotlinx.serialization.encoding.Decoder\n\
        import kotlinx.serialization.encoding.Encoder\n\
        class Hidden { companion object { private fun serializer(): KSerializer<Hidden> = HiddenSerializer } }\n\
        object HiddenSerializer : KSerializer<Hidden> {\n\
        \x20   override val descriptor = PrimitiveSerialDescriptor(\"dep.Hidden\", PrimitiveKind.STRING)\n\
        \x20   override fun serialize(encoder: Encoder, value: Hidden) = encoder.encodeString(\"v\")\n\
        \x20   override fun deserialize(decoder: Decoder) = Hidden()\n\
        }\n";
    let dependency = reference_dependency(dependency, "private_serializer_accessor");
    let mut classpath = vec![dependency];
    classpath.extend(runtime_jars());
    let consumer = "import dep.Hidden\n\
        import kotlinx.serialization.descriptors.buildClassSerialDescriptor\n\
        import kotlinx.serialization.descriptors.element\n\
        val descriptor = buildClassSerialDescriptor(\"Holder\") { element<Hidden>(\"value\") }\n";
    assert_eq!(
        backend_outcome_in_process(
            consumer,
            "inaccessible_companion_serializer_accessor",
            &classpath,
            None,
        ),
        Some(BackendOutcome::Rejected(vec![
            "krusty: this construct is not yet supported by the IR backend".to_owned()
        ])),
    );
}
