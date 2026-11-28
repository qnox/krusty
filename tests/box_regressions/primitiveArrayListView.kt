// WITH_STDLIB

// A primitive array's `asList()` view is a stdlib local class whose members override generic
// collection members with primitive-carried ones; printing it reaches them through the interface.
fun box(): String {
    val list: List<Int> = IntArray(2) { it + 5 }.asList()
    val collection: Collection<Int> = list
    val any: Any = list
    println(list)
    if (collection.size != 2 || !collection.contains(6)) return "contains"
    if (any.toString() != "[5, 6]") return any.toString()
    return "OK"
}
