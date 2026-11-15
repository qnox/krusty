// `Array.iterator()` is an ordinary member of the final `Array` class. A target that does not
// realize it itself runs its library body on the array value.
fun joined(values: Array<String>): String {
    val iterator = values.iterator()
    var result = ""
    while (iterator.hasNext()) result += iterator.next()
    return result
}

fun <T> count(values: Array<out T>): Int {
    var count = 0
    val iterator = values.iterator()
    while (iterator.hasNext()) {
        iterator.next()
        count++
    }
    return count
}

fun box(): String {
    if (joined(arrayOf("1", "2", "3")) != "123") return "fail joined"
    if (count(arrayOf(1, 2, 3)) != 3) return "fail ints"
    if (count(arrayOf<String>()) != 0) return "fail empty"
    if (count(arrayOf("a", "b")) != 2) return "fail count"
    val iterator = arrayOf("O", "K").iterator()
    var result = ""
    while (iterator.hasNext()) result += iterator.next()
    if (iterator.hasNext()) return "fail exhausted"
    return result
}
