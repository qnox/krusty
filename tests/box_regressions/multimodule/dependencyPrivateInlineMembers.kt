// MODULE: library
// FILE: Library.kt
package fixture

class Pool(private var state: Int) {
    @Suppress("NOTHING_TO_INLINE")
    private inline fun Int.isClosed(): Boolean = this and 1 != 0

    private inline fun tryForbid(): Int {
        val current = state
        if (current.isClosed()) return 0
        state = current or 1
        return current
    }

    private inline fun kept(noinline step: (Int) -> Int): (Int) -> Int = step

    fun close(): Int = tryForbid()

    fun describe(): String = if (state.isClosed()) "closed" else "open"

    fun tripled(): Int = kept { it * 3 }(state)
}

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val pool = Pool(4)
    if (pool.describe() != "open") return "fail: ${pool.describe()} before closing"
    if (pool.tripled() != 12) return "fail: tripled ${pool.tripled()}"
    if (pool.close() != 4) return "fail: first close"
    if (pool.describe() != "closed") return "fail: ${pool.describe()} after closing"
    if (pool.close() != 0) return "fail: second close"
    return if (pool.tripled() == 15) "OK" else "fail: tripled ${pool.tripled()} after closing"
}
