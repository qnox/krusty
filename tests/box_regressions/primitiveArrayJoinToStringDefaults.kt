fun box(): String {
    val ints = intArrayOf(1, 2, 3).joinToString(";")
    if (ints != "1;2;3") return "ints: $ints"
    val bracketed = intArrayOf(4, 5).joinToString(prefix = "[", postfix = "]")
    if (bracketed != "[4, 5]") return "bracketed: $bracketed"
    return "OK"
}
