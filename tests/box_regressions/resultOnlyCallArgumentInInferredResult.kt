// WITH_STDLIB
// A nested call whose result variable only the parameter can fix, inside a declaration whose
// result type is inferred.
@Suppress("UNCHECKED_CAST")
fun <T> produce(): T = "O" as T

var seen = ""

fun consume(value: String): Int {
    seen += value
    return value.length
}

fun inferred() = consume(produce())

val inferredProperty = consume(produce())

fun box(): String {
    val length = inferred() + inferredProperty
    return if (length == 2 && seen == "OO") "OK" else "fail: $length $seen"
}
