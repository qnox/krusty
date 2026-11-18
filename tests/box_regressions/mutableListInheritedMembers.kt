// A list the standard library builds answers the interface members its class inherits without
// overriding (`containsAll`, `isEmpty`), called through the interfaces that declare them.
fun containsEvery(collection: Collection<String>, elements: Collection<String>): Boolean =
    collection.containsAll(elements)

fun box(): String {
    val list = mutableListOf("O", "K")
    if (!list.containsAll(listOf("K", "O"))) return "fail containsAll"
    if (list.containsAll(listOf("K", "X"))) return "fail containsAll missing"
    if (list.isEmpty()) return "fail isEmpty"
    if (!containsEvery(list, listOf("O"))) return "fail collection containsAll"
    val items: List<String> = list
    if (items.isEmpty() || !items.containsAll(list)) return "fail list"
    list.addAll(listOf("!", "?"))
    if (list.size != 4 || list.indexOf("?") != 3) return "fail addAll ${list.size}"
    return list[0] + list[1]
}
