@file:OptIn(kotlin.experimental.ExperimentalTypeInference::class)

// A receiver-less callable reference passed where overloads differ only in the result of their
// function parameter (like `flatMap` over Iterable and Sequence results) selects the overload
// whose result the referenced function produces, for a generic and for an overloaded target.
class Source<T>(val value: T)

class Once<T>(val value: T) : Iterable<T> {
    override fun iterator(): Iterator<T> = object : Iterator<T> {
        var taken = false
        override fun hasNext(): Boolean = !taken
        override fun next(): T {
            taken = true
            return value
        }
    }
}

@OverloadResolutionByLambdaReturnType
fun <T, R> Source<T>.expand(transform: (T) -> Iterable<R>): String = "iterable"

@OverloadResolutionByLambdaReturnType
@kotlin.jvm.JvmName("expandSequence")
fun <T, R> Source<T>.expand(transform: (T) -> Sequence<R>): String = "sequence"

fun <T> once(x: T): Iterable<T> = Once(x)

fun twice(x: Int): Iterable<Int> = Once(x)
fun twice(x: String): Sequence<String> = Sequence { Once(x).iterator() }

fun box(): String {
    val generic = Source(1).expand(::once)
    if (generic != "iterable") return "FAIL generic $generic"
    val picked = Source(1).expand(::twice)
    if (picked != "iterable") return "FAIL overloaded $picked"
    val sequence = Source("a").expand(::twice)
    if (sequence != "sequence") return "FAIL sequence $sequence"
    return "OK"
}
