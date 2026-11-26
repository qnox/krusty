fun box(): String {
    val ints = IntArray(3)
    try { ints.fill(1, 2, 5); return "fail past the end" } catch (e: IndexOutOfBoundsException) {}
    try { ints.fill(1, -1, 2); return "fail before the start" } catch (e: IndexOutOfBoundsException) {}
    try { ints.fill(1, 2, 1); return "fail reversed" } catch (e: IllegalArgumentException) {}
    if (ints.toList() != listOf(0, 0, 0)) return "fail written"
    return "OK"
}
