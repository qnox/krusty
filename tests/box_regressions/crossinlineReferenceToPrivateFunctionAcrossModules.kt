// MODULE: lib
// The callee places its `crossinline` argument in an object it creates, a class of its own. A
// caller's private function is not visible from that class, so kotlinc reaches it from the
// argument's body through the caller's synthetic accessor, whether the argument is a callable
// reference or a lambda, and so does a callable reference that body passes on to an ordinary inline
// parameter.
interface Source<T> {
    fun get(): T
}

class Just<T>(private val value: T) : Source<T> {
    override fun get(): T = value
}

inline fun <T, R> Source<T>.map(crossinline transform: (T) -> R): Source<R> {
    val upstream = this
    return object : Source<R> {
        override fun get(): R = transform(upstream.get())
    }
}

inline fun <R> call(f: () -> R): R = f()

// MODULE: main(lib)
private fun Int.isPositive() = this > 0

private fun describe(value: Int) = if (value > 3) "large" else "small"

private fun label() = "label"

class Watcher(value: Int) {
    val positive: Source<Boolean> = Just(value).map(Int::isPositive)
}

fun box(): String {
    if (!Watcher(2).positive.get()) return "extension reference"
    if (Watcher(-1).positive.get()) return "extension reference, not positive"
    val described = Just(3).map(::describe).get()
    if (described != "small") return "reference: $described"
    val viaLambda = Just(4).map { describe(it) }.get()
    if (viaLambda != "large") return "lambda: $viaLambda"
    val nested = Just(5).map { call(::label) }.get()
    if (nested != "label") return "nested reference: $nested"
    return "OK"
}
