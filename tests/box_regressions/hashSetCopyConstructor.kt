// `HashSet(elements)` and `LinkedHashSet(elements)` copy any collection: duplicates collapse, the
// copy owns its storage, and a linked copy keeps the order the elements were walked in. An exception
// from the source's `hasNext` ends the copy before `next` runs.
var walked = ""

val raising = object : AbstractCollection<String>() {
    override val size: Int get() = 1
    override fun iterator(): Iterator<String> = object : Iterator<String> {
        override fun hasNext(): Boolean {
            walked += "hasNext;"
            throw IllegalStateException("from hasNext")
        }

        override fun next(): String {
            walked += "next;"
            return "x"
        }
    }
}

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

    val caught = try {
        HashSet(raising)
        "no exception"
    } catch (e: IllegalStateException) {
        e.message
    }
    if (caught != "from hasNext" || walked != "hasNext;") return "Fail raising: $caught $walked"
    return "OK"
}
