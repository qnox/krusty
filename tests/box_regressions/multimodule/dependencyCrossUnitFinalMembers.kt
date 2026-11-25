// MODULE: lib
// FILE: Box.kt
package fixture

class Box {
    fun first(): Int = 1
    fun second(): Int = 2
}

// MODULE: main(lib)
// FILE: First.kt
package fixture

// The first checked source realizes `Box` with the member it calls.
fun firstValue(): Int = Box().first()

// FILE: Consumer.kt
package fixture

// The later one calls another final member of the class the first realized: the class keeps its
// one owner, and the member runs from this source's support unit.
fun box(): String {
    if (firstValue() != 1) return "first:${firstValue()}"
    val second = Box().second()
    return if (second == 2) "OK" else "second:$second"
}
