// MODULE: library
// FILE: Library.kt
package fixture

private class Empty(val active: Boolean)

private class Full(val size: Int)

private fun twice(full: Full): Full = Full(full.size * 2)

open class Holder(active: Boolean) {
    private var state: Any = Empty(active)

    private fun promote(state: Empty): Full = Full(if (state.active) 1 else 2)

    private fun sizeOf(state: Full): Int = state.size

    fun size(): Int {
        val current = state
        val full = if (current is Empty) promote(current) else current as Full
        state = full
        return sizeOf(full) + sizeOf(twice(full))
    }
}

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val active = Holder(true)
    if (active.size() != 3 || active.size() != 3) return "fail: active"
    val idle = Holder(false)
    return if (idle.size() == 6) "OK" else "fail: idle ${idle.size()}"
}
