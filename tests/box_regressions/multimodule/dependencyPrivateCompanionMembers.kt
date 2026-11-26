// MODULE: library
// FILE: Library.kt
package fixture

class Buffer(capacity: Int) {
    private var data: IntArray = if (capacity == 0) emptyData else IntArray(capacity)

    fun capacity(): Int = data.size

    fun grow(): Int {
        data = IntArray(grown(data.size))
        return data.size
    }

    internal companion object {
        private val emptyData = IntArray(0)

        private fun grown(size: Int): Int = if (size == 0) minimum else size * 2

        private const val minimum = 4
    }
}

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val empty = Buffer(0)
    if (empty.capacity() != 0) return "fail: empty ${empty.capacity()}"
    if (empty.grow() != 4 || empty.grow() != 8) return "fail: grown ${empty.capacity()}"
    return if (Buffer(3).grow() == 6) "OK" else "fail: sized"
}
