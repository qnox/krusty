//! Kotlin's grammar allows line breaks on either side of the `.` that ends a declaration's receiver
//! type (`receiverType NL* '.' NL*`). Code generators rely on it: protobuf's Kotlin DSL writes
//! `public fun DslMap<K, V, P>` and puts `.put(key, value)` on the next line.

use super::common;

/// Both compilers build `sources` as one module and every class they write is byte-identical.
fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn a_generic_receiver_type_continues_on_the_next_line_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Totals.kt",
        "package totals\n\
         \n\
         public fun kotlin.collections.Map<kotlin.String, kotlin.Int>\n\
         \x20 .total(): kotlin.Int = values.sum()\n",
    )]);
}

#[test]
fn a_plain_receiver_type_continues_on_the_next_line_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Shout.kt",
        "package shout\n\
         \n\
         fun String\n\
         \x20   .shout(): String = uppercase()\n",
    )]);
}

#[test]
fn a_nullable_receiver_type_continues_on_the_next_line_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Text.kt",
        "package text\n\
         \n\
         fun String?\n\
         \x20   .orBlank(): String = this ?: \"\"\n",
    )]);
}

#[test]
fn a_declaration_name_follows_the_receiver_dot_on_the_next_line_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Twice.kt",
        "package twice\n\
         \n\
         fun String.\n\
         \x20   twice(): String = this + this\n",
    )]);
}

#[test]
fn an_extension_property_receiver_continues_on_the_next_line_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Initial.kt",
        "package initial\n\
         \n\
         val List<String>\n\
         \x20   .initial: Char\n\
         \x20   get() = first()[0]\n",
    )]);
}
