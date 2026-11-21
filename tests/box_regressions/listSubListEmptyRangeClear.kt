fun box(): String {
    val root = mutableListOf(1)
    val empty = root.subList(0, 0)
    root.add(2)
    try {
        empty.clear()
        return "missing concurrent modification"
    } catch (e: ConcurrentModificationException) {
        return "OK"
    }
}
