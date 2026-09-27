//! kotlinc's `LocalVariableTable` lists an inner class constructor's enclosing instance as `this$0`,
//! nested inner classes and inner classes of local classes alike, and leaves out a destructuring
//! `for` loop's own variable, a temporary only the prepended `val (a, b) = …` reads
//! (`isVisibleInLVT`).
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class Pair2(val first: Int, val second: String) {\n\
    \x20   operator fun component1(): Int = first\n\
    \x20   operator fun component2(): String = second\n\
    }\n\
    \n\
    class Outer(val o: String) {\n\
    \x20   inner class Inner(val x: Int) {\n\
    \x20       fun read(): String = o\n\
    \x20   }\n\
    \x20   inner class Plain {\n\
    \x20       val y = 1\n\
    \x20   }\n\
    \x20   inner class Deep {\n\
    \x20       inner class Deeper(val z: Int)\n\
    \x20   }\n\
    }\n\
    \n\
    class Loops {\n\
    \x20   fun array(items: Array<Pair2>): String {\n\
    \x20       var s = \"\"\n\
    \x20       for ((a, b) in items) {\n\
    \x20           s += b\n\
    \x20           s += a\n\
    \x20       }\n\
    \x20       return s\n\
    \x20   }\n\
    \x20   fun iterable(items: List<Pair2>): Int {\n\
    \x20       var total = 0\n\
    \x20       for ((a, _) in items) total += a\n\
    \x20       return total\n\
    \x20   }\n\
    }\n";

fn assert_identical(class: &str) {
    common::byte_diff_against_kotlinc_cp(
        "OuterInstanceLocals",
        SOURCE,
        class,
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("{class} byte-identical to kotlinc: {error}"));
}

#[test]
fn inner_class_constructors_name_the_outer_instance_like_kotlinc() {
    assert_identical("store/Outer$Inner");
    assert_identical("store/Outer$Plain");
    assert_identical("store/Outer$Deep$Deeper");
}

#[test]
fn destructuring_loops_hide_their_temporary_like_kotlinc() {
    assert_identical("store/Loops");
}
