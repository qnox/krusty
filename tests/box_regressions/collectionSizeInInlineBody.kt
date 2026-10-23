var collectionCalls = 0

fun values(): Collection<Int> {
    collectionCalls++
    return listOf(2, 3, 9)
}

fun box(): String {
    val copied = values().toTypedArray()
    if (collectionCalls != 1) return "repeated receiver"
    if (copied.size != 3) return "size"
    if (copied[0] != 2 || copied[1] != 3 || copied[2] != 9) return "elements"
    val empty: Collection<Int> = emptyList()
    if (empty.toTypedArray().size != 0) return "empty"
    return "OK"
}
