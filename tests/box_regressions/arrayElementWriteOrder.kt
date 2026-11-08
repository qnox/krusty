// An array element write evaluates the array, the index and the value, in that order, before it
// checks the index: a value that leaves the function first means an out-of-range index never
// reaches the check.
private var log = ""

private fun <T> logged(label: String, value: T): T {
    log += label
    return value
}

private fun write(array: Array<String>, index: Int): String {
    logged("1", array)[logged("2", index)] =
        if (index >= array.size) return "left" else logged("3", "d")
    return "written"
}

fun box(): String {
    val array = arrayOf("a", "b")
    if (write(array, 2) != "left") return "out of range"
    if (log != "12") return "order $log"
    if (write(array, 1) != "written") return "in range"
    if (log != "12123" || array[1] != "d") return "write $log ${array[1]}"
    return "OK"
}
