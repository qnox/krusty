//! A `@Serializable` class's `$childSerializers` cache is initialized on its companion's first
//! line, as kotlinc declares the cache on the companion at the companion's offsets. A class whose
//! companion the plugin generates initializes it on the class's own first line, the `@Serializable`
//! one.

use super::compiler_plugin_fixture::{assert_same_classes_and_box, PluginFixture};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SOURCE: &str = "import kotlinx.serialization.Serializable\n\
                      import kotlinx.serialization.json.Json\n\
                      @Serializable\n\
                      data class Declared(\n\
                      \x20   val items: List<String>,\n\
                      ) {\n\
                      \x20   companion object {\n\
                      \x20       fun of(item: String) = Declared(listOf(item))\n\
                      \x20   }\n\
                      }\n\
                      @Serializable\n\
                      data class Annotated(val items: List<Int>) {\n\
                      \x20   @Suppress(\"unused\")\n\
                      \x20   companion object\n\
                      }\n\
                      @Serializable\n\
                      data class Generated(\n\
                      \x20   val items: List<String>,\n\
                      )\n\
                      fun box(): String {\n\
                      \x20   val s = Json.encodeToString(Declared.serializer(), Declared.of(\"a\"))\n\
                      \x20   if (s != \"{\\\"items\\\":[\\\"a\\\"]}\") return s\n\
                      \x20   val a = Json.decodeFromString(Annotated.serializer(), \"{\\\"items\\\":[1]}\")\n\
                      \x20   if (a.items != listOf(1)) return \"annotated\"\n\
                      \x20   val g = Json.decodeFromString(Generated.serializer(), \"{\\\"items\\\":[\\\"b\\\"]}\")\n\
                      \x20   return if (g.items == listOf(\"b\")) \"OK\" else \"generated\"\n\
                      }\n";

#[test]
fn child_serializers_initialize_on_the_companion_line() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("child-serializers-companion-line");
    let sources = [("Main.kt", SOURCE)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}
