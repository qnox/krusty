fun box(): String {
    val values = arrayListOf(1, 2)
    if (values[0] != 1 || values[1] != 2) return "read"
    val previous = values.set(1, 3)
    if (previous != 2 || values[1] != 3) return "write"
    values[0] = 4
    if (values[0] != 4 || values[1] != 3) return "assign"
    val text = arrayListOf("a", "b")
    text[0] = "c"
    if (text[0] != "c" || text[1] != "b") return "strings"
    val erased: Any = values
    values[1] = 9
    if ("$erased" != "[4, 9]") return "through Any: $erased"
    return "OK"
}
