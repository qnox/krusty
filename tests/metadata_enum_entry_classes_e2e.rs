//! `@Metadata` of the class an enum entry with a body becomes, measured against kotlinc 2.4.20.
//!
//! kotlinc models the body as an anonymous object: the class is an `ENUM_ENTRY` with LOCAL
//! visibility and no constructor record, and its class id keeps the entry's `Enum.ENTRY` name but is
//! marked local in the string table. Classes nested in the body are local classes named from that id.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn an_enum_entry_body_is_a_local_enum_entry_class() {
    let src = "package app\n\
        \n\
        enum class Coded(val code: Int) {\n\
        \x20   A(1) { override fun label(): Int = 1 },\n\
        \x20   B(2) { override fun label(): Int = 2 };\n\
        \x20   abstract fun label(): Int\n\
        }\n";
    assert_identical("EntryBody", src, "app/Coded$A");
}

#[test]
fn a_nested_enum_entry_body_keeps_its_dotted_class_id() {
    let src = "package app\n\
        \n\
        interface Shape { fun sides(): Int }\n\
        \n\
        class Outer {\n\
        \x20   enum class Kind {\n\
        \x20       ONE {\n\
        \x20           val extra: Int = 5\n\
        \x20           override fun sides(): Int = 1\n\
        \x20           fun <T> pick(value: T): T = value\n\
        \x20       },\n\
        \x20       TWO { override fun sides(): Int = 2 };\n\
        \x20       abstract fun sides(): Int\n\
        \x20   }\n\
        }\n";
    assert_identical("NestedEntryBody", src, "app/Outer$Kind$ONE");
}

#[test]
fn a_class_nested_in_an_enum_entry_body_is_local() {
    let src = "package app\n\
        \n\
        enum class Coded {\n\
        \x20   A {\n\
        \x20       inner class Part { val size: Int = 1 }\n\
        \x20       val part = Part()\n\
        \x20       override fun label(): Int = part.size\n\
        \x20   };\n\
        \x20   abstract fun label(): Int\n\
        }\n";
    assert_identical("EntryBodyNested", src, "app/Coded$A$Part");
    assert_identical("EntryBodyNesting", src, "app/Coded$A");
}
