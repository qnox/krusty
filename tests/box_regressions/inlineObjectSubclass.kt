package fixture

// An inline function whose body creates an object extending an abstract class, as the standard
// library's `Delegates.observable` extends `ObservableProperty`. The object overrides a protected
// member the class's final member calls, reads a parameter of the function, and the class keeps
// private state.
abstract class Counter(private var count: Int) {
    fun next(): Int {
        count += step()
        return count
    }

    protected abstract fun step(): Int
}

inline fun counter(start: Int, step: Int): Counter =
    object : Counter(start) {
        override fun step(): Int = step
    }

fun box(): String {
    val counter = counter(10, 2)
    if (counter.next() != 12) return "first"
    if (counter.next() != 14) return "second"
    return "OK"
}
