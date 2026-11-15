// FILE: caller.kt
// Calls members another file declares, leaving arguments to their defaults: a final member whose
// default reads the receiver and an earlier parameter, and an override that inherits its defaults
// from the member it overrides and is reached through the base's dispatch.
fun box(): String {
    val greeter = Greeter("O")
    if (greeter.greet() != "OK") return "Fail final: " + greeter.greet()
    if (greeter.greet(suffix = "!") != "O!") return "Fail named: " + greeter.greet(suffix = "!")
    if (greeter.greet("K", "explicit") != "explicit") return "Fail explicit"
    val base: Base = Derived()
    if (base.render() != "derived:1:2") return "Fail dispatch: " + base.render()
    if (Derived().render(b = 5) != "derived:1:5") return "Fail inherited: " + Derived().render(b = 5)
    if (Base().render(3) != "base:3:4") return "Fail base: " + Base().render(3)
    return "OK"
}

// FILE: declarations.kt
class Greeter(val prefix: String) {
    fun greet(suffix: String = "K", tail: String = prefix + suffix): String = tail
}

open class Base {
    open fun render(a: Int = 1, b: Int = a + 1): String = "base:" + a + ":" + b
}

class Derived : Base() {
    override fun render(a: Int, b: Int): String = "derived:" + a + ":" + b
}
