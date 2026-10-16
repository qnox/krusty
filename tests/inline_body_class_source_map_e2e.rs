//! The identity `SourceDebugExtension` kotlinc 2.4.20 writes on a class whose nearest enclosing
//! function is `inline`, whatever that function's visibility: code inlining the function copies
//! the class and maps its lines through that map. A class declared in an ordinary function carries
//! none.

use super::common;

const SOURCE: &str = r#"package app

interface Greeter {
    fun greet(): String
}

inline fun public(): Greeter = object : Greeter {
    override fun greet() = "public"
}

private inline fun hidden(): Greeter = object : Greeter {
    override fun greet() = "hidden"
}

private class Closed {
    inline fun member(): Greeter = object : Greeter {
        override fun greet() = "member"
    }
}

fun plain(): Greeter = object : Greeter {
    override fun greet() = "plain"
}
"#;

#[test]
fn a_class_in_an_inline_body_carries_the_identity_source_map_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Objects.kt", SOURCE)]);
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "app/Closed",
            "app/Closed$member$1",
            "app/Greeter",
            "app/ObjectsKt",
            "app/ObjectsKt$hidden$1",
            "app/ObjectsKt$plain$1",
            "app/ObjectsKt$public$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
