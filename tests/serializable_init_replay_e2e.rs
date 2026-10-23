//! A `@Serializable` class with `init` blocks runs them from its deserialization constructor in
//! declaration order between its property initializers, as kotlinc does. A block reads a constructor
//! property off the object the constructor has already stored, and its locals sit above the
//! constructor's parameters. Only the `init` block itself is spliced into the constructor: a nested
//! scope keeps its own block, so its locals end with it, as kotlinc's do.

use super::compiler_plugin_fixture::{assert_same_classes_and_box, PluginFixture};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SOURCE: &str = "import kotlinx.serialization.Serializable\n\
                      import kotlinx.serialization.json.Json\n\
                      val log = StringBuilder()\n\
                      @Serializable\n\
                      open class Base(val id: Int) {\n\
                      \x20   init { log.append(\"a\") }\n\
                      \x20   val mid: Int = 2\n\
                      \x20   init { val twice = id * 2; log.append(\"b\").append(twice) }\n\
                      }\n\
                      @Serializable\n\
                      class Child(val name: String) : Base(1) {\n\
                      \x20   init { log.append(\"c\") }\n\
                      }\n\
                      fun box(): String {\n\
                      \x20   val back = Json.decodeFromString(Child.serializer(), \"{\\\"id\\\":5,\\\"name\\\":\\\"n\\\"}\")\n\
                      \x20   if (back.id != 5 || back.mid != 2 || back.name != \"n\") return \"fields\"\n\
                      \x20   return if (log.toString() == \"ab10c\") \"OK\" else log.toString()\n\
                      }\n";

#[test]
fn deserialization_constructors_run_init_blocks_in_declaration_order() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-init-replay");
    let sources = [("Main.kt", SOURCE)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}

const NESTED_SCOPE: &str = "import kotlinx.serialization.Serializable\n\
                            import kotlinx.serialization.json.Json\n\
                            val log = StringBuilder()\n\
                            @Serializable\n\
                            class Box(val id: Int) {\n\
                            \x20   init { if (id > 0) { val inner = id * 2; log.append(inner) }; log.append(\"x\") }\n\
                            \x20   val mid: Int = 2\n\
                            }\n\
                            fun box(): String {\n\
                            \x20   val back = Json.decodeFromString(Box.serializer(), \"{\\\"id\\\":5}\")\n\
                            \x20   return if (back.mid == 2 && log.toString() == \"10x\") \"OK\" else log.toString()\n\
                            }\n";

#[test]
fn a_nested_scope_in_an_init_block_keeps_its_own_block() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-init-replay-nested-scope");
    let sources = [("Main.kt", NESTED_SCOPE)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}
