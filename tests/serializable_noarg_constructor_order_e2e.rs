//! A `@Serializable` class whose every constructor parameter has a default gets kotlinc's no-arg
//! convenience constructor, and one with a child-serializer cache gets its factory and `…$cp`
//! accessor. kotlinc declares the no-arg constructor right after the deserialization constructor,
//! then the cache members. krusty declared the cache members first, so the whole class differed.
//!
//! The enum property is what gives the class a cache.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Request(val name: String? = null, val kind: Kind? = null) {\n\
                   \x20   @Serializable\n\
                   \x20   enum class Kind { A, B }\n\
                   }\n";

/// The class's members as javap declares them, in order.
fn members(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
        .map(|line| line.trim().to_string())
        .filter(|line| line.ends_with(';'))
        .collect()
}

#[test]
fn the_no_arg_constructor_precedes_the_child_serializer_cache_members() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "SerializableNoArgConstructorOrder",
        SRC,
        "Request",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    let want = members(&built.reference);
    let no_arg = want
        .iter()
        .position(|member| member == "public Request();")
        .expect("the reference declares the no-arg constructor");
    let factory = want
        .iter()
        .position(|member| member.contains(" _childSerializers$_anonymous_("))
        .expect("the reference declares the cache factory");
    assert!(
        no_arg < factory,
        "the reference declares the no-arg constructor before the cache factory:\n{want:#?}"
    );
    assert_eq!(members(&built.krusty), want);
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "Request must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}
