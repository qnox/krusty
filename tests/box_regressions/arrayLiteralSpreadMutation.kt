// A spread contributes the elements its array holds when the spread is evaluated: a later operand
// that changes the array does not change the literal.
private fun change(a: Array<String>): String {
    a[0] = "after"
    return "tail"
}

fun box(): String {
    val a = arrayOf("before")
    val result = arrayOf(*a, change(a))
    if (result[0] != "before" || result[1] != "tail") return result[0]
    val b = arrayOf("x", "y")
    val twice = arrayOf(*b, *arrayOf(change(b)), *b)
    if (twice.size != 5) return "size ${twice.size}"
    var joined = ""
    for (value in twice) joined += value
    if (joined != "xytailaftery") return "joined $joined"
    return "OK"
}
