fun box(): String {
    val root = mutableListOf(1)
    val walk = root.iterator()
    root.clear()
    try {
        walk.next()
        return "stale next at the end"
    } catch (e: ConcurrentModificationException) {
    }

    val exhausted = mutableListOf(1).iterator()
    exhausted.next()
    try {
        exhausted.next()
        return "next past the end"
    } catch (e: NoSuchElementException) {
    }

    val growable = mutableListOf(1, 2, 3)
    val range = growable.subList(0, 2)
    val ranged = range.iterator()
    ranged.next()
    ranged.next()
    range.add(9)
    try {
        ranged.next()
        return "stale next at the end of a range"
    } catch (e: ConcurrentModificationException) {
    }

    val outer = mutableListOf(1)
    val view = outer.subList(0, 1)
    val sub = view.iterator()
    sub.next()
    outer.add(2)
    try {
        sub.next()
        return "stale range at the end after a change of its root"
    } catch (e: ConcurrentModificationException) {
    }

    val whole = mutableListOf(1, 2, 3).subList(0, 2).iterator()
    whole.next()
    whole.next()
    try {
        whole.next()
        return "next past the end of a range"
    } catch (e: NoSuchElementException) {
    }
    return "OK"
}
