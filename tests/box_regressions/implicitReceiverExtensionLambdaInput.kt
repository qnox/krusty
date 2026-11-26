// A bare call to an implicit receiver's extension: the receiver fixes `T = E` for the lambda's
// unannotated input while its annotated input and result solve `R`.
class Step<A, B>(val previous: A, val current: B)

class Chain<T>(val head: T, val tail: Chain<T>?)

fun <T, R> Chain<T>.fold(initial: R, operation: (R, T) -> R): R {
    var accumulator = operation(initial, head)
    var rest = tail
    while (rest != null) {
        accumulator = operation(accumulator, rest.head)
        rest = rest.tail
    }
    return accumulator
}

fun <E> Chain<E>.lastStep(): Step<E?, E> =
    fold(null) { previous: Step<E?, E>?, current ->
        Step(previous?.current, current)
    }!!

fun <E> Chain<E>.lastStepOrNull(): Step<E?, E>? {
    val step = fold(null) { previous: Step<E?, E>?, current -> Step(previous?.current, current) }
    return step
}

fun box(): String {
    val chain = Chain(1, Chain(2, Chain(3, null)))
    val last = chain.lastStep()
    if (last.previous != 2 || last.current != 3) return "Fail expression body"
    val single = Chain(4, null).lastStepOrNull()
    if (single == null || single.previous != null || single.current != 4) return "Fail block body"
    return "OK"
}
