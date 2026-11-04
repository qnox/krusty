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

const LAMBDA: &str = "import kotlinx.serialization.Serializable\n\
                      import kotlinx.serialization.json.Json\n\
                      val log = StringBuilder()\n\
                      fun call(block: () -> Unit) = block()\n\
                      @Serializable\n\
                      class Box(val id: Int) {\n\
                      \x20   init { call { repeat(1) { log.append(\"a\").append(id) } } }\n\
                      \x20   val mid: Int = 2\n\
                      \x20   init { val f = { log.append(\"b\") }; f() }\n\
                      \x20   init { call { log.append(\"c\") } }\n\
                      }\n\
                      fun box(): String {\n\
                      \x20   val back = Json.decodeFromString(Box.serializer(), \"{\\\"id\\\":5}\")\n\
                      \x20   return if (back.mid == 2 && log.toString() == \"a5bc\") \"OK\" else log.toString()\n\
                      }\n";

/// kotlinc's plugin copies the `init` blocks into the deserialization constructor before lambdas
/// are lowered, so every lambda in the copy is one of its own. An indy lambda gets its own method,
/// numbered after the originals (`_init_$lambda$1`, then `_init_$lambda$4` for its copy), and a
/// lambda spliced at an inline call inside one names its frame marker after the copy's class
/// (`$i$a$-repeat-Box$3$1`). The lambda methods follow the plugin's members, deserialization
/// constructor included, as kotlinc lifts them afterwards.
#[test]
fn lambdas_in_copied_init_blocks_are_lambdas_of_their_own() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-init-replay-lambda");
    let sources = [("Main.kt", LAMBDA)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}

const REQUIRE: &str = "import kotlinx.serialization.Serializable\n\
                       import kotlinx.serialization.json.Json\n\
                       @Serializable\n\
                       data class Amount(val value: String, val unit: String = \"abc\") {\n\
                       \x20   init {\n\
                       \x20       require(unit.length == 3) { \"unit must have three letters\" }\n\
                       \x20   }\n\
                       }\n\
                       fun box(): String {\n\
                       \x20   val back = Json.decodeFromString(Amount.serializer(), \"{\\\"value\\\":\\\"1\\\"}\")\n\
                       \x20   return if (back.unit == \"abc\") \"OK\" else back.unit\n\
                       }\n";

/// A copied `init` block keeps the line kotlinc restates after an inlined call's lambda, so the
/// deserialization constructor's line table matches the primary constructor's.
#[test]
fn a_copied_init_block_restates_its_line_after_an_inlined_call() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-init-replay-require");
    let sources = [("Main.kt", REQUIRE)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}
