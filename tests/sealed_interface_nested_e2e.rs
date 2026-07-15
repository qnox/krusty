//! A `sealed interface` may declare its subclasses in its body (`data object`/`data class : I`).
//! krusty rejected these ("interface bodies support abstract 'fun' and 'val'/'var'") and, once
//! parsed, dropped them. They now hoist to top-level `I$Sub` classes like nested types in a class
//! body — and a `data object` gets NO `copy`/`componentN` (a singleton), matching kotlinc.

use super::common;

fn class_names(src: &str) -> Vec<String> {
    common::compile_in_process(src, "File", &[], None)
        .unwrap_or_else(|| panic!("krusty failed to compile:\n{src}"))
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

fn owned_bytes(src: &str) -> Vec<u8> {
    common::compile_in_process(src, "File", &[], None)
        .unwrap()
        .into_iter()
        .find(|(n, _)| n == "demo/Origin$Owned")
        .expect("demo/Origin$Owned emitted")
        .1
}

const SRC: &str = "package demo\n\
    sealed interface Origin {\n\
        data object Owned : Origin\n\
        data class ImportedRef(val id: String) : Origin\n\
    }\n";

#[test]
fn sealed_interface_nested_subclasses_emit() {
    let names = class_names(SRC);
    assert!(names.contains(&"demo/Origin".to_string()), "{names:?}");
    assert!(
        names.contains(&"demo/Origin$Owned".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"demo/Origin$ImportedRef".to_string()),
        "{names:?}"
    );
}

#[test]
fn data_object_has_no_copy() {
    let bytes = owned_bytes(SRC);
    let has = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    // A data object is a singleton — no copy, unlike a data class.
    assert!(!has("copy"), "a data object must not synthesize copy");
    assert!(has("INSTANCE"), "a data object has an INSTANCE field");
}
