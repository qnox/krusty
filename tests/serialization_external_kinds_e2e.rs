//! A `@Serializable` classifier declared outside the file is reached the way its own compilation
//! generated its serializer, which depends on its kind.
//!
//! Only a plain non-generic class has a `$serializer` object. An object has no serializer class at
//! all: kotlinc constructs `ObjectSerializer(serialName, INSTANCE, …)` where it is used. An enum, a
//! sealed or abstract class, an interface and a generic class publish their serializer through the
//! companion's generated `serializer(…)`, called with one argument serializer per type parameter.
//! krusty assumed a `$serializer` for every one of them, so an element of a dependency's enum,
//! sealed class or object failed with `NoClassDefFoundError: …$$serializer`, and one of a
//! dependency's generic class was rejected as unsupported.

use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, member_body, plugin_and_runtime,
};
use super::serialization_test_support::{
    both_compilers_box_against_dependency, both_compilers_box_files, reference_dependency,
};

const DEPENDENCY: &str = "package dep\n\
\n\
import kotlinx.serialization.SerialName\n\
import kotlinx.serialization.Serializable\n\
\n\
@Serializable enum class Tone { SOFT, @SerialName(\"loud\") LOUD }\n\
\n\
@Serializable\n\
sealed class Step {\n\
\x20   @Serializable @SerialName(\"walk\") data class Walk(val n: Int) : Step()\n\
\x20   @Serializable @SerialName(\"halt\") object Halt : Step()\n\
}\n\
\n\
@Serializable sealed interface Shape\n\
@Serializable @SerialName(\"dot\") data class Dot(val r: Int) : Shape\n\
\n\
@Serializable abstract class Base { abstract val id: Int }\n\
\n\
@Serializable object Anchor\n\
@Serializable @SerialName(\"custom.pin\") object Pin\n\
\n\
@Serializable data class Tagged<T>(val tag: String, val value: T)\n\
@Serializable data class Duo<A, B>(val first: A, val second: B?)\n\
@Serializable data class Plain(val id: Int)\n\
@Serializable class Outer { @Serializable object Inner }\n";

const CONSUMER: &str = "import dep.*\n\
import kotlinx.serialization.Serializable\n\
\n\
@Serializable\n\
data class Holder(\n\
\x20   val tones: Map<Tone, Int>,\n\
\x20   val tone: Tone,\n\
\x20   val steps: List<Step>,\n\
\x20   val shape: Shape,\n\
\x20   val anchor: Anchor,\n\
\x20   val pin: Pin?,\n\
\x20   val tagged: Tagged<Plain>,\n\
\x20   val nested: Tagged<Duo<Tone, Anchor>>,\n\
\x20   val plain: Plain,\n\
)\n\
\n\
@Serializable\n\
data class Polymorphic(val base: Base, val names: Tagged<String?>, val inner: Outer.Inner)\n";

/// Each kind round-trips under both compilers against a dependency kotlinc built.
#[test]
fn a_dependency_classifier_serializes_through_what_its_kind_generated() {
    let src = format!(
        "{CONSUMER}\n\
         fun box(): String {{\n\
         \x20   val json = kotlinx.serialization.json.Json\n\
         \x20   val holder = Holder(\n\
         \x20       mapOf(Tone.LOUD to 1), Tone.SOFT, listOf(Step.Walk(2), Step.Halt), Dot(3),\n\
         \x20       Anchor, Pin, Tagged(\"t\", Plain(4)), Tagged(\"n\", Duo(Tone.LOUD, null)), Plain(5),\n\
         \x20   )\n\
         \x20   val text = json.encodeToString(Holder.serializer(), holder)\n\
         \x20   val back = json.decodeFromString(Holder.serializer(), text)\n\
         \x20   return listOf(text, back == holder, back.anchor === Anchor).joinToString(\" | \")\n\
         }}\n"
    );
    let outcome = both_compilers_box_against_dependency(DEPENDENCY, &src, "external_kinds");
    assert_eq!(
        outcome,
        "{\"tones\":{\"loud\":1},\"tone\":\"SOFT\",\
         \"steps\":[{\"type\":\"walk\",\"n\":2},{\"type\":\"halt\"}],\
         \"shape\":{\"type\":\"dot\",\"r\":3},\"anchor\":{},\"pin\":{},\
         \"tagged\":{\"tag\":\"t\",\"value\":{\"id\":4}},\
         \"nested\":{\"tag\":\"n\",\"value\":{\"first\":\"loud\",\"second\":null}},\
         \"plain\":{\"id\":5}} | true | true"
    );
}

