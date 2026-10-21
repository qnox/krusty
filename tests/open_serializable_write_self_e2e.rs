//! An inheritable serializable class publishes its `write$Self` like kotlinc.
//!
//! kotlinc's serialization plugin keeps a final class's `write$Self` internal, so its JVM name
//! carries the module (`write$Self$main`). An open or abstract class's subclass, possibly in
//! another module, calls it, so it is public under its plain name, and so is the
//! deserialization constructor's metadata. An open or abstract class's metadata also lists its own
//! elements in program order (`propertiesNamesInProgramOrder`), leaving out a `@Transient`
//! property. A `write$Self` without elements is a bare `return` with no line. An abstract class's
//! companion (its `PolymorphicSerializer`) is not generated yet, so only its `write$Self` is
//! compared.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   import kotlinx.serialization.Transient\n\
                   @Serializable\n\
                   open class Base(val id: Int, @Transient val cache: Int = 0, val label: String)\n\
                   @Serializable\n\
                   internal open class Empty\n\
                   @Serializable\n\
                   class Plain\n\
                   @Serializable\n\
                   abstract class Shape(val sides: Int, @Transient val area: Double = 0.0, val name: String)\n";

fn assert_byte_identical(class: &str) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built =
        compare_with_kotlinc_plugin("OpenSerializableWriteSelf", SRC, class, &cp, "25", &extra)
            .expect("the reference compiler and javap must be available to this regression");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}

#[test]
fn an_open_class_publishes_its_write_self_and_element_order() {
    assert_byte_identical("Base");
}

#[test]
fn an_open_class_serializer_calls_the_public_write_self() {
    assert_byte_identical("Base$$serializer");
}

#[test]
fn an_open_class_without_elements_has_a_lineless_write_self() {
    assert_byte_identical("Empty");
}

/// One method's `javap -c -v` listing, up to the blank line that ends it.
fn method_listing<'a>(javap: &'a str, header: &str) -> &'a str {
    let start = javap
        .find(header)
        .unwrap_or_else(|| panic!("`{header}` is missing from:\n{javap}"));
    let end = javap[start..]
        .find("\n\n")
        .map_or(javap.len(), |end| start + end);
    &javap[start..end]
}

/// An abstract class is not compared whole: kotlinc gives its companion a `PolymorphicSerializer`
/// behind `$cachedSerializer`, which krusty does not generate yet. Its `write$Self` is this
/// change's, and must match exactly.
#[test]
fn an_abstract_class_publishes_its_write_self() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built =
        compare_with_kotlinc_plugin("OpenSerializableWriteSelf", SRC, "Shape", &cp, "25", &extra)
            .expect("the reference compiler and javap must be available to this regression");
    let header = "  public static final void write$Self(";
    assert_eq!(
        method_listing(&built.krusty, header),
        method_listing(&built.reference, header)
    );
}

#[test]
fn a_final_class_without_elements_has_a_lineless_write_self() {
    assert_byte_identical("Plain");
}
