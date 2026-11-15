// FILE: Early.kt
// Calls members of classes a later file declares, leaving arguments to the defaults they declare.
fun early(): String {
    val base: Base = Base()
    val derived: Base = Derived()
    return base.label() + derived.label() + base.label("x") + derived.label(suffix = "?") +
        (object : Marked {}).mark()
}

// FILE: Classes.kt
open class Base {
    open fun label(prefix: String = "b", suffix: String = "."): String = prefix + suffix
}

class Derived : Base() {
    override fun label(prefix: String, suffix: String): String = prefix + prefix + suffix + "!"
}

interface Marked {
    fun mark(tag: String = "m"): String = "<" + tag + ">"
}

// FILE: Late.kt
fun box(): String {
    val result = early()
    if (result != "b.bb.!x.bb?!<m>") return "Fail: " + result
    return "OK"
}
