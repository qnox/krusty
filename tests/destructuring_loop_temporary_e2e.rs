//! kotlinc leaves a destructuring `for` loop's own variable out of the `LocalVariableTable`
//! (`isVisibleInLVT`): it is a compiler-generated container that only the prepended
//! `val (a, b) = …` reads. It is not a named source variable either, so a suspend function spills
//! it only while it is live, never merely because it is in scope at a suspension point.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class Pair2(val first: Int, val second: String) {\n\
    \x20   operator fun component1(): Int = first\n\
    \x20   operator fun component2(): String = second\n\
    }\n\
    class PairCursor(private val values: Array<Pair2>) {\n\
    \x20   private var index = 0\n\
    \x20   operator fun hasNext(): Boolean = index < values.size\n\
    \x20   operator fun next(): Pair2 = values[index++]\n\
    }\n\
    class PairItems(private val values: Array<Pair2>) {\n\
    \x20   operator fun iterator(): PairCursor = PairCursor(values)\n\
    }\n\
    \n\
    suspend fun step(x: Int): Int = x\n\
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
    \x20   fun iterable(items: PairItems): Int {\n\
    \x20       var total = 0\n\
    \x20       for ((a, _) in items) total += a\n\
    \x20       return total\n\
    \x20   }\n\
    }\n\
    \n\
    class SuspendLoops {\n\
    \x20   suspend fun sum(items: PairItems): Int {\n\
    \x20       var total = 0\n\
    \x20       for ((a, b) in items) {\n\
    \x20           total = step(a) + total + b.length\n\
    \x20       }\n\
    \x20       return total\n\
    \x20   }\n\
    }\n";

fn assert_identical(class: &str) {
    common::byte_diff_against_kotlinc_cp(
        "DestructuringLoopTemporary",
        SOURCE,
        class,
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("{class} byte-identical to kotlinc: {error}"));
}

#[test]
fn destructuring_loops_hide_their_container_like_kotlinc() {
    assert_identical("store/Loops");
}

/// The container is compiler-generated, so the state machine does not spill it across `step`.
#[test]
fn a_suspend_destructuring_loop_spills_no_container_like_kotlinc() {
    assert_identical("store/SuspendLoops");
    assert_identical("store/SuspendLoops$sum$1");
}
