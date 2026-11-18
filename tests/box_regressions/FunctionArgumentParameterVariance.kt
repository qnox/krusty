interface I
interface J
class A : I, J
class B : I, J
class Apply<T>(val f: (T) -> T, val value: T)
class Sink<T>(val f: (T) -> Unit, val value: T)

var received: A? = A()

object Receiver {
    fun take(value: A?) {
        received = value
    }
}

fun box(): String {
    val f: (Any) -> B = { B() }
    val constructed = Apply(f, A())
    if (constructed.f(constructed.value) !is B) return "Fail: apply"
    var seen = ""
    val g: (Any) -> Unit = { seen = it.toString() }
    val sink = Sink(g, "OK")
    sink.f(sink.value)
    null.let(Receiver::take)
    if (received != null) return "Fail: null"
    return seen
}
