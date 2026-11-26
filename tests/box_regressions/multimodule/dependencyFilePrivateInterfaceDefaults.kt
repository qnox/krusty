// MODULE: library
// FILE: Library.kt
package fixture

interface Named {
    fun name(): String = "named"

    fun greeting(): String = "hello " + name()
}

// A class private to its file implements an interface without overriding its defaults. A call on
// the class and a call dispatched through the interface both run the default.
private class Silent(private val text: String) : Named {
    fun describe(): String = text + "/" + name() + "/" + nameOf(this)
}

private fun nameOf(named: Named): String = named.name()

// Only dispatch through the interface reaches this default.
private class Greeter : Named

private fun greetingOf(named: Named): String = named.greeting()

fun describe(text: String): String = Silent(text).describe() + "/" + greetingOf(Greeter())

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val result = describe("abc")
    return if (result == "abc/named/named/hello named") "OK" else result
}
