// `HashSet(elements)` and `LinkedHashSet(elements)` copy any collection: duplicates collapse, the
// copy owns its storage, and a linked copy keeps the order the elements were walked in.
fun box(): String {
    val source = listOf("b", "a", "b", "c")
    val set = HashSet(source)
    if (set.size != 3) return "Fail size: ${set.size}"
    if ("a" !in set || "b" !in set || "c" !in set || "z" in set) return "Fail contains"

    val copy = HashSet(set)
    copy.add("d")
    if (set.size != 3 || copy.size != 4) return "Fail independent: ${set.size} ${copy.size}"

    val linked = LinkedHashSet(source)
    var order = ""
    for (element in linked) order += element
    if (order != "bac") return "Fail order: $order"

    val empty = HashSet(emptyList<String>())
    if (!empty.isEmpty()) return "Fail empty"
    empty.add("x")
    if (empty.size != 1) return "Fail grow"
    return "OK"
}
