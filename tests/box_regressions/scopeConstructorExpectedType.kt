// Overloaded scope constructors under an expected type: `P(v: List<T>)` is more specific than
// `P(v: T)`, and the expected type infers the chosen constructor's `T` without changing the
// choice. An expected `P<Any>` still picks `P(v: List<T>)` with `T` = `Any`.

class P<T>(val v: T) {
    var picked = "value"

    constructor(v: List<T>) : this(v.first()) {
        picked = "list"
    }
}

fun box(): String {
    val expected: P<Any> = P(listOf(1))
    val inferred = P(listOf(1))
    return if (expected.picked == "list" && inferred.picked == "list") "OK"
    else "expected ${expected.picked}, inferred ${inferred.picked}"
}
