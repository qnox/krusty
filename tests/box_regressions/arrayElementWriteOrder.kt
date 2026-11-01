// An array element write evaluates the array, the index and the value, in that order, before it
// checks the index.
private var log = ""

private fun <T> logged(label: String, value: T): T {
    log += label
    return value
}

fun box(): String {
    val array = arrayOf("a", "b")
    try {
        logged("1", array)[logged("2", 2)] = logged("3", "c")
        return "no exception"
    } catch (e: Throwable) {
    }
    if (log != "123") return "order $log"
    logged("4", array)[logged("5", 1)] = logged("6", "d")
    if (log != "123456" || array[1] != "d") return "write $log ${array[1]}"
    return "OK"
}
