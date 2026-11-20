// WITH_STDLIB
// MODULE: lib
// FILE: Forms.kt
package fixture

// Library bodies the target lowers from the library's own code: a call of a generic private
// top-level function, a `super` call of a public open member, a nested class reading its outer
// class's private property, a generic private member, and named arguments out of order.
private fun <T> firstOf(values: List<T>): T = values[0]

fun first(values: List<String>): String = firstOf(values)

open class Base {
    open fun describe(): Int = 1
}

class Derived : Base() {
    override fun describe(): Int = 10 + super.describe()
}

fun described(): Int = Derived().describe()

class Outer(private val size: Int) {
    private class Reader(val outer: Outer) {
        fun read(): Int = outer.size
    }

    fun read(): Int = Reader(this).read()
}

fun outerSize(size: Int): Int = Outer(size).read()

class Holder(private val value: String) {
    private fun <T> echo(item: T): T = item

    fun get(): String = echo(value)
}

fun held(value: String): String = Holder(value).get()

fun window(from: Int, to: Int): Int = to - from

fun reordered(start: Int, end: Int): Int = window(to = end + 1, from = start * 2)

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (first(listOf("a", "b")) != "a") return "first"
    if (described() != 11) return "super"
    if (outerSize(3) != 3) return "outer"
    if (held("v") != "v") return "held"
    if (reordered(2, 9) != 6) return "reordered"
    return "OK"
}
