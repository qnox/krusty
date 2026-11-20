// WITH_STDLIB

fun box(): String {
    val text = "hello world"
    if (text.indexOf("o w") != 4) return "FAIL indexOf string: ${text.indexOf("o w")}"
    if (text.indexOf("o", 5) != 7) return "FAIL indexOf from: ${text.indexOf("o", 5)}"
    if (text.indexOf("zz") != -1) return "FAIL missing: ${text.indexOf("zz")}"
    if (text.indexOf("l", -5) != 2) return "FAIL negative start: ${text.indexOf("l", -5)}"
    if (text.indexOf('w') != 6) return "FAIL indexOf char: ${text.indexOf('w')}"
    if (text.indexOf('o', 5) != 7) return "FAIL char from: ${text.indexOf('o', 5)}"
    if (text.lastIndexOf('o') != 7) return "FAIL lastIndexOf char: ${text.lastIndexOf('o')}"
    if (text.lastIndexOf('o', 6) != 4) return "FAIL lastIndexOf from: ${text.lastIndexOf('o', 6)}"
    if (text.lastIndexOf('o', -1) != -1) return "FAIL lastIndexOf negative: ${text.lastIndexOf('o', -1)}"
    return "OK"
}
