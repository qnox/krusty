fun box(): String {
    val self = ArrayList<Int>()
    self.add(7)
    self.addAll(self)
    if (self != listOf(7, 7)) return "self"
    val other = ArrayList<Int>()
    other.addAll(listOf(4, 5).subList(0, 2))
    if (other != listOf(4, 5)) return "disjoint"
    return "OK"
}
