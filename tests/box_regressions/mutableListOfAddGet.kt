// A list built by the standard library's factory is read back through its interface members.
fun box(): String {
    val list = mutableListOf<String>()
    list.add("O")
    list.add("K")
    if (list.size != 2) return "fail size ${list.size}"
    return list[0] + list[1]
}
