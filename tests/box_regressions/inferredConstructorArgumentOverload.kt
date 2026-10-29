// A generic constructor call whose type arguments are inferred from its own value arguments has a
// result fixed by those inputs, exactly like a generic function call. Passed to an overloaded
// callee, `Cell("x")` is `Cell<String>` evidence: it selects `pick(Any?)` and never fits
// `pick(Int)` or `pick(String)`. The library form is `inferredLibraryConstructorArgumentOverload.kt`.
class Cell<T>(val value: T)

class Labeled<K, V>(val key: K, val value: V)

fun pick(value: Any?): String = "any"
fun pick(value: Int): String = "int"
fun pick(value: String): String = "string"

fun box(): String {
    val result = pick(Cell("x")) + " " + pick(Labeled("x", Cell("y"))) + " " + pick(Cell("x").value)
    val expected = "any any string"
    return if (result == expected) "OK" else "fail: $result"
}
