// MODULE: lib
// FILE: Buffer.kt
package fixture

// A final library class whose secondary constructors delegate to its primary one, as the
// standard library's `ArrayList()` delegates to `ArrayList(initialCapacity)`. A consumer target
// lowers each construction from the library's bodies: the delegated primary initialization runs
// first, then the secondary constructor's own body.
class Buffer(capacity: Int) {
    val capacity: Int = capacity
    private var length = 0

    constructor() : this(10)

    constructor(first: Int, second: Int) : this(first + second) {
        length = first
    }

    val size: Int
        get() = length

    fun push(): Int {
        length += 1
        return length
    }
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val empty = Buffer()
    if (empty.capacity != 10) return "default capacity ${empty.capacity}"
    if (empty.size != 0) return "default size ${empty.size}"
    if (empty.push() != 1) return "push"
    val sized = Buffer(4)
    if (sized.capacity != 4 || sized.size != 0) return "primary"
    val pair = Buffer(2, 3)
    if (pair.capacity != 5) return "pair capacity ${pair.capacity}"
    if (pair.size != 2) return "pair size ${pair.size}"
    return "OK"
}
