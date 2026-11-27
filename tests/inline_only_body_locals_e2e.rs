//! kotlinc folds an inline body's temporaries before inlining it (`preprocessNodeBeforeInline`),
//! after an `@InlineOnly` body lost its variable table. An unused store becomes a `pop` there, so
//! a stdlib inline that calls another inline (`find` calling `firstOrNull`) keeps the inner call's
//! receiver copy behind that call's marker, while the argument stored for the outer call's
//! parameter folds, and `requireNotNull`'s receiver becomes a `dup`.
//!
//! PLATFORM-SPECIFIC: temporary folding and local-variable tables are JVM bytecode facts.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    if let Err(e) = common::byte_diff_against_kotlinc(name, src, class)
        .expect("the reference kotlinc compiles the fixture")
    {
        panic!("{e}");
    }
}

#[test]
fn an_inline_only_body_keeps_the_receiver_copy_of_the_inline_it_calls() {
    byte_identical(
        "inline_only_nested_receiver",
        "class Item(val id: String, val rank: Int)\n\
         class Registry {\n\
         \x20   private val items = mutableListOf<Item>()\n\
         \x20   fun first(id: String): Item? = items.find { it.id == id }\n\
         \x20   fun last(id: String): Item? = items.findLast { it.id == id }\n\
         \x20   fun inArray(xs: Array<Item>): Item? = xs.find { it.rank > 1 }\n\
         \x20   fun inSequence(xs: Sequence<Item>): Item? = xs.find { it.rank > 1 }\n\
         \x20   fun required(id: String?): String = requireNotNull(id) { \"missing\" }\n\
         }\n",
        "Registry",
    );
}
