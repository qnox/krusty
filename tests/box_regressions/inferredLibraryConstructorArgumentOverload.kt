// A library constructor whose type arguments are inferred from its own value arguments is argument
// evidence for an overloaded callee: `Pair(1, 2)` is `Pair<Int, Int>`, so it selects `pick(Any?)`
// rather than `pick(Int)`; the same selection lets `println(Pair("x", 2))` reach `println(Any?)`.
fun pick(value: Any?): String = "any"
fun pick(value: Int): String = "int"
fun pick(value: String): String = "string"

fun box(): String {
    val result = pick(Pair(1, 2)) + " " + pick(Triple(1, 2, 3)) + " " + pick(Pair("x", 'c'))
    return if (result == "any any any") "OK" else "fail: $result"
}
