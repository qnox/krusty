//! A `@Serializable` class whose primary constructor takes a value class hides that constructor
//! behind a `DefaultConstructorMarker` accessor, and a child-serializer cache gives it an
//! `access$get$childSerializers$cp` accessor. kotlinc's accessor lowering declares the primary's
//! marker accessor, then the `…$cp` accessor, then the deserialization constructor's marker
//! accessor. krusty declared the `…$cp` accessor with the cache's factories, before both markers.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   @JvmInline\n\
                   value class Stamp(val value: String)\n\
                   @Serializable\n\
                   data class Push(val at: Stamp, val refs: List<String>)\n";

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
fn the_cache_accessor_follows_the_hidden_primary_marker_accessor() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "SerializableValueClassCacheAccessorOrder",
        SRC,
        "Push",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    assert_eq!(members(&built.krusty), members(&built.reference));
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "Push must be byte-identical to kotlinc's:\n--- kotlinc\n{}\n--- krusty\n{}",
        built.reference,
        built.krusty
    );
}
