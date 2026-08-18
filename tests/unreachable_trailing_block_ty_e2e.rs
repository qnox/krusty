//! Unreachable code does not change a block's TYPE.
//!
//! `val a = "a".let { throw Error(); it + "a" }` is `String` to kotlinc — the `throw` makes the rest
//! unreachable, which Kotlin reports as a warning, not as a different type. Typing the block
//! `Nothing` because a statement diverged made the declaration untypeable, and since a property of
//! type `Nothing` is rejected outright, the whole file was refused.
use super::common;

#[test]
fn a_diverging_statement_does_not_retype_the_block() {
    const SRC: &str = "val a1 = \"a\".let {\n\
        \x20   throw Error()\n\
        \x20   it + \"a\"\n\
        }\n\
        fun box(): String = \"OK\"\n";
    common::expect_front_end_ok_files_with_stdlib(&[SRC], "UnreachableTrailing");
}

#[test]
fn a_block_that_yields_nothing_is_still_nothing() {
    // The converse: with no trailing value to fall back on, a diverging body still has no type, so
    // the property is still rejected — as kotlinc rejects it.
    const SRC: &str = "val a2: String = run { throw Error() }\n\
        fun box(): String = \"OK\"\n";
    common::expect_front_end_ok_files_with_stdlib(&[SRC], "DivergingBlockAnnotated");
}
