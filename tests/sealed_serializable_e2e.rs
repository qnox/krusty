//! A serializable sealed class is serialized through a cached `SealedClassSerializer`, like kotlinc
//! compiles it.
//!
//! kotlinc gives a sealed `@Serializable` class no `$serializer`: its `Companion.serializer()`
//! returns a `SealedClassSerializer` over every concrete subclass reached through the sealed
//! hierarchy, cached in a `$cachedSerializer$delegate`. The class keeps the `write$Self` and
//! deserialization constructor its subclasses call, and a subclass serializes the sealed parent's
//! elements first. A sealed interface builds the same serializer in its companion's
//! `serializer()`. An element typed by a sealed or abstract class reads its serializer through the
//! child-serializer cache.

use super::common;
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.SerialName\n\
                   import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   sealed class Expr(val depth: Int) {\n\
                   \x20   @Serializable\n\
                   \x20   object Zero : Expr(0)\n\
                   \x20   @Serializable\n\
                   \x20   @SerialName(\"lit\")\n\
                   \x20   data class Lit(val value: Int) : Expr(1)\n\
                   \x20   @Serializable\n\
                   \x20   sealed class Bin : Expr(2) {\n\
                   \x20       abstract val left: Expr\n\
                   \x20   }\n\
                   \x20   @Serializable\n\
                   \x20   data class Add(override val left: Expr, val right: Expr) : Bin()\n\
                   }\n\
                   @Serializable\n\
                   sealed interface Event\n\
                   @Serializable\n\
                   object Start : Event\n\
                   @Serializable\n\
                   data class Tick(val count: Long) : Event\n\
                   @Serializable\n\
                   abstract class Plugin\n\
                   @Serializable\n\
                   class Holder(val expr: Expr, val event: Event, val plugin: Plugin, val all: List<Expr>)\n";

fn assert_byte_identical(class: &str) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin("SealedSerializable", SRC, class, &cp, "25", &extra)
        .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}

#[test]
fn a_sealed_class_caches_a_sealed_class_serializer_over_its_concrete_subclasses() {
    assert_byte_identical("Expr");
}

#[test]
fn a_sealed_class_companion_serves_the_cached_serializer() {
    assert_byte_identical("Expr$Companion");
}

#[test]
fn a_nested_sealed_class_registers_its_own_subclasses() {
    assert_byte_identical("Expr$Bin");
}

#[test]
fn a_sealed_subclass_writes_its_sealed_parent_first() {
    assert_byte_identical("Expr$Lit");
}

#[test]
fn a_sealed_subclass_caches_its_sealed_element_serializers() {
    assert_byte_identical("Expr$Add");
}

#[test]
fn a_sealed_subclass_serializer_covers_the_inherited_elements() {
    assert_byte_identical("Expr$Add$$serializer");
}

#[test]
fn a_sealed_interface_companion_builds_its_serializer() {
    assert_byte_identical("Event$Companion");
}

#[test]
fn sealed_and_abstract_elements_read_the_child_serializer_cache() {
    assert_byte_identical("Holder");
}

const ROUND_TRIP_SRC: &str = "import kotlinx.serialization.Serializable\n\
                              import kotlinx.serialization.json.Json\n\
                              @Serializable\n\
                              sealed class Expr(val depth: Int) {\n\
                              \x20   @Serializable\n\
                              \x20   object Zero : Expr(0)\n\
                              \x20   @Serializable\n\
                              \x20   data class Lit(val value: Int) : Expr(1)\n\
                              \x20   @Serializable\n\
                              \x20   data class Add(val left: Expr, val right: Expr) : Expr(2)\n\
                              }\n\
                              fun box(): String {\n\
                              \x20   val tree: Expr = Expr.Add(Expr.Lit(3), Expr.Zero)\n\
                              \x20   val text = Json.encodeToString(Expr.serializer(), tree)\n\
                              \x20   val expected = \"{\\\"type\\\":\\\"Expr.Add\\\",\\\"depth\\\":2,\\\"left\\\":{\\\"type\\\":\\\"Expr.Lit\\\",\\\"depth\\\":1,\\\"value\\\":3},\\\"right\\\":{\\\"type\\\":\\\"Expr.Zero\\\"}}\"\n\
                              \x20   if (text != expected) return text\n\
                              \x20   val back = Json.decodeFromString(Expr.serializer(), text)\n\
                              \x20   return if (back == tree && back.depth == 2) \"OK\" else \"bad\"\n\
                              }\n";

#[test]
fn a_sealed_class_round_trips_with_its_inherited_elements() {
    let json = krusty::toolchain::serialization_json_jar()
        .expect("kotlinx-serialization-json must be available to this regression");
    let (_, mut cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    cp.push(json);
    let jdk = common::jdk_modules();
    cp.push(jdk.clone());
    assert_eq!(
        common::compile_and_run_box(ROUND_TRIP_SRC, "Main", &cp, Some(jdk.as_path()))
            .expect("the sealed round trip must compile and run"),
        "OK"
    );
}
