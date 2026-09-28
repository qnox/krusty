//! An enum entry with a body compiles to a class of its own, and an object declared in that body
//! is initialized by the entry class's constructor: its `EnclosingMethod` names
//! `Level$HIGH.<init>(Ljava/lang/String;I)V`, not the enum's.
use super::common;

#[test]
fn an_object_in_an_enum_entry_body_is_enclosed_by_the_entry_constructor() {
    let source = "enum class Level {\n\
                  \x20   HIGH {\n\
                  \x20       val probe = object { fun read() = \"h\" }\n\
                  \x20       override fun label(): String = probe.read()\n\
                  \x20   };\n\
                  \x20   abstract fun label(): String\n\
                  }\n";
    let compared = common::compile_with_kotlinc("EntryBody", source, &[], &["Level$HIGH$probe$1"]);
    let (expected, actual) = &compared[0];
    assert!(
        actual == expected,
        "Level$HIGH$probe$1 differs from kotlinc"
    );
}