/// The same kinds declared in another file of the module this one is compiled with.
#[test]
fn a_sibling_file_classifier_serializes_through_what_its_kind_generated() {
    let declarations = "import kotlinx.serialization.SerialName\n\
        import kotlinx.serialization.Serializable\n\
        \n\
        @Serializable enum class Tone { SOFT, @SerialName(\"loud\") LOUD }\n\
        @Serializable sealed class Step {\n\
        \x20   @Serializable data class Walk(val n: Int) : Step()\n\
        }\n\
        @Serializable sealed interface Shape\n\
        @Serializable data class Dot(val r: Int) : Shape\n\
        @Serializable object Anchor\n\
        @Serializable @SerialName(\"custom.pin\") object Pin\n\
        @Serializable data class Tagged<T>(val tag: String, val value: T)\n";
    let consumer = "import kotlinx.serialization.Serializable\n\
        import kotlinx.serialization.json.Json\n\
        \n\
        @Serializable\n\
        data class Holder(\n\
        \x20   val tones: Map<Tone, Int>,\n\
        \x20   val steps: List<Step>,\n\
        \x20   val shape: Shape,\n\
        \x20   val anchor: Anchor,\n\
        \x20   val pin: Pin,\n\
        \x20   val tagged: Tagged<Tone>,\n\
        )\n\
        \n\
        fun box(): String {\n\
        \x20   val holder = Holder(mapOf(Tone.LOUD to 1), listOf(Step.Walk(2)), Dot(3), Anchor, Pin,\n\
        \x20       Tagged(\"t\", Tone.SOFT))\n\
        \x20   val text = Json.encodeToString(Holder.serializer(), holder)\n\
        \x20   return listOf(text, Json.decodeFromString(Holder.serializer(), text) == holder)\n\
        \x20       .joinToString(\" | \")\n\
        }\n";
    let outcome = both_compilers_box_files(
        &[("Declarations.kt", declarations), ("Main.kt", consumer)],
        "sibling_kinds",
    );
    assert_eq!(
        outcome,
        "{\"tones\":{\"loud\":1},\"steps\":[{\"type\":\"Step.Walk\",\"n\":2}],\
         \"shape\":{\"type\":\"Dot\",\"r\":3},\"anchor\":{},\"pin\":{},\
         \"tagged\":{\"tag\":\"t\",\"value\":\"SOFT\"}} | true"
    );
}

/// Each element's cached serializer is the one kotlinc builds: the companion call with its
/// argument serializers narrowed to `KSerializer`, the in-place `ObjectSerializer` with the
/// object's serial name, while a plain class's `$serializer` singleton stays uncached.
#[test]
fn a_dependency_element_is_built_the_way_kotlinc_builds_it() {
    let Some((plugin, runtime)) = plugin_and_runtime() else {
        eprintln!("skipping: serialization plugin or runtime jar not available locally");
        return;
    };
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let mut classpath = vec![reference_dependency(DEPENDENCY, "external_kinds_bytes")];
    classpath.extend(runtime);
    for (class, members) in [
        (
            "Holder",
            &[
                "_childSerializers$_anonymous_()",
                "_childSerializers$_anonymous_$0()",
                "_childSerializers$_anonymous_$1()",
                "_childSerializers$_anonymous_$2()",
                "_childSerializers$_anonymous_$3()",
                "_childSerializers$_anonymous_$4()",
                "_childSerializers$_anonymous_$5()",
                "_childSerializers$_anonymous_$6()",
                "static {}",
            ][..],
        ),
        (
            "Polymorphic",
            &[
                "_childSerializers$_anonymous_()",
                "_childSerializers$_anonymous_$0()",
                "_childSerializers$_anonymous_$1()",
                "static {}",
            ][..],
        ),
    ] {
        let Some(built) =
            compare_with_kotlinc_plugin("Consumer", CONSUMER, class, &classpath, "25", &extra)
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        for member in members {
            let want = member_body(&built.reference, member);
            assert!(
                want.len() > 1,
                "{class}.{member}: the reference declares it"
            );
            assert_eq!(member_body(&built.krusty, member), want, "{class}.{member}");
        }
    }
}
