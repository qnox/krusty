//! kotlinc emits the `<File>Kt` file-facade class ONLY when the file declares top-level
//! callables/properties. A file of only classes/objects gets no facade — emitting an empty one is a
//! spurious extra class (an ABI divergence a drop-in compiler must not introduce).

use super::common;

fn class_names(src: &str) -> Vec<String> {
    let classes = common::compile_in_process(src, "B", &[], None)
        .unwrap_or_else(|| panic!("krusty failed to compile:\n{src}"));
    classes.into_iter().map(|(name, _)| name).collect()
}

#[test]
fn class_only_file_emits_no_facade() {
    let names = class_names("data class Team(val id: String, val name: String)\n");
    assert!(names.contains(&"Team".to_string()), "classes: {names:?}");
    assert!(
        !names.contains(&"BKt".to_string()),
        "spurious empty facade emitted: {names:?}",
    );
}

#[test]
fn top_level_function_emits_facade() {
    let names = class_names("class C\nfun topFun(): Int = 42\n");
    assert!(
        names.contains(&"BKt".to_string()),
        "facade missing for a file with a top-level function: {names:?}",
    );
}

#[test]
fn top_level_property_emits_facade() {
    let names = class_names("class C\nval answer: Int = 42\n");
    assert!(
        names.contains(&"BKt".to_string()),
        "facade missing for a file with a top-level property: {names:?}",
    );
}

#[test]
fn object_only_file_emits_no_facade() {
    let names = class_names("object Registry {\n    val size: Int = 0\n}\n");
    assert!(
        names.contains(&"Registry".to_string()),
        "classes: {names:?}"
    );
    assert!(
        !names.contains(&"BKt".to_string()),
        "spurious empty facade emitted for an object-only file: {names:?}",
    );
}
