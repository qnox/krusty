package fixture

interface Counter {
    fun count(): Int
}

fun counter(start: Int): Counter {
    val step = start * 2
    return object : Counter {
        override fun count(): Int = start + step
    }
}

fun countOf(start: Int): Int = counter(start).count()

fun box(): String {
    val direct = countOf(3)
    val returned = counter(4).count()
    return if (direct == 9 && returned == 12) "OK" else "FAIL $direct $returned"
}
