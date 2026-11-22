fun box(): String {
    val root = ArrayList<Int>()
    root.add(1)
    root.add(2)
    root.add(3)
    val view = (root as List<Int>).subList(0, 2)
    root.addAll(view)
    if (root != listOf(1, 2, 3, 1, 2)) return "aliased"
    val self = ArrayList<Int>()
    self.add(7)
    self.addAll(self)
    if (self != listOf(7, 7)) return "self"
    val other = ArrayList<Int>()
    other.addAll(listOf(4, 5).subList(0, 2))
    if (other != listOf(4, 5)) return "disjoint"
    return "OK"
}
