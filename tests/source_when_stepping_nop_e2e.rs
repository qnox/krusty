//! kotlinc's `visitWhen` gives a source `when` a `nop` on the `when` keyword's line, so a debugger
//! can stop on the `when` itself even when it has no subject; `RedundantNopsCleanup` then keeps the
//! `nop` only where nothing else shares that line. An `if` gets no such point.
use super::common;

/// A subject-less `when` whose first condition is on the next line keeps its `nop`; one written on
/// a single line loses it to the cleanup; a statement `when` and an `else if` chain are covered too.
#[test]
fn a_source_when_is_byte_identical_to_kotlinc() {
    let src = "package store\n\
               \n\
               fun sink(x: Int) {}\n\
               \n\
               fun pick(a: Boolean, b: Boolean): Int {\n\
               \x20   sink(0)\n\
               \x20   return when {\n\
               \x20       a -> 1\n\
               \x20       b -> 2\n\
               \x20       else -> 3\n\
               \x20   }\n\
               }\n\
               \n\
               fun act(a: Boolean) {\n\
               \x20   when {\n\
               \x20       a -> sink(1)\n\
               \x20       else -> sink(2)\n\
               \x20   }\n\
               }\n\
               \n\
               fun oneLine(a: Boolean): Int = when { a -> 1 else -> 2 }\n\
               \n\
               fun ifChain(a: Boolean, b: Boolean): Int {\n\
               \x20   sink(0)\n\
               \x20   return if (a)\n\
               \x20       1\n\
               \x20   else if (b) 2 else 3\n\
               }\n";
    common::byte_diff_against_kotlinc("SourceWhenNop", src, "store/SourceWhenNopKt")
        .expect("reference kotlinc is provisioned")
        .expect("store/SourceWhenNopKt byte-identical to kotlinc");
}
