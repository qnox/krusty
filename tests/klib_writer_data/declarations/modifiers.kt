package lib.sub
suspend fun s(x: Int): Int = x
fun takes(block: suspend () -> Int) {}
tailrec fun t(n: Int): Int = if (n == 0) 0 else t(n - 1)
operator fun String.unaryMinus(): String = this
infix fun Int.pl(o: Int): Int = this + o
