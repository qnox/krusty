// A builder's type variable fixed only through invoking a receiver-typed function value on a
// receiver of that variable's type: the builder call has no expected type (`.result()` follows it),
// so the constraint `Forward<T> <: Collector<R>` is what infers T = R.

interface Sink<E> {
    fun send(e: E)
}

class Box<T>(val value: T) {
    fun result(): T = value
}

class Joined : Sink<String> {
    var text = ""
    override fun send(e: String) {
        text += e
    }
}

fun <T> build(block: Sink<T>.() -> Unit): Box<Sink<T>> {
    val sink = Joined()
    @Suppress("UNCHECKED_CAST")
    val typed = sink as Sink<T>
    typed.block()
    return Box(typed)
}

interface Collector<R> {
    fun out(r: R)
}

class Forward<R>(val sink: Sink<R>) : Collector<R> {
    override fun out(r: R) = sink.send(r)
}

fun <T, R> List<T>.transform(action: Collector<R>.(T) -> Unit): Sink<R> {
    return build {
        val collector = Forward(this)
        for (item in this@transform) {
            collector.action(item)
        }
    }.result()
}

fun box(): String {
    val sink = listOf("O", "K").transform<String, String> { out(it) }
    return (sink as Joined).text
}
