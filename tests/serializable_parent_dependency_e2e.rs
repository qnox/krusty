//! A `@Serializable` class whose serializable superclass a dependency declares compiles like
//! kotlinc compiles it.
//!
//! The subclass inherits the superclass's elements as the dependency's metadata publishes them:
//! the serialization plugin's `propertiesNamesInProgramOrder` list orders them, each property's
//! metadata types it, and the superclass's primary constructor says which ones the input must
//! contain. The subclass's `write$Self` calls the superclass's public `write$Self` and its
//! deserialization constructor delegates to the superclass's, which stores the inherited elements.
//! Each compiler builds the library itself.

use super::compiler_plugin_fixture::{assert_same_classes_and_box, PluginFixture};
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const LIBRARY: &[(&str, &str)] = &[(
    "Parents.kt",
    "package lib\n\
     import kotlinx.serialization.Serializable\n\
     import kotlinx.serialization.Transient\n\
     @Serializable\n\
     open class Base(val zeta: Int, var alpha: String = \"t\", val tags: List<String>) {\n\
     \x20   @Transient val skipped: Int = 3\n\
     \x20   val mid: Long = 4L\n\
     \x20   var beta: Int? = null\n\
     }\n\
     @Serializable\n\
     abstract class Shape(val name: String)\n\
     @Serializable\n\
     open class Middle(val m: Int) : Base(m, tags = listOf(\"m\"))\n",
)];

const MAIN: &str = "import kotlinx.serialization.Serializable\n\
                    import kotlinx.serialization.json.Json\n\
                    import lib.Base\n\
                    import lib.Middle\n\
                    import lib.Shape\n\
                    @Serializable\n\
                    class Child(val b: String) : Base(1, tags = listOf(\"x\"))\n\
                    @Serializable\n\
                    class Square(val side: Double) : Shape(\"sq\")\n\
                    @Serializable\n\
                    class Leaf(val l: Long) : Middle(2)\n\
                    fun box(): String {\n\
                    \x20   val child = Json.encodeToString(Child.serializer(), Child(\"c\"))\n\
                    \x20   if (child != \"{\\\"zeta\\\":1,\\\"tags\\\":[\\\"x\\\"],\\\"b\\\":\\\"c\\\"}\") return child\n\
                    \x20   val back = Json.decodeFromString(Child.serializer(), \"{\\\"zeta\\\":5,\\\"alpha\\\":\\\"a\\\",\\\"tags\\\":[],\\\"beta\\\":7,\\\"b\\\":\\\"d\\\"}\")\n\
                    \x20   if (back.zeta != 5 || back.alpha != \"a\" || back.beta != 7 || back.b != \"d\" || back.mid != 4L) return \"child\"\n\
                    \x20   val square = Json.encodeToString(Square.serializer(), Square(2.0))\n\
                    \x20   if (square != \"{\\\"name\\\":\\\"sq\\\",\\\"side\\\":2.0}\") return square\n\
                    \x20   if (Json.decodeFromString(Square.serializer(), square).name != \"sq\") return \"square\"\n\
                    \x20   val leaf = Json.encodeToString(Leaf.serializer(), Leaf(9))\n\
                    \x20   if (leaf != \"{\\\"zeta\\\":2,\\\"tags\\\":[\\\"m\\\"],\\\"m\\\":2,\\\"l\\\":9}\") return leaf\n\
                    \x20   try {\n\
                    \x20       val incomplete = Json.decodeFromString(Child.serializer(), \"{\\\"b\\\":\\\"d\\\"}\")\n\
                    \x20       return incomplete.b\n\
                    \x20   } catch (e: Exception) {\n\
                    \x20       return \"OK\"\n\
                    \x20   }\n\
                    }\n";

#[test]
fn subclasses_inherit_the_elements_a_dependency_parent_publishes() {
    let (plugin, mut runtime) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    runtime.push(
        krusty::toolchain::serialization_json_jar()
            .expect("kotlinx-serialization-json must be available to this regression"),
    );
    let switches = vec![format!("-Xplugin={}", plugin.display())];
    let fixture = PluginFixture::new("serializable-parent-dependency");
    let reference_lib = fixture.kotlinc("lib", LIBRARY, &runtime, &switches);
    let krusty_lib = fixture.krusty("lib", LIBRARY, &runtime, &switches);
    let sources = [("Main.kt", MAIN)];
    let mut reference_cp = runtime.clone();
    reference_cp.push(reference_lib);
    let mut krusty_cp = runtime.clone();
    krusty_cp.push(krusty_lib);
    let reference = fixture.kotlinc("main", &sources, &reference_cp, &switches);
    let krusty = fixture.krusty("main", &sources, &krusty_cp, &switches);
    assert_same_classes_and_box(&reference, &krusty, &krusty_cp);
}
