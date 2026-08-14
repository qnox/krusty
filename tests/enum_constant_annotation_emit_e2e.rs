//! A RUNTIME-retained annotation on an enum constant (`@Mark("x") RED`) is emitted onto the enum's
//! static field as a `RuntimeVisibleAnnotations` attribute — matching kotlinc. (Previously krusty
//! parsed-and-dropped enum-constant annotations.)

use super::common;

fn role_bytes(src: &str) -> Vec<u8> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::expect_compile_in_process(
        src,
        "File",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
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

const ANNOTATED: &str = "package demo\n\
     @Retention(AnnotationRetention.RUNTIME)\n\
     annotation class Mark(val v: String)\n\
     enum class Role(val v: String) {\n\
         @Mark(\"sys\") SYSTEM(\"system\"),\n\
         @Mark(\"usr\") USER(\"user\"),\n\
     }\n";

/// The same enum with `Mark` DECLARED but never applied — the subject of the negative test.
const DECLARED_UNAPPLIED: &str = "package demo\n\
     @Retention(AnnotationRetention.RUNTIME)\n\
     annotation class Mark(val v: String)\n\
     enum class Role(val v: String) { SYSTEM(\"system\"), USER(\"user\") }\n";

#[test]
fn runtime_annotation_on_enum_constant_is_emitted() {
    let annotated = role_bytes(ANNOTATED);
    // Assert on what only the CONSTANTS can put there: the annotation type and BOTH of its argument
    // values — one per constant, so a single stamped annotation would not satisfy it. The attribute
    // NAME is no evidence at all: it is interned once per class file, and every class krusty emits
    // carries a class-level `@kotlin.Metadata` (as kotlinc's plain enum does).
    for needle in ["Ldemo/Mark;", "sys", "usr"] {
        assert!(
            contains(&annotated, needle),
            "annotated enum constants are missing {needle:?}"
        );
    }
}

/// The negative half: an annotation that is DECLARED but never applied leaves no trace on the enum.
/// The check is the annotation TYPE's descriptor, not the `RuntimeVisibleAnnotations` attribute name —
/// every emitted class carries that attribute for its own `@kotlin.Metadata` (kotlinc's plain `Role`
/// has it too), so its mere presence says nothing about the enum CONSTANTS.
#[test]
fn unapplied_annotation_leaves_no_trace_on_a_plain_enum() {
    let bytes = role_bytes(DECLARED_UNAPPLIED);
    assert!(
        !contains(&bytes, "Ldemo/Mark;"),
        "unexpected annotation on a plain enum's constants",
    );
    assert!(
        contains(&bytes, "Lkotlin/Metadata;"),
        "the enum still carries its own class @Metadata",
    );
}
