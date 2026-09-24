//! A field takes its annotations from wherever its declaration lives.
//!
//! Field-targeted annotations were read from the COMPANION class only, and only when the field was
//! already `@JvmField` — the shape a hoisted companion property has. A static the class owns
//! OUTRIGHT records its annotations on the class itself, and nothing looked there, so a
//! plugin-generated field could not carry any.
//!
//! The serialization plugin's `$childSerializers` is that case: kotlinc marks it
//! `@kotlin.jvm.JvmField` even though the field is PRIVATE, because the annotation declares that
//! the field IS the storage rather than a property behind accessors, and writes it FIRST in
//! `RuntimeInvisibleAnnotations`, ahead of the nullability entry.

use super::common::compare_with_kotlinc_plugin;
use super::serialization_companion_byte_parity_e2e::plugin_and_runtime;

const SRC: &str = "import kotlinx.serialization.Serializable\n\
                   @Serializable\n\
                   data class Item(val id: Int)\n\
                   @Serializable\n\
                   data class Holder(val count: Int, val items: List<Item>)\n";

/// The annotation descriptors on the `$childSerializers` field, in the order written.
fn cache_field_annotations(disassembly: &str) -> Vec<String> {
    let lines: Vec<&str> = disassembly.lines().collect();
    let Some(start) = lines.iter().position(|line| {
        line.starts_with("  ")
            && !line.starts_with("   ")
            && line.trim_end().ends_with(" $childSerializers;")
    }) else {
        return Vec::new();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.starts_with("  ") && !line.starts_with("   "))
        .map_or(lines.len(), |offset| start + 1 + offset);
    let field = &lines[start..end];
    let Some(attribute) = field
        .iter()
        .position(|line| line.trim() == "RuntimeInvisibleAnnotations:")
    else {
        return Vec::new();
    };
    field[attribute + 1..]
        .iter()
        .map(|line| line.trim())
        .take_while(|line| !line.ends_with(':'))
        .filter(|line| line.contains('.') && !line.chars().any(char::is_whitespace))
        .map(str::to_string)
        .collect()
}

#[test]
fn the_cache_field_is_marked_jvm_field() {
    let (plugin, cp) = plugin_and_runtime()
        .expect("the serialization plugin and runtime must be available to this regression");
    let extra = vec![format!("-Xplugin={}", plugin.display())];
    let built = compare_with_kotlinc_plugin(
        "ChildSerializerFieldAnnotations",
        SRC,
        "Holder",
        &cp,
        "25",
        &extra,
    )
    .expect("the reference compiler and javap must be available to this regression");
    let want = vec![
        "kotlin.jvm.JvmField".to_string(),
        "org.jetbrains.annotations.NotNull".to_string(),
    ];
    assert_eq!(
        cache_field_annotations(&built.reference),
        want,
        "the reference field contract changed:\n{}",
        built.reference
    );
    assert_eq!(
        cache_field_annotations(&built.krusty),
        want,
        "krusty must write the exact ordered field annotations"
    );
}
