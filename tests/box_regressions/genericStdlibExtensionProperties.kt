// The stdlib's generic extension property `Array<out T>.lastIndex` is read through its getter,
// both from this program and from inside the stdlib body of `getOrNull`.
fun <T> lastOf(values: Array<out T>): T? = values.getOrNull(values.lastIndex)

fun box(): String {
    val letters = arrayOf("a", "b", "c")
    if (letters.lastIndex != 2) return "array lastIndex ${letters.lastIndex}"
    if (arrayOf<String>().lastIndex != -1) return "empty array lastIndex"
    if (letters.getOrNull(1) != "b") return "getOrNull inside"
    if (letters.getOrNull(3) != null || letters.getOrNull(-1) != null) return "getOrNull outside"
    if (lastOf(letters) != "c" || lastOf(arrayOf<Int>()) != null) return "generic lastOf"
    return "OK"
}

