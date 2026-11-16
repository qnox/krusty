abstract class Counter(private var count: Int) {
    fun next(): Int {
        count += step()
        return count
    }

    protected abstract fun step(): Int
}

private inline fun counter(start: Int, step: Int): Counter =
    object : Counter(start) {
        override fun step(): Int = step
    }

fun box(): String {
    val counter = counter(10, 2)
    return if (counter.next() == 12 && counter.next() == 14) "OK" else "fail"
}
