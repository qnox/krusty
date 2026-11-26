fun box(): String {
    val chars = charArrayOf('a', 'b', 'c', 'd')
    if (chars.concatToString() != "abcd") return "fail whole"
    if (chars.concatToString(1, 3) != "bc") return "fail range"
    if (CharArray(0).concatToString() != "") return "fail empty"
    return "OK"
}
