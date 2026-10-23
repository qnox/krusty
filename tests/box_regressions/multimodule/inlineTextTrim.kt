// MODULE: lib
fun trimmed(value: String): String = value.trim()
// MODULE: main(lib)
fun box(): String {
    if (trimmed("  ab  ") != "ab") return "both ends"
    if (trimmed(" \t\r\n API") != "API") return "whitespace"
    if (trimmed("ab") != "ab") return "unchanged"
    if (trimmed(" \t\n ") != "") return "all whitespace"
    if (trimmed("") != "") return "empty"
    if (trimmed("\u00A0x\u00A0") != "x") return "Unicode whitespace"
    return "OK"
}
