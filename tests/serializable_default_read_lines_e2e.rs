//! A `@Serializable` class's default that reads another property is attributed in `write$Self` and
//! the deserialization constructor exactly as kotlinc attributes it: the default's own line where
//! it starts, and the class's start line for the method's own read of the property, whether the
//! default names a constructor parameter or reads through `this`.

use super::compiler_plugin_fixture::{assert_same_classes_and_box, PluginFixture};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SOURCE: &str = "import kotlinx.serialization.Serializable\n\
                      import kotlinx.serialization.json.Json\n\
                      @Serializable\n\
                      class Account(\n\
                      \x20   val id: Int,\n\
                      \x20   val code: Int = id + 1,\n\
                      ) {\n\
                      \x20   val label: String = \"n\" + code\n\
                      \x20   val twice: Int = this.id * 2\n\
                      }\n\
                      fun box(): String {\n\
                      \x20   val s = Json.encodeToString(Account.serializer(), Account(3))\n\
                      \x20   if (s != \"{\\\"id\\\":3}\") return s\n\
                      \x20   val back = Json.decodeFromString(Account.serializer(), \"{\\\"id\\\":4}\")\n\
                      \x20   return if (back.code == 5 && back.label == \"n5\" && back.twice == 8) \"OK\" else \"fail\"\n\
                      }\n";

#[test]
fn defaults_reading_other_properties_keep_kotlincs_line_numbers() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-default-read-lines");
    let sources = [("Main.kt", SOURCE)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}
