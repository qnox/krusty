// `Array<out T>.lastIndexOf` counts `for (index in indices.reversed())` downwards from the last index.
fun box(): String {
    val values = arrayOf("a", "b", "c", "b")
    if (values.lastIndexOf("b") != 3) return "last match ${values.lastIndexOf("b")}"
    if (values.lastIndexOf("a") != 0) return "first element"
    if (values.lastIndexOf("z") != -1) return "missing"
    val nullable = arrayOf<String?>(null, "a", null)
    if (nullable.lastIndexOf(null) != 2) return "null element"
    if (arrayOf<String>().lastIndexOf("a") != -1) return "empty"
    return "OK"
}
