fun box(): String {
    val growable = mutableListOf(1, 2, 3, 4, 5)
    val outer = growable.subList(0, 4)
    val walk = outer.iterator()
    walk.next()
    outer.add(0, 9)
    try {
        walk.next()
        return "fail walk"
    } catch (e: ConcurrentModificationException) {
    }
    val inner = outer.subList(1, 3)
    outer.add(0, 8)
    try {
        inner[0]
        return "fail inner"
    } catch (e: ConcurrentModificationException) {
    }
    return "OK"
}
