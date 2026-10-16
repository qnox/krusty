//! A property initializer whose value is an inlined call forgets the constructor's line
//! (`markLineNumberAfterInlineIfNeeded`), so kotlinc writes the property's line again at the
//! `putfield`: the constructor's table holds that line twice in a row. A value that is not inlined
//! keeps one entry.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Failure(val message: String)\n\
                      \n\
                      class Inlined {\n\
                      \x20   val failed: MutableList<Failure> = mutableListOf()\n\
                      \x20   val count = 1\n\
                      }\n\
                      \n\
                      class Called {\n\
                      \x20   val failed: MutableList<Failure> = ArrayList()\n\
                      }\n\
                      \n\
                      class Primary(val name: String) {\n\
                      \x20   val failed = mutableListOf<Failure>()\n\
                      }\n";

const ENUM_SOURCE: &str = "package store\n\
                           \n\
                           enum class Checks {\n\
                           \x20   ONLY;\n\
                           \x20   val items: MutableList<String> = mutableListOf()\n\
                           }\n";

#[test]
fn an_inlined_property_initializer_store_repeats_its_line_like_kotlinc() {
    for class in ["store/Inlined", "store/Called", "store/Primary"] {
        common::byte_diff_against_kotlinc_cp(
            "InlinedPropertyInitializerLine",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

#[test]
fn an_enum_inlined_property_initializer_store_repeats_its_line_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "EnumInlinedPropertyInitializerLine",
        ENUM_SOURCE,
        "store/Checks",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/Checks differs from kotlinc: {diff}"));
}
