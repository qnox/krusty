// `Array<out T>.indexOf` counts `for (index in indices)` over a generic array's indices.
fun box(): String {
    val values = arrayOf("a", "b", "c", "b")
    if (values.indexOf("c") != 2) return "indexOf ${values.indexOf("c")}"
    if (values.indexOf("z") != -1) return "missing"
    if (values.indexOf("b") != 1) return "first match"
    val nullable = arrayOf<String?>("a", null, "c")
    if (nullable.indexOf(null) != 1) return "null element"
    return "OK"
}
