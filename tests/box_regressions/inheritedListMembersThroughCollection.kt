// A member `ArrayList` inherits, such as `containsAll`, answers on a list reached through
// `MutableList`, whether the argument holds one element or several.
fun box(): String {
    val list = mutableListOf("a", "b", "c")
    if (!list.containsAll(listOf("a"))) return "fail one"
    if (!list.containsAll(listOf("a", "c"))) return "fail two"
    if (list.containsAll(listOf("a", "z"))) return "fail missing"
    val collection: Collection<String> = list
    if (!collection.containsAll(listOf("b"))) return "fail collection"
    return "OK"
}
