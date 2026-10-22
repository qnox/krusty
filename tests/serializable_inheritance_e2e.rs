//! A serializable class extending a serializable class serializes its parent's elements first.
//!
//! kotlinc's serialization plugin gives the subclass the parent's elements ahead of its own: the
//! descriptor, `childSerializers` and `deserialize` cover every element, the subclass's
//! `write$Self` calls the parent's public `write$Self` before writing its own elements, and its
//! deserialization constructor checks the whole mask, then delegates the inherited elements to the
//! parent's deserialization constructor.

use super::common;
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   import kotlinx.serialization.json.Json\n\
                   @Serializable\n\
                   open class Parent(val a: Int, val b: String = \"x\")\n\
                   @Serializable\n\
                   class Child(val c: Long) : Parent(1)\n\
                   @Serializable\n\
                   class Tagged(val tags: List<String>) : Parent(2)\n\
                   fun box(): String {\n\
                   \x20   val text = Json.encodeToString(Child.serializer(), Child(5))\n\
                   \x20   if (text != \"{\\\"a\\\":1,\\\"c\\\":5}\") return text\n\
                   \x20   val child = Json.decodeFromString(Child.serializer(), \"{\\\"a\\\":2,\\\"b\\\":\\\"y\\\",\\\"c\\\":7}\")\n\
                   \x20   return if (child.a == 2 && child.b == \"y\" && child.c == 7L) \"OK\" else \"bad\"\n\
                   }\n";

fn runtime() -> (String, Vec<std::path::PathBuf>) {
    let json = krusty::toolchain::serialization_json_jar()
        .expect("kotlinx-serialization-json must be available to this regression");
    let (plugin, mut cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    cp.push(json);
    (format!("-Xplugin={}", plugin.display()), cp)
}

fn assert_byte_identical(class: &str) {
    let (plugin, cp) = runtime();
    let built =
        compare_with_kotlinc_plugin("SerializableInheritance", SRC, class, &cp, "25", &[plugin])
            .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}

#[test]
fn a_subclass_writes_its_parent_first_and_delegates_its_deserialization() {
    assert_byte_identical("Child");
}

#[test]
fn a_subclass_serializer_covers_the_inherited_elements() {
    assert_byte_identical("Child$$serializer");
}

/// The subclass's own `$childSerializers` is read after the parent's `write$Self` has run.
#[test]
fn a_subclass_reads_its_child_serializer_cache_after_the_parent_writes() {
    assert_byte_identical("Tagged");
}

#[test]
fn a_subclass_round_trips_its_inherited_elements() {
    let (_, mut cp) = runtime();
    let jdk = common::jdk_modules();
    cp.push(jdk.clone());
    assert_eq!(
        common::compile_and_run_box(SRC, "Main", &cp, Some(jdk.as_path()))
            .expect("the inheritance round trip must compile and run"),
        "OK"
    );
}
