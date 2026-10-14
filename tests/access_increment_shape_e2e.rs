//! A member or index `++`/`--` keeps kotlinc's shape: a postfix update stores the old value in a
//! temporary and writes `temporary.inc()`, a prefix update writes `read.inc()` and reads again, and
//! only an operand that is not a constant or an immutable value is bound once. A compound index
//! assignment binds its operands by the same rule, and a counted loop's variable is not immutable.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Step(val n: Int) {\n\
    operator fun inc() = Step(n + 1)\n\
    operator fun dec() = Step(n - 1)\n\
}\n\
\n\
class Counter {\n\
    var count = 0\n\
    var total = 0L\n\
    var step = Step(0)\n\
\n\
    fun bump(): Int {\n\
        count++\n\
        --count\n\
        return count++\n\
    }\n\
}\n\
\n\
class Grid(var first: Int, var second: Int) {\n\
    operator fun get(i: Int): Int = if (i == 0) first else second\n\
    operator fun set(i: Int, value: Int) {\n\
        if (i == 0) first = value else second = value\n\
    }\n\
}\n\
\n\
fun make(c: Counter): Counter = c\n\
\n\
fun members(c: Counter): Long {\n\
    c.count++\n\
    ++c.count\n\
    c.total--\n\
    val a = c.count++\n\
    val b = --c.total\n\
    val alias = c\n\
    alias.count++\n\
    make(c).count++\n\
    ++make(c).count\n\
    return a + b + c.count\n\
}\n\
\n\
fun steps(c: Counter): Int {\n\
    ++c.step\n\
    val old = c.step++\n\
    val now = --c.step\n\
    return old.n + now.n\n\
}\n\
\n\
fun cells(g: Grid, i: Int): Int {\n\
    val a = g[i]++\n\
    val b = ++g[i + 1]\n\
    val c = g[0]--\n\
    return a + b + c\n\
}\n\
\n\
fun compound(g: IntArray, i: Int): Int {\n\
    g[i + 1] += 2\n\
    g[0] -= 1\n\
    for (j in 0..1) g[j] += 3\n\
    for (k in 0 until 2) g[k]++\n\
    return g[i]\n\
}\n\
\n\
fun lifted(g: IntArray, i: Int, c: Counter): Int {\n\
    val f = {\n\
        g[i] += 3\n\
        c.count++\n\
    }\n\
    f()\n\
    return g[i]\n\
}\n\
";

#[test]
fn member_and_index_updates_are_byte_identical_to_kotlinc() {
    for class in ["store/AccessIncrementsKt", "store/Counter"] {
        common::byte_diff_against_kotlinc_cp(
            "AccessIncrements",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
