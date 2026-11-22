fun box(): String {
    val a = ArrayList<Int>()
    a.add(7)
    a.add(8)
    if (a != listOf(7, 8)) return "equals list"
    if (a == listOf(8, 7)) return "order"
    if (a == listOf(7, 8, 9)) return "length"
    return "OK"
}
