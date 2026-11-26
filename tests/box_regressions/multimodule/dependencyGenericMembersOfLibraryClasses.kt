// MODULE: lib
// FILE: Holders.kt
package fixture

// A generic member of a generic class reads both its own type parameter and its class's: a call
// binds its own from the call and its class's from the receiver.
class Holder<T>(private val value: T) {
    private fun <U> choose(other: U): T = if (other == null) value else value

    fun <U> pickWith(other: U): T = choose(other)

    fun pick(): T = choose(0)
}

fun <T> pickFrom(holder: Holder<T>): T = holder.pickWith("ignored")

// A generic member of a class private to its library binds its type parameter at each call.
private class Echo {
    fun <T> echo(value: T): T = value
}

fun echoed(value: String): String = Echo().echo(value)

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (Holder("a").pick() != "a") return "pick ${Holder("a").pick()}"
    if (pickFrom(Holder("b")) != "b") return "pickFrom ${pickFrom(Holder("b"))}"
    if (echoed("c") != "c") return "echoed ${echoed("c")}"
    return "OK"
}
