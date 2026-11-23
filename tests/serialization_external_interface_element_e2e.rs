//! A `@Serializable` class property whose type is an interface declared outside the file that is
//! not itself `@Serializable`: a JDK interface such as `java.nio.file.Path`.
//!
//! kotlinx.serialization serializes an interface polymorphically, so kotlinc's plugin builds
//! `PolymorphicSerializer(<interface>::class)` at the use site, in the property's
//! `$childSerializers` slot factory, wherever the element sits: directly, nullable, or as a type
//! argument of a generic or custom-serialized wrapper. krusty rejected the whole file ("not yet
//! supported by the IR backend") because only an interface declared in the same file had a plan.
use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::{member_body, plugin_and_runtime};

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   import java.nio.file.Path\n\
                   @Serializable\n\
                   class Direct(val path: Path)\n\
                   @Serializable\n\
                   class Wrapped(val paths: List<Path>, val maybe: Path?)\n";

fn compare(class: &str, members: &[&str]) {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin("ExternalInterface", SRC, class, &cp, "25", &extra)
        .expect("the reference compiler and javap must be available to this regression");
    for member in members {
        let want = member_body(&built.reference, member);
        assert!(
            want.len() > 1,
            "kotlinc must emit {member} on {class}:\n{}",
            built.reference
        );
        assert_eq!(
            member_body(&built.krusty, member),
            want,
            "{class}.{member} differs from kotlinc"
        );
    }
}

/// A direct interface property: one cached slot whose factory builds the `PolymorphicSerializer`.
#[test]
fn an_external_interface_property_is_polymorphic_like_kotlinc() {
    compare("Direct", &["_childSerializers$_anonymous_()", "static {};"]);
}

/// A nullable interface (the slot holds the non-null serializer) and an interface as a `List`
/// element (an operand of the constructed list serializer).
#[test]
fn an_external_interface_nested_in_other_types_matches_kotlinc() {
    compare(
        "Wrapped",
        &[
            "_childSerializers$_anonymous_()",
            "_childSerializers$_anonymous_$0()",
            "static {};",
        ],
    );
}
