//! Which receiver of the implicit tower shapes a lambda argument.
//!
//! `with(list) { forEach { … } }` inside a class that itself declares an extension named `forEach`
//! has two candidates: the INNER receiver's classpath member (`ArrayList.forEach(Consumer<String>)`)
//! and the OUTER receiver's extension. Kotlin takes the innermost receiver, so `it` is `String`.
//!
//! The tower used to be swept once per SOURCE — every receiver asked for an extension, then every
//! receiver asked for a member — so the outer receiver's extension answered first and `it` was
//! typed `Int`. Each receiver is now asked for a whole decision and the innermost one that can
//! shape the lambda wins. Both candidates take a ONE-parameter lambda, so nothing but the receiver
//! order distinguishes them.
use super::common;

#[test]
fn the_innermost_receiver_shapes_the_lambda() {
    const MAIN: &str = "package repro\n\
        class Outer {\n\
        \x20   fun Outer.forEach(block: (Int) -> Unit) { block(7) }\n\
        \x20   fun go(list: java.util.ArrayList<String>): Int {\n\
        \x20       var n = 0\n\
        \x20       with(list) { forEach { n += it.length } }\n\
        \x20       return n\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val list = java.util.ArrayList<String>()\n\
        \x20   list.add(\"abc\")\n\
        \x20   list.add(\"de\")\n\
        \x20   val n = Outer().go(list)\n\
        \x20   return if (n == 5) \"OK\" else \"fail: \" + n\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "the innermost receiver");
}

#[test]
fn the_outer_receiver_still_shapes_a_name_the_inner_one_lacks() {
    // The control: with no candidate on the inner receiver, the outer receiver's extension still
    // shapes the lambda. Asking per receiver must not stop the sweep at the first receiver.
    const MAIN: &str = "package repro\n\
        class Outer {\n\
        \x20   fun Outer.eachIndex(block: (Int) -> Unit) { block(7) }\n\
        \x20   fun go(list: java.util.ArrayList<String>): Int {\n\
        \x20       var n = 0\n\
        \x20       with(list) { eachIndex { n += it + size } }\n\
        \x20       return n\n\
        \x20   }\n\
        }\n\
        fun box(): String {\n\
        \x20   val list = java.util.ArrayList<String>()\n\
        \x20   list.add(\"abc\")\n\
        \x20   val n = Outer().go(list)\n\
        \x20   return if (n == 8) \"OK\" else \"fail: \" + n\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "the outer receiver");
}
