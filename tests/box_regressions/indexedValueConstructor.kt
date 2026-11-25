fun box(): String {
    val made = IndexedValue(2, "b")
    if (made.index != 2 || made.value != "b") return "fail members"
    val walked = listOf("x", "y", "b").withIndex().last()
    if (made != walked) return "fail equal"
    if ("$made" != "IndexedValue(index=2, value=b)") return "fail render: $made"
    return "OK"
}
