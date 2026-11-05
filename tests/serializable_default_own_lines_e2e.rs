//! A `@Serializable` class's default keeps the lines of its own source in the deserialization
//! constructor and in `write$Self`, as kotlinc's copy of the default keeps its offsets.
//!
//! kotlinc never marks the property's declaration line there. A default written on the line after
//! its property (`val b: String? =` then `null`) is attributed to the line the value is on, and a
//! default that only re-reads another property has no line of its own: the read is the method's,
//! on the class's start line. The store that follows each default is marked at the class's start
//! line again, at the `putfield`, whatever line the value ran on.
//!
//! The fixture covers a `null`, a string and a `Long` constant, a call spanning two lines, a
//! re-read constructor property, a class-body string and a class-body arithmetic initializer, each
//! on the line after its property, plus a same-line default.

use super::super::compiler_plugin_fixture::{assert_same_classes_and_box, PluginFixture};
use super::super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SOURCE: &str = "import kotlinx.serialization.Serializable\n\
                      import kotlinx.serialization.json.Json\n\
                      @Serializable\n\
                      class Holder(\n\
                      \x20   val a: Int,\n\
                      \x20   val b: String? =\n\
                      \x20       null,\n\
                      \x20   val c: List<String> =\n\
                      \x20       listOf(\"x\",\n\
                      \x20           \"y\"),\n\
                      \x20   val d: Int =\n\
                      \x20       a,\n\
                      \x20   val e: String = \"same\",\n\
                      \x20   val h: String =\n\
                      \x20       \"h\",\n\
                      \x20   val i: Long =\n\
                      \x20       7L,\n\
                      ) {\n\
                      \x20   val f: String =\n\
                      \x20       \"f\"\n\
                      \x20   val g: Int =\n\
                      \x20       a + 1\n\
                      }\n\
                      fun box(): String {\n\
                      \x20   val s = Json.encodeToString(Holder.serializer(), Holder(3))\n\
                      \x20   if (s != \"{\\\"a\\\":3}\") return s\n\
                      \x20   val back = Json.decodeFromString(Holder.serializer(), \"{\\\"a\\\":4}\")\n\
                      \x20   if (back.b != null || back.c != listOf(\"x\", \"y\") || back.d != 4) return \"ctor\"\n\
                      \x20   if (back.e != \"same\" || back.h != \"h\" || back.i != 7L) return \"constants\"\n\
                      \x20   return if (back.f == \"f\" && back.g == 5) \"OK\" else \"body\"\n\
                      }\n";

#[test]
fn defaults_keep_their_own_lines_in_generated_methods() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-default-own-lines");
    let sources = [("Main.kt", SOURCE)];
    let reference = fixture.kotlinc("main", &sources, &runtime, &switches);
    let krusty = fixture.krusty("main", &sources, &runtime, &switches);
    assert_same_classes_and_box(&reference, &krusty, &runtime);
}
