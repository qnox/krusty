// MODULE: lib
// FILE: Counter.kt
package fixture

// A public final class whose constructor, function and property accessors a consumer target
// lowers from the library's bodies, installing every member with the class. The private `steps`
// stays private to the class's own members.
class Counter(start: Int) {
    var count: Int = start
    private var steps = 0

    val taken: Int
        get() = steps

    fun add(step: Int): Int {
        count += step
        steps += 1
        return count
    }
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val counter = Counter(1)
    if (counter.add(2) != 3) return "add"
    counter.count = 10
    if (counter.count != 10) return "set"
    if (counter.add(1) != 11) return "after set"
    if (counter.taken != 2) return "taken"
    return "OK"
}
