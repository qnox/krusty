abstract class Counter(private var count: Int) {
    fun next(): Int {
        count += step()
        return count
    }

    protected abstract fun step(): Int
}

inline fun counter(start: Int, crossinline step: () -> Int): Counter =
    object : Counter(start) {
        override fun step(): Int = step()
    }

fun box(): String {
    var calls = 0
    val c = counter(10) { calls += 1; calls * 2 }
    return if (c.next() == 12 && c.next() == 16) "OK" else "fail"
}
