//! Property access inside a non-private inline member function.
//!
//! A caller in another class splices the inline body, so the body cannot load this class's private
//! backing field: kotlinc reaches the property through its getter or setter there, while an ordinary
//! method of the same class keeps direct field access.

use super::common;

#[test]
fn a_public_inline_member_reads_a_property_through_its_getter() {
    common::assert_class_code_matches_kotlinc(
        "Holder",
        "package sample\n\
         class Holder(val items: List<String>, var count: Int) {\n\
         \x20   inline fun size(): Int = items.size + count\n\
         \x20   inline fun <reified T> matching(): Int = items.count { it is T }\n\
         \x20   fun direct(): Int = items.size + count\n\
         }\n",
        "sample/Holder",
    );
}

#[test]
fn a_public_inline_object_member_reads_a_property_through_its_getter() {
    common::assert_class_code_matches_kotlinc(
        "Registry",
        "package sample\n\
         object Registry {\n\
         \x20   val log = \"321\"\n\
         \x20   inline fun <reified T> describe(value: T): String = log + value\n\
         \x20   fun direct(): String = log\n\
         }\n",
        "sample/Registry",
    );
}

#[test]
fn a_public_inline_member_writes_a_property_through_its_setter() {
    common::assert_class_code_matches_kotlinc(
        "Counter",
        "package sample\n\
         class Counter(var value: Int) {\n\
         \x20   inline fun increment(): Int { value += 1; return value }\n\
         \x20   fun direct(): Int { value += 1; return value }\n\
         }\n",
        "sample/Counter",
    );
}
