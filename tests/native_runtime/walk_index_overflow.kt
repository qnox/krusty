// Kotlin's answers for `walk_index_overflow.c`: `indexOf`, `forEachIndexed` and `withIndex()` over a
// program `Iterable`, and how many elements each walk left unread. `Many` is the program class the
// driver builds a stand-in for.
//
// The walks that reach the overflow check are not run here: each reads 2^31 elements, which takes
// the JVM seconds on an idle machine and past the harness's 10-second limit on one `box()` on a
// loaded one. kotlinc 2.4.20 answers them, and the driver pins them, as:
//     Many(2147483648L).indexOf("x")     -1, 0 left
//     Many(2147483649L).indexOf("x")     threw ArithmeticException: Index overflow has happened.,
//                                        0 left
// and `forEachIndexed` and `withIndex()` raise the same exception at index 2^31, before the action
// and before fetching the element.
class Many(var left: Long) : Iterable<Any> {
    override fun iterator() = object : Iterator<Any> {
        override fun hasNext() = left > 0
        override fun next(): Any {
            left--
            return Unit
        }
    }
}

fun box(): String = buildString {
    val three = Many(3)
    appendLine("indexOf in 3 elements: ${three.indexOf("x")}, ${three.left} left")
    val indexed = Many(3)
    val seen = StringBuilder()
    indexed.forEachIndexed { i, _ -> seen.append("$i ") }
    appendLine("forEachIndexed over 3 elements: $seen${indexed.left} left")
    val walked = Many(3)
    val first = walked.withIndex().iterator().next()
    appendLine("withIndex() first of 3 elements: ${first.index}, ${walked.left} left")
}
