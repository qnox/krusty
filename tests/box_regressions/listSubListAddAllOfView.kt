// A list appending a range of itself differs by platform: kotlinc-native and kotlinc-wasm grow the
// list before they read the range and raise ConcurrentModificationException. That program is the
// ArrayListAddAllGrowsBeforeReadingItsArgument entry of tests/kotlinc_defects_e2e.rs.
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
