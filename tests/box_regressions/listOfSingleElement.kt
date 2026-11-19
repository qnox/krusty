fun box(): String {
    val one = listOf("a")
    val none = listOf<String>()
    return if (one.size == 1 && one[0] == "a" && none.isEmpty()) "OK" else "Fail: ${one.size} ${none.size}"
}
