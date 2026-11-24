// An anonymous object extending an abstract class another file of the module declares. No header
// names the object, so no module-wide plan numbers it: it extends the table that file laid out,
// replacing the protected member it overrides and keeping the final member and private state.
// FILE: Base.kt
abstract class Base(private var count: Int) {
    fun next(): Int {
        count += step()
        return count
    }

    protected abstract fun step(): Int
}

// FILE: Use.kt
fun make(step: Int): Base = object : Base(10) {
    override fun step(): Int = step
}

fun box(): String {
    val counter = make(2)
    if (counter.next() != 12) return "first"
    return if (counter.next() == 14) "OK" else "second"
}
