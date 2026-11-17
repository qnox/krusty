// A member of a `Box<Nothing>` overrides its interface's `has(value: T)` with a `Nothing`
// parameter, as the stdlib's empty collections override `contains`. No caller can supply that
// value, yet the member keeps the parameter's slot in the interface's table, which every
// implementation of `Box` shares. Calling it through `Box<String>` fails the JVM's cast to `Void`.
interface Box<out T> {
    fun has(value: @UnsafeVariance T): Boolean
    fun count(): Int
}

object Empty : Box<Nothing> {
    override fun has(value: Nothing): Boolean = false
    override fun count(): Int = 0
}

object Full : Box<String> {
    override fun has(value: String): Boolean = value == "x"
    override fun count(): Int = 1
}

class Exhausted : Throwable()

// A read of a `Nothing` parameter is never reached.
fun never(value: Nothing): Boolean = value

fun box(): String {
    val box: Box<String> = Empty
    if (box.count() != 0 || !Full.has("x")) return "FAIL box"
    var reached = false
    try {
        never(throw Exhausted())
    } catch (e: Exhausted) {
        reached = true
    }
    return if (reached) "OK" else "FAIL never"
}
