//! A RUNTIME-retained annotation on an enum constant (`@Mark("x") RED`) is emitted onto the enum's
//! static field as a `RuntimeVisibleAnnotations` attribute — matching kotlinc. (Previously krusty
//! parsed-and-dropped enum-constant annotations.)

use super::common;

fn role_bytes(src: &str) -> Vec<u8> {
    let classes = common::compile_in_process(src, "File", &[], None)
        .unwrap_or_else(|| panic!("krusty failed to compile:\n{src}"));
    classes
        .into_iter()
        .find(|(n, _)| n == "demo/Role")
        .unwrap_or_else(|| panic!("no demo/Role class emitted"))
        .1
}

/// Whether the class bytes contain `needle` as a raw UTF8 constant-pool substring.
fn contains(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn runtime_annotation_on_enum_constant_is_emitted() {
    let bytes = role_bytes(
        "package demo\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         annotation class Mark(val v: String)\n\
         enum class Role(val v: String) {\n\
             @Mark(\"sys\") SYSTEM(\"system\"),\n\
             @Mark(\"usr\") USER(\"user\"),\n\
         }\n",
    );
    // The enum class carries no class-level annotation, so both must come from the constant fields.
    assert!(
        contains(&bytes, "RuntimeVisibleAnnotations"),
        "no field annotation attribute emitted"
    );
    assert!(
        contains(&bytes, "Ldemo/Mark;"),
        "annotation type not referenced"
    );
}

/// The negative half: an annotation that is DECLARED but never applied leaves no trace on the enum.
/// The check is the annotation TYPE's descriptor, not the `RuntimeVisibleAnnotations` attribute name —
/// every emitted class carries that attribute for its own `@kotlin.Metadata` (kotlinc's plain `Role`
/// has it too), so its mere presence says nothing about the enum CONSTANTS.
#[test]
fn unapplied_annotation_leaves_no_trace_on_a_plain_enum() {
    let bytes = role_bytes(
        "package demo\n\
         @Retention(AnnotationRetention.RUNTIME)\n\
         annotation class Mark(val v: String)\n\
         enum class Role(val v: String) { SYSTEM(\"system\"), USER(\"user\") }\n",
    );
    assert!(
        !contains(&bytes, "Ldemo/Mark;"),
        "unexpected annotation on a plain enum's constants",
    );
    assert!(
        contains(&bytes, "Lkotlin/Metadata;"),
        "the enum still carries its own class @Metadata",
    );
}
