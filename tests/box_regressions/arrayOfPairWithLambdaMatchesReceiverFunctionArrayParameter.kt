// A lambda inside `arrayOf("b" to {})` is postponed: its provisional `() -> Unit` shape is not
// evidence against an `Array<Pair<String, Sh.() -> Unit>>` parameter, with or without a receiver.

class Sh

fun Sh.chainArray(branches: Array<Pair<String, Sh.() -> Unit>>): Int = branches.size

fun takeArray(branches: Array<Pair<String, Sh.() -> Unit>>): Int = branches.size

fun box(): String {
    val total = Sh().chainArray(arrayOf("b" to {})) + takeArray(arrayOf("c" to {}, "d" to {}))
    return if (total == 3) "OK" else "FAIL: $total"
}
