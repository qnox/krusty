//! `write$Self` reads an overridable property through its getter.
//!
//! `write$Self` is a static member of the serialized class, and kotlinc's JVM lowering turns a
//! read of a final property's default getter into a read of its backing field. An open or
//! overriding property's getter can be overridden in a subclass, so kotlinc keeps the virtual call:
//! `Impl.name` below is read with `invokevirtual getName`, `Impl.tag` with `getfield tag`.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

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
