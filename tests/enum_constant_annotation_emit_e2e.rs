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

#[test]
fn plain_enum_has_no_annotation_attribute() {
    let bytes = role_bytes(
        "package demo\n\
         enum class Role(val v: String) { SYSTEM(\"system\"), USER(\"user\") }\n",
    );
    assert!(
        !contains(&bytes, "RuntimeVisibleAnnotations"),
        "unexpected annotation attribute on a plain enum",
    );
}
