private class Name(val s: String) : Comparable<String> by s

fun order(first: Comparable<String>, second: String): Int = first.compareTo(second)

fun box(): String {
    val name = Name("b")
    if (name.compareTo("a") <= 0) return "after: ${name.compareTo("a")}"
    if (order(name, "c") >= 0) return "before: ${order(name, "c")}"
    if (order(name, "b") != 0) return "same: ${order(name, "b")}"
    return "OK"
}
