//! A property's own serializer takes precedence over the file's contextual serialization.
//!
//! With `@file:UseContextualSerialization(Flex::class)`, a `Flex` property is written and read through
//! a `ContextualSerializer`, unless the property names its serializer itself with
//! `@Serializable(with = FlexSerializer::class)`: kotlinc consults the property's own serializer
//! before the contextual rules, so that property uses `FlexSerializer` directly. krusty checked the
//! contextual rules first and gave both properties a `ContextualSerializer`, which made the class's
//! child-serializer cache and its `$serializer` differ from kotlinc's.
//!
//! A property that IS contextual has a slot in the class's `$childSerializers` cache, holding
//! `ContextualSerializer(T::class, <T's own serializer or null>, [<T's type-argument serializers>])`
//! that every reader takes out of the cache. krusty built the one-argument form at each use.

use std::path::PathBuf;

use super::common::{self, compare_with_kotlinc_plugin};

const SRC: &str = "@file:UseContextualSerialization(Flex::class)\n\
                   import kotlinx.serialization.*\n\
                   import kotlinx.serialization.descriptors.*\n\
                   import kotlinx.serialization.encoding.*\n\
                   class Flex(val v: String)\n\
                   object FlexSerializer : KSerializer<Flex> {\n\
                   \x20   override val descriptor = PrimitiveSerialDescriptor(\"Flex\", PrimitiveKind.STRING)\n\
                   \x20   override fun serialize(encoder: Encoder, value: Flex) = encoder.encodeString(value.v)\n\
                   \x20   override fun deserialize(decoder: Decoder) = Flex(decoder.decodeString())\n\
                   }\n\
                   @Serializable\n\
                   data class Holder(@Serializable(with = FlexSerializer::class) val named: Flex, val contextual: Flex)\n";

/// Contextual properties whose types have their own serializer, none, or type arguments.
const CONTEXTUAL: &str = "import kotlinx.serialization.*\n\
                          @Serializable class P(val x: Int)\n\
                          class Box<T>(val t: T)\n\
                          @Serializable\n\
                          data class Ctx(\n\
                          \x20   @Contextual val p: P,\n\
                          \x20   @Contextual val s: String,\n\
                          \x20   @Contextual val l: List<P>,\n\
                          \x20   @Contextual val b: Box<String>?,\n\
                          \x20   @Contextual val q: P?,\n\
                          )\n";

fn serialization_core() -> PathBuf {
    krusty::toolchain::ensure_maven(
        "org.jetbrains.kotlinx",
        "kotlinx-serialization-core-jvm",
        "1.9.0",
    )
    .unwrap_or_else(|| {
        panic!(
            "could not provision kotlinx-serialization-core-jvm; this test must not self-skip. \
             Check network access or set KRUSTY_DEPS_CACHE."
        )
    })
}

fn assert_class_matches_kotlinc(name: &str, src: &str, class: &str) {
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("{name}: no reference compiler lib directory"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "{name}: the reference serialization plugin is missing at {}",
        plugin.display()
    );
    let cp = vec![serialization_core(), common::stdlib_jar()];
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(name, src, class, &cp, "25", &extra)
        .unwrap_or_else(|| panic!("{name}: the reference comparison could not run"));
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{name}: {class} differs from kotlinc's\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
        built.reference,
        built.krusty
    );
}

/// The generated serializer writes and reads the named property with its own serializer.
#[test]
fn a_property_serializer_wins_over_file_contextual_in_the_serializer() {
    assert_class_matches_kotlinc(
        "PropertyWithOverContextualSerializer",
        SRC,
        "Holder$$serializer",
    );
}

/// The class's child-serializer cache holds only the contextual property's serializer.
#[test]
fn a_property_serializer_wins_over_file_contextual_in_the_class() {
    assert_class_matches_kotlinc("PropertyWithOverContextualClass", SRC, "Holder");
}

/// The class caches each contextual property's serializer with its fallback and type arguments.
#[test]
fn contextual_properties_are_cached_with_their_fallback_and_type_arguments() {
    assert_class_matches_kotlinc("ContextualPropertyCacheClass", CONTEXTUAL, "Ctx");
}

/// The generated serializer reads each contextual property's serializer out of the cache.
#[test]
fn the_serializer_reads_contextual_properties_from_the_cache() {
    assert_class_matches_kotlinc(
        "ContextualPropertyCacheSerializer",
        CONTEXTUAL,
        "Ctx$$serializer",
    );
}
