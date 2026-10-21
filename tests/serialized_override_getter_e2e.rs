//! `write$Self` reads an overridable property through its getter.
//!
//! `write$Self` is a static member of the serialized class, and kotlinc's JVM lowering turns a
//! read of a final property's default getter into a read of its backing field. An open or
//! overriding property's getter can be overridden in a subclass, so kotlinc keeps the virtual call:
//! `Impl.name` below is read with `invokevirtual getName`, `Impl.tag` with `getfield tag`.

use super::common;
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{gradle_module_jar, plugin_and_runtime};

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   interface Named { val name: String }\n\
                   @Serializable\n\
                   class Impl(override val name: String, val tag: String) : Named\n";

#[test]
fn write_self_reads_an_overriding_property_through_its_getter() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built =
        compare_with_kotlinc_plugin("SerializedOverrideGetter", SRC, "Impl", &cp, "25", &extra)
            .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "Impl must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}

/// An overriding `UInt` property with a declared getter: `write$Self` calls `getU-pVg5ArA`, which
/// the JSON round trip exercises before the classes are compared with kotlinc's.
const DECLARED_GETTER_SRC: &str = "import kotlinx.serialization.Serializable\n\
                                   import kotlinx.serialization.json.Json\n\
                                   interface HasU { val u: UInt }\n\
                                   @Serializable\n\
                                   class Counted(val tag: String) : HasU {\n\
                                   \x20   override val u: UInt = 1u\n\
                                   \x20       get() = field\n\
                                   }\n\
                                   fun box(): String {\n\
                                   \x20   val text = Json { encodeDefaults = true }.encodeToString(Counted.serializer(), Counted(\"t\"))\n\
                                   \x20   return if (text == \"{\\\"tag\\\":\\\"t\\\",\\\"u\\\":1}\") \"OK\" else text\n\
                                   }\n";

#[test]
fn write_self_calls_a_declared_value_class_getter_by_its_jvm_name() {
    let json = gradle_module_jar("org.jetbrains.kotlinx", "kotlinx-serialization-json-jvm")
        .expect("kotlinx-serialization-json must be available to this regression");
    let (plugin, mut cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    cp.push(json);
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    // kotlinc first: its class is the reference the krusty class must equal byte for byte.
    let built = compare_with_kotlinc_plugin(
        "SerializedDeclaredGetter",
        DECLARED_GETTER_SRC,
        "Counted",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "Counted must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
    let jdk = common::jdk_modules();
    cp.push(jdk.clone());
    assert_eq!(
        common::compile_and_run_box(DECLARED_GETTER_SRC, "Main", &cp, Some(jdk.as_path()))
            .expect("the declared-getter round trip must compile and run"),
        "OK"
    );
}
