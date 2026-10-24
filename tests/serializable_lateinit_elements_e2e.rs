//! `write$Self` reads a `lateinit` element as kotlinc does: a non-private one through its getter,
//! which throws for an uninitialized element, and a private one off its field, checked where it is
//! used. krusty read every one off its field.

use super::compiler_plugin_fixture::{assert_same_classes_and_box, PluginFixture};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SOURCE: &str = "import kotlinx.serialization.Serializable\n\
                      import kotlinx.serialization.json.Json\n\
                      @Serializable\n\
                      open class Base {\n\
                      \x20   lateinit var name: String\n\
                      \x20   private lateinit var secret: String\n\
                      \x20   internal lateinit var inner: String\n\
                      \x20   protected lateinit var prot: String\n\
                      \x20   fun fill() { secret = \"s\"; inner = \"i\"; prot = \"p\" }\n\
                      }\n\
                      @Serializable\n\
                      class Child(val id: Int) : Base() {\n\
                      \x20   lateinit var own: String\n\
                      }\n\
                      fun box(): String {\n\
                      \x20   val c = Child(1); c.name = \"n\"; c.own = \"o\"; c.fill()\n\
                      \x20   val s = Json.encodeToString(Child.serializer(), c)\n\
                      \x20   val back = Json.decodeFromString(Child.serializer(), s)\n\
                      \x20   return if (back.name == \"n\" && back.own == \"o\") \"OK\" else s\n\
                      }\n";

#[test]
fn write_self_reads_lateinit_elements_like_kotlinc() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-lateinit-elements");
    let sources = [("Main.kt", SOURCE)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}
