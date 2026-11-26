// MODULE: lib
// The callee places its `crossinline` argument in an object it creates, a class of its own. A
// private member function of the caller's class is not visible from that class, so kotlinc reaches
// it from the argument's body through the class's synthetic `access$<name>(owner, …)` accessor,
// whether the argument is a bound callable reference or a lambda.
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

// MODULE: main(lib)
class Threshold(private val limit: Int) {
    private fun over(value: Int) = value > limit

    val byReference: Source<Boolean> = Just(5).map(::over)
    val byLambda: Source<Boolean> = Just(1).map { over(it) }
}

fun box(): String {
    val threshold = Threshold(3)
    if (!threshold.byReference.get()) return "bound reference"
    if (threshold.byLambda.get()) return "lambda"
    return "OK"
}
