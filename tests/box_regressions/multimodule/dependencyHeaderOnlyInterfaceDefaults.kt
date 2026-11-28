// MODULE: library
// FILE: Library.kt
package fixture

interface Shape {
    fun area(): Int

    fun describe(): String = "shape " + label(area())
}

fun label(value: Int): String = "#$value"

// A public class overriding the interface default makes the default a declaration the consumer
// knows of, though nothing calls it.
class Square : Shape {
    override fun area(): Int = 4

    override fun describe(): String = "square"
}

// A library-private class inherits the default, but no call dispatches to it: neither the
// consumer nor a library body calls `describe` on a `Dot`.
private class Dot : Shape {
    override fun area(): Int = 1
}

fun dot(): Shape = Dot()

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val result = "" + Square().area() + "/" + dot().area()
    return if (result == "4/1") "OK" else result
}
