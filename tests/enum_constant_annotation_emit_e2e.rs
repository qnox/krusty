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

const ANNOTATED: &str = "package demo\n\
     @Retention(AnnotationRetention.RUNTIME)\n\
     annotation class Mark(val v: String)\n\
     enum class Role(val v: String) {\n\
         @Mark(\"sys\") SYSTEM(\"system\"),\n\
         @Mark(\"usr\") USER(\"user\"),\n\
     }\n";

const PLAIN: &str = "package demo\n\
     enum class Role(val v: String) { SYSTEM(\"system\"), USER(\"user\") }\n";

#[test]
fn runtime_annotation_on_enum_constant_is_emitted() {
    let annotated = role_bytes(ANNOTATED);
    let plain = role_bytes(PLAIN);
    // Assert on what only the CONSTANTS can put there: the annotation type and BOTH of its argument
    // values — one per constant, so a single stamped annotation would not satisfy it. The attribute
    // NAME is no evidence at all: it is interned once per class file, and every class krusty emits
    // carries a class-level `@kotlin.Metadata` (as kotlinc's plain enum does), so the same enum
    // WITHOUT the annotations holds exactly the same name — which is what `plain` pins here.
    for needle in ["Ldemo/Mark;", "sys", "usr"] {
        assert!(
            contains(&annotated, needle),
            "annotated enum constants are missing {needle:?}"
        );
    }
    assert!(
        !contains(&plain, "Ldemo/Mark;"),
        "plain enum unexpectedly carries the annotation type"
    );
}

#[test]
fn plain_enum_has_no_constant_annotation() {
    let bytes = role_bytes(PLAIN);
    // Asserted on the annotation TYPE, not the attribute NAME: a plain enum compiled by kotlinc DOES
    // carry `RuntimeVisibleAnnotations` — its class-level `@kotlin.Metadata` is one — so the attribute
    // name says nothing about whether the CONSTANTS were annotated, which is what this test is about.
    // The positive test above pins the same bytes from the other side.
    assert!(
        !contains(&bytes, "Ldemo/Mark;"),
        "unexpected annotation on a plain enum's constants",
    );
}
