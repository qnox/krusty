//! A read written as a statement leaves a `nop` in kotlinc's output: its temporaries pass replaces
//! the `aload; pop` by a `nop`, and a `nop` that is a line's only instruction survives for the
//! debugger.

use super::common;

const SOURCE: &str = "fun touch() {}\n\
    fun parameter(value: Any) {\n\
    \x20   value\n\
    \x20   touch()\n\
    }\n\
    fun primitive(count: Int, wide: Long) {\n\
    \x20   count\n\
    \x20   wide\n\
    \x20   touch()\n\
    }\n\
    class Receiver {\n\
    \x20   fun member() {\n\
    \x20       this\n\
    \x20       touch()\n\
    \x20   }\n\
    }\n\
    fun box(): String {\n\
    \x20   parameter(\"a\")\n\
    \x20   primitive(1, 2L)\n\
    \x20   Receiver().member()\n\
    \x20   return \"OK\"\n\
    }\n";

/// A parameter, a primitive and a two-word parameter, and `this` read as statements each leave the
/// `nop` kotlinc keeps on their line.
#[test]
fn a_read_statement_keeps_kotlincs_nop() {
    common::assert_class_matches_kotlinc("DiscardedRead", SOURCE, "DiscardedReadKt");
    common::assert_class_matches_kotlinc("DiscardedReadReceiver", SOURCE, "Receiver");
    common::expect_box_same_as_kotlinc(SOURCE, "DiscardedReadRun");
}
