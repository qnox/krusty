//! A generic abstract or sealed serializable class, and a subclass of a generic serializable
//! class, compile like kotlinc compiles them.
//!
//! A generic abstract or sealed `@Serializable` class gets no `$serializer`. Its
//! `Companion.serializer(typeSerial0, …)` builds a new `PolymorphicSerializer` or
//! `SealedClassSerializer` on every call, and nothing is cached. The class keeps a `write$Self`
//! that takes one serializer per type parameter and a deserialization constructor that checks no
//! missing fields. A subclass's elements start with the generic parent's, typed by the subclass's
//! type arguments, and its `write$Self` hands the parent's `write$Self` a serializer for each type
//! argument. A generic sealed class's subclass whose type parameter is the sealed class's type
//! argument is served by the sealed class's own `typeSerial`.

use super::common;
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   abstract class Envelope<T>(val payload: T)\n\
                   @Serializable\n\
                   class Note(val n: Int) : Envelope<String>(\"x\")\n\
                   @Serializable\n\
                   class Batch<U>(val tag: String, val items: List<U>) : Envelope<List<U>>(items)\n\
                   @Serializable\n\
                   sealed class Outcome<T> {\n\
                   \x20   @Serializable\n\
                   \x20   class Ok<T>(val value: T) : Outcome<T>()\n\
                   \x20   @Serializable\n\
                   \x20   class Failed(val reason: String) : Outcome<Int>()\n\
                   \x20   @Serializable\n\
                   \x20   object Pending : Outcome<Int>()\n\
                   }\n\
                   @Serializable\n\
                   class Holder(\n\
                   \x20   val e: Envelope<Int>,\n\
                   \x20   val o: Outcome<String>,\n\
                   \x20   val s: Envelope<String>,\n\
                   \x20   val q: Outcome<Int>,\n\
                   \x20   val l: Envelope<List<Long>>,\n\
                   )\n";

fn assert_byte_identical(class: &str) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin("GenericParents", SRC, class, &cp, "25", &extra)
        .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}

#[test]
fn a_generic_abstract_class_builds_a_polymorphic_serializer_per_call() {
    assert_byte_identical("Envelope");
    assert_byte_identical("Envelope$Companion");
}

#[test]
fn a_subclass_hands_the_generic_parent_a_serializer_per_type_argument() {
    for class in ["Note", "Note$$serializer", "Batch", "Batch$$serializer"] {
        assert_byte_identical(class);
    }
}

#[test]
fn a_generic_sealed_class_serves_its_subclasses_with_its_own_type_serializers() {
    for class in [
        "Outcome",
        "Outcome$Companion",
        "Outcome$Ok",
        "Outcome$Ok$$serializer",
        "Outcome$Failed",
        "Outcome$Failed$$serializer",
    ] {
        assert_byte_identical(class);
    }
}

/// A property typed by a generic abstract class is served by its companion's `serializer` with a
/// `UnitSerializer` per type parameter; one typed by a generic sealed class by its companion's
/// `serializer` with the serializers of the property's type arguments. Both are cached.
#[test]
fn properties_typed_by_generic_polymorphic_classes_use_companion_serializers() {
    for class in ["Holder", "Holder$$serializer"] {
        assert_byte_identical(class);
    }
}

const ROUND_TRIP_SRC: &str = "import kotlinx.serialization.Serializable\n\
                              import kotlinx.serialization.builtins.serializer\n\
                              import kotlinx.serialization.json.Json\n\
                              @Serializable\n\
                              abstract class Envelope<T>(val payload: T)\n\
                              @Serializable\n\
                              class Note(val n: Int) : Envelope<String>(\"x\")\n\
                              @Serializable\n\
                              class Batch<U>(val tag: String, val items: List<U>) : Envelope<List<U>>(items)\n\
                              @Serializable\n\
                              sealed class Outcome<T> {\n\
                              \x20   @Serializable\n\
                              \x20   class Ok<T>(val value: T) : Outcome<T>()\n\
                              \x20   @Serializable\n\
                              \x20   class Failed(val reason: String) : Outcome<Int>()\n\
                              }\n\
                              fun box(): String {\n\
                              \x20   val note = Json.encodeToString(Note.serializer(), Note(1))\n\
                              \x20   if (note != \"{\\\"payload\\\":\\\"x\\\",\\\"n\\\":1}\") return note\n\
                              \x20   val noteBack = Json.decodeFromString(Note.serializer(), note)\n\
                              \x20   if (noteBack.payload != \"x\" || noteBack.n != 1) return \"note\"\n\
                              \x20   val batchSerializer = Batch.serializer(Int.serializer())\n\
                              \x20   val batch = Json.encodeToString(batchSerializer, Batch(\"t\", listOf(1, 2)))\n\
                              \x20   if (batch != \"{\\\"payload\\\":[1,2],\\\"tag\\\":\\\"t\\\",\\\"items\\\":[1,2]}\") return batch\n\
                              \x20   val batchBack = Json.decodeFromString(batchSerializer, batch)\n\
                              \x20   if (batchBack.payload != listOf(1, 2) || batchBack.tag != \"t\") return \"batch\"\n\
                              \x20   val outcomeSerializer = Outcome.serializer(Int.serializer())\n\
                              \x20   val ok = Json.encodeToString(outcomeSerializer, Outcome.Ok(3))\n\
                              \x20   if (ok != \"{\\\"type\\\":\\\"Outcome.Ok\\\",\\\"value\\\":3}\") return ok\n\
                              \x20   val okBack = Json.decodeFromString(outcomeSerializer, ok)\n\
                              \x20   if ((okBack as Outcome.Ok<Int>).value != 3) return \"ok\"\n\
                              \x20   val failed = Json.encodeToString(outcomeSerializer, Outcome.Failed(\"no\"))\n\
                              \x20   val failedBack = Json.decodeFromString(outcomeSerializer, failed)\n\
                              \x20   return if ((failedBack as Outcome.Failed).reason == \"no\") \"OK\" else failed\n\
                              }\n";

#[test]
fn generic_parents_round_trip_through_json() {
    let json = krusty::toolchain::serialization_json_jar()
        .expect("kotlinx-serialization-json must be available to this regression");
    let (_, mut cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    cp.push(json);
    let jdk = common::jdk_modules();
    cp.push(jdk.clone());
    assert_eq!(
        common::compile_and_run_box(ROUND_TRIP_SRC, "Main", &cp, Some(jdk.as_path()))
            .expect("the generic round trip must compile and run"),
        "OK"
    );
}
