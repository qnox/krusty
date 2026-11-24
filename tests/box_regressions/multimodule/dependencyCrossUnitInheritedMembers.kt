// MODULE: lib
// FILE: Labelled.kt
package fixture

interface Named {
    fun label(): String
}

fun <T> render(item: T): String = item.toString()

open class Base {
    // `Mid` implements `Named.label` with this body, which its superclass declares.
    open fun label(): String = render(7)
}

open class Mid : Base(), Named

// MODULE: main(lib)
// FILE: First.kt
package fixture

// The first checked source realizes `Base`, and exports it.
fun firstLabel(): String = Base().label()

// FILE: Consumer.kt
package fixture

// The later one realizes `Mid` on top of the exported `Base`: it inherits `label` from it.
fun box(): String {
    if (firstLabel() != "7") return "first:${firstLabel()}"
    val named: Named = Mid()
    return if (named.label() == "7") "OK" else "second:${named.label()}"
}
