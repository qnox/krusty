//! A serializable abstract class is serialized polymorphically, like kotlinc compiles it.
//!
//! kotlinc gives an abstract `@Serializable` class no `$serializer`: its `Companion.serializer()`
//! returns a `PolymorphicSerializer` over the class, cached in a `$cachedSerializer$delegate` the
//! class initializes after its child-serializer cache. The class still has the `write$Self` and
//! deserialization constructor its subclasses call; that constructor checks no element, because
//! the subclass's own constructor already checked the whole mask, and the cache has no
//! `access$get$childSerializers$cp` accessor, because no `$serializer` reads it.

use super::common;
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   import kotlinx.serialization.Transient\n\
                   @Serializable\n\
                   abstract class Shape(val sides: Int)\n\
                   @Serializable\n\
                   class Square(val size: Int) : Shape(4)\n\
                   @Serializable\n\
                   abstract class Labeled(\n\
                   \x20   val tags: List<String>,\n\
                   \x20   val label: String = \"none\",\n\
                   ) {\n\
                   \x20   @Transient\n\
                   \x20   val cache: Int = 0\n\
                   \x20   abstract fun describe(): String\n\
                   }\n\
                   @Serializable\n\
                   class Pin(val x: Long) : Labeled(listOf(\"a\")) {\n\
                   \x20   override fun describe() = \"pin\"\n\
                   }\n";

fn assert_byte_identical(class: &str) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin("AbstractSerializable", SRC, class, &cp, "25", &extra)
        .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}

#[test]
fn an_abstract_class_caches_a_polymorphic_serializer() {
    assert_byte_identical("Shape");
}

#[test]
fn an_abstract_class_companion_serves_the_cached_serializer() {
    assert_byte_identical("Shape$Companion");
}

#[test]
fn an_abstract_class_caches_its_child_serializers_without_an_accessor() {
    assert_byte_identical("Labeled");
}

#[test]
fn a_subclass_of_an_abstract_class_writes_its_parent_first() {
    assert_byte_identical("Pin");
}

#[test]
fn a_subclass_of_an_abstract_class_has_its_own_serializer() {
    assert_byte_identical("Pin$$serializer");
}

const ROUND_TRIP_SRC: &str = "import kotlinx.serialization.Serializable\n\
                              import kotlinx.serialization.json.Json\n\
                              import kotlinx.serialization.modules.SerializersModule\n\
                              import kotlinx.serialization.modules.polymorphic\n\
                              import kotlinx.serialization.modules.subclass\n\
                              @Serializable\n\
                              abstract class Shape(val sides: Int)\n\
                              @Serializable\n\
                              class Square(val size: Int) : Shape(4)\n\
                              fun box(): String {\n\
                              \x20   val json = Json { serializersModule = SerializersModule { polymorphic(Shape::class) { subclass(Square::class) } } }\n\
                              \x20   val text = json.encodeToString(Shape.serializer(), Square(3))\n\
                              \x20   if (text != \"{\\\"type\\\":\\\"Square\\\",\\\"sides\\\":4,\\\"size\\\":3}\") return text\n\
                              \x20   val back = json.decodeFromString(Shape.serializer(), \"{\\\"type\\\":\\\"Square\\\",\\\"sides\\\":4,\\\"size\\\":7}\")\n\
                              \x20   return if (back is Square && back.size == 7 && back.sides == 4) \"OK\" else \"bad\"\n\
                              }\n";

#[test]
fn an_abstract_class_round_trips_through_its_polymorphic_serializer() {
    let json = krusty::toolchain::serialization_json_jar()
        .expect("kotlinx-serialization-json must be available to this regression");
    let (_, mut cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    cp.push(json);
    let jdk = common::jdk_modules();
    cp.push(jdk.clone());
    assert_eq!(
        common::compile_and_run_box(ROUND_TRIP_SRC, "Main", &cp, Some(jdk.as_path()))
            .expect("the polymorphic round trip must compile and run"),
        "OK"
    );
}
