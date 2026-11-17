// `copyInto` copies a range of one array into another through the library body each target
// publishes for it. Overlapping ranges in one array copy as if through a temporary.
fun joined(values: Array<String?>): String {
    var result = ""
    for (value in values) result += value ?: "_"
    return result
}

fun ints(values: IntArray): String {
    var result = ""
    for (value in values) result += value
    return result
}

fun box(): String {
    val source = arrayOf<String?>("a", "b", "c")
    val target = arrayOfNulls<String>(5)
    val returned = source.copyInto(target, 1)
    if (returned !== target) return "fail identity"
    if (joined(target) != "_abc_") return "fail reference: " + joined(target)
    source.copyInto(target, destinationOffset = 3, startIndex = 1, endIndex = 3)
    if (joined(target) != "_abbc") return "fail range: " + joined(target)

    val numbers = intArrayOf(1, 2, 3, 4, 5)
    numbers.copyInto(numbers, destinationOffset = 1, startIndex = 0, endIndex = 4)
    if (ints(numbers) != "11234") return "fail overlap: " + ints(numbers)
    val copied = IntArray(3)
    intArrayOf(7, 8, 9).copyInto(copied)
    if (ints(copied) != "789") return "fail ints: " + ints(copied)
    return "OK"
}
