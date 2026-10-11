//! The Kotlin grammar allows line breaks before a primary constructor (`NL* primaryConstructor`),
//! so its parameter list may start on the line after the class name, also after a KDoc comment.
//! The constructor's trailing `return` maps to the line the constructor starts on: its first
//! annotation, modifier, `constructor` keyword, or `(`, not the class name's line.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class Documented\n\
    /** The constructor's KDoc. */(val a: Int)\n\
    \n\
    class Wrapped<T>\n\
    (val b: T)\n\
    \n\
    class Annotated\n\
    @Suppress(\"unused\") constructor\n\
    (\n\
    \x20   val c: Int\n\
    )\n\
    \n\
    enum class Level\n\
    (val weight: Int) { LOW(1), HIGH(2) }\n";

#[test]
fn primary_constructor_after_a_line_break_is_byte_identical_to_kotlinc() {
    for (name, class) in [
        ("PrimaryConstructorNewlineDocumented", "store/Documented"),
        ("PrimaryConstructorNewlineWrapped", "store/Wrapped"),
        ("PrimaryConstructorNewlineAnnotated", "store/Annotated"),
        ("PrimaryConstructorNewlineLevel", "store/Level"),
    ] {
        common::byte_diff_against_kotlinc_cp(name, SOURCE, class, &[common::stdlib_jar()])
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
