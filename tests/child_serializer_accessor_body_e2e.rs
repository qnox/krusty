//! The generated `access$get$childSerializers$cp` accessor is `getstatic; areturn`.
//!
//! The accessor described its own field a second time, by JVM descriptor, instead of reading the
//! static krusty had just declared. The descriptor-built read carries the erased type, so returning
//! it coerced to the accessor's declared type and emitted a `checkcast [Lkotlin/Lazy;` that
//! kotlinc does not — three bytes plus a pool entry on every `@Serializable` class that carries a
//! child-serializer cache.
//!
//! Reading the declared static instead gives the return its own type, so nothing coerces.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Item(val id: Int)\n\
                   @Serializable\n\
                   data class Holder(val count: Int, val items: List<Item>)\n";

/// The disassembly lines of one method's body.
///
/// Anchored on the `Code:` that FOLLOWS the method's declaration — javap prints the constant pool
/// first, where these generated names also appear as Utf8 entries.
fn body(disassembly: &str, method: &str) -> String {
    let lines: Vec<&str> = disassembly.lines().collect();
    let Some(start) = lines.iter().position(|line| {
        let trimmed = line.trim();
        line.starts_with("  ")
            && !line.starts_with("   ")
            && trimmed.ends_with(");")
            && trimmed.contains(method)
    }) else {
        return String::new();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim().is_empty())
        .map_or(lines.len(), |offset| start + 1 + offset);
    lines[start..end].join("\n")
}

#[test]
fn the_child_serializer_accessor_returns_its_field_uncast() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "ChildSerializerAccessorBody",
        SRC,
        "Holder",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    let reference = body(&built.reference, "access$get$childSerializers$cp");
    assert!(
        reference.contains("getstatic"),
        "the accessor must be found in the reference disassembly:\n{}",
        built.reference
    );
    assert!(
        !reference.contains("checkcast"),
        "the reference accessor is a bare field read:\n{reference}"
    );
    let krusty = body(&built.krusty, "access$get$childSerializers$cp");
    assert!(
        krusty.contains("getstatic"),
        "krusty must emit the accessor:\n{}",
        built.krusty
    );
    assert!(
        !krusty.contains("checkcast"),
        "krusty must not coerce the field it just declared:\n{krusty}"
    );
}
