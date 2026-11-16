// Reading a `String` or a `StringBuilder` through its index operator outside its contents throws
// `IndexOutOfBoundsException`: below zero, at the length, and past it. A builder's spare capacity
// is not part of its contents.

fun charOf(s: String, i: Int): String =
    try {
        "" + s[i]
    } catch (e: IndexOutOfBoundsException) {
        "!"
    }

fun charOf(sb: StringBuilder, i: Int): String =
    try {
        "" + sb[i]
    } catch (e: IndexOutOfBoundsException) {
        "!"
    }

fun box(): String {
    val text = "OK"
    var strings = ""
    for (i in intArrayOf(-1, 0, 1, 2, 3, Int.MIN_VALUE)) strings += charOf(text, i)
    if (strings != "!OK!!!") return "FAIL string: $strings"
    val sb = StringBuilder(64)
    sb.append('O').append('K')
    var builders = ""
    for (i in intArrayOf(-1, 0, 1, 2, 63, Int.MAX_VALUE)) builders += charOf(sb, i)
    if (builders != "!OK!!!") return "FAIL builder: $builders"
    return charOf(text, 0) + charOf(sb, 1)
}
