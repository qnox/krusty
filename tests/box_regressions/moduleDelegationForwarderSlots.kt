interface Greeter {
    fun greet(name: String): String
    fun count(): Int
}

interface SpecificGreeter : Greeter {
    override fun greet(name: String): String
}

var delegatedCalls = 0

class PlainGreeter : SpecificGreeter {
    override fun greet(name: String): String {
        delegatedCalls++
        return "hi " + name
    }
    override fun count(): Int = 4
}

open class GreetingBase {
    open fun greet(name: String): String = "base " + name
    open fun kind(): String = "base"
}

open class GreetingWrapper(inner: SpecificGreeter) : GreetingBase(), SpecificGreeter by inner {
    fun own(): String = greet("own") + ":" + count()
}

class SpecialGreeting(inner: SpecificGreeter) : GreetingWrapper(inner) {
    override fun kind(): String = "special"
}

fun throughBase(value: GreetingBase, name: String): String = value.greet(name) + ":" + value.kind()
fun throughInterface(value: Greeter, name: String): String = value.greet(name) + ":" + value.count()
fun throughWrapper(value: GreetingWrapper, name: String): String =
    value.greet(name) + ":" + value.count() + ":" + value.own()

fun box(): String {
    val wrapper = GreetingWrapper(PlainGreeter())
    val special = SpecialGreeting(PlainGreeter())
    if (throughBase(wrapper, "a") != "hi a:base") return "base forwarder slot"
    if (throughBase(special, "b") != "hi b:special") return "subclass forwarder slot"
    if (throughInterface(wrapper, "c") != "hi c:4") return "interface forwarder"
    if (throughInterface(special, "d") != "hi d:4") return "inherited interface forwarder"
    if (throughWrapper(wrapper, "e") != "hi e:4:hi own:4") return "class forwarder calls"
    if (throughWrapper(special, "f") != "hi f:4:hi own:4") return "subclass forwarder calls"
    if (delegatedCalls != 8) return "delegate evaluation count: " + delegatedCalls
    return "OK"
}
