//! kotlinc's `isVisibleInLVT` leaves out a destructuring `for` loop's own variable: it is a compiler
//! temporary read only by the prepended `val (a, b) = …`. The same identity must remain unnamed in
//! a suspend state machine rather than being promoted as a source local.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class Pair2(val first: Int, val second: Int) {\n\
    \x20   operator fun component1(): Int = first\n\
    \x20   operator fun component2(): Int = second\n\
    }\n\
    \n\
    class Cursor(private val items: Array<Pair2>) {\n\
    \x20   private var index = 0\n\
    \x20   operator fun hasNext(): Boolean = index < items.size\n\
    \x20   operator fun next(): Pair2 = items[index++]\n\
    }\n\
    class Series(private val items: Array<Pair2>) { operator fun iterator(): Cursor = Cursor(items) }\n\
    suspend fun mark(value: Int): Int = value\n\
    \n\
    class Loops {\n\
    \x20   fun ordinary(items: Series): Int {\n\
    \x20       var total = 0\n\
    \x20       for ((a, b) in items) {\n\
    \x20           total += a + b\n\
    \x20       }\n\
    \x20       return total\n\
    \x20   }\n\
    \x20   suspend fun suspended(items: Series): Int {\n\
    \x20       var total = 0\n\
    \x20       for ((a, _) in items) total += mark(a)\n\
    \x20       return total\n\
    \x20   }\n\
    }\n";

fn assert_identical(class: &str) {
    common::byte_diff_against_kotlinc_cp(
        "DestructuringLoopLocals",
        SOURCE,
        class,
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("{class} byte-identical to kotlinc: {error}"));
}

#[test]
fn destructuring_loops_hide_their_temporary_like_kotlinc() {
    assert_identical("store/Loops");
    assert_identical("store/Loops$suspended$1");
}
