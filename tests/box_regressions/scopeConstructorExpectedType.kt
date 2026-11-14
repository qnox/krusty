// Overloaded scope constructors under an expected type: `P(v: Holder<T>)` is more specific than
// `P(v: T)`, and the expected type infers the chosen constructor's `T` without changing the
// choice. An expected `P<Any>` still picks `P(v: Holder<T>)` with `T` = `Any`: the covariant
// `Holder<Int>` is a `Holder<Any>`.

class Holder<out T>(val item: T)

class P<T>(val v: T) {
    var picked = "value"

    constructor(v: Holder<T>) : this(v.item) {
        picked = "holder"
    }
}

fun box(): String {
    val expected: P<Any> = P(Holder(1))
    val inferred = P(Holder(1))
    return if (expected.picked == "holder" && inferred.picked == "holder") "OK"
    else "expected ${expected.picked}, inferred ${inferred.picked}"
}
