fun box(): String {
    val first = ArrayList<String>()
    first.add("e")
    val second = ArrayList<String>()
    second.add("e")
    if (first != second) return "fail equal"
    if (first.hashCode() != second.hashCode()) return "fail hash"
    if (first.hashCode() != 132) return "fail value ${first.hashCode()}"
    if (listOf("e").hashCode() != first.hashCode()) return "fail listOf"
    val other = ArrayList<String>()
    other.add("f")
    if (first.hashCode() == other.hashCode()) return "fail distinct"
    val empty = ArrayList<String>()
    if (empty.hashCode() != 1) return "fail empty"
    return "OK"
}
