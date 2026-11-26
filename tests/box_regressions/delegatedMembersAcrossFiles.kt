// FILE: EarlyCaller.kt
// Calls members a class realizes by interface delegation, declared in a later file: each
// forwarder sits at the slot the module-wide plan numbers for it.
fun early(): String {
    val wrapper = makeWrapper()
    wrapper.count = 4
    return wrapper.greet("a") + ":" + wrapper.count + ":" + wrapper.own() + ":" + wrapper.kind()
}

fun earlyThroughBase(): String = makeWrapper().kind() + makeWrapper().greet("b")

// FILE: Wrappers.kt
interface Greeter {
    fun greet(name: String): String
    var count: Int
}

class Plain : Greeter {
    override fun greet(name: String): String = "hi " + name
    override var count: Int = 0
}

open class Base {
    open fun kind(): String = "base"
}

open class Wrapper(inner: Greeter) : Base(), Greeter by inner {
    fun own(): String = "own" + count
}

class Special(inner: Greeter) : Wrapper(inner) {
    override fun kind(): String = "special"
}

fun makeWrapper(): Wrapper = Special(Plain())

// FILE: LateCaller.kt
fun late(wrapper: Wrapper): String = wrapper.greet("c") + wrapper.count

fun box(): String {
    if (early() != "hi a:4:own4:special") return "Fail early: " + early()
    if (earlyThroughBase() != "specialhi b") return "Fail base: " + earlyThroughBase()
    if (late(makeWrapper()) != "hi c0") return "Fail late: " + late(makeWrapper())
    return "OK"
}
