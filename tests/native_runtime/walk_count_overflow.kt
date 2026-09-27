// Kotlin's answers for `walk_count_overflow.c`: `count()` and `count { }` of a program `Iterable`,
// and how many elements each walk left unread. `Many` is the program class the driver builds a
// stand-in for.
//
// The walks that reach the overflow check are not run here: each reads 2^31 elements, which takes
// the JVM seconds on an idle machine and past the harness's 10-second limit on one `box()` on a
// loaded one. kotlinc 2.4.20 answers them, and the driver pins them, as:
//     Many(2147483647L).count()          2147483647, 0 left
//     Many(2147483648L).count()          threw ArithmeticException: Count overflow has happened.,
//                                        0 left
//     Many(2147483648L).count { true }   threw ArithmeticException: Count overflow has happened.
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
    fun t(label: String, many: Many, walk: (Many) -> Int) {
        val answer = try {
            walk(many).toString()
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label: $answer, ${many.left} left")
    }
    t("count() of 3 elements", Many(3)) { it.count() }
    t("count() of no elements", Many(0)) { it.count() }
    var seen = 0
    t("count { every other } of 5 elements", Many(5)) { m -> m.count { seen++ % 2 == 0 } }
}
