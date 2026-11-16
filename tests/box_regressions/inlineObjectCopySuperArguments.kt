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

inline fun shifted(start: Int, crossinline step: () -> Int): Counter =
    object : Counter(start + 1) {
        override fun step(): Int = step()
    }

fun box(): String {
    val counter = counter(10) { 2 }
    if (counter.next() != 12 || counter.next() != 14) return "fail: captured start"
    val shifted = shifted(10) { 3 }
    if (shifted.next() != 14 || shifted.next() != 17) return "fail: forwarded start"
    return "OK"
}
