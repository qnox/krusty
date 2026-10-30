// USE_INLINE_SCOPES_NUMBERS
inline fun twice(f: (Int) -> Int): Int {
    val a = f(1)
    return f(a)
}

inline fun plain(x: Int): Int {
    val y = x + 1
    return y * 2
}

inline fun each(xs: List<Int>, f: (Int) -> Unit) {
    for (x in xs) f(x)
}

fun direct(): Int = twice { it + 1 }

fun chained(): Int {
    val r = twice { y -> y * 2 }
    return r + plain(r)
}

fun loops(): Int {
    var sum = 0
    each(listOf(1, 2)) { n -> sum += plain(n) }
    return sum
}

fun box(): String = if (direct() == 3 && chained() == 14 && loops() == 10) "OK" else "fail ${direct()} ${chained()} ${loops()}"
